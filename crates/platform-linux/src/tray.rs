//! KDE/freedesktop system-tray icon via the StatusNotifierItem protocol
//! (PRD §4.9), using `ksni` (NOT the legacy XEmbed tray).
//!
//! The tray runs on its own background thread (ksni's blocking service). It
//! talks to the app two ways:
//!  - **out**: menu clicks (Open Settings / Quit) are pushed as [`TrayAction`]s
//!    down an mpsc channel the UI drains every frame.
//!  - **in**: the UI pushes the current [`TrayState`] (Listening / Rate-limited
//!    / API error) back via [`TrayHandle::set_state`], which updates the
//!    tooltip shown on hover.
//!
//! Keeping the tray here (alongside clipboard/window/input) keeps the D-Bus
//! dependency out of the `ui` crate, matching the platform-integration split.

use std::sync::mpsc::{channel, Receiver, Sender};

use ksni::blocking::{Handle, TrayMethods};
use ksni::{menu::StandardItem, Category, Icon, MenuItem, ToolTip};

/// A menu action the user triggered from the tray (PRD §4.9 menu).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    /// "Open Settings" — show the settings surface.
    OpenSettings,
    /// "Quit" — exit the app.
    Quit,
}

/// What the tooltip should report (PRD §4.9: "Listening" / "Rate limited Ns" /
/// "API error").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayState {
    /// Idle, watching for the price-check hotkey.
    Listening,
    /// Throttled by the trade API's rate limiter; `secs` until the next request.
    RateLimited(u64),
    /// The last price check failed; carries a short reason for the tooltip.
    Error(String),
}

impl TrayState {
    /// One-line description for the tooltip body.
    fn description(&self) -> String {
        match self {
            TrayState::Listening => "Listening — Ctrl+C on an item in POE2".to_string(),
            TrayState::RateLimited(secs) => format!("Rate limited — retrying in {secs}s"),
            TrayState::Error(msg) => format!("API error: {msg}"),
        }
    }
}

/// The ksni tray object. Holds the current state (for the tooltip) and the
/// sender menu clicks are pushed down.
struct PoeTray {
    state: TrayState,
    actions: Sender<TrayAction>,
}

impl ksni::Tray for PoeTray {
    fn id(&self) -> String {
        "poe2ddd".into()
    }

    fn title(&self) -> String {
        "poe2ddd".into()
    }

    fn category(&self) -> Category {
        Category::ApplicationStatus
    }

    /// Freedesktop icon name first (resolves once the .desktop icon is
    /// installed); the embedded pixmap is the fallback so the tray is visible
    /// even before install.
    fn icon_name(&self) -> String {
        "poe2ddd".into()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        app_icon()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "poe2ddd".into(),
            description: self.state.description(),
            icon_name: "poe2ddd".into(),
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

    /// Left-click also opens settings (the most useful default action).
    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.actions.send(TrayAction::OpenSettings);
    }
}

/// A handle for pushing state updates to the running tray (tooltip text).
pub struct TrayHandle {
    handle: Handle<PoeTray>,
    /// Last state we pushed, so `set_state` is a no-op (no D-Bus traffic) when
    /// the state hasn't actually changed.
    last: TrayState,
}

impl TrayHandle {
    /// Update the tooltip to reflect the current app state. Cheap and
    /// idempotent — skips the D-Bus round-trip when the state is unchanged.
    pub fn set_state(&mut self, state: TrayState) {
        if self.last == state {
            return;
        }
        self.last = state.clone();
        self.handle.update(move |t| t.state = state);
    }
}

/// Spawn the tray on its own background thread. Returns a handle for tooltip
/// updates and a receiver of menu actions (Open Settings / Quit).
///
/// Errors if the StatusNotifierItem service can't be reached (no SNI host /
/// D-Bus) — the caller logs and carries on without a tray.
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

/// A small gold diamond, generated in code as an ARGB32 pixmap so the tray icon
/// is visible without relying on an installed theme icon. 22px is the usual KDE
/// tray icon size.
fn app_icon() -> Vec<Icon> {
    const S: i32 = 22;
    let center = (S - 1) as f32 / 2.0;
    let radius = center;
    let mut data = Vec::with_capacity((S * S * 4) as usize);
    for y in 0..S {
        for x in 0..S {
            let dist = (x as f32 - center).abs() + (y as f32 - center).abs();
            if dist <= radius - 1.0 {
                // Opaque gold #c8a13a, bytes in A,R,G,B order (ARGB32).
                data.extend_from_slice(&[0xff, 0xc8, 0xa1, 0x3a]);
            } else {
                data.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
            }
        }
    }
    vec![Icon {
        width: S,
        height: S,
        data,
    }]
}
