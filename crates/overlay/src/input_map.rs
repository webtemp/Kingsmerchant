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
