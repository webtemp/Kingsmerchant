//! Active-window detection to gate hotkeys to POE2, via `xdotool` (POE2 runs under
//! XWayland so the X server's `_NET_ACTIVE_WINDOW` sees it).

use std::process::Command;

/// Whether the focused window looks like Path of Exile. `false` if xdotool is
/// unavailable or no X window is active.
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

/// Bring the Path of Exile window to the foreground (best-effort, no `--sync` as
/// it can hang). Returns whether the command ran; confirm with [`is_poe2_active`].
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
