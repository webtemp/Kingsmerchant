//! A thin, mockable HTTP seam so the client can be driven offline in tests.

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

/// Production transport: `reqwest` configured for rustls.
pub struct ReqwestTransport {
    client: reqwest::Client,
    user_agent: String,
    cookie: Option<String>,
}

impl ReqwestTransport {
    pub fn new(user_agent: impl Into<String>) -> Result<Self, Error> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| Error::Transport(e.to_string()))?;
        Ok(ReqwestTransport {
            client,
            user_agent: user_agent.into(),
            cookie: None,
        })
    }

    #[must_use]
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
            builder = builder
                .header("content-type", "application/json")
                .body(body);
        }

        let response = builder.send().await.map_err(|e| transport_error(&e))?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_string(),
                    v.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        let body = response.text().await.map_err(|e| transport_error(&e))?;
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}

/// Turn a `reqwest::Error` into an [`Error::Transport`], naming the kind and walking the cause chain.
fn transport_error(e: &reqwest::Error) -> Error {
    let kind = if e.is_builder() {
        "could not build the request (a header or URL was invalid)"
    } else if e.is_connect() {
        "could not connect to the trade site"
    } else if e.is_timeout() {
        "the request timed out"
    } else if e.is_redirect() {
        "too many redirects"
    } else if e.is_body() || e.is_decode() {
        "could not read the response body"
    } else {
        "the request failed"
    };

    let mut detail = String::new();
    let mut source = std::error::Error::source(e);
    while let Some(cause) = source {
        if !detail.is_empty() {
            detail.push_str(": ");
        }
        detail.push_str(&cause.to_string());
        source = cause.source();
    }

    if detail.is_empty() {
        Error::Transport(kind.to_string())
    } else {
        Error::Transport(format!("{kind} — {detail}"))
    }
}

/// Percent-encode a string for a URL path segment or query value (RFC 3986 unreserved pass through).
pub(crate) fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char);
            }
            _ => {
                use std::fmt::Write as _;
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{transport_error, Error};

    #[test]
    fn builder_error_message_names_the_cause() {
        let err = reqwest::Client::new()
            .get("http://example.invalid/")
            .header("cookie", "POESESSID=bad\nvalue")
            .build()
            .expect_err("an invalid header value must fail to build");
        assert!(err.is_builder());

        let Error::Transport(msg) = transport_error(&err) else {
            panic!("expected a Transport error");
        };
        assert!(
            msg.contains("could not build the request"),
            "message should name the failure mode, got: {msg}"
        );
        assert!(
            msg.to_lowercase().contains("header"),
            "message should include the underlying cause, got: {msg}"
        );
        assert_ne!(msg.to_lowercase(), "builder error");
    }
}
