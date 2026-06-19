//! POE2 bulk currency **exchange**.
//!
//! Stackables (currency, runes, fragments, essences, waystones, …) aren't sold
//! as individual listings, so the per-item `search` finds nothing. They trade
//! through `POST /api/trade2/exchange/{league}` with a `want` currency id and
//! the `have` currencies you'll pay with. Each result is an *offer* (a ratio),
//! so unit price is `pay_amount / get_amount`.
//!
//! Names map to exchange ids via `trade2/data/static` ([`CurrencyDefinitions`]).
//! We query one pay currency at a time (default Exalted) so every offer is in
//! one currency and trivially sortable, sidestepping exalted-vs-divine
//! normalisation.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::model::{Account, Status};

/// The `trade2/data/static` snapshot: the bulk-exchange currency catalogue.
/// Maps an item's display name (e.g. `Exalted Orb`, `Farrul's Rune of the
/// Chase`) to its short exchange id (`exalted`, `farruls-rune-of-the-chase`).
#[derive(Debug, Default)]
pub struct CurrencyDefinitions {
    by_name: HashMap<String, CurrencyEntry>,
}

/// One exchangeable currency: its short id and display text.
#[derive(Debug, Clone)]
pub struct CurrencyEntry {
    pub id: String,
    pub text: String,
}

#[derive(Deserialize)]
struct StaticDoc {
    #[serde(default)]
    result: Vec<StaticGroup>,
}

#[derive(Deserialize)]
struct StaticGroup {
    #[serde(default)]
    entries: Vec<StaticEntry>,
}

#[derive(Deserialize)]
struct StaticEntry {
    id: String,
    #[serde(default)]
    text: Option<String>,
}

impl CurrencyDefinitions {
    /// Parse the `trade2/data/static` JSON into a name → entry lookup.
    pub fn from_json(json: &str) -> Result<Self, Error> {
        let doc: StaticDoc =
            serde_json::from_str(json).map_err(|e| Error::decode("data/static", e))?;
        let mut by_name = HashMap::new();
        for group in doc.result {
            for entry in group.entries {
                if let Some(text) = entry.text {
                    by_name
                        .entry(text.clone())
                        .or_insert(CurrencyEntry { id: entry.id, text });
                }
            }
        }
        Ok(CurrencyDefinitions { by_name })
    }

    /// Exchange entry for an item display name, if it's a known exchangeable
    /// currency (so the caller routes it to the exchange instead of `search`).
    pub fn lookup(&self, name: &str) -> Option<&CurrencyEntry> {
        self.by_name.get(name.trim())
    }

    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}

// ---- Request -------------------------------------------------------------

#[derive(Serialize)]
struct ExchangeRequest {
    query: ExchangeQuery,
    sort: ExchangeSort,
    engine: &'static str,
}

#[derive(Serialize)]
struct ExchangeQuery {
    status: Status,
    have: Vec<String>,
    want: Vec<String>,
}

#[derive(Serialize)]
struct ExchangeSort {
    have: String,
}

/// Serialised request body for `POST /api/trade2/exchange/{league}`: price
/// `want_id` paying in `pay` currency, cheapest first.
pub fn exchange_body(want_id: &str, pay: &str, status_option: &str) -> Result<String, Error> {
    let request = ExchangeRequest {
        query: ExchangeQuery {
            status: Status::new(status_option),
            have: vec![pay.to_string()],
            want: vec![want_id.to_string()],
        },
        sort: ExchangeSort {
            have: "asc".to_string(),
        },
        engine: "new",
    };
    serde_json::to_string(&request).map_err(|e| Error::decode("exchange request", e))
}

// ---- Response ------------------------------------------------------------

#[derive(Deserialize)]
struct ExchangeResponse {
    #[serde(default)]
    id: String,
    #[serde(default)]
    result: HashMap<String, RawListingWrap>,
}

#[derive(Deserialize)]
struct RawListingWrap {
    listing: RawListing,
}

#[derive(Deserialize)]
struct RawListing {
    #[serde(default)]
    indexed: Option<String>,
    account: Account,
    #[serde(default)]
    offers: Vec<RawOffer>,
    #[serde(default)]
    whisper: Option<String>,
}

#[derive(Deserialize)]
struct RawOffer {
    exchange: RawSide,
    item: RawSide,
}

