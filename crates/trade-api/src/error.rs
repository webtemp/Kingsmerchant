//! Error type for the trade-api crate.

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HTTP transport error: {0}")]
    Transport(String),

    #[error("trade API returned HTTP {status}: {body}")]
    Api { status: u16, body: String },

    #[error(
        "Cloudflare is challenging requests (HTTP {status}). The trade site blocks \
         automated traffic when it sees too many requests in a short time — wait a \
         few minutes, then try again."
    )]
    Cloudflare { status: u16 },

    #[error("failed to decode {what}: {source}")]
    Decode {
        what: &'static str,
        source: serde_json::Error,
    },
}

impl Error {
    pub(crate) fn decode(what: &'static str, source: serde_json::Error) -> Self {
        Error::Decode { what, source }
    }
}
