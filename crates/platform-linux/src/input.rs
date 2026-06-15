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
    /// F5 — run the configured chat macro (e.g. `/hideout`) via uinput.
    Macro,
    /// F2 — run the second configured chat macro (e.g. `/exit`) via uinput.
    Macro2,
}

/// A key plus an exact modifier combination, parsed from a string like
/// `"Ctrl+Alt+C"` / `"F5"` / `"Escape"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    key: Key,
    ctrl: bool,
    alt: bool,
    shift: bool,
}

impl Binding {
    /// Parse `"Ctrl+Alt+C"`-style strings (case-insensitive modifiers; the last
    /// `+`-segment is the key). Errors on an unknown key name.
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        let mut ctrl = false;
        let mut alt = false;
        let mut shift = false;
        let mut key = None;
        for part in s.split('+').map(str::trim).filter(|p| !p.is_empty()) {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => ctrl = true,
                "alt" => alt = true,
                "shift" => shift = true,
                other => key = Some(key_from_name(other).ok_or_else(|| {
                    anyhow::anyhow!("unknown key `{part}` in hotkey `{s}`")
                })?),
            }
        }
        let key = key.ok_or_else(|| anyhow::anyhow!("no key in hotkey `{s}`"))?;
        Ok(Binding { key, ctrl, alt, shift })
    }

    fn matches(&self, key: Key, ctrl: bool, alt: bool, shift: bool) -> bool {
        self.key == key && self.ctrl == ctrl && self.alt == alt && self.shift == shift
    }
}

/// The configurable hotkeys (PRD §4.8 makes these rebindable).
#[derive(Debug, Clone, Copy)]
pub struct HotkeyBindings {
    pub quick: Binding,
    pub detailed: Binding,
    pub close: Binding,
    pub macro_: Binding,
    pub macro2: Binding,
}

impl Default for HotkeyBindings {
    fn default() -> Self {
        HotkeyBindings {
            quick: Binding { key: Key::KEY_C, ctrl: true, alt: false, shift: false },
            detailed: Binding { key: Key::KEY_C, ctrl: true, alt: true, shift: false },
            close: Binding { key: Key::KEY_ESC, ctrl: false, alt: false, shift: false },
            macro_: Binding { key: Key::KEY_F5, ctrl: false, alt: false, shift: false },
            macro2: Binding { key: Key::KEY_F2, ctrl: false, alt: false, shift: false },
        }
    }
}

impl HotkeyBindings {
    /// Build from config strings, falling back to the default for any that fail
    /// to parse (logged, so a typo'd binding doesn't disable the whole hotkey).
    pub fn from_strings(
        quick: &str,
        detailed: &str,
        macro_: &str,
        macro2: &str,
        close: &str,
    ) -> Self {
        let d = Self::default();
        let one = |s: &str, fallback: Binding| {
            Binding::parse(s).unwrap_or_else(|e| {
                warn!(error = %e, "invalid hotkey; using default");
                fallback
            })
        };
        HotkeyBindings {
            quick: one(quick, d.quick),
            detailed: one(detailed, d.detailed),
            macro_: one(macro_, d.macro_),
            macro2: one(macro2, d.macro2),
            close: one(close, d.close),
        }
    }

    /// Which action a key-press maps to, given the exact modifier state.
    /// Detailed (more modifiers) is checked first; exact matching means at most
    /// one binding applies.
    fn event_for(&self, key: Key, ctrl: bool, alt: bool, shift: bool) -> Option<HotkeyEvent> {
        if self.detailed.matches(key, ctrl, alt, shift) {
            Some(HotkeyEvent::DetailedCopy)
        } else if self.quick.matches(key, ctrl, alt, shift) {
            Some(HotkeyEvent::QuickCopy)
        } else if self.close.matches(key, ctrl, alt, shift) {
            Some(HotkeyEvent::Close)
        } else if self.macro_.matches(key, ctrl, alt, shift) {
            Some(HotkeyEvent::Macro)
        } else if self.macro2.matches(key, ctrl, alt, shift) {
            Some(HotkeyEvent::Macro2)
        } else {
            None
        }
    }
}

