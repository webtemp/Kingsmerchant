//! The price-check popup and settings panel, each on its own `wlr-layer-shell`
//! overlay surface. [`run`] builds both, shares one Wayland loop and EGL
//! display, and routes events to whichever surface owns them.

#![allow(unsafe_code)]

use std::sync::mpsc::channel;

use anyhow::{anyhow, Context as _, Result};
use glutin::display::Display;
use smithay_client_toolkit::{
    compositor::CompositorState,
    output::OutputState,
    registry::RegistryState,
    seat::{relative_pointer::RelativePointerState, SeatState},
    shell::{wlr_layer::LayerShell, WaylandSurface},
};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_pointer, wl_surface},
    Connection, QueueHandle,
};
use wayland_protocols::wp::relative_pointer::zv1::client::zwp_relative_pointer_v1;

use ui::{Hotkey, QuickModeApp};

use crate::surface::{Placement, Shared, WinSurface, POPUP_INIT_WIDTH, SETTINGS_WIDTH};

mod handlers;
mod input_map;
mod surface;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Which {
    Popup,
    Settings,
}

impl Which {
    fn other(self) -> Which {
        match self {
            Which::Popup => Which::Settings,
            Which::Settings => Which::Popup,
        }
    }
}

/// Launch the overlay: build the egui app + tray, bind the layer surfaces, run
/// the Wayland event loop until closed.
pub fn run() -> Result<()> {
    // Default verbosity from config; `RUST_LOG` still wins. Write-free read.
    let log_filter = ui::resolve_log_filter(&ui::config::Config::load_no_write().log_level);
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| log_filter.into()),
        )
        .init();

    // One egui context per surface (independent layout + GL).
    let popup_ctx = egui::Context::default();
    ui::install_loaders(&popup_ctx);
    ui::configure_style(&popup_ctx);
    let settings_ctx = egui::Context::default();
    ui::install_loaders(&settings_ctx);
    ui::configure_style(&settings_ctx);

    let (hk_tx, hk_rx) = channel::<Hotkey>();
    let hotkeys = ui::spawn_hotkey_watcher(popup_ctx.clone(), hk_tx.clone());

    // Tray on its own thread; menu clicks forward into the hotkey channel.
    let tray = match platform_linux::spawn_tray() {
        Ok((handle, actions)) => {
            let tx = hk_tx.clone();
            let ctx = popup_ctx.clone();
            std::thread::spawn(move || {
                for action in actions {
                    let hk = match action {
                        platform_linux::TrayAction::OpenSettings => Hotkey::OpenSettings,
                        platform_linux::TrayAction::Quit => Hotkey::Quit,
                    };
                    if tx.send(hk).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            });
            Some(handle)
        }
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "tray disabled");
            None
        }
    };
    let quick = ui::build_app(hk_rx, tray, hotkeys).context("building price-check app")?;

    // Config hot-reload, started after build_app to avoid a spurious reload.
    ui::spawn_config_watcher(popup_ctx.clone(), hk_tx);

    let conn = Connection::connect_to_env().context("connect to Wayland")?;
    let (globals, mut event_queue) = registry_queue_init(&conn).context("registry init")?;
    let qh = event_queue.handle();

    let compositor =
        CompositorState::bind(&globals, &qh).map_err(|e| anyhow!("wl_compositor: {e}"))?;
    let layer_shell =
        LayerShell::bind(&globals, &qh).map_err(|e| anyhow!("wlr layer shell unavailable: {e}"))?;

    let popup = WinSurface::new(
        &compositor,
        &layer_shell,
        &qh,
        popup_ctx,
        "kingsmerchant",
        "popup",
        POPUP_INIT_WIDTH,
    );
    let settings = WinSurface::new(
        &compositor,
        &layer_shell,
        &qh,
        settings_ctx,
        "kingsmerchant-settings",
        "settings",
        SETTINGS_WIDTH,
    );

    let mut app = App {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        relative_pointer_state: RelativePointerState::bind(&globals, &qh),
        conn: conn.clone(),
        compositor,
        display: None,
        pointer: None,
        relative_pointer: None,
        keyboard: None,
        kbd_modifiers: egui::Modifiers::default(),
        focused: None,
        popup,
        settings,
        quick,
        pending_bootstrap: None,
        exit: false,
    };

    tracing::info!("overlay running (hidden) — Ctrl+C on an item in POE2 to pop it");
    while !app.exit {
        event_queue
            .blocking_dispatch(&mut app)
            .context("dispatch")?;
    }
    Ok(())
}

