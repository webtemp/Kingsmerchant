//! Global hotkey detection by reading evdev keyboards directly.
//!
//! We read `/dev/input/by-id/*-event-kbd` rather than going through the
//! compositor (KDE Plasma 6 Wayland has no usable global-shortcut path for an
//! XWayland-targeted overlay). The user must be in the `input` group for the
//! device nodes to be readable.
//!
//! We bind to POE2's own copy combos on purpose: the game does the copy, we just
//! observe the keypress and then read the resulting clipboard.
//!
//! One blocking reader thread per keyboard; std threads have no pool cap, so
//! many keyboards is fine.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, RwLock};
use std::thread;

use evdev::{Device, InputEventKind, Key};
use tracing::{debug, warn};

const KBD_DIR: &str = "/dev/input/by-id";
const KBD_SUFFIX: &str = "-event-kbd";

/// A recognized global hotkey press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    /// The price-check hotkey (Ctrl+C by default). When it isn't POE2's own
    /// Ctrl+C copy the caller synthesizes that copy before reading the clipboard.
    QuickCopy,
    /// Dismiss the overlay — Escape, or Alt+Tab (the user switching away).
    /// Detected globally because the overlay takes no keyboard focus (so Wayland
    /// never delivers it the key). Observe-only: POE2 still sees the keys too.
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
    pub(crate) fn parse(s: &str) -> anyhow::Result<Self> {
        let mut ctrl = false;
        let mut alt = false;
        let mut shift = false;
        let mut key = None;
        for part in s.split('+').map(str::trim).filter(|p| !p.is_empty()) {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => ctrl = true,
                "alt" => alt = true,
                "shift" => shift = true,
                other => {
                    key =
                        Some(key_from_name(other).ok_or_else(|| {
                            anyhow::anyhow!("unknown key `{part}` in hotkey `{s}`")
                        })?);
                }
            }
        }
        let key = key.ok_or_else(|| anyhow::anyhow!("no key in hotkey `{s}`"))?;
        Ok(Binding {
            key,
            ctrl,
            alt,
            shift,
        })
    }

    fn matches(self, key: Key, ctrl: bool, alt: bool, shift: bool) -> bool {
        self.key == key && self.ctrl == ctrl && self.alt == alt && self.shift == shift
    }

    /// Whether this is exactly POE2's own copy combo (Ctrl+C) — the keypress the
    /// game itself turns into "copy the item under the cursor". When the quick
    /// hotkey is this, the game does the copy; otherwise we synthesize it.
    fn is_native_copy(self) -> bool {
        self.key == Key::KEY_C && self.ctrl && !self.alt && !self.shift
    }
}

/// The configurable, rebindable hotkeys.
#[derive(Debug, Clone, Copy)]
pub struct HotkeyBindings {
    pub quick: Binding,
    pub close: Binding,
    pub macro_: Binding,
    pub macro2: Binding,
}

impl Default for HotkeyBindings {
    fn default() -> Self {
        HotkeyBindings {
            quick: Binding {
                key: Key::KEY_C,
                ctrl: true,
                alt: false,
                shift: false,
            },
            close: Binding {
                key: Key::KEY_ESC,
                ctrl: false,
                alt: false,
                shift: false,
            },
            macro_: Binding {
                key: Key::KEY_F5,
                ctrl: false,
                alt: false,
                shift: false,
            },
            macro2: Binding {
                key: Key::KEY_F2,
                ctrl: false,
                alt: false,
                shift: false,
            },
        }
    }
}

impl HotkeyBindings {
    /// Build from config strings, falling back to the default for any that fail
    /// to parse (logged, so a typo'd binding doesn't disable the whole hotkey).
    pub fn from_strings(quick: &str, macro_: &str, macro2: &str, close: &str) -> Self {
        let d = Self::default();
        let one = |s: &str, fallback: Binding| {
            Binding::parse(s).unwrap_or_else(|e| {
                warn!(error = %e, "invalid hotkey; using default");
                fallback
            })
        };
        HotkeyBindings {
            quick: one(quick, d.quick),
            macro_: one(macro_, d.macro_),
            macro2: one(macro2, d.macro2),
            close: one(close, d.close),
        }
    }

    /// Whether the quick hotkey needs us to synthesize a Ctrl+C copy before
    /// reading the clipboard. True unless quick *is* Ctrl+C, which POE2 copies on
    /// its own; for any other binding (e.g. Ctrl+D) the game never copies, so we
    /// must do it.
    pub fn quick_needs_synthetic_copy(&self) -> bool {
        !self.quick.is_native_copy()
    }

