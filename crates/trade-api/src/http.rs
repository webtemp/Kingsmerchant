//! A thin, mockable HTTP seam so the client can be driven offline in tests.
//!
//! The trade client speaks only [`HttpTransport`]; production wires in
//! [`ReqwestTransport`] (reqwest + rustls), tests wire in a recorded-response
//! fake. Keeping the seam at "request in, response out" means the rate-limit
//! and orchestration logic is exercised without a network.

use async_trait::async_trait;

use crate::error::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl HttpResponse {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// The seam the [`TradeClient`](crate::TradeClient) talks through.
#[async_trait]
pub trait HttpTransport: Send + Sync {
    async fn execute(&self, request: HttpRequest) -> Result<HttpResponse, Error>;
}

/// Production transport: `reqwest` configured for rustls (PRD §6, "not
/// openssl").
pub struct ReqwestTransport {
    client: reqwest::Client,
    user_agent: String,
    /// Optional `Cookie:` header value. Anonymous queries work for the public
    /// data endpoints, but the live `search` POST is session-gated, so this is
    /// where a `POESESSID=…` (and any `cf_clearance`) goes.
    cookie: Option<String>,
}

impl ReqwestTransport {
    pub fn new(user_agent: impl Into<String>) -> Result<Self, Error> {
        let client = reqwest::Client::builder()
            .build()
            .map_err(|e| Error::Transport(e.to_string()))?;
        Ok(ReqwestTransport {
            client,
            user_agent: user_agent.into(),
            cookie: None,
        })
    }

    /// Attach a `Cookie:` header value sent with every request.
    pub fn with_cookie(mut self, cookie: impl Into<String>) -> Self {
        self.cookie = Some(cookie.into());
        self
    }
}

#[async_trait]
impl HttpTransport for ReqwestTransport {
    async fn execute(&self, request: HttpRequest) -> Result<HttpResponse, Error> {
        let method = match request.method {
            Method::Get => reqwest::Method::GET,
            Method::Post => reqwest::Method::POST,
        };
        let mut builder = self
            .client
            .request(method, &request.url)
            .header("user-agent", &self.user_agent);
        if let Some(cookie) = &self.cookie {
            builder = builder.header("cookie", cookie);
        }
        for (k, v) in &request.headers {
            builder = builder.header(k, v);
        }
        if let Some(body) = request.body {
            builder = builder.header("content-type", "application/json").body(body);
        }

        let response = builder
            .send()
            .await
            .map_err(|e| Error::Transport(e.to_string()))?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or_default().to_string()))
            .collect();
        let body = response
            .text()
            .await
            .map_err(|e| Error::Transport(e.to_string()))?;
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}
