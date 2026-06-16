//! Fast chat-command injection into the focused window (POE2).
//!
//! Rather than type character-by-character, we put the command on the clipboard
//! and paste it: tap Enter (open chat), Ctrl+V, Enter (send). The user's
//! clipboard is saved and restored around it. Two things keep it instant: paste
//! not type (one Ctrl+V regardless of length), and a persistent uinput keyboard
//! reused across presses (avoiding the ~250ms enumeration wait each time).
//!
//! uinput is a kernel virtual hardware keyboard, so the compositor delivers keys
//! to the focused window, including XWayland POE2 (unlike X11 XTEST / wtype).
//! Types into whatever's focused, so POE2 must be focused (the hotkey gates on
//! that). Opt-in.

use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use evdev::{
    uinput::{VirtualDevice, VirtualDeviceBuilder},
    AttributeSet, EventType, InputEvent, Key,
};

use crate::clipboard::{read_clipboard_text, write_clipboard_text};

/// The virtual keyboard, created once on first use and kept alive.
static DEVICE: OnceLock<Mutex<VirtualDevice>> = OnceLock::new();

/// Chat prefixes that open a fresh command line in POE — no need to clear any
/// existing text with Ctrl+A first (mirrors EE2's `AUTO_CLEAR`).
const AUTO_CLEAR_PREFIXES: &[char] = &['#', '%', '@', '$', '&', '/'];

/// Brief settle so the xclip selection helper is serving before the first
/// keystroke. A safety margin (write already blocks until xclip owns the
/// selection); invisible since it's before chat opens.
const CLIPBOARD_SETTLE: Duration = Duration::from_millis(25);

/// Open chat, paste `command`, and send it — fast, so the chat box barely
/// flashes. Blocks ~250ms only on the first call (device setup); ~80ms after.
/// Call off the UI thread.
pub fn send_chat_command(command: &str) -> Result<()> {
    // Save the user's clipboard, set ours, paste, restore (so we don't clobber
    // whatever they had copied — e.g. the item they just priced).
    let saved = read_clipboard_text().ok().flatten();
    write_clipboard_text(command).context("set clipboard for chat paste")?;

    let auto_clear = command
        .chars()
        .next()
        .is_some_and(|c| AUTO_CLEAR_PREFIXES.contains(&c));
    {
        // Recover from a poisoned lock: a prior panic mid-emit doesn't corrupt
        // the VirtualDevice, and we'd rather keep injecting than panic forever.
        let mut device = device()?
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Settle before the burst — invisible, and guarantees the paste reads
        // our text rather than stale content.
        thread::sleep(CLIPBOARD_SETTLE);
        // Fire the whole sequence with no gaps, so POE2 drains it in order within
        // one input tick and closes chat before drawing a frame with it open
        // ("chat never appears"). No pacing sleeps: tiny sleeps overshoot and push
        // a key into the next frame — the visible flash we're avoiding. The kernel
        // preserves uinput emit order, so the burst still pastes into now-open chat.
        tap(&mut device, Key::KEY_ENTER)?; // open chat
        if !auto_clear {
            ctrl_tap(&mut device, Key::KEY_A)?; // clear any leftover text first
        }
        ctrl_tap(&mut device, Key::KEY_V)?; // paste the whole command at once
        tap(&mut device, Key::KEY_ENTER)?; // send (POE2 closes the chat input)
    }

    // Let POE2 finish reading the pasted selection before we restore the old
    // value. Invisible — the chat is already closed by now.
    thread::sleep(Duration::from_millis(120));
    if let Some(s) = saved {
        let _ = write_clipboard_text(&s);
    }
    Ok(())
}

/// Create the virtual keyboard ahead of time so the first real use is instant.
pub fn warm_up() {
    if let Err(e) = device() {
        tracing::warn!(error = %format!("{e:#}"), "could not pre-create injection device");
    }
}

/// The shared virtual keyboard, created on first call (with the one-time
/// enumeration wait). Only declares the keys we emit (Enter, Ctrl, A, V).
fn device() -> Result<&'static Mutex<VirtualDevice>> {
    if let Some(d) = DEVICE.get() {
        return Ok(d);
    }
    let mut keys = AttributeSet::<Key>::new();
    for k in [Key::KEY_ENTER, Key::KEY_LEFTCTRL, Key::KEY_A, Key::KEY_V] {
        keys.insert(k);
    }
    let device = VirtualDeviceBuilder::new()
        .context("open /dev/uinput (in the `input` group?)")?
        .name("poe2ddd-virtual-kbd")
        .with_keys(&keys)
        .context("declare virtual keyboard keys")?
        .build()
        .context("create uinput keyboard")?;
    // Let the compositor enumerate the new device before it's first used.
    thread::sleep(Duration::from_millis(250));
    // If another thread won the race, ours is dropped here — harmless.
    Ok(DEVICE.get_or_init(|| Mutex::new(device)))
}

fn emit(device: &mut VirtualDevice, key: Key, value: i32) -> Result<()> {
    device
        .emit(&[InputEvent::new(EventType::KEY, key.code(), value)])
        .context("emit key event")
}

/// Press then release a key.
fn tap(device: &mut VirtualDevice, key: Key) -> Result<()> {
    emit(device, key, 1)?;
    emit(device, key, 0)
}

/// Ctrl + key (press Ctrl, tap key, release Ctrl).
fn ctrl_tap(device: &mut VirtualDevice, key: Key) -> Result<()> {
    emit(device, Key::KEY_LEFTCTRL, 1)?;
    let tapped = emit(device, key, 1).and_then(|()| emit(device, key, 0));
    // Always release Ctrl, even if the inner tap failed, so we never leave the
    // virtual modifier logically stuck down in POE2.
    let released = emit(device, Key::KEY_LEFTCTRL, 0);
    tapped.and(released)
}
