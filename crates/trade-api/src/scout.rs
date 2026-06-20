//! Currency economy data from poe2scout (unofficial, <https://poe2scout.com>).
//!
//! poe2scout publishes a computed economy price per currency that tracks the
//! in-game Currency Exchange better than the official cheapest-listing API. We
//! cache hard, send a contact User-Agent, and fail gracefully.

use serde::Deserialize;

use crate::error::Error;
use crate::http::{percent_encode, HttpRequest, HttpResponse, HttpTransport, Method};

/// Default poe2scout API host (scheme + host, no trailing slash).
pub const DEFAULT_BASE_URL: &str = "https://api.poe2scout.com";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ScoutLeague {
    #[serde(rename = "Value")]
    pub league_name: String,
    /// Exalted Orbs per Divine Orb. Optional for low-volume leagues.
    #[serde(default)]
    pub divine_price: Option<f64>,
    #[serde(default)]
    pub is_current: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ScoutCurrency {
    pub api_id: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub category_api_id: Option<String>,
    /// Computed price, in the requested `ReferenceCurrency`.
    pub current_price: f64,
    #[serde(default)]
    pub price_logs: Vec<Option<ScoutPriceLog>>,
    #[serde(default)]
    pub icon_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ScoutPriceLog {
    #[serde(default)]
    pub time: Option<String>,
    #[serde(default)]
    pub price: Option<f64>,
    #[serde(default)]
    pub quantity: Option<f64>,
}

/// A currency priced from poe2scout, in both Exalted and Divine.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoutPrice {
    pub api_id: String,
    pub text: Option<String>,
    pub icon_url: Option<String>,
    pub exalted: f64,
    pub divine: Option<f64>,
    /// Exalted-per-Divine rate used for the conversion.
    pub divine_price: Option<f64>,
    pub low: Option<f64>,
    pub high: Option<f64>,
    /// Latest listed quantity, a rough liquidity hint.
    pub volume: Option<f64>,
    pub as_of: Option<String>,
}

/// Turn a display name into poe2scout's `ApiId` slug.
/// `"Farrul's Rune of the Chase"` → `farruls-rune-of-the-chase`.
#[must_use]
pub fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut pending_hyphen = false;
    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_hyphen && !out.is_empty() {
                out.push('-');
            }
            pending_hyphen = false;
            out.push(ch.to_ascii_lowercase());
        } else if ch.is_alphanumeric() {
            if pending_hyphen && !out.is_empty() {
                out.push('-');
            }
            pending_hyphen = false;
            out.extend(ch.to_lowercase());
        } else if ch.is_whitespace() || ch == '-' || ch == '_' {
            pending_hyphen = true;
        }
    }
    out
}

pub fn parse_leagues(json: &str) -> Result<Vec<ScoutLeague>, Error> {
    serde_json::from_str(json).map_err(|e| Error::decode("poe2scout leagues", e))
}

/// The Divine rate for `league`: match its name, else the `IsCurrent` league.
#[must_use]
pub fn resolve_divine_price(leagues: &[ScoutLeague], league: &str) -> Option<f64> {
    leagues
        .iter()
        .find(|l| l.league_name.trim().eq_ignore_ascii_case(league.trim()))
        .or_else(|| leagues.iter().find(|l| l.is_current))
        .and_then(|l| l.divine_price)
}

/// Parse a `Currencies/{ApiId}` body (in Exalted) into a [`ScoutPrice`].
pub fn parse_currency(json: &str, divine_price: Option<f64>) -> Result<ScoutPrice, Error> {
    let raw: ScoutCurrency =
        serde_json::from_str(json).map_err(|e| Error::decode("poe2scout currency", e))?;
    Ok(price_from_currency(&raw, divine_price))
}

/// Build a [`ScoutPrice`] from a raw currency (in Exalted), converting to Divine.
#[must_use]
pub fn price_from_currency(raw: &ScoutCurrency, divine_price: Option<f64>) -> ScoutPrice {
    let exalted = raw.current_price;
    let divine = divine_price.filter(|d| *d > 0.0).map(|d| exalted / d);

    let logs: Vec<&ScoutPriceLog> = raw.price_logs.iter().flatten().collect();
    let prices: Vec<f64> = logs
        .iter()
        .filter_map(|l| l.price)
        .filter(|p| p.is_finite())
        .collect();
    let low = prices.iter().copied().reduce(f64::min);
    let high = prices.iter().copied().reduce(f64::max);
    let volume = logs.iter().rev().find_map(|l| l.quantity);
    let as_of = logs
        .iter()
        .rev()
        .find_map(|l| l.time.clone())
        .or_else(|| logs.iter().find_map(|l| l.time.clone()));

    ScoutPrice {
        api_id: raw.api_id.clone(),
        text: raw.text.clone(),
        icon_url: raw.icon_url.clone(),
        exalted,
        divine,
        divine_price: divine_price.filter(|d| *d > 0.0),
        low,
        high,
        volume,
        as_of,
    }
}

/// Fetch the league list (for the Divine rate).
pub async fn fetch_leagues<T: HttpTransport>(
    transport: &T,
    base_url: &str,
) -> Result<Vec<ScoutLeague>, Error> {
    let resp = get(transport, &format!("{base_url}/poe2/Leagues")).await?;
    let resp = ok_or_api_error(resp)?;
    parse_leagues(&resp.body)
}

