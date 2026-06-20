//! Clipboard reads for POE2 interop. Reads POE2's X11 CLIPBOARD directly via
//! `xclip`; read-only so POE2 keeps selection ownership.

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::Context;

/// Read the X11 CLIPBOARD selection as UTF-8 text. `Ok(None)` when empty/no text.
pub fn read_clipboard_text() -> anyhow::Result<Option<String>> {
    let output = Command::new("xclip")
        .args(["-selection", "clipboard", "-out", "-target", "UTF8_STRING"])
        .output()
        .context("failed to run `xclip` (is it installed and is DISPLAY set?)")?;

    if !output.status.success() {
        return Ok(None);
    }

    if output.stdout.is_empty() {
        return Ok(None);
    }

    Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned()))
}

/// Read clipboard text for pasting into our own fields: Wayland clipboard first
/// (`wl-paste`), falling back to X11.
pub fn read_paste_text() -> anyhow::Result<Option<String>> {
    if let Some(text) = wl_paste_text() {
        if !text.is_empty() {
            return Ok(Some(text));
        }
    }
    read_clipboard_text()
}

/// Read the Wayland clipboard via `wl-paste`; `None` falls through to the X11 read.
fn wl_paste_text() -> Option<String> {
    let output = Command::new("wl-paste")
        .args(["--no-newline", "--type", "text/plain"])
        .output()
        .ok()?;
    if !output.status.success() || output.stdout.is_empty() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Open a URL in the user's default browser via `xdg-open`.
pub fn open_url(url: &str) -> anyhow::Result<()> {
    let child = Command::new("xdg-open")
        .arg(url)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to run `xdg-open` (is xdg-utils installed?)")?;
    // Reap off-thread so the short-lived xdg-open doesn't linger as a zombie.
    std::thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });
    Ok(())
}

/// Write UTF-8 text to the X11 CLIPBOARD selection (for the chat-paste buttons).
/// POE2 reads X11 so we must write there; one-shot, POE2 reclaims on its next copy.
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
    // Non-zero exit means the selection wasn't taken; fail loudly to avoid stale paste.
    anyhow::ensure!(
        status.success(),
        "xclip exited with {status} while writing the clipboard"
    );
    Ok(())
}
