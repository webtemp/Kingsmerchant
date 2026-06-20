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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_key_folds_letters_and_maps_named_keys() {
        // Letters fold to uppercase regardless of shift state.
        assert_eq!(keysym_to_binding_key(Keysym::a).as_deref(), Some("A"));
        assert_eq!(keysym_to_binding_key(Keysym::A).as_deref(), Some("A"));
        assert_eq!(keysym_to_binding_key(Keysym::c).as_deref(), Some("C"));
        // Digits and F-keys map directly (F-key offset is easy to get wrong).
        assert_eq!(keysym_to_binding_key(Keysym::_5).as_deref(), Some("5"));
        assert_eq!(keysym_to_binding_key(Keysym::F1).as_deref(), Some("F1"));
        assert_eq!(keysym_to_binding_key(Keysym::F5).as_deref(), Some("F5"));
        assert_eq!(keysym_to_binding_key(Keysym::F12).as_deref(), Some("F12"));
        // Named anchors.
        assert_eq!(
            keysym_to_binding_key(Keysym::Escape).as_deref(),
            Some("Escape")
        );
        assert_eq!(
            keysym_to_binding_key(Keysym::Return).as_deref(),
            Some("Enter")
        );
        assert_eq!(
            keysym_to_binding_key(Keysym::space).as_deref(),
            Some("Space")
        );
        assert_eq!(keysym_to_binding_key(Keysym::Tab).as_deref(), Some("Tab"));
        // Modifiers can't anchor a binding → None (caller keeps listening).
        assert_eq!(keysym_to_binding_key(Keysym::Control_L), None);
        assert_eq!(keysym_to_binding_key(Keysym::Shift_L), None);
    }

    #[test]
    fn format_binding_emits_canonical_ctrl_alt_shift_order() {
        assert_eq!(format_binding(false, false, false, "C"), "C");
        assert_eq!(format_binding(true, false, false, "C"), "Ctrl+C");
        assert_eq!(format_binding(true, true, true, "F5"), "Ctrl+Alt+Shift+F5");
        // Order is fixed regardless of which flags are set.
        assert_eq!(format_binding(false, true, true, "X"), "Alt+Shift+X");
    }

    #[test]
    fn map_button_covers_the_three_mouse_buttons() {
        assert_eq!(map_button(BTN_LEFT), Some(egui::PointerButton::Primary));
        assert_eq!(map_button(0x111), Some(egui::PointerButton::Secondary));
        assert_eq!(map_button(0x112), Some(egui::PointerButton::Middle));
        assert_eq!(map_button(0x113), None); // BTN_SIDE and beyond are unmapped
    }
}
