//! The trade client: ties query building, the HTTP seam, rate-limit gating and
//! response parsing into search + fetch + price-check operations (PRD §4.4).

use std::time::Instant;

use parser::Item;
use tokio::sync::Mutex;

use crate::definitions::{ItemDefinitions, StatDefinitions};
use crate::error::Error;
use crate::http::{HttpRequest, HttpResponse, HttpTransport, Method};
use crate::model::{FetchResponse, Price, ResultEntry, SearchRequest, SearchResponse};
use crate::price;
use crate::query::{build_search_query, QueryOptions};
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
    stats: StatDefinitions,
    items: ItemDefinitions,
    limiter: Mutex<RateLimiter>,
}

impl<T: HttpTransport> TradeClient<T> {
    pub fn new(
        transport: T,
        config: ClientConfig,
        stats: StatDefinitions,
        items: ItemDefinitions,
    ) -> Self {
        TradeClient {
            transport,
            config,
            stats,
            items,
            limiter: Mutex::new(RateLimiter::new()),
        }
    }

    pub fn stats(&self) -> &StatDefinitions {
        &self.stats
    }

    pub fn items(&self) -> &ItemDefinitions {
        &self.items
    }

    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    /// Submit a search query, returning the query id + result hashes.
    pub async fn search(&self, request: &SearchRequest) -> Result<SearchResponse, Error> {
        let body = serde_json::to_string(request).map_err(|e| Error::decode("search request", e))?;
        let url = self.with_realm(format!(
            "{}/api/trade2/search/{}",
            self.config.base_url, self.config.league
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
        let search = self.search(&request).await?;
        let ids: Vec<String> = search.result.iter().take(max_listings).cloned().collect();
        let listings = self.fetch(&ids, &search.id).await?;
        Ok(PriceCheck {
            query_id: search.id,
            total: search.total,
            listings,
        })
    }

    fn with_realm(&self, mut url: String) -> String {
        if let Some(realm) = &self.config.realm {
            url.push_str(&format!("?realm={realm}"));
        }
        url
    }

    /// Send one request through the rate-limit gate, retrying through a 429.
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, Error> {
        const MAX_ATTEMPTS: u32 = 3;
        for attempt in 0..MAX_ATTEMPTS {
            let delay = self.limiter.lock().await.delay_before_next(Instant::now());
            if !delay.is_zero() {
                tracing::warn!(
                    seconds = delay.as_secs(),
                    "rate limited, retrying in {}s",
                    delay.as_secs()
                );
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
            return Ok(response);
        }
        unreachable!("loop returns on the final attempt")
    }
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
