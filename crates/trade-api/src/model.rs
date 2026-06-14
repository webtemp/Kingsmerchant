//! Serde models for the trade2 search request and the search/fetch responses.

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
}

impl Filters {
    pub fn is_empty(&self) -> bool {
        self.type_filters.is_none()
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
