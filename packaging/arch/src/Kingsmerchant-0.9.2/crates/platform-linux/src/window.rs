//! Active-window detection, to gate hotkeys to POE2. POE2 runs under XWayland,
//! so `xdotool` can see it via the X server's `_NET_ACTIVE_WINDOW`.
//!
//! When a Wayland-native window is focused there is no active X window, so
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

/// Bring the Path of Exile window to the foreground (so a following chat-command
/// injection lands in the game, not in our overlay which had click focus).
///
/// Best-effort via `xdotool windowactivate`. Returns whether the command ran;
/// the caller should still confirm with [`is_poe2_active`] after a short settle
/// before injecting. Deliberately does NOT use `--sync` (which can hang forever
/// if the compositor never activates the window).
pub fn focus_poe2() -> bool {
    Command::new("xdotool")
        .args([
            "search",
            "--limit",
            "1",
            "--name",
            "Path of Exile",
            "windowactivate",
        ])
        .status()
        .is_ok_and(|s| s.success())
}
