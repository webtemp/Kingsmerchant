//! Response-parser tests: decode recorded search/fetch JSON fixtures and assert fields + price aggregation.

use trade_api::model::{FetchResponse, SearchResponse};
use trade_api::price::{cheapest, median_price, modal_currency};

fn fetch_entries() -> Vec<trade_api::ResultEntry> {
    let json = include_str!("fixtures/api/fetch_response.json");
    let parsed: FetchResponse = serde_json::from_str(json).expect("fetch fixture parses");
    parsed.result.into_iter().flatten().collect()
}

#[test]
fn parses_search_response() {
    let json = include_str!("fixtures/api/search_response.json");
    let resp: SearchResponse = serde_json::from_str(json).unwrap();
    assert_eq!(resp.id, "kA2eGYh9");
    assert_eq!(resp.total, 137);
    assert_eq!(resp.complexity, Some(14));
    assert_eq!(resp.result.len(), 5);
    assert_eq!(resp.result[0].len(), 64);
}

#[test]
fn parses_fetch_listings_with_prices_and_whispers() {
    let entries = fetch_entries();
    assert_eq!(entries.len(), 5);

    let first = &entries[0];
    assert_eq!(first.listing.account.name, "SellerOne#1111");
    assert_eq!(
        first.listing.account.last_character_name.as_deref(),
        Some("ZappyMcZap")
    );
    let price = first.listing.price.as_ref().unwrap();
    assert_eq!(price.amount, 2.0);
    assert_eq!(price.currency, "exalted");
    assert!(first
        .listing
        .whisper
        .as_ref()
        .unwrap()
        .starts_with("@SellerOne"));
}

#[test]
fn online_afk_and_offline_are_distinguished() {
    let entries = fetch_entries();
    assert!(entries[0].listing.is_online());
    assert!(!entries[1].listing.is_online());
    assert!(!entries[3].listing.is_online());
}

#[test]
fn null_priced_listing_decodes_without_a_price() {
    let entries = fetch_entries();
    let last = &entries[4];
    assert_eq!(last.listing.account.name, "SellerFive#5555");
    assert!(last.listing.price.is_none());
}

#[test]
fn median_is_computed_over_the_modal_currency() {
    let entries = fetch_entries();
    assert_eq!(modal_currency(&entries).as_deref(), Some("exalted"));
    let median = median_price(&entries).unwrap();
    assert_eq!(median.currency, "exalted");
    assert_eq!(median.amount, 3.0);
}

#[test]
fn cheapest_keeps_search_order_and_skips_unpriced() {
    let entries = fetch_entries();
    let top = cheapest(&entries, 3);
    assert_eq!(top.len(), 3);
    assert!(top.iter().all(|e| e.listing.price.is_some()));
    assert_eq!(top[0].listing.account.name, "SellerOne#1111");
}

#[test]
fn empty_listing_set_has_no_median() {
    assert!(median_price(&[]).is_none());
    assert!(modal_currency(&[]).is_none());
}

#[test]
fn decodes_real_search_capture() {
    let json = include_str!("fixtures/api/search_response_real.json");
    let resp: SearchResponse = serde_json::from_str(json).expect("real search decodes");
    assert!(!resp.id.is_empty());
    assert!(!resp.result.is_empty());
    assert!(resp.result.iter().all(|id| id.len() == 64));
}

#[test]
fn decodes_real_fetch_capture_with_varied_currencies() {
    let json = include_str!("fixtures/api/fetch_response_real.json");
    let parsed: FetchResponse = serde_json::from_str(json).expect("real fetch decodes");
    let entries: Vec<_> = parsed.result.into_iter().flatten().collect();
    assert!(!entries.is_empty());
    for e in &entries {
        assert!(!e.listing.account.name.is_empty());
        if let Some(p) = &e.listing.price {
            assert!(!p.currency.is_empty());
        }
    }
    assert!(median_price(&entries).is_some());
}
