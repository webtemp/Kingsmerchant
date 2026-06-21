//! Windows platform layer for kingsmerchant.
//!
//! This mirrors the public surface of `platform-linux` exactly so the shared
//! `ui`/`overlay` code type-checks against it via the [`platform`](../platform)
//! facade. The native Win32 implementations are TODO; until then, fallible
//! functions return an error (so the app runs in a degraded, button-driven mode
//! instead of panicking) and infallible ones return safe defaults.
//!
//! Implementation map (Win32 APIs to use when filling these in):
//! - hotkeys: `RegisterHotKey` / `RAWINPUT` low-level keyboard hook
//! - clipboard: `OpenClipboard` / `GetClipboardData` / `SetClipboardData`
//! - injection: `SendInput`
//! - tray: `Shell_NotifyIcon`
//! - window: `FindWindow` / `GetWindowRect` / `SetForegroundWindow`

use std::sync::mpsc::Receiver;
use std::sync::{Arc, RwLock};

// ---------------------------------------------------------------------------
// clipboard
// ---------------------------------------------------------------------------

/// Read the current clipboard text, if any.
pub fn read_clipboard_text() -> anyhow::Result<Option<String>> {
    anyhow::bail!("platform-windows: read_clipboard_text not yet implemented")
}

/// Read clipboard text intended for a paste (settings import).
pub fn read_paste_text() -> anyhow::Result<Option<String>> {
    anyhow::bail!("platform-windows: read_paste_text not yet implemented")
}

/// Write `text` to the clipboard.
pub fn write_clipboard_text(_text: &str) -> anyhow::Result<()> {
    anyhow::bail!("platform-windows: write_clipboard_text not yet implemented")
}

/// Open `url` in the default browser (`ShellExecute`).
pub fn open_url(_url: &str) -> anyhow::Result<()> {
    anyhow::bail!("platform-windows: open_url not yet implemented")
}

// ---------------------------------------------------------------------------
// injection
// ---------------------------------------------------------------------------

/// Type a chat command into POE2 (focus, paste, Enter) via `SendInput`.
pub fn send_chat_command(_command: &str) -> anyhow::Result<()> {
    anyhow::bail!("platform-windows: send_chat_command not yet implemented")
}

/// Synthesize a Ctrl+C copy of the item under the cursor.
pub fn copy_item_under_cursor() -> anyhow::Result<()> {
    anyhow::bail!("platform-windows: copy_item_under_cursor not yet implemented")
}

/// Pre-warm the injection path so the first synthetic keypress isn't slow.
/// No-op on Windows (`SendInput` has no device-open cost); kept for API parity.
pub fn warm_up_injection() {}

// ---------------------------------------------------------------------------
// window
// ---------------------------------------------------------------------------

/// Whether the POE2 window is currently focused.
pub fn is_poe2_active() -> bool {
    false
}

/// The POE2 window geometry as `(x, y, width, height)`, if found.
pub fn poe2_window_geometry() -> Option<(i32, i32, i32, i32)> {
    None
}

/// Bring the POE2 window to the foreground. Returns whether it succeeded.
pub fn focus_poe2() -> bool {
    false
}

// ---------------------------------------------------------------------------
// hotkeys
// ---------------------------------------------------------------------------

/// A recognized global hotkey press. Variants mirror `platform-linux`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    /// The price-check hotkey (Ctrl+C by default).
    QuickCopy,
    /// Dismiss the overlay — Escape or Alt+Tab.
    Close,
    /// Ctrl / Alt state changed.
    Modifiers { ctrl: bool, alt: bool },
    /// F5 — run the configured chat macro.
    Macro,
    /// F2 — run the second configured chat macro.
    Macro2,
    /// Open the settings surface.
    OpenSettings,
}

/// A key plus an exact modifier combination, e.g. `"Ctrl+Alt+C"` / `"F5"`.
///
/// Stored as the raw config string for now; the native backend will parse this
/// into a Win32 virtual-key code + modifier flags.
#[derive(Debug, Clone)]
pub struct Binding {
    spec: String,
}

impl Binding {
    fn parse(s: &str) -> Self {
        Binding {
            spec: s.to_string(),
        }
    }

    fn is_native_copy(&self) -> bool {
        // Ctrl+C is POE2's own copy; anything else needs a synthetic copy.
        let s = self.spec.replace(' ', "").to_ascii_lowercase();
        s == "ctrl+c" || s == "control+c"
    }
}

/// The full set of rebindable hotkeys. Fields mirror `platform-linux`.
#[derive(Debug, Clone)]
pub struct HotkeyBindings {
    pub quick: Binding,
    pub close: Binding,
    pub macro_: Binding,
    pub macro2: Binding,
    pub settings: Binding,
}

impl Default for HotkeyBindings {
    fn default() -> Self {
        HotkeyBindings {
            quick: Binding::parse("Ctrl+C"),
            close: Binding::parse("Escape"),
            macro_: Binding::parse("F5"),
            macro2: Binding::parse("F2"),
            settings: Binding::parse("Ctrl+Alt+S"),
        }
    }
}

impl HotkeyBindings {
    /// Build from config strings (argument order matches `platform-linux`).
    pub fn from_strings(
        quick: &str,
        macro_: &str,
        macro2: &str,
        close: &str,
        settings: &str,
    ) -> Self {
        HotkeyBindings {
            quick: Binding::parse(quick),
            macro_: Binding::parse(macro_),
            macro2: Binding::parse(macro2),
            close: Binding::parse(close),
            settings: Binding::parse(settings),
        }
    }

    /// Whether the quick hotkey needs a synthesized Ctrl+C copy.
    pub fn quick_needs_synthetic_copy(&self) -> bool {
        !self.quick.is_native_copy()
    }
}

/// A shared, live-updatable set of [`HotkeyBindings`].
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

    /// Replace the live bindings.
    pub fn set(&self, bindings: HotkeyBindings) {
        *self
            .bindings
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = bindings;
    }

    /// A snapshot of the current bindings.
    pub fn snapshot(&self) -> HotkeyBindings {
        self.bindings
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// Start watching for the global hotkeys. Returns a receiver of [`HotkeyEvent`].
pub fn watch_hotkeys(_control: &HotkeyControl) -> anyhow::Result<Receiver<HotkeyEvent>> {
    anyhow::bail!("platform-windows: watch_hotkeys not yet implemented")
}

// ---------------------------------------------------------------------------
// tray
// ---------------------------------------------------------------------------

/// A menu action the user triggered from the tray.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    OpenSettings,
    Quit,
}

/// What the tooltip should report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayState {
    Listening,
    /// `secs` until the next allowed request.
    RateLimited(u64),
    /// Short reason for the tooltip.
    Error(String),
}

/// A handle for pushing state updates to the running tray.
pub struct TrayHandle {
    _private: (),
}

impl TrayHandle {
    /// Update the tooltip text.
    pub fn set_state(&mut self, _state: TrayState) {}
}

/// Spawn the system-tray icon, returning a handle and a receiver of menu actions.
pub fn spawn_tray() -> anyhow::Result<(TrayHandle, Receiver<TrayAction>)> {
    anyhow::bail!("platform-windows: spawn_tray not yet implemented")
}
