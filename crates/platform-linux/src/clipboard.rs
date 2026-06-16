//! Clipboard reads for POE2 interop.
//!
//! POE2 runs under Proton, so it's an X11 client on XWayland; its Ctrl+C writes
//! the X11 CLIPBOARD selection. Reading that directly (X11→X11) is reliable and
//! instant, and avoids KWin's fragile X11→Wayland selection bridge.
//!
//! X11 CLIPBOARD reads aren't focus-gated, so this works without window focus.
//! We only ever read — POE2 keeps ownership. That's load-bearing: if anything
//! grabs selection ownership, POE2's next copy can't reliably reclaim it and
//! reads go stale. So: read only, never write/clear.
//!
//! Shells out to `xclip`; `DISPLAY` is inherited.

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::Context;

/// Read the X11 CLIPBOARD selection as UTF-8 text.
///
/// Returns `Ok(None)` when the clipboard is empty or holds no text target
/// (`xclip` exits non-zero / prints nothing) — normal states, not errors.
pub fn read_clipboard_text() -> anyhow::Result<Option<String>> {
    let output = Command::new("xclip")
        .args(["-selection", "clipboard", "-out", "-target", "UTF8_STRING"])
        .output()
        .context("failed to run `xclip` (is it installed and is DISPLAY set?)")?;

    if !output.status.success() {
        // xclip exits non-zero when the selection is empty or the UTF8_STRING
        // target isn't offered — treat as "nothing to read".
        return Ok(None);
    }

    if output.stdout.is_empty() {
        return Ok(None);
    }

    Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned()))
}

/// Open a URL in the user's default browser via `xdg-open`. Fire-and-forget:
/// `xdg-open` detaches the browser and returns immediately.
pub fn open_url(url: &str) -> anyhow::Result<()> {
    let child = Command::new("xdg-open")
        .arg(url)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to run `xdg-open` (is xdg-utils installed?)")?;
    // Reap the short-lived `xdg-open` off-thread so it doesn't linger as a
    // zombie in this long-running process, without blocking the UI.
    std::thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });
    Ok(())
}

/// Write UTF-8 text to the X11 CLIPBOARD selection (for the chat-paste buttons).
///
/// Breaks the "read only" rule above, deliberately: POE2 reads the X11 clipboard
/// so we must write there (egui's clipboard goes to Wayland, which KWin's flaky
/// bridge usually fails to deliver). Bounded — a one-shot action right before
/// pasting, and POE2 reclaims ownership on its next copy.
///
/// `xclip -in` forks a helper to serve the selection and returns, so this call
/// doesn't block.
pub fn write_clipboard_text(text: &str) -> anyhow::Result<()> {
    let mut child = Command::new("xclip")
        .args(["-selection", "clipboard", "-in"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to run `xclip` for write (is it installed?)")?;
    child
        .stdin
        .take()
        .context("xclip stdin unavailable")?
        .write_all(text.as_bytes())
        .context("failed to write to xclip stdin")?;
    let status = child.wait().context("xclip did not exit cleanly")?;
    // A non-zero exit (e.g. DISPLAY unset) means the selection wasn't taken —
    // fail loudly, else the caller pastes whatever stale text was there before.
    anyhow::ensure!(
        status.success(),
        "xclip exited with {status} while writing the clipboard"
    );
    Ok(())
}
