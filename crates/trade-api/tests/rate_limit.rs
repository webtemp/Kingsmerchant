//! Rate-limit bucket tests (PRD §7): simulated header sequences and request
//! patterns → expected wait times. The header values mirror a real capture
//! (`x-rate-limit-ip: 5:10:60,15:60:300,30:300:1800`).

use std::time::{Duration, Instant};

use trade_api::rate_limit::RateLimiter;

/// Build the `(name, value)` header list for the `Ip` rule.
fn ip_headers(limit: &str, state: &str) -> Vec<(String, String)> {
    vec![
        ("X-Rate-Limit-Policy".into(), "trade-search-request-limit".into()),
        ("X-Rate-Limit-Rules".into(), "Ip".into()),
        ("X-Rate-Limit-Ip".into(), limit.into()),
        ("X-Rate-Limit-Ip-State".into(), state.into()),
    ]
}

#[test]
fn parses_the_real_captured_limit_header() {
    let mut rl = RateLimiter::new();
    rl.observe(&ip_headers("5:10:60,15:60:300,30:300:1800", "1:10:0,1:60:0,1:300:0"), Instant::now());

    let buckets = rl.buckets();
    assert_eq!(buckets.len(), 3);
    assert_eq!(buckets[0].max, 5);
    assert_eq!(buckets[0].window, Duration::from_secs(10));
    assert_eq!(buckets[0].penalty, Duration::from_secs(60));
    assert_eq!(buckets[2].max, 30);
    assert_eq!(buckets[2].window, Duration::from_secs(300));
    assert_eq!(rl.policy(), Some("trade-search-request-limit"));
}

#[test]
fn under_the_limit_never_waits() {
    let t0 = Instant::now();
    let mut rl = RateLimiter::new();
    rl.observe(&ip_headers("5:10:60", "1:10:0"), t0);

    // Four requests against a max of five: still room.
    for i in 0..4 {
        rl.on_request(t0 + Duration::from_secs(i));
    }
    assert_eq!(rl.delay_before_next(t0 + Duration::from_secs(4)), Duration::ZERO);
}

#[test]
fn fifth_request_in_window_waits_for_the_oldest_to_age_out() {
    let t0 = Instant::now();
    let mut rl = RateLimiter::new();
    rl.observe(&ip_headers("5:10:60", "1:10:0"), t0);

    // Five requests at t0 fills the 10s bucket; the oldest ages out at t0+10.
    for _ in 0..5 {
        rl.on_request(t0);
    }
    assert_eq!(rl.delay_before_next(t0), Duration::from_secs(10));
    // Three seconds later, seven seconds remain.
    assert_eq!(rl.delay_before_next(t0 + Duration::from_secs(3)), Duration::from_secs(7));
    // Once the window has fully passed, we're clear.
    assert_eq!(rl.delay_before_next(t0 + Duration::from_secs(10)), Duration::ZERO);
}

#[test]
fn spread_requests_wait_only_until_the_oldest_relevant_one_expires() {
    let t0 = Instant::now();
    let mut rl = RateLimiter::new();
    rl.observe(&ip_headers("5:10:60", "1:10:0"), t0);

    // Requests at 0,1,2,3,4s — five within the window as of t0+4.
    for i in 0..5 {
        rl.on_request(t0 + Duration::from_secs(i));
    }
    // The oldest (t0) ages out at t0+10, i.e. 6s after t0+4.
    assert_eq!(rl.delay_before_next(t0 + Duration::from_secs(4)), Duration::from_secs(6));
}

#[test]
fn the_tightest_bucket_dominates() {
    let t0 = Instant::now();
    let mut rl = RateLimiter::new();
    // 2 per 10s, but also 10 per 60s. The per-10s bucket bites first.
    rl.observe(&ip_headers("2:10:60,10:60:300", "0:10:0,0:60:0"), t0);

    rl.on_request(t0);
    rl.on_request(t0);
    // Per-10s bucket full (2/2); per-60s nowhere near (2/10).
    assert_eq!(rl.delay_before_next(t0), Duration::from_secs(10));
}

#[test]
fn active_server_penalty_forces_a_wait() {
    let t0 = Instant::now();
    let mut rl = RateLimiter::new();
    // State's third field is an active penalty of 30s even though usage is low.
    rl.observe(&ip_headers("5:10:60", "5:10:30"), t0);
    assert_eq!(rl.delay_before_next(t0), Duration::from_secs(30));
    assert_eq!(rl.delay_before_next(t0 + Duration::from_secs(12)), Duration::from_secs(18));
}

#[test]
fn retry_after_header_on_429_is_honoured() {
    let t0 = Instant::now();
    let mut rl = RateLimiter::new();
    let headers = vec![("Retry-After".to_string(), "8".to_string())];
    rl.observe(&headers, t0);
    assert_eq!(rl.delay_before_next(t0), Duration::from_secs(8));
}

#[test]
fn header_lookup_is_case_insensitive() {
    let t0 = Instant::now();
    let mut rl = RateLimiter::new();
    let headers = vec![
        ("x-rate-limit-rules".to_string(), "ip".to_string()),
        ("x-rate-limit-ip".to_string(), "3:10:60".to_string()),
    ];
    rl.observe(&headers, t0);
    assert_eq!(rl.buckets().len(), 1);
    assert_eq!(rl.buckets()[0].max, 3);
}
