//! The OS-thread watchers that feed the UI: the global-hotkey watcher (Ctrl+C /
//! macros / Escape via evdev) and the `config.json` hot-reload watcher, plus the
//! clipboard-polling step that decides when a fresh item has landed.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use platform_linux::{HotkeyBindings, HotkeyControl};

use crate::config::Config;
use crate::model::{item_hash, normalize_item_text};
use crate::{Hotkey, CLIPBOARD_TIMEOUT, POLL_INTERVAL};

/// Debounce window for chat macros — long enough to swallow the duplicate
/// events each keyboard's multiple event-kbd nodes emit for one press.
const MACRO_DEBOUNCE: Duration = Duration::from_millis(300);

/// A live handle to the running hotkey watcher: the rebindable bindings the
/// evdev reader threads consult on every press, plus the POE2-focus gate they
/// honour. [`apply_config`](Self::apply_config) pushes new settings in without a
/// restart — the watcher threads pick them up on the next keypress.
#[derive(Clone)]
pub struct HotkeyHandle {
    control: HotkeyControl,
    require_focus: Arc<AtomicBool>,
}

impl HotkeyHandle {
    fn bindings_from(config: &Config) -> HotkeyBindings {
        HotkeyBindings::from_strings(
            &config.hotkey_quick,
            &config.hotkey_macro,
            &config.hotkey_macro2,
            &config.hotkey_close,
        )
    }

    /// Apply the hotkey-relevant fields of `config` to the running watcher
    /// (bindings + focus gate). Effective immediately; no restart.
    pub fn apply_config(&self, config: &Config) {
        self.control.set(Self::bindings_from(config));
        self.require_focus
            .store(config.require_poe2_focus, Ordering::Relaxed);
    }
}

