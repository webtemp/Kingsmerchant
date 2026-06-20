//! Serde models for the trade2 search request and the search/fetch responses.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchRequest {
    pub query: Query,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort: Option<Sort>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Query {
    pub status: Status,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_: Option<String>,
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
    pub fn new(option: impl Into<String>) -> Self {
        Status {
            option: option.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StatGroup {
    #[serde(rename = "type")]
    pub type_: String,
    pub filters: Vec<StatFilter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<StatValue>,
}

impl StatGroup {
    pub fn and(filters: Vec<StatFilter>) -> Self {
        StatGroup {
            type_: "and".to_string(),
            filters,
            value: None,
        }
    }

    /// At least `min` of the member filters must match.
    pub fn count(filters: Vec<StatFilter>, min: u32) -> Self {
        StatGroup {
            type_: "count".to_string(),
            filters,
            value: Some(StatValue::min(f64::from(min))),
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
    pub fn min(v: f64) -> Self {
        StatValue {
            min: number(v),
            max: None,
        }
    }

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
    pub misc_filters: Option<MiscFilters>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub map_filters: Option<MapFilters>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trade_filters: Option<TradeFilters>,
}

impl Filters {
    pub fn is_empty(&self) -> bool {
        self.type_filters.is_none()
            && self.equipment_filters.is_none()
            && self.misc_filters.is_none()
            && self.map_filters.is_none()
            && self.trade_filters.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MapFilters {
    pub filters: MapFilterFields,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct MapFilterFields {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub map_tier: Option<StatValue>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MiscFilters {
    pub filters: BTreeMap<String, OptionFilter>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EquipmentFilters {
    pub filters: BTreeMap<String, StatValue>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TradeFilters {
    pub filters: TradeFilterFields,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct TradeFilterFields {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<PriceRange>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct PriceRange {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<serde_json::Number>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<serde_json::Number>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub option: Option<String>,
}

impl PriceRange {
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<StatValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ilvl: Option<StatValue>,
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

/// Entries can be `null` when an id has gone stale between search and fetch.
#[derive(Debug, Clone, Deserialize)]
pub struct FetchResponse {
    #[serde(default)]
    pub result: Vec<Option<ResultEntry>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResultEntry {
    pub id: String,
    pub listing: Listing,
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
    /// Short-lived JWT for Instant Buyout teleport; only on authenticated fetch.
    #[serde(default)]
    pub hideout_token: Option<String>,
}

impl Listing {
    pub fn is_online(&self) -> bool {
        self.account.is_online()
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

impl Account {
    pub fn is_online(&self) -> bool {
        self.online.as_ref().is_some_and(|o| o.status.is_none())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Online {
    #[serde(default)]
    pub league: Option<String>,
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
