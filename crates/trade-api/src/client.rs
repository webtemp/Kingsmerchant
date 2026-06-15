//! The trade client: ties query building, the HTTP seam, rate-limit gating and
//! response parsing into search + fetch + price-check operations (PRD §4.4).

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

/// The trade API only ever hands back ≤ 10 listings per fetch.
pub const FETCH_BATCH: usize = 10;

/// Where and how to talk to the trade API.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Scheme + host, no trailing slash. Defaults to the official site.
    pub base_url: String,
    /// League id, e.g. `Mirage` (PRD §4.8 populates this from the leagues API).
    pub league: String,
    /// Realm (`pc` / `sony` / `xbox`); anonymous queries are realm-aware.
    pub realm: Option<String>,
}

impl ClientConfig {
    pub fn new(league: impl Into<String>) -> Self {
        ClientConfig {
            base_url: "https://www.pathofexile.com".to_string(),
            league: league.into(),
            realm: None,
        }
    }
}

/// A finished price check: the listings plus the query handle that produced
/// them (so the UI can deep-link back to the trade site).
#[derive(Debug, Clone)]
pub struct PriceCheck {
    pub query_id: String,
    pub total: u64,
    pub listings: Vec<ResultEntry>,
}

impl PriceCheck {
    /// Median asking price over the modal currency (PRD §4.6).
    pub fn median_price(&self) -> Option<Price> {
        price::median_price(&self.listings)
    }

    /// The cheapest `n` priced listings.
    pub fn cheapest(&self, n: usize) -> Vec<&ResultEntry> {
        price::cheapest(&self.listings, n)
    }
}

