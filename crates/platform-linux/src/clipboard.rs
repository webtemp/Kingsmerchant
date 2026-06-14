//! Clipboard reads for POE2 interop (PRD §4.2).
//!
//! IMPORTANT — this differs from PRD §4.2/§9.2, which assumed the Wayland
//! `wlr-data-control` protocol. That was wrong for this target:
//!
//! POE2 runs under Proton, so it is an **X11 client** on the session's
//! XWayland server. Its Ctrl+C writes the **X11 CLIPBOARD selection**. Reading
//! that selection directly (X11→X11, same server) is reliable and instant —
//! it's exactly what pasting into any X11 app (e.g. Sublime) does, which works
//! every time.
//!
//! Reading the *Wayland* clipboard instead would force KWin to bridge the
//! selection X11→Wayland on every read. That bridge does work, but it is
//! fragile under selection-ownership contention — reading X11 directly avoids
//! it entirely, so there is no reason to cross it for an XWayland source.
//!
//! X11 CLIPBOARD reads are NOT focus-gated (that limitation is specific to
//! Wayland's core `wl_data_device`), so this works without window focus. We
//! only ever *read* the selection — POE2 keeps ownership, matching §4.2's
//! "never take the selection" rule. That rule is load-bearing: if anything
//! (our app, a clipboard manager, or a stray `xclip -i`/`wl-copy`) grabs
//! selection ownership, POE2's next copy can't reliably reclaim it and reads
//! go stale or empty. So: read only, never write/clear.
//!
//! Implementation: shell out to `xclip`, consistent with the PRD's decision to
//! shell out to `xdotool` for window position (§6). `DISPLAY` is inherited.

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

/// Write UTF-8 text to the X11 CLIPBOARD selection (for the Whisper / Invite /
/// … buttons, PRD §4.6).
///
/// This deliberately breaks the "read only, never write" rule above — but it's
/// the right tradeoff: POE2 reads the **X11** clipboard, so we must write there
/// (egui's own clipboard goes to Wayland and KWin's flaky X11 bridge usually
/// fails to deliver it to the game). The ownership concern is bounded: this is
/// a one-shot user action right before pasting into chat, and POE2 reclaims the
/// selection the moment it copies the next item.
///
/// `xclip -in` forks a helper that serves the selection and then returns, so
/// this call doesn't block.
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
    child.wait().context("xclip did not exit cleanly")?;
    Ok(())
}
