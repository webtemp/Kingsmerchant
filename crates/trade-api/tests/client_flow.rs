//! End-to-end client flow over a mocked HTTP transport.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use parser::parse_item;
use trade_api::http::{HttpRequest, HttpResponse, HttpTransport, Method};
use trade_api::{
    ClientConfig, CurrencyDefinitions, Error, ItemDefinitions, QueryOptions, SessionStatus,
    StatDefinitions, TradeClient,
};

const RARE_RING: &str = "Item Class: Rings
Rarity: Rare
Honour Spiral
Topaz Ring
--------
{ Implicit Modifier - Elemental, Lightning, Resistance }
+30(20-30)% to Lightning Resistance";

fn rate_headers() -> Vec<(String, String)> {
    vec![
        (
            "X-Rate-Limit-Policy".into(),
            "trade-search-request-limit".into(),
        ),
        ("X-Rate-Limit-Rules".into(), "Ip".into()),
        (
            "X-Rate-Limit-Ip".into(),
            "5:10:60,15:60:300,30:300:1800".into(),
        ),
        (
            "X-Rate-Limit-Ip-State".into(),
            "1:10:0,1:60:0,1:300:0".into(),
        ),
    ]
}

fn ok(body: &str) -> HttpResponse {
    HttpResponse {
        status: 200,
        headers: rate_headers(),
        body: body.to_string(),
    }
}

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
    let client = TradeClient::new(
        transport,
        ClientConfig::new("Mirage"),
        stats,
        items,
        CurrencyDefinitions::default(),
    );

    let item = parse_item(RARE_RING).unwrap();
    let pc = client
        .price_check(&item, QueryOptions::default(), 10)
        .await
        .unwrap();

    assert_eq!(pc.query_id, "kA2eGYh9");
    assert_eq!(pc.total, 137);
    assert_eq!(pc.listings.len(), 5);
    assert_eq!(pc.median_price().unwrap().amount, 3.0);
    assert_eq!(pc.cheapest(5).len(), 4);

    let reqs = requests.lock().unwrap();
    assert_eq!(reqs.len(), 2);

    assert_eq!(reqs[0].method, Method::Post);
    assert!(reqs[0].url.ends_with("/api/trade2/search/Mirage"));
    let body = reqs[0].body.as_ref().unwrap();
    assert!(body.contains("accessory.ring"));
    assert!(!body.contains("Topaz Ring"));

    assert_eq!(reqs[1].method, Method::Get);
    assert!(reqs[1].url.contains("/api/trade2/fetch/"));
    assert!(reqs[1].url.contains("query=kA2eGYh9"));
    assert!(reqs[1]
        .url
        .contains("a1b2c3d4e5f60718293a4b5c6d7e8f90112233445566778899aabbccddeeff00"));
}

#[tokio::test]
async fn fetch_batches_ids_in_groups_of_ten() {
    let empty = r#"{"result":[]}"#;
    let (transport, requests) = MockTransport::new(vec![ok(empty), ok(empty), ok(empty)]);
    let (stats, items) = defs();
    let client = TradeClient::new(
        transport,
        ClientConfig::new("Mirage"),
        stats,
        items,
        CurrencyDefinitions::default(),
    );

    let ids: Vec<String> = (0..23).map(|i| format!("id{i:02}")).collect();
    let listings = client.fetch(&ids, "QID").await.unwrap();
    assert!(listings.is_empty());

    let reqs = requests.lock().unwrap();
    assert_eq!(reqs.len(), 3);
    assert!(reqs.iter().all(|r| r.method == Method::Get));
    assert_eq!(reqs[0].url.matches("id").count(), 10);
    assert_eq!(reqs[2].url.matches("id").count(), 3);
}

