//! OS-thread watchers feeding the UI: the global-hotkey watcher and the config hot-reload watcher.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use platform::{HotkeyBindings, HotkeyControl};

use crate::config::Config;
use crate::model::{item_hash, normalize_item_text};
use crate::{Hotkey, CLIPBOARD_TIMEOUT, POLL_INTERVAL};

/// Debounce window for chat macros (swallows duplicate events from multiple event-kbd nodes).
const MACRO_DEBOUNCE: Duration = Duration::from_millis(300);

/// A live handle to the running hotkey watcher: rebindable bindings + the POE2-focus gate.
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
            &config.hotkey_settings,
        )
    }

    /// Apply the hotkey-relevant fields of `config` to the running watcher (live).
    pub fn apply_config(&self, config: &Config) {
        self.control.set(Self::bindings_from(config));
        self.require_focus
            .store(config.require_poe2_focus, Ordering::Relaxed);
    }
}

/// Watch the global price-check hotkeys on a background thread. Best-effort; logs and carries on.
///
/// Returns a [`HotkeyHandle`] so the app can rebind hotkeys / toggle the focus gate live.
pub fn spawn_hotkey_watcher(ctx: egui::Context, tx: Sender<Hotkey>) -> HotkeyHandle {
    use platform::HotkeyEvent;
    // Read-only load so this thread doesn't race the startup write or re-trigger the watcher.
    let config = Config::load_no_write();
    let control = HotkeyControl::new(HotkeyHandle::bindings_from(&config));
    let require_focus = Arc::new(AtomicBool::new(config.require_poe2_focus));
    let handle = HotkeyHandle {
        control: control.clone(),
        require_focus: require_focus.clone(),
    };

    std::thread::spawn(move || {
        let hotkeys = match platform::watch_hotkeys(&control) {
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
        // Pre-create the injection device so the first macro/copy avoids the ~250ms uinput wait.
        if control.snapshot().quick_needs_synthetic_copy()
            || config.f5_command.is_some()
            || config.macro2_command.is_some()
        {
            std::thread::spawn(platform::warm_up_injection);
        }
        // The last item we showed, so we only accept a clipboard that actually changed.
        let last_seen = Arc::new(Mutex::new(platform::read_clipboard_text().unwrap_or(None)));
        // Debounce duplicate device-node echoes (slot 0 = F5, 1 = F2).
        let mut last_macro: [Option<Instant>; 2] = [None, None];
        for event in hotkeys {
            match event {
                HotkeyEvent::Close => {
                    let _ = tx.send(Hotkey::Close);
                    ctx.request_repaint();
                }
                // Deliberately NOT focus-gated (you may have tabbed away).
                HotkeyEvent::OpenSettings => {
                    let _ = tx.send(Hotkey::OpenSettings);
                    ctx.request_repaint();
                }
                // Must be forwarded instantly (overlay drag/show).
                HotkeyEvent::Modifiers { ctrl, alt } => {
                    let _ = tx.send(Hotkey::Mods { ctrl, alt });
                    ctx.request_repaint();
                }
                // Chat macros — only into POE2. Off-thread so the focus check doesn't stall the loop.
                HotkeyEvent::Macro | HotkeyEvent::Macro2 => {
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
                        if require_focus.load(Ordering::Relaxed) && !platform::is_poe2_active() {
                            tracing::info!("macro ignored — POE2 not focused");
                            return;
                        }
                        let _ = tx.send(msg);
                        ctx.request_repaint();
                    });
                }
                // Price-check combo: focus check + clipboard poll run off-thread.
                HotkeyEvent::QuickCopy => {
                    let (tx, ctx, last, require_focus, control) = (
                        tx.clone(),
                        ctx.clone(),
                        last_seen.clone(),
                        require_focus.clone(),
                        control.clone(),
                    );
                    std::thread::spawn(move || {
                        if require_focus.load(Ordering::Relaxed) && !platform::is_poe2_active() {
                            tracing::info!("price-check hotkey ignored — POE2 not focused");
                            return;
                        }
                        // Synthesize the copy BEFORE showing the popup: the popup grabs
                        // keyboard focus, so a synth Ctrl+C sent afterwards lands on the
                        // overlay instead of POE2. Only runs for hotkeys rebound off Ctrl+C;
                        // a Ctrl+C hotkey already copied via the user's own keypress.
                        if control.snapshot().quick_needs_synthetic_copy() {
                            if let Err(e) = platform::copy_item_under_cursor() {
                                tracing::warn!(error = %format!("{e:#}"), "synthetic copy failed");
                            }
                        }
                        // Pop the popup now (focus confirmed), before the clipboard poll.
                        let _ = tx.send(Hotkey::CopyStarted);
                        ctx.request_repaint();

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
                            tracing::info!("clipboard: no new item → ignored (POE2 didn't copy)");
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

/// Watch `config.json` for external edits and push the reloaded config to the UI thread.
///
/// Watches the containing directory (editors save by rename, dropping an inode watch).
/// Reads are write-free so our own reload can't re-trigger the watcher.
pub fn spawn_config_watcher(ctx: egui::Context, tx: Sender<Hotkey>) {
    use notify::{RecursiveMode, Watcher};
    let path = Config::path();
    let Some(dir) = path.parent().map(std::path::Path::to_path_buf) else {
        tracing::warn!("config has no parent dir; hot-reload disabled");
        return;
    };
    let file_name = path.file_name().map(std::ffi::OsStr::to_os_string);

    std::thread::spawn(move || {
        // Coalesce the several events editors fire per save; seed the timer in the past.
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
        loop {
            std::thread::park();
        }
    });
}

/// Poll the clipboard until it holds a parseable POE2 item *different* from the
/// last-shown one — i.e. POE2 actually copied the newly-hovered item. On timeout
/// return `None` (→ retry hint) rather than the stale last item, so a failed copy
/// never silently shows the previous item.
fn wait_for_item(last_seen: Option<&str>) -> Option<String> {
    let deadline = Instant::now() + CLIPBOARD_TIMEOUT;
    let last = last_seen.map(normalize_item_text);
    loop {
        if let Ok(Some(text)) = platform::read_clipboard_text() {
            if parser::parse_item(&text).is_ok()
                && last.as_deref() != Some(normalize_item_text(&text).as_str())
            {
                return Some(text);
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}
