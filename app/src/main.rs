//! poe2-pricer — Phase 0 spike.
//!
//! Wires the two platform primitives end to end: detect a global Ctrl+C /
//! Ctrl+Alt+C, read the clipboard the game just wrote, print the item text to
//! stdout. No parsing (Phase 1), no trade API (Phase 2), no UI (Phase 3+).
//!
//! Run it, alt-tab into POE2, hover an item, press Ctrl+C — the copied item
//! text appears here.

use std::time::{Duration, Instant};

use anyhow::Context;
use platform_linux::{read_clipboard_text, watch_hotkeys, HotkeyEvent};
use tracing::{debug, info, warn};

/// How long to wait for the game to write the clipboard after the hotkey.
/// PRD §4.2 budgets 500 ms before aborting. Reads land in ~2 ms in practice.
const CLIPBOARD_TIMEOUT: Duration = Duration::from_millis(500);
/// Poll interval while waiting for the clipboard to change.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let hotkeys = watch_hotkeys().context("failed to start hotkey watcher")?;

    // Baseline so we don't reprint stale clipboard contents. Per PRD §4.2 we
    // never clear or take ownership of the clipboard; we only watch it change.
    let mut last_seen = read_clipboard_text().unwrap_or(None);

    info!("listening for Ctrl+C / Ctrl+Alt+C — hover an item in POE2 and copy");

    for event in hotkeys {
        let mode = match event {
            HotkeyEvent::QuickCopy => "quick (Ctrl+C)",
            HotkeyEvent::DetailedCopy => "detailed (Ctrl+Alt+C)",
        };

        match wait_for_new_clipboard(&last_seen) {
            Some(text) => {
                last_seen = Some(text.clone());
                println!("\n=== {mode} ===\n{text}\n=== end ===");
            }
            // No change usually means POE2's hover-copy didn't fire (PRD §9.3:
            // a static cursor often eats the first Ctrl+C). Not an error.
            None => warn!("{mode}: clipboard did not change within {CLIPBOARD_TIMEOUT:?}"),
        }
    }

    Ok(())
}

/// Poll the clipboard until it differs from `last_seen` or the timeout hits.
/// Returns the new text, or `None` on timeout / read failure.
fn wait_for_new_clipboard(last_seen: &Option<String>) -> Option<String> {
    let start = Instant::now();
    let deadline = start + CLIPBOARD_TIMEOUT;
    loop {
        match read_clipboard_text() {
            Ok(Some(text)) if Some(&text) != last_seen.as_ref() => {
                debug!("clipboard changed after {:?}", start.elapsed());
                return Some(text);
            }
            Ok(_) => {}
            Err(err) => warn!(%err, "clipboard read failed"),
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}