/// Watch the global price-check hotkeys on a background thread. On each press we
/// wait for POE2 to write the clipboard, then push the item text to the UI. If
/// the watcher can't start (e.g. not in the `input` group), we log and carry on
/// — the window still works manually.
///
/// Returns a [`HotkeyHandle`] so the app can rebind hotkeys / toggle the focus
/// gate live (see [`HotkeyHandle::apply_config`]); the bindings live behind a
/// shared lock the reader threads read per keypress.
pub fn spawn_hotkey_watcher(ctx: egui::Context, tx: Sender<Hotkey>) -> HotkeyHandle {
    use platform_linux::HotkeyEvent;
    // Read-only load: this background thread must not race `build_app`'s startup
    // write (or re-trigger the config watcher) by backfilling the file.
    let config = Config::load_no_write();
    let control = HotkeyControl::new(HotkeyHandle::bindings_from(&config));
    let require_focus = Arc::new(AtomicBool::new(config.require_poe2_focus));
    let handle = HotkeyHandle {
        control: control.clone(),
        require_focus: require_focus.clone(),
    };

    std::thread::spawn(move || {
        let hotkeys = match platform_linux::watch_hotkeys(&control) {
            Ok(rx) => rx,
            Err(e) => {
                tracing::warn!(error = %e, "hotkey watcher disabled; use the buttons");
                return;
            }
        };
        tracing::info!(
            quick = %config.hotkey_quick,
            macro_ = %config.hotkey_macro,
            require_poe2_focus = config.require_poe2_focus,
            synthetic_copy = control.snapshot().quick_needs_synthetic_copy(),
            "listening for hotkeys"
        );
        // Pre-create the injection device (after the watcher scanned keyboards,
        // so it isn't picked up) so the first macro — or synthesized copy — is
        // instant rather than paying the ~250ms uinput enumeration wait. A later
        // live rebind to a synthetic-copy hotkey just pays that once, lazily.
        if control.snapshot().quick_needs_synthetic_copy()
            || config.f5_command.is_some()
            || config.macro2_command.is_some()
        {
            std::thread::spawn(platform_linux::warm_up_injection);
        }
        // Shared so the clipboard wait can run OFF this loop (below): the loop
        // must NOT block, or evdev modifier events (Ctrl/Alt for the overlay's
        // show + Alt-drag) queue behind a ≤1s clipboard poll and the drag lags.
        let last_seen = Arc::new(Mutex::new(
            platform_linux::read_clipboard_text().unwrap_or(None),
        ));
        // One physical press is echoed by several event-kbd nodes; debounce so
        // each macro fires once (slot 0 = F5, 1 = F2).
        let mut last_macro: [Option<Instant>; 2] = [None, None];
        for event in hotkeys {
            match event {
                // Escape dismisses — overlay control, not gated to POE2 focus.
                HotkeyEvent::Close => {
                    let _ = tx.send(Hotkey::Close);
                    ctx.request_repaint();
                }
                // Ctrl/Alt state — must be forwarded INSTANTLY (overlay drag/show).
                HotkeyEvent::Modifiers { ctrl, alt } => {
                    let _ = tx.send(Hotkey::Mods { ctrl, alt });
                    ctx.request_repaint();
                }
                // Chat macros — only into POE2. Off-thread so the focus check
                // (xdotool) doesn't stall the loop.
                HotkeyEvent::Macro | HotkeyEvent::Macro2 => {
                    // Drop a duplicate press echoed by another device node.
                    let slot = usize::from(event == HotkeyEvent::Macro2);
                    let now = Instant::now();
                    if last_macro[slot].is_some_and(|t| now.duration_since(t) < MACRO_DEBOUNCE) {
                        continue;
                    }
                    last_macro[slot] = Some(now);

                    let (tx, ctx, require_focus) = (tx.clone(), ctx.clone(), require_focus.clone());
                    let msg = if event == HotkeyEvent::Macro2 {
                        Hotkey::Macro2
                    } else {
                        Hotkey::Macro
                    };
                    std::thread::spawn(move || {
                        if require_focus.load(Ordering::Relaxed)
                            && !platform_linux::is_poe2_active()
                        {
                            tracing::info!("macro ignored — POE2 not focused");
                            return;
                        }
                        let _ = tx.send(msg);
                        ctx.request_repaint();
                    });
                }
                // The price-check combo: the focus check + the ≤1s clipboard poll
                // run on their own thread so this loop keeps forwarding modifier
                // events.
                HotkeyEvent::QuickCopy => {
                    let (tx, ctx, last, require_focus, control) = (
                        tx.clone(),
                        ctx.clone(),
                        last_seen.clone(),
                        require_focus.clone(),
                        control.clone(),
                    );
                    std::thread::spawn(move || {
                        if require_focus.load(Ordering::Relaxed)
                            && !platform_linux::is_poe2_active()
                        {
                            tracing::info!("price-check hotkey ignored — POE2 not focused");
                            return;
                        }
                        // Pop the popup NOW (focus is confirmed), before the
                        // clipboard poll, so the UI reacts instantly.
                        let _ = tx.send(Hotkey::CopyStarted);
                        ctx.request_repaint();

                        // If the (live) hotkey isn't Ctrl+C, POE2 hasn't copied
                        // anything — synthesize the copy now (POE2 is focused) so
                        // the clipboard holds the item under the cursor when we
                        // poll below.
                        if control.snapshot().quick_needs_synthetic_copy() {
                            if let Err(e) = platform_linux::copy_item_under_cursor() {
                                tracing::warn!(error = %format!("{e:#}"), "synthetic copy failed");
                            }
                        }

                        let prev = last.lock().expect("last_seen lock").clone();
                        let start = Instant::now();
                        let outcome = if let Some(text) = wait_for_item(prev.as_deref()) {
                            tracing::info!(
                                elapsed_ms = start.elapsed().as_millis(),
                                hash = item_hash(&text),
                                "clipboard: item → showing (UI de-dups the query)"
                            );
                            *last.lock().expect("last_seen lock") = Some(text.clone());
                            Hotkey::Item { text }
                        } else {
                            tracing::info!("clipboard: no item → ignored");
                            Hotkey::Missed
                        };
                        let _ = tx.send(outcome);
                        ctx.request_repaint();
                    });
                }
            }
        }
    });

    handle
}

