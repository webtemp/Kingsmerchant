//! Linux platform layer for poe2-pricer.
//!
//! Phase 0 spike scope: global hotkey detection via evdev and clipboard
//! reads via the wlr-data-control protocol. Later phases add layer-shell
//! overlay, xdotool window polling, etc. (see PRD §6).

pub mod clipboard;
pub mod input;

pub use clipboard::read_clipboard_text;
pub use input::{watch_hotkeys, HotkeyEvent};
