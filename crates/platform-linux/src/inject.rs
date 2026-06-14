//! Chat-command injection into the focused window (POE2), the fast way.
//!
//! We do NOT type the command out character-by-character — that's slow and you
//! watch every letter appear. Instead, like Exiled-Exchange-2, we put the
//! command on the clipboard and **paste** it: tap Enter (open chat), Ctrl+V
//! (whole command appears at once), Enter (send). The user's clipboard is saved
//! and restored around it.
//!
//! Two things make this instant vs the old approach:
//!   * **paste, not type** — one Ctrl+V regardless of command length;
//!   * a **persistent** uinput virtual keyboard, created once and reused, so we
//!     don't pay the ~250ms device-enumeration wait on every press.
//!
//! Same uinput mechanism as ydotool: a kernel virtual *hardware* keyboard, so
//! the compositor delivers the keys to the focused window — including XWayland
//! POE2 (PRD §9.1's "can't inject" is only true for X11 XTEST / wtype). It types
//! into whatever's focused, so POE2 must be focused (the hotkey is gated on
//! that). Opt-in; steps past the clipboard-only anti-cheat envelope (App. B).

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

/// Open chat, paste `command`, and send it. Blocks ~250ms only on the very first
/// call (device setup); afterwards a few tens of ms. Call off the UI thread.
pub fn send_chat_command(command: &str) -> Result<()> {
    // Save the user's clipboard, set ours, paste, restore (so we don't clobber
    // whatever they had copied — e.g. the item they just priced).
    let saved = read_clipboard_text().ok().flatten();
    write_clipboard_text(command).context("set clipboard for chat paste")?;

    {
        let mut device = device()?.lock().expect("inject device lock");
        // Let the xclip selection helper start serving before we paste.
        thread::sleep(Duration::from_millis(30));
        tap(&mut device, Key::KEY_ENTER)?; // open chat
        thread::sleep(Duration::from_millis(40));
        let auto_clear = command
            .chars()
            .next()
            .is_some_and(|c| AUTO_CLEAR_PREFIXES.contains(&c));
        if !auto_clear {
            ctrl_tap(&mut device, Key::KEY_A)?; // clear any leftover text
        }
        ctrl_tap(&mut device, Key::KEY_V)?; // paste the whole command at once
        thread::sleep(Duration::from_millis(40));
        tap(&mut device, Key::KEY_ENTER)?; // send
    }

    // Let POE2 read the clipboard (the paste) before we put the old value back.
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
    emit(device, key, 1)?;
    emit(device, key, 0)?;
    emit(device, Key::KEY_LEFTCTRL, 0)
}
