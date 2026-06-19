//! Error type for the trade-api crate.

/// Anything that can go wrong talking to the official trade API.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The underlying HTTP transport failed (DNS, TLS, connection reset, …).
    #[error("HTTP transport error: {0}")]
    Transport(String),

    /// The API answered with a non-success status we don't otherwise model.
    #[error("trade API returned HTTP {status}: {body}")]
    Api { status: u16, body: String },

    /// A response body (or a definition snapshot) couldn't be decoded as the
    /// JSON shape we expect.
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