struct App {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    relative_pointer_state: RelativePointerState,
    conn: Connection,
    compositor: CompositorState,
    /// Shared EGL display, created lazily by the first surface that inits GL.
    display: Option<Display>,
    pointer: Option<wl_pointer::WlPointer>,
    /// Held only to keep the relative-pointer object (drag deltas) alive.
    relative_pointer: Option<zwp_relative_pointer_v1::ZwpRelativePointerV1>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    kbd_modifiers: egui::Modifiers,
    /// Which surface currently holds keyboard focus.
    focused: Option<Which>,
    popup: WinSurface,
    settings: WinSurface,
    quick: QuickModeApp,
    /// A surface to kick back into its redraw loop after the current draw.
    pending_bootstrap: Option<Which>,
    exit: bool,
}

impl App {
    fn which(&self, surface: &wl_surface::WlSurface) -> Option<Which> {
        if surface == self.popup.layer.wl_surface() {
            Some(Which::Popup)
        } else if surface == self.settings.layer.wl_surface() {
            Some(Which::Settings)
        } else {
            None
        }
    }

    fn surf_mut(&mut self, which: Which) -> &mut WinSurface {
        match which {
            Which::Popup => &mut self.popup,
            Which::Settings => &mut self.settings,
        }
    }

    fn surf(&self, which: Which) -> &WinSurface {
        match which {
            Which::Popup => &self.popup,
            Which::Settings => &self.settings,
        }
    }

    /// Whether a surface should keep requesting frame callbacks. Exactly one
    /// spins at a time so the two never compete for the vsync swap.
    fn should_spin(&self, which: Which) -> bool {
        match which {
            Which::Popup => self.popup.shown || !self.settings.shown,
            Which::Settings => self.settings.shown,
        }
    }

    /// Re-activate the POE2 window after hiding a surface (the compositor won't
    /// hand keyboard focus back on its own). Best-effort, off the UI thread.
    fn refocus_game() {
        std::thread::spawn(|| {
            platform_linux::focus_poe2();
        });
    }

    fn tick(&mut self, current: Which) {
        // Always pump on the popup's egui context (where price results render).
        self.quick.pump(&self.popup.egui_ctx);
        surface::set_perf_metrics_enabled(self.quick.perf_metrics_enabled());
        if self.quick.take_pop_request() {
            self.popup.shown = true;
            self.settings.shown = false;
        }
        if self.quick.take_close_request() {
            let was_shown = self.popup.shown || self.settings.shown;
            self.popup.shown = false;
            self.settings.shown = false;
            // Hand focus back to the game only if something was actually open.
            if was_shown {
                Self::refocus_game();
            }
        }
        if self.quick.take_settings_request() {
            self.settings.shown = true;
            self.popup.shown = false;
        }
        if self.quick.take_settings_close_request() {
            self.settings.shown = false;
            Self::refocus_game();
        }
        if self.quick.take_quit_request() {
            self.exit = true;
        }
        // Kick the other surface if a transition left it needing to spin.
        let other = current.other();
        if self.should_spin(other) && !self.surf(other).spinning {
            self.pending_bootstrap = Some(other);
        }
    }

    /// Draw `which`, then kick any surface a transition left needing a redraw.
    fn render(&mut self, which: Which, qh: &QueueHandle<Self>) {
        self.draw_surface(which, qh);
        if let Some(boot) = self.pending_bootstrap.take() {
            if boot != which {
                self.draw_surface(boot, qh);
            }
        }
    }

    /// Draw one surface (popup or settings); they differ only in width,
    /// placement, and content closure.
    fn draw_surface(&mut self, which: Which, qh: &QueueHandle<Self>) {
        self.tick(which);
        let request_next = self.should_spin(which);
        let (want_w, place) = match which {
            Which::Popup => {
                let place = match self.quick.position_mode() {
                    "fixed" => {
                        let (x, y) = self.quick.fixed_pos();
                        Placement::Fixed { x, y }
                    }
                    _ => Placement::Center,
                };
                (self.quick.surface_width(), place)
            }
            Which::Settings => (SETTINGS_WIDTH, Placement::Center),
        };
        // Themeable popup colours, read fresh each frame.
        let fill = self.quick.overlay_fill();
        let stroke = self.quick.overlay_stroke();

        let App {
            popup,
            settings,
            quick,
            conn,
            compositor,
            output_state,
            display,
            kbd_modifiers,
            exit,
            ..
        } = self;
        let surf = match which {
            Which::Popup => popup,
            Which::Settings => settings,
        };
        let mut shared = Shared {
            conn,
            compositor,
            output_state,
            display,
            kbd_modifiers: *kbd_modifiers,
        };
        let result = surf.draw(
            &mut shared,
            qh,
            want_w,
            place,
            request_next,
            fill,
            stroke,
            |ui| match which {
                Which::Popup => quick.content(ui),
                Which::Settings => quick.settings_content(ui),
            },
        );
        if let Err(e) = result {
            tracing::error!(error = %format!("{e:#}"), which = ?which, "surface draw failed");
            *exit = true;
        }
    }
}
