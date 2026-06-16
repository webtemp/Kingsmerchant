//! Chat-injection actions the buttons and macros fire into POE2.

use std::time::Duration;

/// Send a chat command straight into POE2. Refocuses the game, confirms it's
/// active, then injects via the same uinput paste path as the macros. Falls back
/// to leaving the command on the clipboard if POE2 can't be focused. Off-thread:
/// the focus settle + inject block ~½s.
pub(super) fn send_chat_to_poe2(command: String) {
    if command.trim().is_empty() {
        return;
    }
    std::thread::spawn(move || {
        platform_linux::focus_poe2();
        // Let the compositor move keyboard focus to POE2 before injecting, else
        // the keystrokes land in our overlay.
        std::thread::sleep(Duration::from_millis(120));
        if platform_linux::is_poe2_active() {
            if let Err(e) = platform_linux::send_chat_command(&command) {
                tracing::warn!(error = %format!("{e:#}"), "chat send failed; left on clipboard");
                let _ = platform_linux::write_clipboard_text(&command);
            }
        } else {
            tracing::info!("POE2 not focusable — left command on clipboard to paste");
            let _ = platform_linux::write_clipboard_text(&command);
        }
    });
}

/// Run a chat macro (e.g. `/hideout`, `/exit`) off-thread — injects via uinput
/// into the focused window (POE2) and blocks ~½s. `None` / empty is a no-op.
pub(crate) fn run_chat_macro(command: Option<String>) {
    let Some(cmd) = command else { return };
    if cmd.trim().is_empty() {
        return;
    }
    tracing::info!(command = %cmd, "running chat macro");
    std::thread::spawn(move || {
        if let Err(e) = platform_linux::send_chat_command(&cmd) {
            tracing::warn!(error = %format!("{e:#}"), "chat macro failed");
        }
    });
}
