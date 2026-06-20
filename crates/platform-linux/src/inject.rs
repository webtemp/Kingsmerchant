//! Fast chat-command injection into the focused window (POE2) via a persistent
//! uinput virtual keyboard: clipboard the command, then Enter, Ctrl+V, Enter.

use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use evdev::{
    uinput::{VirtualDevice, VirtualDeviceBuilder},
    AttributeSet, EventType, InputEvent, Key,
};

use crate::clipboard::{read_clipboard_text, write_clipboard_text};

static DEVICE: OnceLock<Mutex<VirtualDevice>> = OnceLock::new();

/// Chat prefixes that open a fresh command line in POE (no Ctrl+A clear needed).
const AUTO_CLEAR_PREFIXES: &[char] = &['#', '%', '@', '$', '&', '/'];

/// Whether `command` opens a fresh chat line (starts with a chat prefix).
fn auto_clears(command: &str) -> bool {
    command
        .chars()
        .next()
        .is_some_and(|c| AUTO_CLEAR_PREFIXES.contains(&c))
}

/// Brief settle so the xclip selection helper is serving before the first keystroke.
const CLIPBOARD_SETTLE: Duration = Duration::from_millis(25);

/// Open chat, paste `command`, and send it. Blocks ~250ms on the first call
/// (device setup); ~80ms after. Call off the UI thread.
pub fn send_chat_command(command: &str) -> Result<()> {
    // Save/restore the user's clipboard so we don't clobber what they copied.
    let saved = read_clipboard_text().ok().flatten();
    write_clipboard_text(command).context("set clipboard for chat paste")?;

    let auto_clear = auto_clears(command);
    {
        // Recover from a poisoned lock rather than panic forever.
        let mut device = device()?
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        thread::sleep(CLIPBOARD_SETTLE);
        // Fire the sequence with no gaps so POE2 drains it in one input tick and
        // closes chat before drawing a frame with it open; no pacing sleeps.
        tap(&mut device, Key::KEY_ENTER)?;
        if !auto_clear {
            ctrl_tap(&mut device, Key::KEY_A)?;
        }
        ctrl_tap(&mut device, Key::KEY_V)?;
        tap(&mut device, Key::KEY_ENTER)?;
    }

    // Let POE2 finish reading the pasted selection before restoring the old value.
    thread::sleep(Duration::from_millis(120));
    if let Some(s) = saved {
        let _ = write_clipboard_text(&s);
    }
    Ok(())
}

/// Synthesize Ctrl+C into the focused window so POE2 copies the item under the
/// cursor; needed when the price-check hotkey is rebound off Ctrl+C.
pub fn copy_item_under_cursor() -> Result<()> {
    let mut device = device()?
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    ctrl_tap(&mut device, Key::KEY_C)
}

/// Create the virtual keyboard ahead of time so the first real use is instant.
pub fn warm_up() {
    if let Err(e) = device() {
        tracing::warn!(error = %format!("{e:#}"), "could not pre-create injection device");
    }
}

/// The shared virtual keyboard, created on first call (one-time enumeration wait).
fn device() -> Result<&'static Mutex<VirtualDevice>> {
    if let Some(d) = DEVICE.get() {
        return Ok(d);
    }
    let mut keys = AttributeSet::<Key>::new();
    for k in [
        Key::KEY_ENTER,
        Key::KEY_LEFTCTRL,
        Key::KEY_A,
        Key::KEY_V,
        Key::KEY_C,
    ] {
        keys.insert(k);
    }
    let device = VirtualDeviceBuilder::new()
        .context("open /dev/uinput (in the `input` group?)")?
        .name("kingsmerchant-virtual-kbd")
        .with_keys(&keys)
        .context("declare virtual keyboard keys")?
        .build()
        .context("create uinput keyboard")?;
    // Let the compositor enumerate the new device before it's first used.
    thread::sleep(Duration::from_millis(250));
    Ok(DEVICE.get_or_init(|| Mutex::new(device)))
}

fn emit(device: &mut VirtualDevice, key: Key, value: i32) -> Result<()> {
    device
        .emit(&[InputEvent::new(EventType::KEY, key.code(), value)])
        .context("emit key event")
}

fn tap(device: &mut VirtualDevice, key: Key) -> Result<()> {
    emit(device, key, 1)?;
    emit(device, key, 0)
}

fn ctrl_tap(device: &mut VirtualDevice, key: Key) -> Result<()> {
    emit(device, Key::KEY_LEFTCTRL, 1)?;
    let tapped = emit(device, key, 1).and_then(|()| emit(device, key, 0));
    // Always release Ctrl even if the tap failed, so it's never left stuck down.
    let released = emit(device, Key::KEY_LEFTCTRL, 0);
    tapped.and(released)
}

#[cfg(test)]
mod tests {
    use super::auto_clears;

    #[test]
    fn auto_clears_recognizes_chat_prefixes() {
        for cmd in [
            "/hideout",
            "@friend hi",
            "#global hi",
            "%party",
            "$trade",
            "&guild",
        ] {
            assert!(auto_clears(cmd), "{cmd:?} should auto-clear");
        }
    }

    #[test]
    fn auto_clears_false_for_plain_text_or_empty() {
        assert!(!auto_clears("hello world"));
        assert!(!auto_clears(""));
        assert!(!auto_clears(" /hideout"));
    }
}