/// Map a key name (`"c"`, `"f5"`, `"escape"`, `"space"`, …) to an evdev [`Key`].
fn key_from_name(name: &str) -> Option<Key> {
    let n = name.to_ascii_lowercase();
    // Single letters a-z and digits 0-9.
    if n.len() == 1 {
        let c = n.chars().next().unwrap();
        if c.is_ascii_alphanumeric() {
            return ascii_key(c);
        }
    }
    Some(match n.as_str() {
        "escape" | "esc" => Key::KEY_ESC,
        "enter" | "return" => Key::KEY_ENTER,
        "space" => Key::KEY_SPACE,
        "tab" => Key::KEY_TAB,
        "f1" => Key::KEY_F1,
        "f2" => Key::KEY_F2,
        "f3" => Key::KEY_F3,
        "f4" => Key::KEY_F4,
        "f5" => Key::KEY_F5,
        "f6" => Key::KEY_F6,
        "f7" => Key::KEY_F7,
        "f8" => Key::KEY_F8,
        "f9" => Key::KEY_F9,
        "f10" => Key::KEY_F10,
        "f11" => Key::KEY_F11,
        "f12" => Key::KEY_F12,
        _ => return None,
    })
}

fn ascii_key(c: char) -> Option<Key> {
    Some(match c {
        'a' => Key::KEY_A, 'b' => Key::KEY_B, 'c' => Key::KEY_C, 'd' => Key::KEY_D,
        'e' => Key::KEY_E, 'f' => Key::KEY_F, 'g' => Key::KEY_G, 'h' => Key::KEY_H,
        'i' => Key::KEY_I, 'j' => Key::KEY_J, 'k' => Key::KEY_K, 'l' => Key::KEY_L,
        'm' => Key::KEY_M, 'n' => Key::KEY_N, 'o' => Key::KEY_O, 'p' => Key::KEY_P,
        'q' => Key::KEY_Q, 'r' => Key::KEY_R, 's' => Key::KEY_S, 't' => Key::KEY_T,
        'u' => Key::KEY_U, 'v' => Key::KEY_V, 'w' => Key::KEY_W, 'x' => Key::KEY_X,
        'y' => Key::KEY_Y, 'z' => Key::KEY_Z,
        '0' => Key::KEY_0, '1' => Key::KEY_1, '2' => Key::KEY_2, '3' => Key::KEY_3,
        '4' => Key::KEY_4, '5' => Key::KEY_5, '6' => Key::KEY_6, '7' => Key::KEY_7,
        '8' => Key::KEY_8, '9' => Key::KEY_9,
        _ => return None,
    })
}

/// Start watching every connected keyboard for the price-check hotkeys.
///
/// Returns a receiver that yields a [`HotkeyEvent`] on each matching press.
/// Reader threads are detached and live for the process lifetime.
pub fn watch_hotkeys(bindings: HotkeyBindings) -> anyhow::Result<Receiver<HotkeyEvent>> {
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
                    .spawn(move || reader_loop(device, label, tx, bindings))?;
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

/// Blocking read loop for one keyboard. Tracks this keyboard's modifier state
/// and emits a hotkey when a key matching a configured [`Binding`] is pressed.
fn reader_loop(mut device: Device, label: String, tx: Sender<HotkeyEvent>, bindings: HotkeyBindings) {
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
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
                    // The overlay still needs Ctrl/Alt state (drag + show).
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
                Key::KEY_LEFTSHIFT | Key::KEY_RIGHTSHIFT => shift = pressed,
                _ => {}
            }
            // Action bindings fire on the initial press only (not autorepeat).
            if event.value() == 1 {
                if let Some(hotkey) = bindings.event_for(key, ctrl, alt, shift) {
                    if tx.send(hotkey).is_err() {
                        return;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modifiers_and_key() {
        let b = Binding::parse("Ctrl+Alt+C").unwrap();
        assert!(b.ctrl && b.alt && !b.shift && b.key == Key::KEY_C);
        let f5 = Binding::parse("F5").unwrap();
        assert!(!f5.ctrl && !f5.alt && f5.key == Key::KEY_F5);
        assert_eq!(Binding::parse("escape").unwrap().key, Key::KEY_ESC);
        assert!(Binding::parse("Ctrl+Nonsense").is_err());
    }

    #[test]
    fn exact_match_disambiguates_quick_and_detailed() {
        let b = HotkeyBindings::default();
        // Ctrl+C → quick; Ctrl+Alt+C → detailed; neither bare nor wrong-mods.
        assert_eq!(b.event_for(Key::KEY_C, true, false, false), Some(HotkeyEvent::QuickCopy));
        assert_eq!(b.event_for(Key::KEY_C, true, true, false), Some(HotkeyEvent::DetailedCopy));
        assert_eq!(b.event_for(Key::KEY_C, false, false, false), None);
        assert_eq!(b.event_for(Key::KEY_F5, false, false, false), Some(HotkeyEvent::Macro));
        assert_eq!(b.event_for(Key::KEY_F2, false, false, false), Some(HotkeyEvent::Macro2));
        assert_eq!(b.event_for(Key::KEY_ESC, false, false, false), Some(HotkeyEvent::Close));
    }
}
