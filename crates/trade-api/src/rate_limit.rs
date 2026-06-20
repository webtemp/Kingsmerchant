//! Rate-limit bucket tracking from `X-Rate-Limit-*` headers.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// One advertised limit: at most `max` requests per `window`, else `penalty` ban.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bucket {
    pub max: u32,
    pub window: Duration,
    pub penalty: Duration,
}

/// Tracks request history against advertised buckets to compute safe wait times.
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

    pub fn buckets(&self) -> &[Bucket] {
        &self.buckets
    }

    pub fn policy(&self) -> Option<&str> {
        self.policy.as_deref()
    }

    /// Fold a response's rate-limit headers into our state.
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

    pub fn on_request(&mut self, now: Instant) {
        self.hits.push_back(now);
        self.prune(now);
    }

    /// How long to wait before the next request is safe; `ZERO` means fire now.
    pub fn delay_before_next(&self, now: Instant) -> Duration {
        let mut wait = Duration::ZERO;

        if let Some(until) = self.penalty_until {
            if until > now {
                wait = wait.max(until - now);
            }
        }

        for bucket in &self.buckets {
            let window_start = now.checked_sub(bucket.window).unwrap_or(now);
            let in_window: Vec<Instant> = self
                .hits
                .iter()
                .copied()
                .filter(|&t| t > window_start)
                .collect();
            let count = in_window.len() as u32;
            if count >= bucket.max {
                let to_expire = (count - bucket.max + 1) as usize;
                let ready = in_window[to_expire - 1] + bucket.window;
                if ready > now {
                    wait = wait.max(ready - now);
                }
            }
        }

        wait
    }

    /// Drop hits older than the widest window.
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

fn parse_retry_after(value: &str) -> Option<Duration> {
    value.trim().parse::<u64>().ok().map(Duration::from_secs)
}