#[derive(Deserialize)]
struct RawSide {
    currency: String,
    amount: f64,
    #[serde(default)]
    stock: Option<f64>,
    #[serde(default)]
    whisper: Option<String>,
}

/// A priced bulk-exchange result: offers for one item, in one pay currency,
/// sorted cheapest-first.
#[derive(Debug, Clone)]
pub struct ExchangeCheck {
    /// The exchange saved-search hash, for the trade-site deep link
    /// (`/trade2/exchange/poe2/{league}/{id}`).
    pub id: String,
    /// The currency id we priced (the `want`).
    pub want_id: String,
    /// The currency offers are priced in (the `have`).
    pub pay_currency: String,
    /// Offers, cheapest unit price first.
    pub offers: Vec<ExchangeOffer>,
}

/// One bulk-exchange offer (a single seller's ratio).
#[derive(Debug, Clone)]
pub struct ExchangeOffer {
    /// Price per unit of the wanted item, in the pay currency
    /// (the seller's pay-amount divided by their get-amount).
    pub unit_price: f64,
    /// How many units the seller has in stock.
    pub stock: Option<f64>,
    pub account: String,
    pub character: Option<String>,
    pub online: bool,
    pub indexed: Option<String>,
    /// Ready-to-paste whisper (placeholders already filled).
    pub whisper: Option<String>,
}

impl ExchangeCheck {
    /// Median unit price across all offers (they're all in the pay currency).
    pub fn median_unit_price(&self) -> Option<f64> {
        if self.offers.is_empty() {
            return None;
        }
        // offers is already sorted ascending by unit_price.
        let mid = self.offers.len() / 2;
        Some(if self.offers.len().is_multiple_of(2) {
            f64::midpoint(self.offers[mid - 1].unit_price, self.offers[mid].unit_price)
        } else {
            self.offers[mid].unit_price
        })
    }

    /// The cheapest `n` offers.
    pub fn cheapest(&self, n: usize) -> &[ExchangeOffer] {
        &self.offers[..n.min(self.offers.len())]
    }
}

/// Parse an exchange response, keeping only offers in the `pay` currency and
/// sorting them cheapest-first.
pub fn parse_exchange(json: &str, want_id: &str, pay: &str) -> Result<ExchangeCheck, Error> {
    let resp: ExchangeResponse =
        serde_json::from_str(json).map_err(|e| Error::decode("exchange response", e))?;

    let mut offers = Vec::new();
    for wrap in resp.result.into_values() {
        let listing = wrap.listing;
        let online = listing.account.is_online();
        let account = listing.account.name.clone();
        let character = listing.account.last_character_name.clone();
        for offer in &listing.offers {
            // Only offers priced in our chosen pay currency (single-`have`).
            if offer.exchange.currency != pay || offer.item.amount <= 0.0 {
                continue;
            }
            offers.push(ExchangeOffer {
                unit_price: offer.exchange.amount / offer.item.amount,
                stock: offer.item.stock,
                account: account.clone(),
                character: character.clone(),
                online,
                indexed: listing.indexed.clone(),
                whisper: build_whisper(listing.whisper.as_deref(), &offer.item, &offer.exchange),
            });
        }
    }
    // Cheapest first; account name as a stable tiebreaker (the result map's
    // iteration order isn't deterministic).
    offers.sort_by(|a, b| {
        a.unit_price
            .partial_cmp(&b.unit_price)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.account.cmp(&b.account))
    });

    Ok(ExchangeCheck {
        id: resp.id,
        want_id: want_id.to_string(),
        pay_currency: pay.to_string(),
        offers,
    })
}

/// Build the ready-to-paste whisper from the exchange templates. The outer
/// whisper has `{0}` (item) and `{1}` (price); each is itself a template with
/// `{0}` for the amount.
fn build_whisper(template: Option<&str>, item: &RawSide, pay: &RawSide) -> Option<String> {
    let template = template?;
    let item_part = item
        .whisper
        .as_deref()?
        .replace("{0}", &fmt_amount(item.amount));
    let pay_part = pay
        .whisper
        .as_deref()?
        .replace("{0}", &fmt_amount(pay.amount));
    Some(
        template
            .replace("{0}", &item_part)
            .replace("{1}", &pay_part),
    )
}

