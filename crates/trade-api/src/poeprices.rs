//! Secondary ML price estimate from poeprices.info, for detailed-mode rares.

use serde::Deserialize;

use crate::error::Error;
use crate::http::{HttpRequest, HttpResponse, HttpTransport, Method};

const BASE_URL: &str = "https://www.poeprices.info";

#[derive(Debug, Clone, PartialEq)]
pub struct PriceEstimate {
    pub min: f64,
    pub max: f64,
    pub currency: String,
    pub confidence: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct RawEstimate {
    /// 0 on success; non-zero means poeprices declined.
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

/// `Ok(None)` when poeprices can't price it; `Err` only on transport failures.
pub async fn price_estimate<T: HttpTransport>(
    transport: &T,
    league: &str,
    item_text: &str,
) -> Result<Option<PriceEstimate>, Error> {
    let url = format!(
        "{BASE_URL}/api?l={}&i={}",
        crate::http::percent_encode(league),
        crate::http::percent_encode(&base64_encode(item_text.as_bytes())),
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
        _ => Ok(None),
    }
}

/// Standard base64 (with padding) of `input`.
fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = u32::from(*chunk.get(1).unwrap_or(&0));
        let b2 = u32::from(*chunk.get(2).unwrap_or(&0));
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
