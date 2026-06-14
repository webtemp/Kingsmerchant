//! Manual end-to-end harness for the trade-api crate (Phase 2 has no UI yet).
//!
//! Reads a POE2 item from stdin, fetches the live stat/item definitions, builds
//! the search query (printing it and the stat mapping so you can eyeball them),
//! then runs the real search + fetch and prints the median + cheapest listings.
//!
//! ```text
//!   # copy an item in-game with Ctrl+C (advanced text), then:
//!   wl-paste | POE_LEAGUE=Mirage cargo run -p trade-api --example price_check
//!   #   X11 clipboard instead of Wayland:  xclip -o -selection clipboard | …
//!   #   from a saved file:                 cargo run … < some_item.txt
//!
//! Env vars:
//!   POE_LEAGUE   league id (default: Standard). See the leagues API.
//!   POE_REALM    pc | sony | xbox (optional).
//!   POE_COOKIE   full Cookie header, e.g. "POESESSID=abc123" — the live search
//!                POST is session-gated, so without this it will likely 400.
//!                Grab it from your browser's devtools on pathofexile.com.
//!   POE_UA       override the User-Agent (be polite: include contact info).
//! ```

use std::io::Read;

use trade_api::http::{HttpRequest, HttpTransport, Method};
use trade_api::{
    build_search_query, ClientConfig, ItemDefinitions, QueryOptions, ReqwestTransport,
    StatDefinitions, TradeClient,
};

const BASE: &str = "https://www.pathofexile.com";

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Read the item the way the real app will: from the clipboard, piped in.
    let mut item_text = String::new();
    std::io::stdin().read_to_string(&mut item_text)?;
    let item = match parser::parse_item(&item_text) {
        Ok(item) => item,
        Err(e) => {
            eprintln!("not a POE2 item: {e}");
            eprintln!("(copy an item in-game with Ctrl+C, then pipe it in)");
            std::process::exit(1);
        }
    };
    println!(
        "── parsed item ──\n  {:?}  class={:?}\n  name={:?}  base={:?}\n  {} modifier(s)\n",
        item.rarity,
        item.item_class,
        item.name,
        item.base_type,
        item.modifiers.len()
    );

    // 2. Fetch the live definition snapshots (anonymous; these always work).
    let ua = std::env::var("POE_UA")
        .unwrap_or_else(|_| "poe2-pricer/0.1 (+manual test harness)".to_string());
    let mut transport = ReqwestTransport::new(ua)?;
    if let Ok(cookie) = std::env::var("POE_COOKIE") {
        transport = transport.with_cookie(cookie);
    }

    println!("fetching trade2/data/stats + data/items …");
    let stats = StatDefinitions::from_json(&get(&transport, "/api/trade2/data/stats").await?)?;
    let items = ItemDefinitions::from_json(&get(&transport, "/api/trade2/data/items").await?)?;
    println!(
        "  loaded {} stat entries, {} base types\n",
        stats.len(),
        items.base_count()
    );

    // 3. Show the stat-text → GGG-id mapping, so you can see it work.
    if !item.modifiers.is_empty() {
        println!("── stat mapping ──");
        for m in &item.modifiers {
            for mapped in stats.map_modifier(m) {
                println!(
                    "  {:?}{:<10} {} → {}  (value {:?})",
                    m.kind,
                    m.source.map(|s| format!(" {s:?}")).unwrap_or_default(),
                    mapped.template,
                    mapped.id,
                    mapped.filter_value()
                );
            }
        }
        println!();
    }

    // 4. Build and print the search query body.
    let opts = QueryOptions::default();
    let request = build_search_query(&item, &stats, &items, opts);
    println!("── search query body ──\n{}\n", serde_json::to_string_pretty(&request)?);

    // 5. Run the real search + fetch through the client.
    let league = std::env::var("POE_LEAGUE").unwrap_or_else(|_| "Standard".to_string());
    let mut config = ClientConfig::new(&league);
    config.realm = std::env::var("POE_REALM").ok();
    let client = TradeClient::new(transport, config, stats, items);

    println!("searching league {league:?} …");
    match client.price_check(&item, opts, 10).await {
        Ok(pc) => {
            println!("  {} total online listings, query id {}\n", pc.total, pc.query_id);
            match pc.median_price() {
                Some(p) => println!("  median asking price: {} {}\n", p.amount, p.currency),
                None => println!("  (no priced listings)\n"),
            }
            println!("── cheapest listings ──");
            for (i, entry) in pc.cheapest(5).iter().enumerate() {
                let l = &entry.listing;
                let price = l
                    .price
                    .as_ref()
                    .map(|p| format!("{} {}", p.amount, p.currency))
                    .unwrap_or_else(|| "—".to_string());
                let status = if l.is_online() { "online" } else { "afk/offline" };
                println!(
                    "  {}. {:<14} {} ({})",
                    i + 1,
                    price,
                    l.account.name,
                    status
                );
                if let Some(w) = &l.whisper {
                    println!("     whisper: {w}");
                }
            }
        }
        Err(e) => {
            eprintln!("\nsearch/fetch failed: {e}");
            eprintln!(
                "\nIf this is an HTTP 400 'Invalid query', the search POST is \
                 session-gated:\nset POE_COOKIE to your browser's \"POESESSID=…\" \
                 (and cf_clearance if present).\nThe definition fetch + query build \
                 above are the parts that work anonymously."
            );
            std::process::exit(1);
        }
    }
    Ok(())
}

/// Minimal GET through the transport, for the public definition endpoints.
async fn get(transport: &ReqwestTransport, path: &str) -> Result<String, Box<dyn std::error::Error>> {
    let resp = transport
        .execute(HttpRequest {
            method: Method::Get,
            url: format!("{BASE}{path}"),
            headers: Vec::new(),
            body: None,
        })
        .await?;
    if !resp.is_success() {
        return Err(format!("GET {path} → HTTP {}", resp.status).into());
    }
    Ok(resp.body)
}
