//! poeprices.info ML estimate over a mocked transport.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use trade_api::http::{HttpRequest, HttpResponse, HttpTransport, Method};
use trade_api::poeprices::price_estimate;
use trade_api::Error;

struct OneShot {
    response: HttpResponse,
    seen: Arc<Mutex<Vec<HttpRequest>>>,
}

impl OneShot {
    fn new(response: HttpResponse) -> (Self, Arc<Mutex<Vec<HttpRequest>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        (
            OneShot {
                response,
                seen: Arc::clone(&seen),
            },
            seen,
        )
    }
}

#[async_trait]
impl HttpTransport for OneShot {
    async fn execute(&self, request: HttpRequest) -> Result<HttpResponse, Error> {
        self.seen.lock().unwrap().push(request);
        Ok(self.response.clone())
    }
}

fn ok(body: &str) -> HttpResponse {
    HttpResponse {
        status: 200,
        headers: Vec::new(),
        body: body.to_string(),
    }
}

#[tokio::test]
async fn successful_prediction_parses_and_builds_the_url() {
    let (transport, seen) = OneShot::new(ok(include_str!("fixtures/api/poeprices_estimate.json")));
    let est = price_estimate(&transport, "Standard", "Test")
        .await
        .unwrap()
        .expect("a prediction");

    assert_eq!(est.min, 1.0);
    assert_eq!(est.max, 2.5);
    assert_eq!(est.currency, "divine");
    assert_eq!(est.confidence, Some(84.21));

    let req = &seen.lock().unwrap()[0];
    assert_eq!(req.method, Method::Get);
    assert_eq!(
        req.url,
        "https://www.poeprices.info/api?l=Standard&i=VGVzdA%3D%3D"
    );
}

#[tokio::test]
async fn league_with_spaces_is_percent_encoded() {
    let (transport, seen) = OneShot::new(ok(include_str!("fixtures/api/poeprices_estimate.json")));
    let _ = price_estimate(&transport, "Runes of Aldur", "x")
        .await
        .unwrap();
    let url = &seen.lock().unwrap()[0].url;
    assert!(url.contains("l=Runes%20of%20Aldur"), "got {url}");
}

#[tokio::test]
async fn a_decline_is_none_not_an_error() {
    let (transport, _) = OneShot::new(ok(include_str!("fixtures/api/poeprices_declined.json")));
    let est = price_estimate(&transport, "Standard", "Test")
        .await
        .unwrap();
    assert!(est.is_none());
}

#[tokio::test]
async fn http_error_surfaces_as_err() {
    let (transport, _) = OneShot::new(HttpResponse {
        status: 429,
        headers: Vec::new(),
        body: "rate limited".to_string(),
    });
    let result = price_estimate(&transport, "Standard", "Test").await;
    assert!(matches!(result, Err(Error::Api { status: 429, .. })));
}
