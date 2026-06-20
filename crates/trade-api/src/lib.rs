//! POE2 trade API client: prices a parsed [`parser::Item`] against the official trade API.

pub mod client;
pub mod definitions;
pub mod error;
pub mod exchange;
pub mod http;
pub mod model;
pub mod poeprices;
pub mod price;
pub mod query;
pub mod rate_limit;
pub mod scout;
pub mod stat_text;

pub use client::{
    fetch_definitions, fetch_leagues, poesessid_format, ClientConfig, League, PriceCheck,
    SessionIdFormat, SessionStatus, TradeClient, FETCH_BATCH,
};
pub use definitions::{
    unrevealed_affix_counts, ItemDefinitions, LocalContext, MappedStat, StatDefinitions,
};
pub use error::Error;
pub use exchange::{CurrencyDefinitions, CurrencyEntry, ExchangeCheck, ExchangeOffer};
pub use http::{HttpRequest, HttpResponse, HttpTransport, Method, ReqwestTransport};
pub use model::{Price, ResultEntry, SearchRequest, SearchResponse, StatGroup, StatValue};
pub use poeprices::PriceEstimate;
pub use query::{
    build_detailed_query, build_search_query, category_for, is_elemental_resistance,
    DetailedFilters, EquipmentSelection, ListingStatus, MiscSelection, MiscState, PriceFilter,
    QueryOptions, ResistanceMode, StatSelection,
};
pub use rate_limit::{Bucket, RateLimiter};
pub use scout::{ScoutLeague, ScoutPrice};
