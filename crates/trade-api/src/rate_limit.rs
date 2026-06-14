//! Rate-limit bucket tracking (PRD §4.4).
//!
//! The official trade API advertises its limits on every response via
//! `X-Rate-Limit-<Rule>` headers and the live usage via `…-<Rule>-State`.
//! A rule value is a comma-separated list of `max:window:penalty` triples
//! (seconds), e.g. real captured `5:10:60,15:60:300,30:300:1800` — "5 requests
//! per 10s (else a 60s ban), 15 per 60s, 30 per 300s". The matching state
//! header reads `hits:window:active_penalty`.
//!
//! We mirror EE2 / awakened-poe-trade: track the timestamps of the requests we
//! send and, before sending another, project whether it would breach any
//! bucket. If it would, we report how long to wait rather than firing blindly.
//! Active server-side penalties (the state header's third field, or a 429's
//! `Retry-After`) are honoured as hard waits on top of that.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// One advertised limit: at most [`max`](Bucket::max) requests per
/// [`window`](Bucket::window); breaching it earns a [`penalty`](Bucket::penalty)
/// ban.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bucket {
    pub max: u32,
    pub window: Duration,
    pub penalty: Duration,
}

/// Tracks our request history against the buckets advertised by the API and
/// computes how long to wait before the next request is safe.
#[derive(Debug, Default)]
pub struct RateLimiter {
    buckets: Vec<Bucket>,
    /// Timestamps of requests we've sent, oldest first.
    hits: VecDeque<Instant>,
    /// Hard wait imposed by a server-side penalty / `Retry-After`.
    penalty_until: Option<Instant>,
    policy: Option<String>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// The buckets currently known from the most recent response.
    pub fn buckets(&self) -> &[Bucket] {
        &self.buckets
    }

    /// The policy name (e.g. `trade-search-request-limit`), if seen.
    pub fn policy(&self) -> Option<&str> {
        self.policy.as_deref()
    }

    /// Fold a response's rate-limit headers into our state. `headers` is a list
    /// of `(name, value)` pairs; names are matched case-insensitively.
    pub fn observe(&mut self, headers: &[(String, String)], now: Instant) {
        if let Some(policy) = header(headers, "x-rate-limit-policy") {
            self.policy = Some(policy.to_string());
        }

        let rules: Vec<String> = header(headers, "x-rate-limit-rules")
            .map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        let mut buckets = Vec::new();
        let mut max_penalty = Duration::ZERO;
        for rule in &rules {
            let lower = rule.to_ascii_lowercase();
            if let Some(spec) = header(headers, &format!("x-rate-limit-{lower}")) {
                buckets.extend(parse_buckets(spec));
            }
            if let Some(state) = header(headers, &format!("x-rate-limit-{lower}-state")) {
                for (_, _, penalty) in parse_state(state) {
                    max_penalty = max_penalty.max(penalty);
                }
            }
        }
        if !buckets.is_empty() {
            self.buckets = buckets;
        }

        // A 429 carries the authoritative wait directly.
        if let Some(retry) = header(headers, "retry-after").and_then(parse_retry_after) {
            max_penalty = max_penalty.max(retry);
        }

        if max_penalty > Duration::ZERO {
            let until = now + max_penalty;
            self.penalty_until = Some(match self.penalty_until {
                Some(existing) if existing > until => existing,
                _ => until,
            });
        }
    }

    /// Record that we've just sent a request at `now`.
    pub fn on_request(&mut self, now: Instant) {
        self.hits.push_back(now);
        self.prune(now);
    }

    /// How long to wait before the next request is safe. `Duration::ZERO` means
    /// fire now.
    pub fn delay_before_next(&self, now: Instant) -> Duration {
        let mut wait = Duration::ZERO;

        if let Some(until) = self.penalty_until {
            if until > now {
                wait = wait.max(until - now);
            }
        }

        for bucket in &self.buckets {
            let window_start = now.checked_sub(bucket.window).unwrap_or(now);
            // Our hits still inside this bucket's window, oldest first.
            let in_window: Vec<Instant> = self
                .hits
                .iter()
                .copied()
                .filter(|&t| t > window_start)
                .collect();
            let count = in_window.len() as u32;
            if count >= bucket.max {
                // We must let enough requests age out to drop below `max`.
                let to_expire = (count - bucket.max + 1) as usize;
                let ready = in_window[to_expire - 1] + bucket.window;
                if ready > now {
                    wait = wait.max(ready - now);
                }
            }
        }

        wait
    }

    /// Drop hits older than the widest window — they can't affect any bucket.
    fn prune(&mut self, now: Instant) {
        let max_window = self
            .buckets
            .iter()
            .map(|b| b.window)
            .max()
            .unwrap_or(Duration::ZERO);
        if let Some(cutoff) = now.checked_sub(max_window) {
            while let Some(&front) = self.hits.front() {
                if front <= cutoff {
                    self.hits.pop_front();
                } else {
                    break;
                }
            }
        }
    }
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// Parse a limit spec like `5:10:60,15:60:300` into buckets.
fn parse_buckets(spec: &str) -> Vec<Bucket> {
    spec.split(',')
        .filter_map(|part| {
            let mut it = part.split(':');
            let max = it.next()?.trim().parse().ok()?;
            let window = it.next()?.trim().parse::<u64>().ok()?;
            let penalty = it.next()?.trim().parse::<u64>().ok()?;
            Some(Bucket {
                max,
                window: Duration::from_secs(window),
                penalty: Duration::from_secs(penalty),
            })
        })
        .collect()
}

/// Parse a state spec like `1:10:0,2:60:0` into `(hits, window, active_penalty)`.
fn parse_state(spec: &str) -> Vec<(u32, Duration, Duration)> {
    spec.split(',')
        .filter_map(|part| {
            let mut it = part.split(':');
            let hits = it.next()?.trim().parse().ok()?;
            let window = it.next()?.trim().parse::<u64>().ok()?;
            let penalty = it.next()?.trim().parse::<u64>().ok()?;
            Some((
                hits,
                Duration::from_secs(window),
                Duration::from_secs(penalty),
            ))
        })
        .collect()
}

/// `Retry-After` is "delta-seconds" in the trade API's usage.
fn parse_retry_after(value: &str) -> Option<Duration> {
    value.trim().parse::<u64>().ok().map(Duration::from_secs)
}
