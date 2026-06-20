//! Currency economy data from **poe2scout** (an unofficial community price
//! index, <https://poe2scout.com>).
//!
//! The official `trade2/exchange` bulk API prices a stackable from its
//! cheapest live listing, which in practice is dominated by scam/bait offers and
//! reads far below the real trading value. poe2scout instead publishes a
//! computed economy price per currency (a mid/last value plus a recent
//! price-log series), which matched the in-game Currency Exchange far more
//! closely in testing — so we price currency from here and keep the official
//! exchange only as an "open on the trade site" link.
//!
//! This is an **unofficial** API: we cache hard (see the client), send a real
//! contact User-Agent, and fail gracefully (the UI falls back to the official
//! exchange when poe2scout is down or doesn't know the currency).
//!
//! Endpoints used (base `https://api.poe2scout.com`, realm `poe2`):
//! - `GET /poe2/Leagues` → leagues with their `DivinePrice` (Exalted per Divine)
//!   and an `IsCurrent` flag.
//! - `GET /poe2/Leagues/{LeagueName}/Currencies/{ApiId}?ReferenceCurrency=…` →
//!   one currency's `CurrentPrice` (in the reference currency) plus its
//!   `PriceLogs`. `ApiId` is the official `data/static` exchange id (`divine`,
//!   `exalted`, …) — verified to match for the entire indexed catalogue — so
//!   the caller keys on that.

use serde::Deserialize;

use crate::error::Error;
use crate::http::{percent_encode, HttpRequest, HttpResponse, HttpTransport, Method};

/// Default poe2scout API host (scheme + host, no trailing slash).
pub const DEFAULT_BASE_URL: &str = "https://api.poe2scout.com";

// ---- Raw response shapes -------------------------------------------------

/// One league from `GET /poe2/Leagues`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ScoutLeague {
    /// League name (the API calls this field `Value`), used verbatim
    /// (URL-encoded) as the path segment. Matches the app's resolved
    /// `config.league`.
    #[serde(rename = "Value")]
    pub league_name: String,
    /// Exalted Orbs per Divine Orb, for exalted↔divine conversion. Optional —
    /// a league without trading volume may not report one yet.
    #[serde(default)]
    pub divine_price: Option<f64>,
    /// Whether this is the active challenge league.
    #[serde(default)]
    pub is_current: bool,
}

/// One currency from `GET /poe2/Leagues/{LeagueName}/Currencies/{ApiId}`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ScoutCurrency {
    /// Slugified id (e.g. `preserved-cranium`); the path segment we looked up.
    pub api_id: String,
    /// Display name (e.g. `Preserved Cranium`).
    #[serde(default)]
    pub text: Option<String>,
    /// Category slug (`currency`, `abyss`, …).
    #[serde(default)]
    pub category_api_id: Option<String>,
    /// Computed price, in the requested `ReferenceCurrency`.
    pub current_price: f64,
    /// Recent price series (newest-last is not guaranteed; we just take the
    /// extent). Entries can be `null`, so they're optional.
    #[serde(default)]
    pub price_logs: Vec<Option<ScoutPriceLog>>,
    /// Currency icon URL.
    #[serde(default)]
    pub icon_url: Option<String>,
}

/// One point in a currency's recent price series.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ScoutPriceLog {
    /// ISO-8601 timestamp of the sample, when present.
    #[serde(default)]
    pub time: Option<String>,
    /// Price at this sample, in the same reference currency as `CurrentPrice`.
    #[serde(default)]
    pub price: Option<f64>,
    /// Listed quantity at this sample (a rough liquidity proxy).
    #[serde(default)]
    pub quantity: Option<f64>,
}

// ---- Priced result handed to the UI --------------------------------------

/// A currency priced from poe2scout: the current value in both Exalted and
/// Divine, a recent low/high to convey movement, and an "as of" timestamp.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoutPrice {
    /// Slug id we resolved (`divine`, `preserved-cranium`, …).
    pub api_id: String,
    /// Display name, when the API reported one.
    pub text: Option<String>,
    /// Icon URL, when reported.
    pub icon_url: Option<String>,
    /// Current value in Exalted Orbs.
    pub exalted: f64,
    /// Current value in Divine Orbs (`exalted / divine_price`); `None` when no
    /// Divine rate is known for the league.
    pub divine: Option<f64>,
    /// Exalted-per-Divine rate used for the conversion (for display/debug).
    pub divine_price: Option<f64>,
    /// Recent low value (Exalted) across the price logs, if any.
    pub low: Option<f64>,
    /// Recent high value (Exalted) across the price logs, if any.
    pub high: Option<f64>,
    /// A recent listed quantity (latest log point with one) — a rough liquidity
    /// hint, since poe2scout exposes no true buy/sell spread.
    pub volume: Option<f64>,
    /// Timestamp of the most recent price-log sample, when available.
    pub as_of: Option<String>,
}

// ---- Slugify -------------------------------------------------------------

