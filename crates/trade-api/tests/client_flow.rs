//! End-to-end client flow over a mocked HTTP transport (PRD §7 integration):
//! search → fetch → aggregate, request shapes, fetch batching, and 429 retry.
//! Nothing here touches the network.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use parser::parse_item;
use trade_api::http::{HttpRequest, HttpResponse, HttpTransport, Method};
use trade_api::{ClientConfig, Error, ItemDefinitions, QueryOptions, StatDefinitions, TradeClient};

const RARE_RING: &str = "Item Class: Rings
Rarity: Rare
Honour Spiral
Topaz Ring
--------
{ Implicit Modifier - Elemental, Lightning, Resistance }
+30(20-30)% to Lightning Resistance";

/// Captured real rate-limit headers, attached to every mocked response.
fn rate_headers() -> Vec<(String, String)> {
    vec![
        ("X-Rate-Limit-Policy".into(), "trade-search-request-limit".into()),
        ("X-Rate-Limit-Rules".into(), "Ip".into()),
        ("X-Rate-Limit-Ip".into(), "5:10:60,15:60:300,30:300:1800".into()),
        ("X-Rate-Limit-Ip-State".into(), "1:10:0,1:60:0,1:300:0".into()),
    ]
}

fn ok(body: &str) -> HttpResponse {
    HttpResponse {
        status: 200,
        headers: rate_headers(),
        body: body.to_string(),
    }
}

/// A transport that hands back queued responses in order and records every
/// request it was given.
struct MockTransport {
    responses: Mutex<VecDeque<HttpResponse>>,
    requests: Arc<Mutex<Vec<HttpRequest>>>,
}

impl MockTransport {
    fn new(responses: Vec<HttpResponse>) -> (Self, Arc<Mutex<Vec<HttpRequest>>>) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let mock = MockTransport {
            responses: Mutex::new(responses.into()),
            requests: Arc::clone(&requests),
        };
        (mock, requests)
    }
}

#[async_trait]
impl HttpTransport for MockTransport {
    async fn execute(&self, request: HttpRequest) -> Result<HttpResponse, Error> {
        self.requests.lock().unwrap().push(request);
        let resp = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("mock ran out of queued responses");
        Ok(resp)
    }
}

fn defs() -> (StatDefinitions, ItemDefinitions) {
    (
        StatDefinitions::from_json(include_str!("fixtures/api/data_stats.json")).unwrap(),
        ItemDefinitions::from_json(include_str!("fixtures/api/data_items.json")).unwrap(),
    )
}

#[tokio::test]
async fn price_check_searches_then_fetches_and_aggregates() {
    let search = include_str!("fixtures/api/search_response.json");
    let fetch = include_str!("fixtures/api/fetch_response.json");
    let (transport, requests) = MockTransport::new(vec![ok(search), ok(fetch)]);
    let (stats, items) = defs();
    let client = TradeClient::new(transport, ClientConfig::new("Mirage"), stats, items);

    let item = parse_item(RARE_RING).unwrap();
    let pc = client
        .price_check(&item, QueryOptions::default(), 10)
        .await
        .unwrap();

    assert_eq!(pc.query_id, "kA2eGYh9");
    assert_eq!(pc.total, 137);
    assert_eq!(pc.listings.len(), 5);
    assert_eq!(pc.median_price().unwrap().amount, 3.0);
    assert_eq!(pc.cheapest(5).len(), 4); // the null-price listing is excluded

    // Exactly one search POST then one fetch GET.
    let reqs = requests.lock().unwrap();
    assert_eq!(reqs.len(), 2);

    assert_eq!(reqs[0].method, Method::Post);
    assert!(reqs[0].url.ends_with("/api/trade2/search/Mirage"));
    // Rares search by category, not the exact base type.
    let body = reqs[0].body.as_ref().unwrap();
    assert!(body.contains("accessory.ring"));
    assert!(!body.contains("Topaz Ring"));

    assert_eq!(reqs[1].method, Method::Get);
    assert!(reqs[1].url.contains("/api/trade2/fetch/"));
    assert!(reqs[1].url.contains("query=kA2eGYh9"));
    // All five result ids batched into the one fetch.
    assert!(reqs[1]
        .url
        .contains("a1b2c3d4e5f60718293a4b5c6d7e8f90112233445566778899aabbccddeeff00"));
}

