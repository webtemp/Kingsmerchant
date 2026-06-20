//! KDE/freedesktop system-tray icon via StatusNotifierItem (`ksni`), on its own
//! background thread. Menu clicks come out as [`TrayAction`]s; the UI pushes
//! [`TrayState`] in via [`TrayHandle::set_state`] to update the tooltip.

use std::sync::mpsc::{channel, Receiver, Sender};

use ksni::blocking::{Handle, TrayMethods};
use ksni::{menu::StandardItem, Category, Icon, MenuItem, ToolTip};

/// A menu action the user triggered from the tray.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    OpenSettings,
    Quit,
}

/// What the tooltip should report (Listening / Rate limited / API error).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayState {
    Listening,
    /// `secs` until the next allowed request.
    RateLimited(u64),
    /// Short reason for the tooltip.
    Error(String),
}

impl TrayState {
    fn description(&self) -> String {
        match self {
            TrayState::Listening => "Listening — Ctrl+C on an item in POE2".to_string(),
            TrayState::RateLimited(secs) => format!("Rate limited — retrying in {secs}s"),
            TrayState::Error(msg) => format!("API error: {msg}"),
        }
    }
}

struct PoeTray {
    state: TrayState,
    actions: Sender<TrayAction>,
}

impl ksni::Tray for PoeTray {
    fn id(&self) -> String {
        "kingsmerchant".into()
    }

    fn title(&self) -> String {
        "kingsmerchant".into()
    }

    fn category(&self) -> Category {
        Category::ApplicationStatus
    }

    fn icon_name(&self) -> String {
        "kingsmerchant".into()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        app_icon()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "kingsmerchant".into(),
            description: self.state.description(),
            icon_name: "kingsmerchant".into(),
            icon_pixmap: Vec::new(),
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            StandardItem {
                label: "Open Settings".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.actions.send(TrayAction::OpenSettings);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.actions.send(TrayAction::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }

    /// Left-click also opens settings.
    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.actions.send(TrayAction::OpenSettings);
    }
}

/// A handle for pushing state updates to the running tray (tooltip text).
pub struct TrayHandle {
    handle: Handle<PoeTray>,
    /// Last pushed state, so `set_state` is a no-op when unchanged.
    last: TrayState,
}

impl TrayHandle {
    /// Update the tooltip; skips the D-Bus round-trip when the state is unchanged.
    pub fn set_state(&mut self, state: TrayState) {
        if self.last == state {
            return;
        }
        self.last = state.clone();
        self.handle.update(move |t| t.state = state);
    }
}

/// Spawn the tray on its own background thread, returning a tooltip handle and a
/// receiver of menu actions. Errors if StatusNotifierItem can't be reached.
pub fn spawn_tray() -> anyhow::Result<(TrayHandle, Receiver<TrayAction>)> {
    let (tx, rx) = channel();
    let tray = PoeTray {
        state: TrayState::Listening,
        actions: tx,
    };
    let handle = tray
        .spawn()
        .map_err(|e| anyhow::anyhow!("tray (StatusNotifierItem) unavailable: {e}"))?;
    Ok((
        TrayHandle {
            handle,
            last: TrayState::Listening,
        },
        rx,
    ))
}

/// The app icon as ARGB32 pixmaps, pre-rasterised from `assets/kingsmerchant.svg`.
/// Regenerate with `assets/tray/regen.sh`.
fn app_icon() -> Vec<Icon> {
    macro_rules! pixmap {
        ($size:expr) => {
            Icon {
                width: $size,
                height: $size,
                data: include_bytes!(concat!(
                    "../../../assets/tray/icon",
                    stringify!($size),
                    ".argb"
                ))
                .to_vec(),
            }
        };
    }
    vec![
        pixmap!(16),
        pixmap!(22),
        pixmap!(24),
        pixmap!(32),
        pixmap!(48),
        pixmap!(64),
    ]
}
