//! Pure translation from Wayland input codes to egui events: keysyms to
//! [`egui::Key`] and evdev button codes to [`egui::PointerButton`].

use smithay_client_toolkit::seat::keyboard::Keysym;

/// Map a keysym to an egui [`Key`] for the editing/navigation keys a text field
/// needs (printable characters go through `Event::Text` instead).
pub(crate) fn map_keysym(k: Keysym) -> Option<egui::Key> {
    use egui::Key;
    let key = if k == Keysym::BackSpace {
        Key::Backspace
    } else if k == Keysym::Return || k == Keysym::KP_Enter {
        Key::Enter
    } else if k == Keysym::Tab {
        Key::Tab
    } else if k == Keysym::Escape {
        Key::Escape
    } else if k == Keysym::Delete {
        Key::Delete
    } else if k == Keysym::Left {
        Key::ArrowLeft
    } else if k == Keysym::Right {
        Key::ArrowRight
    } else if k == Keysym::Up {
        Key::ArrowUp
    } else if k == Keysym::Down {
        Key::ArrowDown
    } else if k == Keysym::Home {
        Key::Home
    } else if k == Keysym::End {
        Key::End
    } else if k == Keysym::a || k == Keysym::A {
        Key::A
    } else if k == Keysym::c || k == Keysym::C {
        Key::C
    } else if k == Keysym::v || k == Keysym::V {
        Key::V
    } else if k == Keysym::x || k == Keysym::X {
        Key::X
    } else if k == Keysym::z || k == Keysym::Z {
        Key::Z
    } else {
        return None;
    };
    Some(key)
}

/// Map a keysym to the key name used in a hotkey-binding string (e.g. `"C"`,
/// `"F5"`, `"Escape"`) for the settings click-to-record picker. Returns `None`
/// for keysyms that can't anchor a binding — modifier keys (Ctrl/Alt/Shift) and
/// anything outside the set the evdev parser understands — so the caller keeps
/// listening until a real key is pressed.
///
/// Works on the raw keysym value (layout-independent for the X11 keysym block):
/// letters fold to uppercase (the parser is case-insensitive, and uppercase
/// reads cleanly), digits and F-keys map directly.
pub(crate) fn keysym_to_binding_key(k: Keysym) -> Option<String> {
    let raw = k.raw();
    if (0x0041..=0x005a).contains(&raw) {
        // 'A'..='Z'
        return Some((raw as u8 as char).to_string());
    }
    if (0x0061..=0x007a).contains(&raw) {
        // 'a'..='z' → uppercase
        return Some(((raw as u8 - 0x20) as char).to_string());
    }
    if (0x0030..=0x0039).contains(&raw) {
        // '0'..='9'
        return Some((raw as u8 as char).to_string());
    }
    if (0xffbe..=0xffc9).contains(&raw) {
        // F1..=F12
        return Some(format!("F{}", raw - 0xffbe + 1));
    }
    Some(match raw {
        0xff1b => "Escape".to_string(),
        0xff0d | 0xff8d => "Enter".to_string(), // Return / KP_Enter
        0x0020 => "Space".to_string(),
        0xff09 => "Tab".to_string(),
        _ => return None,
    })
}

/// Format a hotkey-binding string from modifier flags + a key name, in the
/// canonical `Ctrl+Alt+Shift+Key` order the evdev parser expects.
pub(crate) fn format_binding(ctrl: bool, alt: bool, shift: bool, key: &str) -> String {
    let mut s = String::new();
    if ctrl {
        s.push_str("Ctrl+");
    }
    if alt {
        s.push_str("Alt+");
    }
    if shift {
        s.push_str("Shift+");
    }
    s.push_str(key);
    s
}

/// Linux evdev left-button code (the drag button).
pub(crate) const BTN_LEFT: u32 = 0x110;

/// Linux evdev button codes → egui buttons.
pub(crate) fn map_button(code: u32) -> Option<egui::PointerButton> {
    match code {
        BTN_LEFT => Some(egui::PointerButton::Primary),
        0x111 => Some(egui::PointerButton::Secondary), // BTN_RIGHT
        0x112 => Some(egui::PointerButton::Middle),    // BTN_MIDDLE
        _ => None,
    }
}
