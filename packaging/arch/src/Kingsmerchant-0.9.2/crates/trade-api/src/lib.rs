//! POE2 trade API client.
//!
//! Takes a parsed [`parser::Item`] and prices it against the official trade
//! API: build a search query, `POST .../trade2/search/{league}` for a query id
//! + result hashes, then `GET .../trade2/fetch/{ids}` in batches of ten.
//!
//! Three concerns each live in their own module and are unit-tested in
//! isolation against recorded fixtures — no test touches the network:
//!
//! * [`definitions`] + [`stat_text`] — map the parser's raw stat text (e.g.
//!   `+118(100-119) to maximum Life`) to GGG stat ids + filter values, using
//!   the `trade2/data/stats` / `data/items` snapshots, and split magic bases.
//! * [`query`] — assemble a [`SearchRequest`] from an [`Item`](parser::Item).
//! * [`rate_limit`] — track the `X-Rate-Limit-*` headers into per-window
//!   buckets and report how long to wait before the next request is safe.
//!
//! [`TradeClient`] wires them together over a mockable [`HttpTransport`].

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
    DetailedFilters, EquipmentSelection, ListingStatus, MiscSelection, PriceFilter, QueryOptions,
    ResistanceMode, StatSelection,
};
pub use rate_limit::{Bucket, RateLimiter};
pub use scout::{ScoutLeague, ScoutPrice};