#[tokio::test]
async fn realm_is_appended_to_search_and_fetch_urls() {
    let (transport, requests) = MockTransport::new(vec![ok(r#"{"id":"q","result":[],"total":0}"#)]);
    let (stats, items) = defs();
    let mut config = ClientConfig::new("Runes of Aldur");
    config.realm = Some("poe2".to_string());
    let client = TradeClient::new(
        transport,
        config,
        stats,
        items,
        CurrencyDefinitions::default(),
    );

    let item = parse_item(RARE_RING).unwrap();
    client
        .price_check(&item, QueryOptions::default(), 10)
        .await
        .unwrap();

    let reqs = requests.lock().unwrap();
    assert_eq!(reqs.len(), 1);
    assert!(reqs[0]
        .url
        .ends_with("/api/trade2/search/Runes%20of%20Aldur?realm=poe2"));
}

#[tokio::test]
async fn retries_through_a_429_then_succeeds() {
    let throttled = HttpResponse {
        status: 429,
        headers: rate_headers(),
        body: r#"{"error":{"code":3,"message":"Rate limit exceeded"}}"#.to_string(),
    };
    let good = ok(r#"{"id":"q2","result":[],"total":0}"#);
    let (transport, requests) = MockTransport::new(vec![throttled, good]);
    let (stats, items) = defs();
    let client = TradeClient::new(
        transport,
        ClientConfig::new("Mirage"),
        stats,
        items,
        CurrencyDefinitions::default(),
    );

    let item = parse_item(RARE_RING).unwrap();
    let req = trade_api::build_search_query(
        &item,
        client.stats(),
        client.items(),
        QueryOptions::default(),
    );
    let resp = client.search(&req).await.unwrap();
    assert_eq!(resp.id, "q2");
    assert_eq!(requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn api_error_status_surfaces_as_error() {
    let (transport, _requests) = MockTransport::new(vec![HttpResponse {
        status: 400,
        headers: rate_headers(),
        body: r#"{"error":{"code":2,"message":"Invalid query"}}"#.to_string(),
    }]);
    let (stats, items) = defs();
    let client = TradeClient::new(
        transport,
        ClientConfig::new("Mirage"),
        stats,
        items,
        CurrencyDefinitions::default(),
    );

    let item = parse_item(RARE_RING).unwrap();
    let req = trade_api::build_search_query(
        &item,
        client.stats(),
        client.items(),
        QueryOptions::default(),
    );
    let err = client.search(&req).await.unwrap_err();
    match err {
        Error::Api { status, .. } => assert_eq!(status, 400),
        other => panic!("expected Api error, got {other:?}"),
    }
}

fn client_with(transport: MockTransport) -> TradeClient<MockTransport> {
    let (stats, items) = defs();
    TradeClient::new(
        transport,
        ClientConfig::new("Mirage"),
        stats,
        items,
        CurrencyDefinitions::default(),
    )
}

#[tokio::test]
async fn malformed_poesessid_is_dropped_well_formed_is_sent() {
    let (transport, requests) = MockTransport::new(vec![ok(r#"{"id":"q","result":[],"total":0}"#)]);
    let client = client_with(transport);

    client.set_poesessid(Some("POESESSID=deadbeef".to_string()));
    assert!(
        !client.has_poesessid(),
        "a malformed session must not be stored"
    );

    let good = "0123456789abcdef0123456789abcdef";
    client.set_poesessid(Some(good.to_string()));
    assert!(client.has_poesessid());

    let item = parse_item(RARE_RING).unwrap();
    let req = trade_api::build_search_query(
        &item,
        client.stats(),
        client.items(),
        QueryOptions::default(),
    );
    client.search(&req).await.unwrap();

    let reqs = requests.lock().unwrap();
    assert!(reqs[0].headers.iter().any(|(k, v)| {
        k.eq_ignore_ascii_case("cookie") && v.contains(&format!("POESESSID={good}"))
    }));
}

#[tokio::test]
async fn validate_session_reports_valid_with_account() {
    let (transport, requests) =
        MockTransport::new(vec![ok(r#"{"name":"ExileBro","realm":"poe2"}"#)]);
    let client = client_with(transport);
    client.set_poesessid(Some("0123456789abcdef0123456789abcdef".to_string()));

    match client.validate_session().await {
        SessionStatus::Valid { account } => assert_eq!(account.as_deref(), Some("ExileBro")),
        other => panic!("expected Valid, got {other:?}"),
    }
    let reqs = requests.lock().unwrap();
    assert!(reqs[0].url.ends_with("/api/profile"));
    assert!(reqs[0]
        .headers
        .iter()
        .any(|(k, v)| k.eq_ignore_ascii_case("cookie") && v.contains("POESESSID=")));
}

#[tokio::test]
async fn validate_session_reports_invalid_on_401() {
    let denied = HttpResponse {
        status: 401,
        headers: rate_headers(),
        body: "Unauthorized".to_string(),
    };
    let (transport, _requests) = MockTransport::new(vec![denied]);
    let client = client_with(transport);
    client.set_poesessid(Some("0123456789abcdef0123456789abcdef".to_string()));
    assert_eq!(client.validate_session().await, SessionStatus::Invalid);
}

#[tokio::test]
async fn validate_session_without_a_session_does_not_call_the_network() {
    let (transport, requests) = MockTransport::new(vec![]);
    let client = client_with(transport);
    assert_eq!(client.validate_session().await, SessionStatus::Invalid);
    assert!(requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn scout_price_resolves_divine_rate_then_prices_currency() {
    let leagues = include_str!("fixtures/api/scout_leagues.json");
    let currency = include_str!("fixtures/api/scout_currency.json");
    let (transport, requests) = MockTransport::new(vec![ok(leagues), ok(currency)]);
    let client = client_with(transport);

    let price = client
        .scout_price("preserved-cranium", "Preserved Cranium")
        .await
        .unwrap()
        .expect("a known currency is priced");

    assert_eq!(price.api_id, "preserved-cranium");
    assert_eq!(price.exalted, 654.0);
    assert_eq!(price.divine, Some(2.0));
    assert_eq!(price.divine_price, Some(327.0));
    assert_eq!(price.low, Some(640.0));
    assert_eq!(price.high, Some(668.0));

    let reqs = requests.lock().unwrap();
    assert_eq!(reqs.len(), 2);
    assert!(reqs[0].url.ends_with("/poe2/Leagues"));
    assert!(reqs[1].url.contains("/Currencies/preserved-cranium"));
    assert!(reqs[1].url.contains("ReferenceCurrency=exalted"));
}

#[tokio::test]
async fn scout_price_caches_within_ttl() {
    let leagues = include_str!("fixtures/api/scout_leagues.json");
    let currency = include_str!("fixtures/api/scout_currency.json");
    let (transport, requests) = MockTransport::new(vec![ok(leagues), ok(currency)]);
    let client = client_with(transport);

    let first = client
        .scout_price("preserved-cranium", "Preserved Cranium")
        .await
        .unwrap();
    let second = client
        .scout_price("preserved-cranium", "Preserved Cranium")
        .await
        .unwrap();
    assert_eq!(first, second);
    assert_eq!(requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn scout_price_returns_none_when_not_indexed() {
    let unknown = HttpResponse {
        status: 400,
        headers: rate_headers(),
        body: r#"{"detail":"unknown currency"}"#.to_string(),
    };
    let (transport, requests) = MockTransport::new(vec![
        ok(include_str!("fixtures/api/scout_leagues.json")),
        unknown,
    ]);
    let client = client_with(transport);

    let result = client
        .scout_price("totally-made-up-orb", "Totally Made Up Orb")
        .await
        .unwrap();
    assert!(result.is_none());
    assert_eq!(requests.lock().unwrap().len(), 2);
}
