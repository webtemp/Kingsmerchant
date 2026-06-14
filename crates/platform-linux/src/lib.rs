//! Linux platform layer for poe2ddd.
//!
//! Phase 0 spike scope: global hotkey detection via evdev and clipboard
//! reads via the wlr-data-control protocol. Later phases add layer-shell
//! overlay, xdotool window polling, etc. (see PRD §6).

pub mod clipboard;
pub mod input;

pub use clipboard::{open_url, read_clipboard_text, write_clipboard_text};
pub use input::{watch_hotkeys, HotkeyEvent};
