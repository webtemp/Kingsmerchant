//! The trade client: search + fetch + price-check operations.

use std::collections::HashMap;
use std::sync::{Mutex as StdMutex, RwLock};
use std::time::{Duration, Instant};

use parser::Item;
use tokio::sync::Mutex;

use crate::definitions::{ItemDefinitions, StatDefinitions};
use crate::error::Error;
use crate::exchange::{CurrencyDefinitions, ExchangeCheck};
use crate::http::{HttpRequest, HttpResponse, HttpTransport, Method};
use crate::model::{FetchResponse, Price, ResultEntry, SearchRequest, SearchResponse};
use crate::price;
use crate::query::{build_detailed_query, build_search_query, DetailedFilters, QueryOptions};
use crate::rate_limit::RateLimiter;
use crate::scout::{self, ScoutPrice};

pub const FETCH_BATCH: usize = 10;

const SCOUT_TTL: Duration = Duration::from_mins(10);

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub base_url: String,
    pub league: String,
    pub realm: Option<String>,
    pub scout_base_url: String,
}

impl ClientConfig {
    pub fn new(league: impl Into<String>) -> Self {
        ClientConfig {
            base_url: "https://www.pathofexile.com".to_string(),
            league: league.into(),
            realm: None,
            scout_base_url: crate::scout::DEFAULT_BASE_URL.to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PriceCheck {
    pub query_id: String,
    pub total: u64,
    pub listings: Vec<ResultEntry>,
}

impl PriceCheck {
    pub fn median_price(&self) -> Option<Price> {
        price::median_price(&self.listings)
    }

    pub fn cheapest(&self, n: usize) -> Vec<&ResultEntry> {
        price::cheapest(&self.listings, n)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionIdFormat {
    Empty,
    WellFormed,
    Malformed,
}

/// Classify a raw `POESESSID`. The 32-hex shape doubles as a header-safety guard.
#[must_use]
pub fn poesessid_format(raw: &str) -> SessionIdFormat {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        SessionIdFormat::Empty
    } else if trimmed.len() == 32 && trimmed.bytes().all(|b| b.is_ascii_hexdigit()) {
        SessionIdFormat::WellFormed
    } else {
        SessionIdFormat::Malformed
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionStatus {
    Valid { account: Option<String> },
    Invalid,
    Unknown(String),
}

#[derive(Default)]
struct ScoutCache {
    divine_price: Option<(f64, Instant)>,
    divine_league: String,
    currencies: HashMap<String, (ScoutPrice, Instant)>,
}

pub struct TradeClient<T: HttpTransport> {
    transport: T,
    config: ClientConfig,
    league: RwLock<String>,
    poesessid: RwLock<Option<String>>,
    stats: StatDefinitions,
    items: ItemDefinitions,
    currencies: CurrencyDefinitions,
    scout_cache: StdMutex<ScoutCache>,
    limiter: Mutex<RateLimiter>,
    retry_at: StdMutex<Option<Instant>>,
}

impl<T: HttpTransport> TradeClient<T> {
    pub fn new(
        transport: T,
        config: ClientConfig,
        stats: StatDefinitions,
        items: ItemDefinitions,
        currencies: CurrencyDefinitions,
    ) -> Self {
        TradeClient {
            league: RwLock::new(config.league.clone()),
            poesessid: RwLock::new(None),
            transport,
            config,
            stats,
            items,
            currencies,
            scout_cache: StdMutex::new(ScoutCache::default()),
            limiter: Mutex::new(RateLimiter::new()),
            retry_at: StdMutex::new(None),
        }
    }

    pub fn retry_in(&self) -> Option<Duration> {
        let at = *self.retry_at.lock().expect("retry_at lock");
        at.and_then(|t| t.checked_duration_since(Instant::now()))
    }

    pub fn league(&self) -> String {
        self.league.read().expect("league lock").clone()
    }

    pub fn set_league(&self, league: impl Into<String>) {
        *self.league.write().expect("league lock") = league.into();
    }

    /// Only a well-formed value is stored; anything else clears it (keeps a bad value out of the Cookie header).
    pub fn set_poesessid(&self, sessid: Option<String>) {
        let normalized = sessid
            .filter(|s| poesessid_format(s) == SessionIdFormat::WellFormed)
            .map(|s| s.trim().to_string());
        *self.poesessid.write().expect("poesessid lock") = normalized;
    }

    pub fn has_poesessid(&self) -> bool {
        self.poesessid.read().expect("poesessid lock").is_some()
    }

    fn cookie_header(&self) -> Option<String> {
        self.poesessid
            .read()
            .expect("poesessid lock")
            .as_ref()
            .map(|s| format!("POESESSID={s}"))
    }

    /// Validate the configured `POESESSID` against the account-profile endpoint.
    pub async fn validate_session(&self) -> SessionStatus {
        let Some(cookie) = self.cookie_header() else {
            return SessionStatus::Invalid;
        };
        let request = HttpRequest {
            method: Method::Get,
            url: format!("{}/api/profile", self.config.base_url),
            headers: vec![("Cookie".to_string(), cookie)],
            body: None,
        };
        match self.transport.execute(request).await {
            Ok(resp) if resp.is_success() => {
                let account = serde_json::from_str::<serde_json::Value>(&resp.body)
                    .ok()
                    .and_then(|v| {
                        v.get("name")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                    });
                SessionStatus::Valid { account }
            }
            Ok(resp) if resp.status == 401 || resp.status == 403 => SessionStatus::Invalid,
            Ok(resp) => SessionStatus::Unknown(format!("unexpected HTTP {}", resp.status)),
            Err(e) => SessionStatus::Unknown(e.to_string()),
        }
    }

    pub fn stats(&self) -> &StatDefinitions {
        &self.stats
    }

    pub fn items(&self) -> &ItemDefinitions {
        &self.items
    }

    pub fn currencies(&self) -> &CurrencyDefinitions {
        &self.currencies
    }

    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    /// Submit a search query, returning the query id + result hashes.
    pub async fn search(&self, request: &SearchRequest) -> Result<SearchResponse, Error> {
        let body =
            serde_json::to_string(request).map_err(|e| Error::decode("search request", e))?;
        let url = self.with_realm(format!(
            "{}/api/trade2/search/{}",
            self.config.base_url,
            crate::http::percent_encode(&self.league())
        ));
        let resp = self
            .send(HttpRequest {
                method: Method::Post,
                url,
                headers: Vec::new(),
                body: Some(body),
            })
            .await?;
        let resp = ok_or_api_error(resp)?;
        serde_json::from_str(&resp.body).map_err(|e| Error::decode("search response", e))
    }

    /// Fetch listing details for `ids`, batching at [`FETCH_BATCH`]; null entries dropped, order preserved.
    pub async fn fetch(&self, ids: &[String], query_id: &str) -> Result<Vec<ResultEntry>, Error> {
        let mut out = Vec::new();
        for chunk in ids.chunks(FETCH_BATCH) {
            let csv = chunk.join(",");
            let mut url = format!(
                "{}/api/trade2/fetch/{}?query={}",
                self.config.base_url, csv, query_id
            );
            if let Some(realm) = &self.config.realm {
                url.push_str("&realm=");
                url.push_str(realm);
            }
            let resp = self
                .send(HttpRequest {
                    method: Method::Get,
                    url,
                    headers: Vec::new(),
                    body: None,
                })
                .await?;
            let resp = ok_or_api_error(resp)?;
            let parsed: FetchResponse =
                serde_json::from_str(&resp.body).map_err(|e| Error::decode("fetch response", e))?;
            out.extend(parsed.result.into_iter().flatten());
        }
        Ok(out)
    }

    /// Quick-mode price check: build query from the item, search, then fetch up to `max_listings`.
    pub async fn price_check(
        &self,
        item: &Item,
        opts: QueryOptions,
        max_listings: usize,
    ) -> Result<PriceCheck, Error> {
        let request = build_search_query(item, &self.stats, &self.items, opts);
        self.run_query(&request, max_listings).await
    }

    /// Detailed-mode price check: filters come from the UI instead of base-type defaults.
    pub async fn price_check_detailed(
        &self,
        item: &Item,
        filters: &DetailedFilters,
        max_listings: usize,
    ) -> Result<PriceCheck, Error> {
        let request = build_detailed_query(item, &self.items, filters);
        self.run_query(&request, max_listings).await
    }

    /// Price a stackable item via the bulk exchange: one POST, no fetch round, cheapest-first.
    pub async fn price_check_exchange(
        &self,
        want_id: &str,
        pay: &str,
    ) -> Result<ExchangeCheck, Error> {
        let body = crate::exchange::exchange_body(want_id, pay, "online")?;
        let url = self.with_realm(format!(
            "{}/api/trade2/exchange/{}",
            self.config.base_url,
            crate::http::percent_encode(&self.league())
        ));
        let resp = self
            .send(HttpRequest {
                method: Method::Post,
                url,
                headers: Vec::new(),
                body: Some(body),
            })
            .await?;
        let resp = ok_or_api_error(resp)?;
        crate::exchange::parse_exchange(&resp.body, want_id, pay)
    }

    /// Price a stackable currency from poe2scout. `Ok(None)` when it doesn't index the currency.
    pub async fn scout_price(
        &self,
        exchange_id: &str,
        name: &str,
    ) -> Result<Option<ScoutPrice>, Error> {
        if let Some(price) = self.cached_scout(exchange_id) {
            return Ok(Some(price));
        }

        let league = self.league();
        let divine_price = self.scout_divine_price(&league).await;
        let base = &self.config.scout_base_url;

        let slug = scout::slugify(name);
        let mut candidates = vec![exchange_id.to_string()];
        if !slug.is_empty() && slug != exchange_id {
            candidates.push(slug);
        }

        let mut raw = None;
        for api_id in &candidates {
            if let Some(c) =
                scout::fetch_currency(&self.transport, base, &league, api_id, "exalted").await?
            {
                raw = Some(c);
                break;
            }
        }
        let Some(raw) = raw else {
            return Ok(None);
        };

        let price = scout::price_from_currency(&raw, divine_price);
        self.scout_cache
            .lock()
            .expect("scout cache lock")
            .currencies
            .insert(exchange_id.to_string(), (price.clone(), Instant::now()));
        Ok(Some(price))
    }

    fn cached_scout(&self, api_id: &str) -> Option<ScoutPrice> {
        let cache = self.scout_cache.lock().expect("scout cache lock");
        cache
            .currencies
            .get(api_id)
            .filter(|(_, at)| at.elapsed() < SCOUT_TTL)
            .map(|(price, _)| price.clone())
    }

    /// The cached Divine rate (Exalted per Divine) for `league`, refreshed from poe2scout.
    async fn scout_divine_price(&self, league: &str) -> Option<f64> {
        {
            let cache = self.scout_cache.lock().expect("scout cache lock");
            if cache.divine_league == league {
                if let Some((rate, at)) = cache.divine_price {
                    if at.elapsed() < SCOUT_TTL {
                        return Some(rate);
                    }
                }
            }
        }
        let leagues = match scout::fetch_leagues(&self.transport, &self.config.scout_base_url).await
        {
            Ok(l) => l,
            Err(e) => {
                tracing::debug!(error = %e, "poe2scout leagues fetch failed");
                return None;
            }
        };
        let rate = scout::resolve_divine_price(&leagues, league);
        if let Some(rate) = rate {
            let mut cache = self.scout_cache.lock().expect("scout cache lock");
            cache.divine_price = Some((rate, Instant::now()));
            cache.divine_league = league.to_string();
        }
        rate
    }

    /// Teleport into a seller's hideout. Has an in-game effect; fire only on explicit user action.
    pub async fn teleport_to_hideout(&self, token: &str) -> Result<(), Error> {
        let url = format!("{}/api/trade2/whisper", self.config.base_url);
        let body = serde_json::json!({ "token": token }).to_string();
        let resp = self
            .send(HttpRequest {
                method: Method::Post,
                url,
                headers: vec![
                    ("X-Requested-With".to_string(), "XMLHttpRequest".to_string()),
                    ("Origin".to_string(), self.config.base_url.clone()),
                    (
                        "Referer".to_string(),
                        format!("{}/trade2", self.config.base_url),
                    ),
                ],
                body: Some(body),
            })
            .await?;
        ok_or_api_error(resp)?;
        Ok(())
    }

    async fn run_query(
        &self,
        request: &SearchRequest,
        max_listings: usize,
    ) -> Result<PriceCheck, Error> {
        let search = self.search(request).await?;
        let ids: Vec<String> = search.result.iter().take(max_listings).cloned().collect();
        let listings = self.fetch(&ids, &search.id).await?;
        Ok(PriceCheck {
            query_id: search.id,
            total: search.total,
            listings,
        })
    }

    /// Secondary ML price estimate from poeprices.info (detailed mode only).
    pub async fn price_estimate(
        &self,
        item_text: &str,
    ) -> Result<Option<crate::poeprices::PriceEstimate>, Error> {
        crate::poeprices::price_estimate(&self.transport, &self.league(), item_text).await
    }

    fn with_realm(&self, mut url: String) -> String {
        if let Some(realm) = &self.config.realm {
            url.push_str("?realm=");
            url.push_str(realm);
        }
        url
    }

    /// Send one request through the rate-limit gate, retrying through a 429.
    async fn send(&self, mut request: HttpRequest) -> Result<HttpResponse, Error> {
        const MAX_ATTEMPTS: u32 = 3;
        if let Some(cookie) = self.cookie_header() {
            if !request
                .headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("cookie"))
            {
                request.headers.push(("Cookie".to_string(), cookie));
            }
        }
        for attempt in 0..MAX_ATTEMPTS {
            let delay = self.limiter.lock().await.delay_before_next(Instant::now());
            if !delay.is_zero() {
                tracing::warn!(
                    seconds = delay.as_secs(),
                    "rate limited, retrying in {}s",
                    delay.as_secs()
                );
                *self.retry_at.lock().expect("retry_at lock") = Some(Instant::now() + delay);
                tokio::time::sleep(delay).await;
            }

            self.limiter.lock().await.on_request(Instant::now());
            let response = self.transport.execute(request.clone()).await?;
            self.limiter
                .lock()
                .await
                .observe(&response.headers, Instant::now());

            if response.status == 429 && attempt + 1 < MAX_ATTEMPTS {
                tracing::warn!("got 429, honouring rate-limit headers and retrying");
                continue;
            }
            *self.retry_at.lock().expect("retry_at lock") = None;
            return Ok(response);
        }
        unreachable!("loop returns on the final attempt")
    }
}

/// Fetch the live `trade2/data/stats` + `data/items` + `data/static` snapshots.
pub async fn fetch_definitions<T: HttpTransport>(
    transport: &T,
    base_url: &str,
) -> Result<(StatDefinitions, ItemDefinitions, CurrencyDefinitions), Error> {
    let stats_json = get_body(transport, &format!("{base_url}/api/trade2/data/stats")).await?;
    let items_json = get_body(transport, &format!("{base_url}/api/trade2/data/items")).await?;
    let static_json = get_body(transport, &format!("{base_url}/api/trade2/data/static")).await?;
    Ok((
        StatDefinitions::from_json(&stats_json)?,
        ItemDefinitions::from_json(&items_json)?,
        CurrencyDefinitions::from_json(&static_json)?,
    ))
}

/// A trade league as offered by `trade2/data/leagues`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct League {
    pub id: String,
    pub text: String,
}

/// Fetch the list of POE2 trade leagues for the league selector.
pub async fn fetch_leagues<T: HttpTransport>(
    transport: &T,
    base_url: &str,
) -> Result<Vec<League>, Error> {
    #[derive(serde::Deserialize)]
    struct Wrapper {
        result: Vec<League>,
    }
    let body = get_body(transport, &format!("{base_url}/api/trade2/data/leagues")).await?;
    let parsed: Wrapper = serde_json::from_str(&body).map_err(|e| Error::decode("leagues", e))?;
    Ok(parsed.result)
}

async fn get_body<T: HttpTransport>(transport: &T, url: &str) -> Result<String, Error> {
    let resp = transport
        .execute(HttpRequest {
            method: Method::Get,
            url: url.to_string(),
            headers: Vec::new(),
            body: None,
        })
        .await?;
    ok_or_api_error(resp).map(|r| r.body)
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
    use super::{poesessid_format, SessionIdFormat};

    #[test]
    fn well_formed_poesessid_is_32_hex() {
        let sid = "0123456789abcdef0123456789ABCDEF";
        assert_eq!(poesessid_format(sid), SessionIdFormat::WellFormed);
        assert_eq!(
            poesessid_format("  0123456789abcdef0123456789abcdef \n"),
            SessionIdFormat::WellFormed
        );
    }

    #[test]
    fn blank_poesessid_is_empty() {
        assert_eq!(poesessid_format(""), SessionIdFormat::Empty);
        assert_eq!(poesessid_format("   \t\n"), SessionIdFormat::Empty);
    }

    #[test]
    fn bad_paste_is_malformed() {
        assert_eq!(
            poesessid_format("POESESSID=0123456789abcdef0123456789abcdef"),
            SessionIdFormat::Malformed
        );
        assert_eq!(poesessid_format("deadbeef"), SessionIdFormat::Malformed);
        assert_eq!(
            poesessid_format("0123456789abcdef0123456789abcdeg"),
            SessionIdFormat::Malformed
        );
        assert_eq!(
            poesessid_format("0123456789abcdef\n0123456789abcde"),
            SessionIdFormat::Malformed
        );
    }
}