/// Watch `config.json` for external edits and push the reloaded config to the
/// UI thread. Best-effort: if the watcher can't start we log and carry on
/// (settings still apply on the next launch).
///
/// We watch the containing directory (not the file) because editors often save
/// by replacing the file via rename, which drops a watch on the inode itself.
/// Reads are write-free ([`Config::load_no_write`]) so our own reload can't
/// re-trigger the watcher.
pub fn spawn_config_watcher(ctx: egui::Context, tx: Sender<Hotkey>) {
    use notify::{RecursiveMode, Watcher};
    let path = Config::path();
    let Some(dir) = path.parent().map(std::path::Path::to_path_buf) else {
        tracing::warn!("config has no parent dir; hot-reload disabled");
        return;
    };
    let file_name = path.file_name().map(std::ffi::OsStr::to_os_string);

    std::thread::spawn(move || {
        // Editors fire several events per save; coalesce them. Seed the timer a
        // debounce-window in the past so the first save isn't swallowed; on a
        // just-booted machine where `Instant` can't go back that far, fall back
        // to "now" (at worst a config edit in the first 200ms isn't hot-reloaded).
        let last = Mutex::new(
            Instant::now()
                .checked_sub(Duration::from_secs(1))
                .unwrap_or_else(Instant::now),
        );
        let handler = move |res: notify::Result<notify::Event>| {
            let Ok(event) = res else { return };
            // Only our file, and only content-changing events.
            let touches_config = event
                .paths
                .iter()
                .any(|p| p.file_name().map(std::ffi::OsStr::to_os_string) == file_name);
            if !touches_config
                || !matches!(
                    event.kind,
                    notify::EventKind::Modify(_) | notify::EventKind::Create(_)
                )
            {
                return;
            }
            {
                let mut l = last.lock().expect("config-watch debounce lock");
                if l.elapsed() < Duration::from_millis(200) {
                    return;
                }
                *l = Instant::now();
            }
            // Let the writer finish flushing before we read.
            std::thread::sleep(Duration::from_millis(60));
            let config = Config::load_no_write();
            tracing::info!("config.json changed → reloading");
            if tx.send(Hotkey::ConfigReloaded(Box::new(config))).is_ok() {
                ctx.request_repaint();
            }
        };
        let mut watcher = match notify::recommended_watcher(handler) {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!(error = %e, "config watcher disabled");
                return;
            }
        };
        if let Err(e) = watcher.watch(&dir, RecursiveMode::NonRecursive) {
            tracing::warn!(error = %e, dir = %dir.display(), "config watch failed");
            return;
        }
        tracing::info!(dir = %dir.display(), "watching config.json for changes");
        // Keep the watcher alive for the process lifetime.
        loop {
            std::thread::park();
        }
    });
}

/// Poll the clipboard until it both *changed* and *parses as a POE2 item*, or
/// the timeout hits. Gating on "is an item" (not merely "changed") avoids
/// grabbing the stale value the X11↔Wayland bridge can briefly expose before
/// POE2 finishes writing.
fn wait_for_item(last_seen: Option<&str>) -> Option<String> {
    let deadline = Instant::now() + CLIPBOARD_TIMEOUT;
    let last = last_seen.map(normalize_item_text);
    // If the clipboard only ever holds the same item, that's a re-view — return
    // it so the popup re-shows (the UI de-dups the API call). Poll the full
    // window first so a genuine switch (whose write can lag the keypress) wins.
    // Return `None` only if the clipboard never holds a parseable item.
    let mut same: Option<String> = None;
    loop {
        if let Ok(Some(text)) = platform_linux::read_clipboard_text() {
            match clip_step(&text, last.as_deref()) {
                ClipStep::Different => return Some(text), // a new item appeared → use it now
                ClipStep::Same => same = Some(text), // same as loaded; keep watching for a switch
                ClipStep::NotItem => {}
            }
        }
        if Instant::now() >= deadline {
            return same;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// What a single clipboard read means relative to the last-seen item.
#[derive(Debug, PartialEq, Eq)]
enum ClipStep {
    /// A different parseable item than last seen — show it now.
    Different,
    /// The same item as last seen — a re-view (show it; the UI caches the query).
    Same,
    /// Not a POE2 item (ignore this read).
    NotItem,
}

/// Classify a clipboard read against the whitespace-normalised last-seen item.
fn clip_step(text: &str, last_normalized: Option<&str>) -> ClipStep {
    if parser::parse_item(text).is_err() {
        return ClipStep::NotItem;
    }
    if last_normalized == Some(normalize_item_text(text).as_str()) {
        ClipStep::Same
    } else {
        ClipStep::Different
    }
}

#[cfg(test)]
mod tests {
    use super::{clip_step, ClipStep};
    use crate::model::normalize_item_text;

    const RING: &str = "Item Class: Rings\nRarity: Rare\nHonour Spiral\nTopaz Ring\n--------\n+30% to Lightning Resistance";
    const RUNE: &str = "Item Class: Augment\nRarity: Currency\nFarrul's Rune of the Chase\n--------\nStack Size: 1/10\nRune";

    #[test]
    fn reviewing_the_same_item_is_not_ignored() {
        let last = normalize_item_text(RING);
        // Re-copying the SAME item must classify as Same (so the popup re-shows),
        // NOT be dropped — this was the "re-view does nothing" bug.
        assert_eq!(clip_step(RING, Some(&last)), ClipStep::Same);
    }

    #[test]
    fn a_different_item_is_new() {
        let last = normalize_item_text(RING);
        assert_eq!(clip_step(RUNE, Some(&last)), ClipStep::Different);
        // With nothing seen yet, any item is new.
        assert_eq!(clip_step(RING, None), ClipStep::Different);
    }

    #[test]
    fn non_item_clipboard_is_ignored() {
        assert_eq!(
            clip_step("https://example.com/not-an-item", None),
            ClipStep::NotItem
        );
    }
}