/// Turn a display name into poe2scout's `ApiId` slug: lowercase, spaces →
/// hyphens, apostrophes/punctuation dropped, hyphen runs collapsed and trimmed.
/// `"Preserved Cranium"` → `preserved-cranium`; `"Farrul's Rune of the Chase"` →
/// `farruls-rune-of-the-chase`.
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
            // Non-ASCII letters/digits: keep them lowercased rather than drop a
            // whole word (slugs are ASCII in practice, but this is harmless).
            if pending_hyphen && !out.is_empty() {
                out.push('-');
            }
            pending_hyphen = false;
            out.extend(ch.to_lowercase());
        } else if ch.is_whitespace() || ch == '-' || ch == '_' {
            // Word boundary → a single hyphen (deferred so trailing runs vanish).
            pending_hyphen = true;
        }
        // Everything else (apostrophes, punctuation) is dropped.
    }
    out
}

// ---- Parsing -------------------------------------------------------------

/// Parse the `GET /poe2/Leagues` body.
pub fn parse_leagues(json: &str) -> Result<Vec<ScoutLeague>, Error> {
    serde_json::from_str(json).map_err(|e| Error::decode("poe2scout leagues", e))
}

/// The Divine rate (Exalted per Divine) for `league`: match its name, else fall
/// back to the league flagged `IsCurrent`. `None` if neither reports one.
#[must_use]
pub fn resolve_divine_price(leagues: &[ScoutLeague], league: &str) -> Option<f64> {
    leagues
        .iter()
        .find(|l| l.league_name.trim().eq_ignore_ascii_case(league.trim()))
        .or_else(|| leagues.iter().find(|l| l.is_current))
        .and_then(|l| l.divine_price)
}

/// Parse a `Currencies/{ApiId}` body (fetched with `ReferenceCurrency=exalted`)
/// into a [`ScoutPrice`], converting to Divine via `divine_price`.
pub fn parse_currency(json: &str, divine_price: Option<f64>) -> Result<ScoutPrice, Error> {
    let raw: ScoutCurrency =
        serde_json::from_str(json).map_err(|e| Error::decode("poe2scout currency", e))?;
    Ok(price_from_currency(&raw, divine_price))
}

/// Build a [`ScoutPrice`] from a raw currency whose `CurrentPrice`/`PriceLogs`
/// are in **Exalted**, converting to Divine via `divine_price` (Exalted per
/// Divine).
#[must_use]
pub fn price_from_currency(raw: &ScoutCurrency, divine_price: Option<f64>) -> ScoutPrice {
    let exalted = raw.current_price;
    // A Divine rate ≤ 0 is meaningless (would divide-by-zero / go negative).
    let divine = divine_price.filter(|d| *d > 0.0).map(|d| exalted / d);

    let logs: Vec<&ScoutPriceLog> = raw.price_logs.iter().flatten().collect();
    let prices: Vec<f64> = logs
        .iter()
        .filter_map(|l| l.price)
        .filter(|p| p.is_finite())
        .collect();
    // `reduce` already yields `None` on an empty `prices`, so no extra guard.
    let low = prices.iter().copied().reduce(f64::min);
    let high = prices.iter().copied().reduce(f64::max);
    // Latest log point that carries a quantity, as a rough liquidity hint.
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

// ---- Async fetch ---------------------------------------------------------

/// Fetch the league list (for the Divine rate). `base_url` is scheme+host with
/// no trailing slash (see [`DEFAULT_BASE_URL`]).
pub async fn fetch_leagues<T: HttpTransport>(
    transport: &T,
    base_url: &str,
) -> Result<Vec<ScoutLeague>, Error> {
    let resp = get(transport, &format!("{base_url}/poe2/Leagues")).await?;
    let resp = ok_or_api_error(resp)?;
    parse_leagues(&resp.body)
}

/// Fetch one currency by its slug `api_id`, priced in `reference` (e.g.
/// `exalted`). `Ok(None)` on a 404 (the slug isn't a known currency), so the
/// caller can fall back to a category name-search.
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
    // An unknown `ApiId` comes back as 400 (bad slug) or 404 (no such currency);
    // both mean "not this id, try the next candidate", not a hard failure.
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
        // Punctuation dropped, not turned into a separator.
        assert_eq!(slugify("Maven's Writ"), "mavens-writ");
        // Leading/trailing/duplicate separators collapse away.
        assert_eq!(slugify("  Chaos   Orb  "), "chaos-orb");
        assert_eq!(slugify("Vaal—Orb"), "vaalorb"); // em-dash dropped (not ASCII '-')
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
        // Exact name wins.
        assert_eq!(
            resolve_divine_price(&leagues, "Rise of the Abyssal"),
            Some(327.0)
        );
        assert_eq!(resolve_divine_price(&leagues, "Standard"), Some(100.0));
        // Unknown name → the current league's rate.
        assert_eq!(resolve_divine_price(&leagues, "Nonexistent"), Some(327.0));
    }

    #[test]
    fn parse_leagues_reads_divine_price() {
        // The league name field is `Value` in the real API.
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
        // CurrentPrice + logs are in Exalted; 327 ex per Divine.
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
        // 327 ex / 327 (ex per div) = 1.0 div.
        assert_eq!(p.divine, Some(1.0));
        assert_eq!(p.low, Some(320.0));
        assert_eq!(p.high, Some(330.0));
        // Latest log carrying a quantity / time.
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
        // A zero/negative rate is rejected like a missing one.
        let p0 = parse_currency(json, Some(0.0)).unwrap();
        assert_eq!(p0.divine, None);
    }
}
