//! Aggregating listing prices for quick mode.

use std::collections::HashMap;

use crate::model::{Price, ResultEntry};

/// The most common currency among priced listings (ties broken by first seen).
pub fn modal_currency(entries: &[ResultEntry]) -> Option<String> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    let mut order: Vec<&str> = Vec::new();
    for price in entries.iter().filter_map(|e| e.listing.price.as_ref()) {
        let c = price.currency.as_str();
        if !counts.contains_key(c) {
            order.push(c);
        }
        *counts.entry(c).or_default() += 1;
    }
    // max_by_key keeps the last max; reverse so ties resolve to first-seen.
    order
        .into_iter()
        .rev()
        .max_by_key(|c| counts[c])
        .map(str::to_string)
}

/// Median asking price within the modal currency. `None` if nothing is priced.
pub fn median_price(entries: &[ResultEntry]) -> Option<Price> {
    let currency = modal_currency(entries)?;
    let mut amounts: Vec<f64> = entries
        .iter()
        .filter_map(|e| e.listing.price.as_ref())
        .filter(|p| p.currency == currency)
        .map(|p| p.amount)
        .collect();
    if amounts.is_empty() {
        return None;
    }
    amounts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = amounts.len() / 2;
    let amount = if amounts.len().is_multiple_of(2) {
        f64::midpoint(amounts[mid - 1], amounts[mid])
    } else {
        amounts[mid]
    };
    Some(Price {
        type_: "~price".to_string(),
        amount,
        currency,
    })
}

/// The cheapest `n` priced listings (search is already price-ascending).
pub fn cheapest(entries: &[ResultEntry], n: usize) -> Vec<&ResultEntry> {
    entries
        .iter()
        .filter(|e| e.listing.price.is_some())
        .take(n)
        .collect()
}
