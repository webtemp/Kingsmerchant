//! Keystroke injection into the focused window via a uinput virtual keyboard.
//!
//! PRD §9.1 says "cannot synthesize keys into POE2 on KDE Wayland" — but that's
//! only true for X11 XTEST (xdotool) and the Wayland virtual-keyboard protocol
//! (wtype), both of which KWin blocks for XWayland clients. A **uinput** device
//! is different: it's a kernel-level virtual *hardware* keyboard, so the
//! compositor treats its events as real input and delivers them to whatever
//! window has focus — including XWayland POE2. (Same mechanism ydotool uses.)
//!
//! Caveats this carries:
//!   * It types into the **focused** window — so POE2 must be focused (the
//!     overlay takes focus only while its popup is shown, so this works while
//!     playing).
//!   * Key *codes* are emitted; the compositor maps them through the user's
//!     layout. The char→key table below assumes a US/QWERTY layout (fine for
//!     `/hideout`, `/invite name`, …).
//!   * It steps slightly past the clipboard-only anti-cheat envelope (PRD
//!     Appendix B); it's an explicit opt-in, used only for chat commands.

use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use evdev::{
    uinput::VirtualDeviceBuilder, AttributeSet, EventType, InputEvent, Key,
};

/// Open chat, type `command`, and send it: Enter → `command` → Enter.
///
/// Blocks for roughly half a second (device setup + inter-key delays), so call
/// it off the UI thread.
pub fn send_chat_command(command: &str) -> Result<()> {
    let mut keys = AttributeSet::<Key>::new();
    keys.insert(Key::KEY_ENTER);
    keys.insert(Key::KEY_LEFTSHIFT);
    for c in command.chars() {
        if let Some((key, _)) = char_to_key(c) {
            keys.insert(key);
        }
    }

    let mut device = VirtualDeviceBuilder::new()
        .context("open /dev/uinput (in the `input` group?)")?
        .name("poe2ddd-virtual-kbd")
        .with_keys(&keys)
        .context("declare virtual keyboard keys")?
        .build()
        .context("create uinput keyboard")?;

    // Let the compositor enumerate the new device before we emit, or the first
    // events are dropped.
    thread::sleep(Duration::from_millis(250));

    // Open the chat box.
    tap(&mut device, Key::KEY_ENTER)?;
    thread::sleep(Duration::from_millis(90));

    for c in command.chars() {
        let Some((key, shift)) = char_to_key(c) else {
            continue; // skip anything not in the table
        };
        if shift {
            emit(&mut device, Key::KEY_LEFTSHIFT, 1)?;
        }
        tap(&mut device, key)?;
        if shift {
            emit(&mut device, Key::KEY_LEFTSHIFT, 0)?;
        }
        thread::sleep(Duration::from_millis(14));
    }

    thread::sleep(Duration::from_millis(90));
    // Send.
    tap(&mut device, Key::KEY_ENTER)?;
    // Hold the device briefly so the final events flush before it's destroyed.
    thread::sleep(Duration::from_millis(40));
    Ok(())
}

/// Emit a single key state change (value 1 = press, 0 = release).
fn emit(device: &mut evdev::uinput::VirtualDevice, key: Key, value: i32) -> Result<()> {
    device
        .emit(&[InputEvent::new(EventType::KEY, key.code(), value)])
        .context("emit key event")
}

/// Press then release a key.
fn tap(device: &mut evdev::uinput::VirtualDevice, key: Key) -> Result<()> {
    emit(device, key, 1)?;
    emit(device, key, 0)
}

/// Map a character to its (key, shift) on a US/QWERTY layout. `None` for
/// characters we don't type.
fn char_to_key(c: char) -> Option<(Key, bool)> {
    let shifted = c.is_ascii_uppercase()
        || matches!(c, '#' | '@' | '_' | '!' | '?' | ':' | '"' | '(' | ')');
    let key = match c.to_ascii_lowercase() {
        'a' => Key::KEY_A,
        'b' => Key::KEY_B,
        'c' => Key::KEY_C,
        'd' => Key::KEY_D,
        'e' => Key::KEY_E,
        'f' => Key::KEY_F,
        'g' => Key::KEY_G,
        'h' => Key::KEY_H,
        'i' => Key::KEY_I,
        'j' => Key::KEY_J,
        'k' => Key::KEY_K,
        'l' => Key::KEY_L,
        'm' => Key::KEY_M,
        'n' => Key::KEY_N,
        'o' => Key::KEY_O,
        'p' => Key::KEY_P,
        'q' => Key::KEY_Q,
        'r' => Key::KEY_R,
        's' => Key::KEY_S,
        't' => Key::KEY_T,
        'u' => Key::KEY_U,
        'v' => Key::KEY_V,
        'w' => Key::KEY_W,
        'x' => Key::KEY_X,
        'y' => Key::KEY_Y,
        'z' => Key::KEY_Z,
        ' ' => Key::KEY_SPACE,
        '/' => Key::KEY_SLASH,
        '.' => Key::KEY_DOT,
        ',' => Key::KEY_COMMA,
        '-' | '_' => Key::KEY_MINUS,
        '\'' => Key::KEY_APOSTROPHE,
        '1' | '!' => Key::KEY_1,
        '2' | '@' => Key::KEY_2,
        '3' | '#' => Key::KEY_3,
        '4' => Key::KEY_4,
        '5' => Key::KEY_5,
        '6' => Key::KEY_6,
        '7' => Key::KEY_7,
        '8' => Key::KEY_8,
        '9' | '(' => Key::KEY_9,
        '0' | ')' => Key::KEY_0,
        ':' => Key::KEY_SEMICOLON,
        '"' => Key::KEY_APOSTROPHE,
        '?' => Key::KEY_SLASH,
        _ => return None,
    };
    Some((key, shifted))
}
