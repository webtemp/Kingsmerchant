//! Optional Cloudflare-bypass transport.
//!
//! Cloudflare's bot-fight challenges key on the TLS/HTTP fingerprint (JA3/JA4),
//! which a plain rustls client can't match. This wraps a Chrome-emulating `wreq`
//! client (BoringSSL fingerprint + browser header set) and a `cf_clearance`
//! cookie copied from the user's browser. It's a runtime toggle: when disabled
//! it transparently delegates to the default transport, so the normal rustls
//! path is unchanged.
//!
//! Self-contained on purpose — deleting this file plus the `wreq` / `wreq-util`
//! dependencies removes the feature entirely.

use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use wreq::Client as ChromeClient;
use wreq_util::Emulation;

use crate::error::Error;
use crate::http::{HttpRequest, HttpResponse, HttpTransport, Method};

/// Live, user-editable knobs for the bypass, shared with the Settings UI so a
/// toggle takes effect without rebuilding the client.
#[derive(Debug, Default, Clone)]
pub struct ImpersonateSettings {
    /// Route requests through the Chrome-emulating client instead of the default.
    pub enabled: bool,
    /// `cf_clearance` cookie from the browser (bound to the issuing IP + UA).
    pub cf_clearance: Option<String>,
}

impl ImpersonateSettings {
    fn snapshot(lock: &RwLock<Self>) -> Self {
        lock.read().map_or_else(|_| Self::default(), |s| s.clone())
    }
}

/// Dispatches each request to either `inner` (the default transport) or a
/// Chrome-emulating `wreq` client, decided per request from [`ImpersonateSettings`].
pub struct ImpersonateTransport<T: HttpTransport> {
    inner: T,
    chrome: ChromeClient,
    settings: Arc<RwLock<ImpersonateSettings>>,
}

impl<T: HttpTransport> ImpersonateTransport<T> {
    /// Wrap `inner`, building the emulation client up front (cheap; reused).
    pub fn new(inner: T, settings: Arc<RwLock<ImpersonateSettings>>) -> Result<Self, Error> {
        // No cookie jar is configured, so cookies aren't persisted across requests;
        // we set the Cookie header (POESESSID + cf_clearance) explicitly each call.
        let chrome = ChromeClient::builder()
            .emulation(Emulation::Chrome137)
            .timeout(std::time::Duration::from_secs(15))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| {
                Error::Transport(format!("could not build the impersonation client: {e}"))
            })?;
        Ok(Self {
            inner,
            chrome,
            settings,
        })
    }
}

#[async_trait]
impl<T: HttpTransport> HttpTransport for ImpersonateTransport<T> {
    async fn execute(&self, request: HttpRequest) -> Result<HttpResponse, Error> {
        let settings = ImpersonateSettings::snapshot(&self.settings);
        if !settings.enabled {
            return self.inner.execute(request).await;
        }

        let method = match request.method {
            Method::Get => wreq::Method::GET,
            Method::Post => wreq::Method::POST,
        };
        let mut builder = self.chrome.request(method, &request.url);

        // Forward the caller's headers, folding cf_clearance into the Cookie
        // header so it rides alongside POESESSID. Crucially we do NOT set a
        // user-agent: the Chrome emulation supplies a matching one, and an
        // override would break the fingerprint cf_clearance is bound to.
        let cf = settings.cf_clearance.as_deref().filter(|c| !c.is_empty());
        let mut cookie_sent = false;
        for (key, value) in &request.headers {
            if key.eq_ignore_ascii_case("cookie") {
                cookie_sent = true;
                let merged = match cf {
                    Some(c) => format!("{value}; cf_clearance={c}"),
                    None => value.clone(),
                };
                builder = builder.header("cookie", merged);
            } else {
                builder = builder.header(key.as_str(), value.as_str());
            }
        }
        if !cookie_sent {
            if let Some(c) = cf {
                builder = builder.header("cookie", format!("cf_clearance={c}"));
            }
        }
        if let Some(body) = request.body {
            builder = builder
                .header("content-type", "application/json")
                .body(body);
        }

        let response = builder
            .send()
            .await
            .map_err(|e| Error::Transport(format!("impersonated request failed: {e}")))?;
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
        let body = response
            .text()
            .await
            .map_err(|e| Error::Transport(format!("impersonated response read failed: {e}")))?;
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}