pub struct TradeClient<T: HttpTransport> {
    transport: T,
    config: ClientConfig,
    /// The active league. Held behind a lock so the UI can switch leagues at
    /// runtime (PRD §4.8 selector) without rebuilding the whole client — the
    /// definitions are league-independent. Seeded from `config.league`.
    league: RwLock<String>,
    /// Optional `POESESSID` session cookie. When set, every gated request
    /// (search / fetch / exchange / teleport) carries `Cookie: POESESSID=…`, so
    /// the fetch response includes the per-listing `hideout_token` needed to
    /// teleport to Instant Buyout sellers. Runtime-settable from Settings.
    poesessid: RwLock<Option<String>>,
    stats: StatDefinitions,
    items: ItemDefinitions,
    /// Bulk-exchange currency catalogue (`data/static`), for pricing stackables.
    currencies: CurrencyDefinitions,
    limiter: Mutex<RateLimiter>,
    /// When the in-flight (or next) request is allowed to fire, if it's been
    /// rate-limit-delayed. The UI polls [`retry_in`](Self::retry_in) to show a
    /// "throttled, retrying in Ns" note (PRD §4.4). Plain `std` mutex: only ever
    /// locked briefly, never across an await.
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
            limiter: Mutex::new(RateLimiter::new()),
            retry_at: StdMutex::new(None),
        }
    }

    /// How long until the next request is allowed to fire, if currently
    /// rate-limit-throttled (else `None`). For the "retrying in Ns" UI note.
    pub fn retry_in(&self) -> Option<Duration> {
        let at = *self.retry_at.lock().expect("retry_at lock");
        at.and_then(|t| t.checked_duration_since(Instant::now()))
    }

    /// The league searches currently target.
    pub fn league(&self) -> String {
        self.league.read().expect("league lock").clone()
    }

    /// Switch the league future searches target (PRD §4.8). Cheap — definitions
    /// are league-independent, so no rebuild.
    pub fn set_league(&self, league: impl Into<String>) {
        *self.league.write().expect("league lock") = league.into();
    }

    /// Set (or clear, with `None`) the `POESESSID` session cookie. An empty or
    /// whitespace-only string clears it. Cheap; takes effect on the next request.
    pub fn set_poesessid(&self, sessid: Option<String>) {
        let normalized = sessid
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        *self.poesessid.write().expect("poesessid lock") = normalized;
    }

    /// Whether a `POESESSID` is currently set (so the UI can enable/disable the
    /// teleport button and explain why).
    pub fn has_poesessid(&self) -> bool {
        self.poesessid.read().expect("poesessid lock").is_some()
    }

    /// The `Cookie:` header value for authenticated requests, if a session is set.
    fn cookie_header(&self) -> Option<String> {
        self.poesessid
            .read()
            .expect("poesessid lock")
            .as_ref()
            .map(|s| format!("POESESSID={s}"))
    }

    pub fn stats(&self) -> &StatDefinitions {
        &self.stats
    }

    pub fn items(&self) -> &ItemDefinitions {
        &self.items
    }

    /// The bulk-exchange currency catalogue (`data/static`). The UI uses it to
    /// decide whether an item is a stackable priced via the exchange.
    pub fn currencies(&self) -> &CurrencyDefinitions {
        &self.currencies
    }

    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    /// Submit a search query, returning the query id + result hashes.
    pub async fn search(&self, request: &SearchRequest) -> Result<SearchResponse, Error> {
        let body = serde_json::to_string(request).map_err(|e| Error::decode("search request", e))?;
        // The league is a path segment and can contain spaces ("Runes of
        // Aldur"), so it must be percent-encoded.
        let url = self.with_realm(format!(
            "{}/api/trade2/search/{}",
            self.config.base_url,
            encode_path_segment(&self.league())
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

    /// Fetch listing details for `ids`, batching at [`FETCH_BATCH`]. Null
    /// entries (ids that went stale) are dropped; order is preserved.
    pub async fn fetch(&self, ids: &[String], query_id: &str) -> Result<Vec<ResultEntry>, Error> {
        let mut out = Vec::new();
        for chunk in ids.chunks(FETCH_BATCH) {
            let csv = chunk.join(",");
            let mut url = format!(
                "{}/api/trade2/fetch/{}?query={}",
                self.config.base_url, csv, query_id
            );
            if let Some(realm) = &self.config.realm {
                url.push_str(&format!("&realm={realm}"));
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

    /// End-to-end quick-mode price check: build the query from the item, search,
    /// then fetch up to `max_listings` (capped at [`FETCH_BATCH`] for one round).
    pub async fn price_check(
        &self,
        item: &Item,
        opts: QueryOptions,
        max_listings: usize,
    ) -> Result<PriceCheck, Error> {
        let request = build_search_query(item, &self.stats, &self.items, opts);
        self.run_query(&request, max_listings).await
    }

    /// Detailed-mode price check (PRD §4.7): the filters (per-stat, equipment,
    /// price) come from the UI instead of the quick query's base-type defaults.
    pub async fn price_check_detailed(
        &self,
        item: &Item,
        filters: &DetailedFilters,
        max_listings: usize,
    ) -> Result<PriceCheck, Error> {
        let request = build_detailed_query(item, &self.items, filters);
        self.run_query(&request, max_listings).await
    }

    /// Price a stackable item via the bulk **exchange** (PRD §4.4): one POST,
    /// no fetch round. `want_id` is the `data/static` currency id; `pay` is the
    /// currency to price in (e.g. `exalted`). Offers come back cheapest-first.
    pub async fn price_check_exchange(
        &self,
        want_id: &str,
        pay: &str,
    ) -> Result<ExchangeCheck, Error> {
        // The exchange has no "instant buyout" notion — every offer is a buyout
        // ratio — so we just ask for live (online) sellers.
        let body = crate::exchange::exchange_body(want_id, pay, "online")?;
        let url = self.with_realm(format!(
            "{}/api/trade2/exchange/{}",
            self.config.base_url,
            encode_path_segment(&self.league())
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

    /// Teleport into an Instant Buyout seller's hideout (to buy from Ange the
    /// Merchant), the way the trade site's button does. `token` is a listing's
    /// [`hideout_token`](crate::model::Listing::hideout_token) from an
    /// authenticated fetch — it expires ≈5 min after the fetch, so call this
    /// promptly. Requires a `POESESSID` (else the API answers 401 Unauthorized).
    ///
    /// **This has an in-game effect**: it pulls your character into the seller's
    /// hideout. Fire it only on an explicit user action.
    pub async fn teleport_to_hideout(&self, token: &str) -> Result<(), Error> {
        let url = format!("{}/api/trade2/whisper", self.config.base_url);
        let body = serde_json::json!({ "token": token }).to_string();
        let resp = self
            .send(HttpRequest {
                method: Method::Post,
                url,
                // Match the trade site's XHR so GGG accepts the request.
                headers: vec![
                    ("X-Requested-With".to_string(), "XMLHttpRequest".to_string()),
                    ("Origin".to_string(), self.config.base_url.clone()),
                    ("Referer".to_string(), format!("{}/trade2", self.config.base_url)),
                ],
                body: Some(body),
            })
            .await?;
        ok_or_api_error(resp)?;
        Ok(())
    }

    /// Search then fetch the cheapest `max_listings` for a built request.
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

    /// Secondary ML price estimate from poeprices.info (PRD §4.4, detailed mode
    /// only). Takes the raw clipboard `item_text`. Not run through the GGG rate
    /// limiter — it's a different service with its own limits, handled inside.
    pub async fn price_estimate(
        &self,
        item_text: &str,
    ) -> Result<Option<crate::poeprices::PriceEstimate>, Error> {
        crate::poeprices::price_estimate(&self.transport, &self.league(), item_text).await
    }

    fn with_realm(&self, mut url: String) -> String {
        if let Some(realm) = &self.config.realm {
            url.push_str(&format!("?realm={realm}"));
        }
        url
    }

    /// Send one request through the rate-limit gate, retrying through a 429.
    async fn send(&self, mut request: HttpRequest) -> Result<HttpResponse, Error> {
        // Attach the session cookie (if set) so the response carries the
        // authenticated-only fields (e.g. `hideout_token`). Don't clobber a
        // Cookie the caller already supplied.
        if let Some(cookie) = self.cookie_header() {
            if !request
                .headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("cookie"))
            {
                request.headers.push(("Cookie".to_string(), cookie));
            }
        }
        const MAX_ATTEMPTS: u32 = 3;
        for attempt in 0..MAX_ATTEMPTS {
            let delay = self.limiter.lock().await.delay_before_next(Instant::now());
            if !delay.is_zero() {
                tracing::warn!(
                    seconds = delay.as_secs(),
                    "rate limited, retrying in {}s",
                    delay.as_secs()
                );
                // Expose the wait to the UI for the duration of the sleep.
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
            // No longer waiting on the limiter.
            *self.retry_at.lock().expect("retry_at lock") = None;
            return Ok(response);
        }
        unreachable!("loop returns on the final attempt")
    }
}

/// Fetch the live `trade2/data/stats` + `data/items` snapshots (PRD §4.3,
/// refreshed on app start). Anonymous; no auth required.
pub async fn fetch_definitions<T: HttpTransport>(
    transport: &T,
    base_url: &str,
) -> Result<(StatDefinitions, ItemDefinitions, CurrencyDefinitions), Error> {
    let stats_json = get_body(transport, &format!("{base_url}/api/trade2/data/stats")).await?;
    let items_json = get_body(transport, &format!("{base_url}/api/trade2/data/items")).await?;
    // The bulk-exchange currency catalogue (currency/runes/fragments/…).
    let static_json = get_body(transport, &format!("{base_url}/api/trade2/data/static")).await?;
    Ok((
        StatDefinitions::from_json(&stats_json)?,
        ItemDefinitions::from_json(&items_json)?,
        CurrencyDefinitions::from_json(&static_json)?,
    ))
}

/// A trade league as offered by `trade2/data/leagues` (PRD §4.8 selector).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct League {
    /// League id used as the search path segment (e.g. `Runes of Aldur`).
    pub id: String,
    /// Human label for the dropdown (usually the same as `id`).
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

/// Percent-encode a URL path segment (RFC 3986 unreserved chars pass through).
/// League ids like `Runes of Aldur` carry spaces that would otherwise produce
/// an invalid URL.
fn encode_path_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
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
