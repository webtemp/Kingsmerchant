//! Global hotkey detection by reading evdev keyboards directly.
//!
//! Per PRD §4.1 we read `/dev/input/by-id/*-event-kbd` rather than going
//! through the compositor (KDE Plasma 6 Wayland has no usable global-shortcut
//! path for an XWayland-targeted overlay). The user must be in the `input`
//! group for the device nodes to be readable.
//!
//! We bind to POE2's own copy combos on purpose (§4.1): the game does the
//! copy, we just observe the keypress and then read the resulting clipboard.
//!
//! One blocking reader thread per keyboard. The default-threadpool footgun
//! called out in the PRD is a Node/libuv concern; std threads have no such
//! cap, so connecting >4 keyboards is fine here.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use evdev::{Device, InputEventKind, Key};
use tracing::{debug, warn};

const KBD_DIR: &str = "/dev/input/by-id";
const KBD_SUFFIX: &str = "-event-kbd";

/// A recognized global hotkey press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    /// Ctrl+C — quick price check.
    QuickCopy,
    /// Ctrl+Alt+C — detailed price check.
    DetailedCopy,
    /// Escape — dismiss the popup. Detected globally because the overlay takes
    /// no keyboard focus (so Wayland never delivers it the key). Observe-only:
    /// POE2 still sees Escape too.
    Close,
    /// Ctrl / Alt state changed. The overlay is visible only while Ctrl is held
    /// and drags on Ctrl+Alt — but with no keyboard focus it can't read
    /// modifiers from Wayland, so we report them from evdev.
    Modifiers { ctrl: bool, alt: bool },
}

/// Start watching every connected keyboard for the price-check hotkeys.
///
/// Returns a receiver that yields a [`HotkeyEvent`] on each matching press.
/// Reader threads are detached and live for the process lifetime.
pub fn watch_hotkeys() -> anyhow::Result<Receiver<HotkeyEvent>> {
    let devices = keyboard_paths()?;
    if devices.is_empty() {
        anyhow::bail!(
            "no keyboards found under {KBD_DIR} (expected *{KBD_SUFFIX}); \
             is the session XWayland-capable and are you in the `input` group?"
        );
    }

    let (tx, rx) = mpsc::channel();
    let mut opened = 0;
    for path in devices {
        match Device::open(&path) {
            Ok(device) => {
                let tx = tx.clone();
                let label = path.display().to_string();
                thread::Builder::new()
                    .name(format!("evdev:{label}"))
                    .spawn(move || reader_loop(device, label, tx))?;
                opened += 1;
            }
            Err(err) => {
                // A single unreadable device (e.g. permissions on one node)
                // shouldn't sink the whole watcher.
                warn!(device = %path.display(), %err, "could not open keyboard");
            }
        }
    }

    if opened == 0 {
        anyhow::bail!(
            "found keyboard device nodes but could not open any \
             (are you in the `input` group?)"
        );
    }
    debug!(keyboards = opened, "watching for hotkeys");
    Ok(rx)
}

/// Enumerate keyboard event devices via the stable `by-id` symlinks.
fn keyboard_paths() -> anyhow::Result<Vec<PathBuf>> {
    let dir = Path::new(KBD_DIR);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry
            .file_name()
            .to_string_lossy()
            .ends_with(KBD_SUFFIX)
        {
            paths.push(entry.path());
        }
    }
    paths.sort();
    Ok(paths)
}

/// Blocking read loop for one keyboard. Tracks this keyboard's own modifier
/// state and emits a hotkey when `C` is pressed with the right modifiers.
fn reader_loop(mut device: Device, label: String, tx: Sender<HotkeyEvent>) {
    let mut ctrl = false;
    let mut alt = false;
    loop {
        let events = match device.fetch_events() {
            Ok(events) => events,
            Err(err) => {
                // Device unplugged or transient read error — drop this thread.
                warn!(device = %label, %err, "keyboard read ended");
                return;
            }
        };
        for event in events {
            let InputEventKind::Key(key) = event.kind() else {
                continue;
            };
            // value: 0 = release, 1 = press, 2 = autorepeat.
            let pressed = event.value() != 0;
            match key {
                Key::KEY_LEFTCTRL | Key::KEY_RIGHTCTRL if ctrl != pressed => {
                    ctrl = pressed;
                    if tx.send(HotkeyEvent::Modifiers { ctrl, alt }).is_err() {
                        return;
                    }
                }
                Key::KEY_LEFTALT | Key::KEY_RIGHTALT if alt != pressed => {
                    alt = pressed;
                    if tx.send(HotkeyEvent::Modifiers { ctrl, alt }).is_err() {
                        return;
                    }
                }
                Key::KEY_ESC if event.value() == 1 => {
                    if tx.send(HotkeyEvent::Close).is_err() {
                        return;
                    }
                }
                Key::KEY_C if event.value() == 1 && ctrl => {
                    let hotkey = if alt {
                        HotkeyEvent::DetailedCopy
                    } else {
                        HotkeyEvent::QuickCopy
                    };
                    // If the main thread is gone, so are we.
                    if tx.send(hotkey).is_err() {
                        return;
                    }
                }
                _ => {}
            }
        }
    }
}
