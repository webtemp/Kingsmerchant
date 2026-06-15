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

/// Brief settle so the xclip selection helper is serving before the FIRST
/// keystroke. `write_clipboard_text` already blocks until xclip forks and owns
/// the selection, so this is just a safety margin — and it's BEFORE chat opens,
/// so it's invisible (unlike a mid-sequence sleep).
const CLIPBOARD_SETTLE: Duration = Duration::from_millis(25);

/// Open chat, paste `command`, and send it — fast, so the chat box only flashes
/// (the EE2 feel) instead of visibly lingering open. Blocks ~250ms only on the
/// very first call (device setup); afterwards ~80ms. Call off the UI thread.
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
        let mut device = device()?.lock().expect("inject device lock");
        // Settle the clipboard BEFORE the burst — invisible (no key sent yet), and
        // it guarantees the paste reads our text rather than stale content.
        thread::sleep(CLIPBOARD_SETTLE);
        // Fire the whole sequence with NO gaps between keys, so POE2 receives it
        // within a single input tick: it drains the queue in order (open chat →
        // [clear] → paste → send) and closes the input before ever drawing a frame
        // with the chat box open — the EE2 "chat never appears" effect. We do NOT
        // pace with thread::sleep: tiny sleeps overshoot (scheduler granularity)
        // and would push a key into the next frame, which is exactly the visible
        // flash we're removing. Ordering is preserved by the kernel (uinput emits
        // are delivered in order), so a same-tick burst still pastes into the
        // now-open chat, not the game.
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
    emit(device, key, 1)?;
    emit(device, key, 0)?;
    emit(device, Key::KEY_LEFTCTRL, 0)
}
