//! The trade client: ties query building, the HTTP seam, rate-limit gating and
//! response parsing into search + fetch + price-check operations.

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
    /// League id, e.g. `Mirage` (populated from the leagues API).
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
    /// Median asking price over the modal currency.
    pub fn median_price(&self) -> Option<Price> {
        price::median_price(&self.listings)
    }

    /// The cheapest `n` priced listings.
    pub fn cheapest(&self, n: usize) -> Vec<&ResultEntry> {
        price::cheapest(&self.listings, n)
    }
}

/// How a pasted `POESESSID` looks, judged with no network round-trip — drives
/// the instant Settings feedback before the live check can confirm it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionIdFormat {
    /// Blank — no session set (anonymous pricing, which is all most users need).
    Empty,
    /// 32 hexadecimal characters — the shape POE's `POESESSID` cookie takes.
    WellFormed,
    /// Non-empty but not 32 hex chars: a bad paste (the whole `POESESSID=…`
    /// cookie, surrounding quotes, extra characters, …). Never sent.
    Malformed,
}

/// Classify a raw `POESESSID` for instant UI feedback. Pure; no network. The
/// 32-hex shape POE uses doubles as a header-safety guard — hex is printable
/// ASCII, so a well-formed value can never break the `Cookie` header.
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

/// Outcome of a live `POESESSID` validation against pathofexile.com.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionStatus {
    /// The server accepted the session; `account` is the logged-in name when the
    /// profile response exposed it.
    Valid { account: Option<String> },
    /// The server rejected the session (401/403) — wrong, expired, or logged out.
    Invalid,
    /// Couldn't determine validity (offline, or an unexpected status). The
    /// session may still be fine; we just couldn't confirm it.
    Unknown(String),
}

pub struct TradeClient<T: HttpTransport> {
    transport: T,
    config: ClientConfig,
    /// Active league, behind a lock so the UI can switch at runtime without
    /// rebuilding the client (definitions are league-independent).
    league: RwLock<String>,
    /// Optional `POESESSID` session cookie. When set, gated requests carry
    /// `Cookie: POESESSID=…` so fetch responses include the per-listing
    /// `hideout_token` needed to teleport to Instant Buyout sellers.
    poesessid: RwLock<Option<String>>,
    stats: StatDefinitions,
    items: ItemDefinitions,
    /// Bulk-exchange currency catalogue (`data/static`), for pricing stackables.
    currencies: CurrencyDefinitions,
    limiter: Mutex<RateLimiter>,
    /// When the next request may fire, if rate-limit-delayed; polled by
    /// [`retry_in`](Self::retry_in) for the "retrying in Ns" note. Plain `std`
    /// mutex: locked only briefly, never across an await.
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

    /// Switch the league future searches target. Cheap — definitions are
    /// league-independent, so no rebuild.
    pub fn set_league(&self, league: impl Into<String>) {
        *self.league.write().expect("league lock") = league.into();
    }

    /// Set (or clear, with `None`) the `POESESSID` session cookie. Only a
    /// well-formed value (32 hex chars — see [`poesessid_format`]) is stored;
    /// anything else (empty, or a bad paste) clears it. This is the guard that
    /// keeps a malformed value out of the `Cookie` header, where it would
    /// otherwise brick *every* request — anonymous search included — with a
    /// reqwest "Builder error". Cheap; takes effect on the next request.
    pub fn set_poesessid(&self, sessid: Option<String>) {
        let normalized = sessid
            .filter(|s| poesessid_format(s) == SessionIdFormat::WellFormed)
            .map(|s| s.trim().to_string());
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

    /// Validate the configured `POESESSID` against pathofexile.com by fetching
    /// the account profile (auth-gated, side-effect-free): 2xx ⇒ valid, 401/403
    /// ⇒ rejected (wrong/expired), anything else ⇒ couldn't confirm. Goes
    /// straight through the transport (not the trade rate-limiter — different
    /// endpoint, different budget); the transport still attaches the user-agent.
    /// Returns [`SessionStatus::Invalid`] when no session is set to validate.
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
        let body =
            serde_json::to_string(request).map_err(|e| Error::decode("search request", e))?;
        // The league is a path segment and can contain spaces ("Runes of
        // Aldur"), so it must be percent-encoded.
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

    /// Detailed-mode price check: the filters (per-stat, equipment, price) come
    /// from the UI instead of the quick query's base-type defaults.
    pub async fn price_check_detailed(
        &self,
        item: &Item,
        filters: &DetailedFilters,
        max_listings: usize,
    ) -> Result<PriceCheck, Error> {
        let request = build_detailed_query(item, &self.items, filters);
        self.run_query(&request, max_listings).await
    }

    /// Price a stackable item via the bulk **exchange**: one POST, no fetch
    /// round. `want_id` is the `data/static` currency id; `pay` is the currency
    /// to price in (e.g. `exalted`). Offers come back cheapest-first.
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

    /// Teleport into an Instant Buyout seller's hideout, as the trade site's
    /// button does. `token` is a listing's
    /// [`hideout_token`](crate::model::Listing::hideout_token) from an
    /// authenticated fetch; it expires ≈5 min, so call promptly. Needs a
    /// `POESESSID` (else 401).
    ///
    /// **Has an in-game effect**: pulls your character into the seller's
    /// hideout. Fire only on explicit user action.
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

    /// Secondary ML price estimate from poeprices.info (detailed mode only).
    /// Takes raw clipboard `item_text`. Not gated by the GGG rate limiter — a
    /// different service with its own limits, handled inside.
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
        // Attach the session cookie (if set) for authenticated-only fields
        // (e.g. `hideout_token`); don't clobber a caller-supplied Cookie.
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

/// Fetch the live `trade2/data/stats` + `data/items` snapshots (refreshed on
/// app start). Anonymous; no auth required.
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

/// A trade league as offered by `trade2/data/leagues`.
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
        let sid = "0123456789abcdef0123456789ABCDEF"; // 32 hex, mixed case
        assert_eq!(poesessid_format(sid), SessionIdFormat::WellFormed);
        // Surrounding whitespace is tolerated (trimmed).
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
        // Common bad pastes: the whole cookie, wrong length, non-hex chars, an
        // embedded newline — none should ever reach the Cookie header.
        assert_eq!(
            poesessid_format("POESESSID=0123456789abcdef0123456789abcdef"),
            SessionIdFormat::Malformed
        );
        assert_eq!(poesessid_format("deadbeef"), SessionIdFormat::Malformed); // too short
        assert_eq!(
            poesessid_format("0123456789abcdef0123456789abcdeg"), // 'g' isn't hex
            SessionIdFormat::Malformed
        );
        assert_eq!(
            poesessid_format("0123456789abcdef\n0123456789abcde"), // embedded newline
            SessionIdFormat::Malformed
        );
    }
}
