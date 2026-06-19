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

/// Production transport: `reqwest` configured for rustls (not openssl).
pub struct ReqwestTransport {
    client: reqwest::Client,
    user_agent: String,
    /// Optional `Cookie:` header value, reserved for later auth (e.g. a
    /// `POESESSID=…`). Anonymous queries work for search and fetch.
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

/// Turn a `reqwest::Error` into an [`Error::Transport`] whose message both names
/// the failure mode and includes the underlying cause. reqwest's own `Display`
/// is often unhelpfully terse — a malformed header surfaces only as "builder
/// error" — because the real reason sits in the error's `source()` chain, which
/// `to_string()` drops. We name the kind and walk that chain.
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

    // Walk the cause chain so a wrapped reason (e.g. "failed to parse header
    // value" behind a builder error) actually reaches the message.
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

/// Percent-encode a string for use in a URL path segment or query value
/// (RFC 3986 unreserved chars pass through; everything else becomes `%XX`).
/// League ids like `Runes of Aldur` carry spaces that would otherwise produce
/// an invalid URL.
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
        // A header value with a newline is what a malformed POESESSID paste used
        // to produce — reqwest reports it as a terse "builder error". Build one
        // for real and check our message says more than that.
        let err = reqwest::Client::new()
            .get("http://example.invalid/")
            .header("cookie", "POESESSID=bad\nvalue")
            .build()
            .expect_err("an invalid header value must fail to build");
        assert!(err.is_builder());

        let Error::Transport(msg) = transport_error(&err) else {
            panic!("expected a Transport error");
        };
        // Names the failure mode...
        assert!(
            msg.contains("could not build the request"),
            "message should name the failure mode, got: {msg}"
        );
        // ...and surfaces the underlying cause rather than just "builder error".
        assert!(
            msg.to_lowercase().contains("header"),
            "message should include the underlying cause, got: {msg}"
        );
        assert_ne!(msg.to_lowercase(), "builder error");
    }
}