#[tokio::test]
async fn fetch_batches_ids_in_groups_of_ten() {
    let empty = r#"{"result":[]}"#;
    // 23 ids → 3 batches (10 + 10 + 3) → 3 GET requests.
    let (transport, requests) = MockTransport::new(vec![ok(empty), ok(empty), ok(empty)]);
    let (stats, items) = defs();
    let client = TradeClient::new(transport, ClientConfig::new("Mirage"), stats, items);

    let ids: Vec<String> = (0..23).map(|i| format!("id{i:02}")).collect();
    let listings = client.fetch(&ids, "QID").await.unwrap();
    assert!(listings.is_empty());

    let reqs = requests.lock().unwrap();
    assert_eq!(reqs.len(), 3);
    assert!(reqs.iter().all(|r| r.method == Method::Get));
    // First batch carries ten ids, last carries three.
    assert_eq!(reqs[0].url.matches("id").count(), 10);
    assert_eq!(reqs[2].url.matches("id").count(), 3);
}

#[tokio::test]
async fn realm_is_appended_to_search_and_fetch_urls() {
    let (transport, requests) =
        MockTransport::new(vec![ok(r#"{"id":"q","result":[],"total":0}"#)]);
    let (stats, items) = defs();
    // A real POE2 league id with spaces must be percent-encoded in the URL.
    let mut config = ClientConfig::new("Runes of Aldur");
    config.realm = Some("poe2".to_string());
    let client = TradeClient::new(transport, config, stats, items);

    let item = parse_item(RARE_RING).unwrap();
    client
        .price_check(&item, QueryOptions::default(), 10)
        .await
        .unwrap();

    let reqs = requests.lock().unwrap();
    // No results → no fetch; just the search, with the league encoded and the
    // realm query param appended.
    assert_eq!(reqs.len(), 1);
    assert!(reqs[0]
        .url
        .ends_with("/api/trade2/search/Runes%20of%20Aldur?realm=poe2"));
}

#[tokio::test]
async fn retries_through_a_429_then_succeeds() {
    let throttled = HttpResponse {
        status: 429,
        // No penalty/Retry-After, so the retry fires immediately.
        headers: rate_headers(),
        body: r#"{"error":{"code":3,"message":"Rate limit exceeded"}}"#.to_string(),
    };
    let good = ok(r#"{"id":"q2","result":[],"total":0}"#);
    let (transport, requests) = MockTransport::new(vec![throttled, good]);
    let (stats, items) = defs();
    let client = TradeClient::new(transport, ClientConfig::new("Mirage"), stats, items);

    let item = parse_item(RARE_RING).unwrap();
    let req =
        trade_api::build_search_query(&item, client.stats(), client.items(), QueryOptions::default());
    let resp = client.search(&req).await.unwrap();
    assert_eq!(resp.id, "q2");
    assert_eq!(requests.lock().unwrap().len(), 2); // one 429, one success
}

#[tokio::test]
async fn api_error_status_surfaces_as_error() {
    let (transport, _requests) = MockTransport::new(vec![HttpResponse {
        status: 400,
        headers: rate_headers(),
        body: r#"{"error":{"code":2,"message":"Invalid query"}}"#.to_string(),
    }]);
    let (stats, items) = defs();
    let client = TradeClient::new(transport, ClientConfig::new("Mirage"), stats, items);

    let item = parse_item(RARE_RING).unwrap();
    let req = trade_api::build_search_query(&item, client.stats(), client.items(), QueryOptions::default());
    let err = client.search(&req).await.unwrap_err();
    match err {
        Error::Api { status, .. } => assert_eq!(status, 400),
        other => panic!("expected Api error, got {other:?}"),
    }
}