/// Format a currency amount: whole numbers without a decimal point.
fn fmt_amount(amount: f64) -> String {
    if amount.fract() == 0.0 {
        format!("{}", amount as i64)
    } else {
        format!("{amount}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATIC: &str = r#"{"result":[
        {"id":"Currency","label":"Currency","entries":[
            {"id":"exalted","text":"Exalted Orb","image":"/img/ex.png"},
            {"id":"divine","text":"Divine Orb"}
        ]},
        {"id":"Runes","label":"Runes","entries":[
            {"id":"farruls-rune-of-the-chase","text":"Farrul's Rune of the Chase"}
        ]}
    ]}"#;

    #[test]
    fn static_lookup_maps_name_to_id() {
        let defs = CurrencyDefinitions::from_json(STATIC).unwrap();
        assert_eq!(defs.len(), 3);
        assert_eq!(defs.lookup("Exalted Orb").unwrap().id, "exalted");
        assert_eq!(
            defs.lookup("Farrul's Rune of the Chase").unwrap().id,
            "farruls-rune-of-the-chase"
        );
        assert!(defs.lookup("Some Rare Ring").is_none());
    }

    #[test]
    fn request_body_shape() {
        let body = exchange_body("farruls-rune-of-the-chase", "exalted", "online").unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["query"]["want"][0], "farruls-rune-of-the-chase");
        assert_eq!(v["query"]["have"][0], "exalted");
        assert_eq!(v["query"]["status"]["option"], "online");
        assert_eq!(v["engine"], "new");
    }

    #[test]
    fn parses_real_exchange_response_fixture() {
        // A trimmed real `trade2/exchange` capture (3 listings), so the parser
        // is exercised against the live shape (extra fields, unicode whispers).
        let json = include_str!("../tests/fixtures/api/exchange_response.json");
        let check = parse_exchange(json, "farruls-rune-of-the-chase", "exalted").unwrap();
        assert_eq!(check.id, "5n2ORePVta");
        assert!(!check.offers.is_empty());
        // Cheapest-first, all priced in exalted, with a filled whisper.
        assert!(check
            .offers
            .windows(2)
            .all(|w| w[0].unit_price <= w[1].unit_price));
        let first = &check.offers[0];
        assert!(first.unit_price > 0.0);
        let whisper = first.whisper.as_deref().unwrap_or("");
        assert!(whisper.starts_with('@') && !whisper.contains("{0}") && !whisper.contains("{1}"));
    }

    #[test]
    fn parse_computes_unit_price_sorts_and_builds_whisper() {
        let json = r#"{"id":"AbC123","result":{
            "k1":{"listing":{"indexed":"2026-06-15T11:00:00Z",
                "account":{"name":"seller_b#1","lastCharacterName":"Bee","online":{"league":"L"}},
                "offers":[{"exchange":{"currency":"exalted","amount":3,"whisper":"{0} Exalted Orb"},
                           "item":{"currency":"rune","amount":1,"stock":4,"whisper":"{0} Rune"}}],
                "whisper":"@Bee hi {0} for {1}"}},
            "k2":{"listing":{
                "account":{"name":"seller_a#2","online":{"status":"afk"}},
                "offers":[{"exchange":{"currency":"exalted","amount":1,"whisper":"{0} Exalted Orb"},
                           "item":{"currency":"rune","amount":1,"stock":1,"whisper":"{0} Rune"}}],
                "whisper":"@Aay hi {0} for {1}"}},
            "k3":{"listing":{
                "account":{"name":"seller_c#3"},
                "offers":[{"exchange":{"currency":"divine","amount":1,"whisper":"{0} Divine Orb"},
                           "item":{"currency":"rune","amount":1,"whisper":"{0} Rune"}}],
                "whisper":"@Cee hi {0} for {1}"}}
        }}"#;
        let check = parse_exchange(json, "rune", "exalted").unwrap();
        assert_eq!(check.id, "AbC123");
        // The divine offer is dropped (we pay exalted); two remain, cheapest first.
        assert_eq!(check.offers.len(), 2);
        assert_eq!(check.offers[0].unit_price, 1.0);
        assert_eq!(check.offers[1].unit_price, 3.0);
        assert_eq!(check.median_unit_price(), Some(2.0));
        // Whisper placeholders filled: item {0}=1, pay {0}=1.
        assert_eq!(
            check.offers[0].whisper.as_deref(),
            Some("@Aay hi 1 Rune for 1 Exalted Orb")
        );
        assert!(!check.offers[0].online); // afk
        assert!(check.offers[1].online); // plain online
    }
}