/// Fetch one currency by its slug `api_id`, priced in `reference`. `Ok(None)`
/// when the slug isn't a known currency.
pub async fn fetch_currency<T: HttpTransport>(
    transport: &T,
    base_url: &str,
    league: &str,
    api_id: &str,
    reference: &str,
) -> Result<Option<ScoutCurrency>, Error> {
    let url = format!(
        "{base_url}/poe2/Leagues/{}/Currencies/{}?ReferenceCurrency={}",
        percent_encode(league),
        percent_encode(api_id),
        percent_encode(reference),
    );
    let resp = get(transport, &url).await?;
    // 400/404 both mean "unknown id, try the next candidate".
    if resp.status == 404 || resp.status == 400 {
        return Ok(None);
    }
    let resp = ok_or_api_error(resp)?;
    let cur: ScoutCurrency =
        serde_json::from_str(&resp.body).map_err(|e| Error::decode("poe2scout currency", e))?;
    Ok(Some(cur))
}

async fn get<T: HttpTransport>(transport: &T, url: &str) -> Result<HttpResponse, Error> {
    transport
        .execute(HttpRequest {
            method: Method::Get,
            url: url.to_string(),
            headers: Vec::new(),
            body: None,
        })
        .await
}

fn ok_or_api_error(resp: HttpResponse) -> Result<HttpResponse, Error> {
    if resp.is_success() {
        Ok(resp)
    } else {
        Err(Error::Api {
            status: resp.status,
            body: resp.body,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_matches_poe2scout_ids() {
        assert_eq!(slugify("Preserved Cranium"), "preserved-cranium");
        assert_eq!(
            slugify("Farrul's Rune of the Chase"),
            "farruls-rune-of-the-chase"
        );
        assert_eq!(slugify("Exalted Orb"), "exalted-orb");
        assert_eq!(slugify("Divine Orb"), "divine-orb");
        assert_eq!(slugify("Maven's Writ"), "mavens-writ");
        assert_eq!(slugify("  Chaos   Orb  "), "chaos-orb");
        assert_eq!(slugify("Vaal—Orb"), "vaalorb");
    }

    #[test]
    fn resolve_divine_price_prefers_name_then_current() {
        let leagues = vec![
            ScoutLeague {
                league_name: "Standard".into(),
                divine_price: Some(100.0),
                is_current: false,
            },
            ScoutLeague {
                league_name: "Rise of the Abyssal".into(),
                divine_price: Some(327.0),
                is_current: true,
            },
        ];
        assert_eq!(
            resolve_divine_price(&leagues, "Rise of the Abyssal"),
            Some(327.0)
        );
        assert_eq!(resolve_divine_price(&leagues, "Standard"), Some(100.0));
        assert_eq!(resolve_divine_price(&leagues, "Nonexistent"), Some(327.0));
    }

    #[test]
    fn parse_leagues_reads_divine_price() {
        let json = r#"[
            {"Value":"Runes of Aldur","DivinePrice":191.2,"IsCurrent":true},
            {"Value":"Standard","DivinePrice":425.5,"IsCurrent":false}
        ]"#;
        let leagues = parse_leagues(json).unwrap();
        assert_eq!(leagues.len(), 2);
        assert_eq!(leagues[0].league_name, "Runes of Aldur");
        assert!(leagues[0].is_current);
        assert_eq!(leagues[0].divine_price, Some(191.2));
        assert_eq!(
            resolve_divine_price(&leagues, "Runes of Aldur"),
            Some(191.2)
        );
    }

    #[test]
    fn parse_currency_converts_exalted_to_divine_and_finds_range() {
        let json = r#"{
            "ApiId":"divine-orb","Text":"Divine Orb","CategoryApiId":"currency",
            "CurrentPrice":327.0,"IconUrl":"https://img/divine.png",
            "PriceLogs":[
                {"Time":"2026-06-18T00:00:00Z","Price":320.0,"Quantity":12},
                null,
                {"Time":"2026-06-18T06:00:00Z","Price":330.0,"Quantity":8}
            ]
        }"#;
        let p = parse_currency(json, Some(327.0)).unwrap();
        assert_eq!(p.api_id, "divine-orb");
        assert_eq!(p.exalted, 327.0);
        assert_eq!(p.divine, Some(1.0));
        assert_eq!(p.low, Some(320.0));
        assert_eq!(p.high, Some(330.0));
        assert_eq!(p.volume, Some(8.0));
        assert_eq!(p.as_of.as_deref(), Some("2026-06-18T06:00:00Z"));
    }

    #[test]
    fn parse_currency_without_divine_rate_leaves_divine_none() {
        let json = r#"{"ApiId":"chaos-orb","CurrentPrice":2.5,"PriceLogs":[]}"#;
        let p = parse_currency(json, None).unwrap();
        assert_eq!(p.exalted, 2.5);
        assert_eq!(p.divine, None);
        assert_eq!(p.divine_price, None);
        assert_eq!(p.low, None);
        assert_eq!(p.high, None);
        let p0 = parse_currency(json, Some(0.0)).unwrap();
        assert_eq!(p0.divine, None);
    }
}
