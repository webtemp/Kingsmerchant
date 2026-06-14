//! Active-window detection, to gate hotkeys to POE2 (PRD §9.4: POE2 runs under
//! XWayland, so `xdotool` can see it via the X server's `_NET_ACTIVE_WINDOW`).
//!
//! When a Wayland-native window is focused there is no active *X* window, so
//! `xdotool` reports nothing — which correctly reads as "POE2 not focused".

use std::process::Command;

/// Whether the focused window looks like Path of Exile.
///
/// Returns `false` if xdotool is unavailable or no X window is active (e.g. a
/// Wayland-native app has focus). Because a wrong `false` would silently disable
/// the price-check hotkey, this is gated behind `config.require_poe2_focus`.
pub fn is_poe2_active() -> bool {
    let Ok(out) = Command::new("xdotool")
        .args(["getactivewindow", "getwindowname"])
        .output()
    else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    String::from_utf8_lossy(&out.stdout)
        .to_lowercase()
        .contains("path of exile")
}
