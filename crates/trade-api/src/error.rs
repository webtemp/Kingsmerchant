//! Error type for the trade-api crate.

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HTTP transport error: {0}")]
    Transport(String),

    #[error("trade API returned HTTP {status}: {body}")]
    Api { status: u16, body: String },

    #[error(
        "Cloudflare bot-check (HTTP {status}) — backing off ~30s. If it keeps \
         happening, enable the Cloudflare bypass in Settings."
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