    /// Which action a key-press maps to, given the exact modifier state. Exact
    /// matching means at most one binding applies.
    fn event_for(&self, key: Key, ctrl: bool, alt: bool, shift: bool) -> Option<HotkeyEvent> {
        if self.quick.matches(key, ctrl, alt, shift) {
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
    if let Some(c) = n.chars().next() {
        if n.len() == 1 && c.is_ascii_alphanumeric() {
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
        'a' => Key::KEY_A,
        'b' => Key::KEY_B,
        'c' => Key::KEY_C,
        'd' => Key::KEY_D,
        'e' => Key::KEY_E,
        'f' => Key::KEY_F,
        'g' => Key::KEY_G,
        'h' => Key::KEY_H,
        'i' => Key::KEY_I,
        'j' => Key::KEY_J,
        'k' => Key::KEY_K,
        'l' => Key::KEY_L,
        'm' => Key::KEY_M,
        'n' => Key::KEY_N,
        'o' => Key::KEY_O,
        'p' => Key::KEY_P,
        'q' => Key::KEY_Q,
        'r' => Key::KEY_R,
        's' => Key::KEY_S,
        't' => Key::KEY_T,
        'u' => Key::KEY_U,
        'v' => Key::KEY_V,
        'w' => Key::KEY_W,
        'x' => Key::KEY_X,
        'y' => Key::KEY_Y,
        'z' => Key::KEY_Z,
        '0' => Key::KEY_0,
        '1' => Key::KEY_1,
        '2' => Key::KEY_2,
        '3' => Key::KEY_3,
        '4' => Key::KEY_4,
        '5' => Key::KEY_5,
        '6' => Key::KEY_6,
        '7' => Key::KEY_7,
        '8' => Key::KEY_8,
        '9' => Key::KEY_9,
        _ => return None,
    })
}

/// A shared, live-updatable set of [`HotkeyBindings`]. The detached evdev reader
/// threads read it on every keypress, so [`set`](Self::set) takes effect at once
/// — no restart needed when the user rebinds a hotkey.
#[derive(Clone)]
pub struct HotkeyControl {
    bindings: Arc<RwLock<HotkeyBindings>>,
}

impl HotkeyControl {
    pub fn new(bindings: HotkeyBindings) -> Self {
        HotkeyControl {
            bindings: Arc::new(RwLock::new(bindings)),
        }
    }

    /// Replace the live bindings (e.g. after a settings change). Recovers from a
    /// poisoned lock: a reader-thread panic mustn't freeze rebinding forever.
    pub fn set(&self, bindings: HotkeyBindings) {
        *self
            .bindings
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = bindings;
    }

    /// A snapshot of the current bindings ([`HotkeyBindings`] is `Copy`).
    pub fn snapshot(&self) -> HotkeyBindings {
        *self
            .bindings
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Start watching every connected keyboard for the price-check hotkeys.
///
/// `control` holds the live bindings the reader threads consult per keypress, so
/// rebinds apply without a restart. Returns a receiver that yields a
/// [`HotkeyEvent`] on each matching press. Reader threads are detached and live
/// for the process lifetime.
pub fn watch_hotkeys(control: &HotkeyControl) -> anyhow::Result<Receiver<HotkeyEvent>> {
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
                let bindings = control.bindings.clone();
                let label = path.display().to_string();
                thread::Builder::new()
                    .name(format!("evdev:{label}"))
                    .spawn(move || reader_loop(device, label, tx, &bindings))?;
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
        if entry.file_name().to_string_lossy().ends_with(KBD_SUFFIX) {
            paths.push(entry.path());
        }
    }
    paths.sort();
    Ok(paths)
}

/// Blocking read loop for one keyboard. Tracks this keyboard's modifier state
/// and emits a hotkey when a key matching a configured [`Binding`] is pressed.
/// Reads `bindings` afresh on each action keypress, so a live rebind applies
/// immediately.
// Owns its inputs: this runs as a thread body for the process lifetime.
#[allow(clippy::needless_pass_by_value)]
fn reader_loop(
    mut device: Device,
    label: String,
    tx: Sender<HotkeyEvent>,
    bindings: &RwLock<HotkeyBindings>,
) {
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
            // Alt+Tab — the compositor's window switcher — also dismisses the
            // overlay, matching Escape / click-outside. Detected here (globally,
            // on the initial press) because the overlay holds no keyboard focus
            // while merely shown, so Wayland never delivers it the combo.
            if event.value() == 1 && alt && key == Key::KEY_TAB && tx.send(HotkeyEvent::Close).is_err()
            {
                return;
            }
            // Action bindings fire on the initial press only (not autorepeat).
            if event.value() == 1 {
                let matched = bindings
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .event_for(key, ctrl, alt, shift);
                if let Some(hotkey) = matched {
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
    fn parse_is_case_insensitive_and_trims_whitespace() {
        let spaced = Binding::parse("CONTROL + alt + c").unwrap();
        let tight = Binding::parse("ctrl+ALT+C").unwrap();
        assert_eq!(spaced, tight);
        assert!(spaced.ctrl && spaced.alt && !spaced.shift && spaced.key == Key::KEY_C);
    }

    #[test]
    fn parse_handles_shift_and_control_alias() {
        let b = Binding::parse("Shift+Control+A").unwrap();
        assert!(b.ctrl && b.shift && !b.alt && b.key == Key::KEY_A);
    }

    #[test]
    fn parse_rejects_missing_key_or_empty() {
        assert!(Binding::parse("Ctrl+Alt").is_err()); // modifiers only
        assert!(Binding::parse("").is_err());
        assert!(Binding::parse("+").is_err());
        assert!(Binding::parse("Ctrl+").is_err()); // trailing + → no key
    }

    #[test]
    fn parse_last_key_segment_wins() {
        // Each non-modifier segment sets the key; the last one wins.
        assert_eq!(Binding::parse("a+b").unwrap().key, Key::KEY_B);
    }

    #[test]
    fn key_from_name_covers_letters_digits_and_named_keys() {
        assert_eq!(key_from_name("c"), Some(Key::KEY_C));
        assert_eq!(key_from_name("Z"), Some(Key::KEY_Z));
        assert_eq!(key_from_name("0"), Some(Key::KEY_0));
        assert_eq!(key_from_name("9"), Some(Key::KEY_9));
        assert_eq!(key_from_name("esc"), Some(Key::KEY_ESC));
        assert_eq!(key_from_name("Escape"), Some(Key::KEY_ESC));
        assert_eq!(key_from_name("return"), Some(Key::KEY_ENTER));
        assert_eq!(key_from_name("enter"), Some(Key::KEY_ENTER));
        assert_eq!(key_from_name("space"), Some(Key::KEY_SPACE));
        assert_eq!(key_from_name("tab"), Some(Key::KEY_TAB));
        assert_eq!(key_from_name("f1"), Some(Key::KEY_F1));
        assert_eq!(key_from_name("F12"), Some(Key::KEY_F12));
    }

    #[test]
    fn key_from_name_rejects_unknown() {
        assert_eq!(key_from_name("f13"), None);
        assert_eq!(key_from_name("nonsense"), None);
        assert_eq!(key_from_name("!"), None);
        assert_eq!(key_from_name(""), None);
    }

    #[test]
    fn exact_match_maps_quick_and_others() {
        let b = HotkeyBindings::default();
        // Ctrl+C → quick; bare C and wrong-mods (Ctrl+Alt+C) don't map.
        assert_eq!(
            b.event_for(Key::KEY_C, true, false, false),
            Some(HotkeyEvent::QuickCopy)
        );
        assert_eq!(b.event_for(Key::KEY_C, true, true, false), None);
        assert_eq!(b.event_for(Key::KEY_C, false, false, false), None);
        assert_eq!(
            b.event_for(Key::KEY_F5, false, false, false),
            Some(HotkeyEvent::Macro)
        );
        assert_eq!(
            b.event_for(Key::KEY_F2, false, false, false),
            Some(HotkeyEvent::Macro2)
        );
        assert_eq!(
            b.event_for(Key::KEY_ESC, false, false, false),
            Some(HotkeyEvent::Close)
        );
    }

    #[test]
    fn event_for_requires_exact_modifiers() {
        let b = HotkeyBindings::default();
        // An extra Shift means Ctrl+C no longer matches the quick binding.
        assert_eq!(b.event_for(Key::KEY_C, true, false, true), None);
        // F5 with any modifier held isn't the macro.
        assert_eq!(b.event_for(Key::KEY_F5, true, false, false), None);
        // An unmapped key is never an event.
        assert_eq!(b.event_for(Key::KEY_X, false, false, false), None);
    }

    #[test]
    fn from_strings_falls_back_on_invalid_binding() {
        // A garbage `quick` binding falls back to the default Ctrl+C; the rest
        // parse from the given strings.
        let b = HotkeyBindings::from_strings("not-a-key", "F5", "F2", "Escape");
        assert_eq!(b.quick, HotkeyBindings::default().quick);
        assert_eq!(
            b.event_for(Key::KEY_C, true, false, false),
            Some(HotkeyEvent::QuickCopy)
        );
    }

    #[test]
    fn from_strings_rebinds_to_custom_keys() {
        // Args: quick, macro, macro2, close.
        let b = HotkeyBindings::from_strings("Ctrl+D", "F8", "F9", "Q");
        assert_eq!(
            b.event_for(Key::KEY_D, true, false, false),
            Some(HotkeyEvent::QuickCopy)
        );
        assert_eq!(
            b.event_for(Key::KEY_F8, false, false, false),
            Some(HotkeyEvent::Macro)
        );
        assert_eq!(
            b.event_for(Key::KEY_Q, false, false, false),
            Some(HotkeyEvent::Close)
        );
    }

    #[test]
    fn quick_needs_synthetic_copy_only_when_not_ctrl_c() {
        // Default quick is Ctrl+C — POE2 copies on its own.
        assert!(!HotkeyBindings::default().quick_needs_synthetic_copy());
        // Any other binding (e.g. Ctrl+D) needs a synthesized Ctrl+C.
        let rebound = HotkeyBindings::from_strings("Ctrl+D", "F5", "F2", "Escape");
        assert!(rebound.quick_needs_synthetic_copy());
        // Even C without Ctrl, or with extra modifiers, isn't the native copy.
        assert!(HotkeyBindings::from_strings("C", "F5", "F2", "Escape")
            .quick_needs_synthetic_copy());
        assert!(HotkeyBindings::from_strings("Ctrl+Alt+C", "F5", "F2", "Escape")
            .quick_needs_synthetic_copy());
    }
}
