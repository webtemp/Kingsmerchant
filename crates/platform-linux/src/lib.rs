//! Linux platform layer: evdev hotkeys, X11 clipboard, uinput injection,
//! xdotool window polling, and the system tray.

pub mod clipboard;
pub mod inject;
pub mod input;
pub mod tray;
pub mod window;

pub use clipboard::{open_url, read_clipboard_text, read_paste_text, write_clipboard_text};
pub use inject::{copy_item_under_cursor, send_chat_command, warm_up as warm_up_injection};
pub use input::{watch_hotkeys, Binding, HotkeyBindings, HotkeyControl, HotkeyEvent};
pub use tray::{spawn_tray, TrayAction, TrayHandle, TrayState};
pub use window::{focus_poe2, is_poe2_active, poe2_window_geometry};
