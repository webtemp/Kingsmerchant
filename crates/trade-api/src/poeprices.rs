//! Secondary ML price estimate from poeprices.info (PRD §4.4), used only in
//! detailed mode for rares where the official exact-match search is too narrow.
//!
//! `GET https://www.poeprices.info/api?l={league}&i={base64(item text)}` returns
//! a predicted `min`/`max`/`currency` plus a confidence score. Its `error` flag
//! (non-zero when it can't price the item) and its own rate limits are handled
//! gracefully: a decline is `Ok(None)`, only transport/HTTP/decoding problems
//! surface as `Err` (the UI shows no badge either way, but can log the cause).

use serde::Deserialize;

use crate::error::Error;
use crate::http::{HttpRequest, HttpResponse, HttpTransport, Method};

const BASE_URL: &str = "https://www.poeprices.info";

/// A poeprices.info ML price prediction.
#[derive(Debug, Clone, PartialEq)]
pub struct PriceEstimate {
    pub min: f64,
    pub max: f64,
    /// Currency the bounds are in (e.g. `exalted`, `divine`).
    pub currency: String,
    /// Confidence percentage (0–100), when reported.
    pub confidence: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct RawEstimate {
    /// 0 on success; non-zero means poeprices declined (bad item, not enough
    /// data, rate limited, …).
    #[serde(default)]
    error: i64,
    #[serde(default)]
    error_msg: Option<String>,
    #[serde(default)]
    min: Option<f64>,
    #[serde(default)]
    max: Option<f64>,
    #[serde(default)]
    currency: Option<String>,
    #[serde(default)]
    pred_confidence_score: Option<f64>,
}

/// Fetch an ML price estimate for `item_text` (the raw clipboard text) in
/// `league`. `Ok(None)` when poeprices can't price it; `Err` only on
/// transport/HTTP/decoding failures.
pub async fn price_estimate<T: HttpTransport>(
    transport: &T,
    league: &str,
    item_text: &str,
) -> Result<Option<PriceEstimate>, Error> {
    let url = format!(
        "{BASE_URL}/api?l={}&i={}",
        encode_query(league),
        encode_query(&base64_encode(item_text.as_bytes())),
    );
    let resp = transport
        .execute(HttpRequest {
            method: Method::Get,
            url,
            headers: Vec::new(),
            body: None,
        })
        .await?;
    parse_estimate(&resp)
}

/// Parse a poeprices response: HTTP errors → `Err`, an `error` flag → `Ok(None)`,
/// a complete prediction → `Ok(Some(..))`.
fn parse_estimate(resp: &HttpResponse) -> Result<Option<PriceEstimate>, Error> {
    if !resp.is_success() {
        return Err(Error::Api {
            status: resp.status,
            body: resp.body.clone(),
        });
    }
    let raw: RawEstimate =
        serde_json::from_str(&resp.body).map_err(|e| Error::decode("poeprices response", e))?;
    if raw.error != 0 {
        tracing::debug!(error = raw.error, msg = ?raw.error_msg, "poeprices declined to price item");
        return Ok(None);
    }
    match (raw.min, raw.max, raw.currency) {
        (Some(min), Some(max), Some(currency)) => Ok(Some(PriceEstimate {
            min,
            max,
            currency,
            confidence: raw.pred_confidence_score,
        })),
        // Success flag but missing fields — treat as "no estimate".
        _ => Ok(None),
    }
}

/// Standard base64 (with padding) of `input`. Hand-rolled to avoid a dependency
/// for the one place we need it (the poeprices `i` parameter).
fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[((n >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(n & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Percent-encode a query-string value (RFC 3986 unreserved chars pass through).
fn encode_query(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }
}
