//! Serde models for the trade2 search request and the search/fetch responses.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

// ---- search request --------------------------------------------------------

/// Body of `POST /api/trade2/search/{league}`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchRequest {
    pub query: Query,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort: Option<Sort>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Query {
    pub status: Status,
    /// Base type (exact match). Set for uniques/normals and for magic items
    /// once the base has been split out of the fused name.
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_: Option<String>,
    /// Rolled name — only uniques have one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub stats: Vec<StatGroup>,
    #[serde(skip_serializing_if = "Filters::is_empty")]
    pub filters: Filters,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Status {
    pub option: String,
}

impl Status {
    pub fn online() -> Self {
        Status::new("online")
    }

    pub fn new(option: impl Into<String>) -> Self {
        Status {
            option: option.into(),
        }
    }
}

/// A group of stat filters combined with one logical operator.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StatGroup {
    #[serde(rename = "type")]
    pub type_: String,
    pub filters: Vec<StatFilter>,
}

impl StatGroup {
    pub fn and(filters: Vec<StatFilter>) -> Self {
        StatGroup {
            type_: "and".to_string(),
            filters,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StatFilter {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<StatValue>,
    pub disabled: bool,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct StatValue {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<serde_json::Number>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<serde_json::Number>,
}

impl StatValue {
    /// A `{ "min": v }` value, emitting an integer when `v` is whole so the
    /// request body reads `"min": 30` rather than `"min": 30.0`.
    pub fn min(v: f64) -> Self {
        StatValue {
            min: number(v),
            max: None,
        }
    }

    /// A `{ "min": .., "max": .. }` value from optional bounds (detailed mode).
    pub fn range(min: Option<f64>, max: Option<f64>) -> Self {
        StatValue {
            min: min.and_then(number),
            max: max.and_then(number),
        }
    }
}

fn number(v: f64) -> Option<serde_json::Number> {
    if v.fract() == 0.0 && v.is_finite() {
        Some(serde_json::Number::from(v as i64))
    } else {
        serde_json::Number::from_f64(v)
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct Filters {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_filters: Option<TypeFilters>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub equipment_filters: Option<EquipmentFilters>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trade_filters: Option<TradeFilters>,
}

impl Filters {
    pub fn is_empty(&self) -> bool {
        self.type_filters.is_none()
            && self.equipment_filters.is_none()
            && self.trade_filters.is_none()
    }
}

/// The `equipment_filters` group: an item's defence/offence properties (armour
/// `ar`, evasion `ev`, energy shield `es`, block, spirit, …), keyed by the
/// trade filter id. These are item *properties*, not affix mods.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EquipmentFilters {
    pub filters: BTreeMap<String, StatValue>,
}

/// The `trade_filters` group: seller/price constraints. We use it for the
/// detailed-mode price-range filter (PRD §4.7).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TradeFilters {
    pub filters: TradeFilterFields,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct TradeFilterFields {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<PriceRange>,
}

/// A `{ "min": .., "max": .., "option": "exalted" }` price filter.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct PriceRange {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<serde_json::Number>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<serde_json::Number>,
    /// Currency the bounds are expressed in (`exalted`, `divine`, …). `None`
    /// lets the trade site match across currencies (chaos-equivalent).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub option: Option<String>,
}

impl PriceRange {
    /// Build from optional float bounds + currency; whole numbers serialize as
    /// integers. Returns `None` when there are no bounds at all.
    pub fn new(min: Option<f64>, max: Option<f64>, option: Option<String>) -> Option<Self> {
        if min.is_none() && max.is_none() {
            return None;
        }
        Some(PriceRange {
            min: min.and_then(number),
            max: max.and_then(number),
            option,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TypeFilters {
    pub filters: TypeFilterFields,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct TypeFilterFields {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<OptionFilter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rarity: Option<OptionFilter>,
    /// Item quality (`{min,max}`) — `type_filters` is where the trade API keeps
    /// it, not `equipment_filters`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<StatValue>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OptionFilter {
    pub option: String,
}

impl OptionFilter {
    pub fn new(option: impl Into<String>) -> Self {
        OptionFilter {
            option: option.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Sort {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<String>,
}

impl Sort {
    pub fn price_asc() -> Self {
        Sort {
            price: Some("asc".to_string()),
        }
    }
}

// ---- responses -------------------------------------------------------------

/// Response of `POST /api/trade2/search/{league}`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SearchResponse {
    pub id: String,
    #[serde(default)]
    pub complexity: Option<u32>,
    #[serde(default)]
    pub result: Vec<String>,
    #[serde(default)]
    pub total: u64,
}

/// Response of `GET /api/trade2/fetch/{ids}`. Entries can be `null` when an id
/// has gone stale between search and fetch.
#[derive(Debug, Clone, Deserialize)]
pub struct FetchResponse {
    #[serde(default)]
    pub result: Vec<Option<ResultEntry>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResultEntry {
    pub id: String,
    pub listing: Listing,
    /// The item block is kept raw; the UI phase decides what to render.
    #[serde(default)]
    pub item: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Listing {
    #[serde(default)]
    pub indexed: Option<String>,
    pub account: Account,
    #[serde(default)]
    pub price: Option<Price>,
    #[serde(default)]
    pub whisper: Option<String>,
}

impl Listing {
    /// Whether the seller is online (and not merely afk/dnd).
    pub fn is_online(&self) -> bool {
        self.account
            .online
            .as_ref()
            .map(|o| o.status.is_none())
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Account {
    pub name: String,
    #[serde(default, rename = "lastCharacterName")]
    pub last_character_name: Option<String>,
    #[serde(default)]
    pub online: Option<Online>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Online {
    #[serde(default)]
    pub league: Option<String>,
    /// `afk`, `dnd`, … — absent means plainly online.
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Price {
    #[serde(rename = "type")]
    pub type_: String,
    pub amount: f64,
    pub currency: String,
}
