//! Linux platform layer for poe2ddd.
//!
//! Phase 0 spike scope: global hotkey detection via evdev and clipboard
//! reads via the wlr-data-control protocol. Later phases add layer-shell
//! overlay, xdotool window polling, etc. (see PRD §6).

pub mod clipboard;
pub mod inject;
pub mod input;
pub mod tray;
pub mod window;

pub use clipboard::{open_url, read_clipboard_text, write_clipboard_text};
pub use inject::{send_chat_command, warm_up as warm_up_injection};
pub use input::{watch_hotkeys, Binding, HotkeyBindings, HotkeyEvent};
pub use tray::{spawn_tray, TrayAction, TrayHandle, TrayState};
pub use window::{focus_poe2, is_poe2_active};
