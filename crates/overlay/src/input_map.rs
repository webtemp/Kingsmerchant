//! Translation from Wayland input codes to egui events.

use smithay_client_toolkit::seat::keyboard::Keysym;

/// Map a keysym to an egui [`Key`] for editing/navigation keys.
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

/// Map a keysym to the key name used in a hotkey-binding string (e.g. `"F5"`);
/// `None` for modifier keys and keys the evdev parser can't anchor on.
pub(crate) fn keysym_to_binding_key(k: Keysym) -> Option<String> {
    let raw = k.raw();
    if (0x0041..=0x005a).contains(&raw) {
        return Some((raw as u8 as char).to_string());
    }
    if (0x0061..=0x007a).contains(&raw) {
        return Some(((raw as u8 - 0x20) as char).to_string());
    }
    if (0x0030..=0x0039).contains(&raw) {
        return Some((raw as u8 as char).to_string());
    }
    if (0xffbe..=0xffc9).contains(&raw) {
        return Some(format!("F{}", raw - 0xffbe + 1));
    }
    Some(match raw {
        0xff1b => "Escape".to_string(),
        0xff0d | 0xff8d => "Enter".to_string(),
        0x0020 => "Space".to_string(),
        0xff09 => "Tab".to_string(),
        _ => return None,
    })
}

/// Format a hotkey-binding string in canonical `Ctrl+Alt+Shift+Key` order.
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

pub(crate) fn map_button(code: u32) -> Option<egui::PointerButton> {
    match code {
        BTN_LEFT => Some(egui::PointerButton::Primary),
        0x111 => Some(egui::PointerButton::Secondary),
        0x112 => Some(egui::PointerButton::Middle),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_key_folds_letters_and_maps_named_keys() {
        assert_eq!(keysym_to_binding_key(Keysym::a).as_deref(), Some("A"));
        assert_eq!(keysym_to_binding_key(Keysym::A).as_deref(), Some("A"));
        assert_eq!(keysym_to_binding_key(Keysym::c).as_deref(), Some("C"));
        assert_eq!(keysym_to_binding_key(Keysym::_5).as_deref(), Some("5"));
        assert_eq!(keysym_to_binding_key(Keysym::F1).as_deref(), Some("F1"));
        assert_eq!(keysym_to_binding_key(Keysym::F5).as_deref(), Some("F5"));
        assert_eq!(keysym_to_binding_key(Keysym::F12).as_deref(), Some("F12"));
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
        assert_eq!(keysym_to_binding_key(Keysym::Control_L), None);
        assert_eq!(keysym_to_binding_key(Keysym::Shift_L), None);
    }

    #[test]
    fn format_binding_emits_canonical_ctrl_alt_shift_order() {
        assert_eq!(format_binding(false, false, false, "C"), "C");
        assert_eq!(format_binding(true, false, false, "C"), "Ctrl+C");
        assert_eq!(format_binding(true, true, true, "F5"), "Ctrl+Alt+Shift+F5");
        assert_eq!(format_binding(false, true, true, "X"), "Alt+Shift+X");
    }

    #[test]
    fn map_button_covers_the_three_mouse_buttons() {
        assert_eq!(map_button(BTN_LEFT), Some(egui::PointerButton::Primary));
        assert_eq!(map_button(0x111), Some(egui::PointerButton::Secondary));
        assert_eq!(map_button(0x112), Some(egui::PointerButton::Middle));
        assert_eq!(map_button(0x113), None);
    }
}
