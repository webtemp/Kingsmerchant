//! The price-check popup and the settings panel, each rendered onto its own
//! `wlr-layer-shell` *overlay* surface.
//!
//! Both surfaces share one Wayland event loop and one EGL display, but each has
//! its own GL context + `egui::Context` so they lay out and paint
//! independently. `WinSurface` is the per-window bundle; `App` owns the two
//! (popup + settings) plus the shared [`ui::QuickModeApp`]. Pointer / keyboard /
//! configure / frame events route to whichever surface they belong to.
//!
//! The price popup pops on a valid Ctrl+C and is pinned until Esc / the X
//! button; drag it with Alt held. The settings surface opens from the gear
//! button or the tray and closes by its own X / the tray Quit. Neither surface
//! takes keyboard focus while hidden, so POE2 keeps it.
//!
//! Entry point is [`run`], shared by the `kingsmerchant` binary (`cargo run`) and the
//! `kingsmerchant-overlay` binary (`cargo run -p overlay`). The league/config come from
//! `~/.config/kingsmerchant/config.json`; set `POE_LEAGUE` only to override one run.

// The workspace denies `unsafe_code`; this crate is the sole exception, for the
// glutin EGL bindings in `surface.rs` (inherently `unsafe` FFI).
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

/// Which of the two surfaces an event belongs to.
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

/// Launch the overlay: build the egui app + tray, bind the two layer surfaces,
/// and run the Wayland event loop until the app is closed.
pub fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // One egui context per surface (independent layout + GL). The hotkey watcher
    // and price-check tasks repaint the popup context.
    let popup_ctx = egui::Context::default();
    ui::install_loaders(&popup_ctx);
    ui::configure_style(&popup_ctx);
    let settings_ctx = egui::Context::default();
    ui::install_loaders(&settings_ctx);
    ui::configure_style(&settings_ctx);

    let (hk_tx, hk_rx) = channel::<Hotkey>();
    let hotkeys = ui::spawn_hotkey_watcher(popup_ctx.clone(), hk_tx.clone());

    // Tray: runs on its own thread; menu clicks (Open Settings / Quit) forward
    // into the same hotkey channel `pump()` drains every frame, so no extra
    // wake-up of the Wayland loop is needed. The handle pushes tooltip state.
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

    // Config hot-reload: watch config.json and push reloaded configs down the
    // same channel. Started after `build_app` so the startup backfill write
    // doesn't trigger a spurious reload. Best-effort.
    ui::spawn_config_watcher(popup_ctx.clone(), hk_tx);

    // Wayland side.
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
        POPUP_INIT_WIDTH,
    );
    let settings = WinSurface::new(
        &compositor,
        &layer_shell,
        &qh,
        settings_ctx,
        "kingsmerchant-settings",
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
    /// Held only to keep the relative-pointer object alive: dropping the proxy
    /// stops the compositor sending relative-motion deltas (used for dragging).
    relative_pointer: Option<zwp_relative_pointer_v1::ZwpRelativePointerV1>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    /// Current modifier state from Wayland (for egui key events).
    kbd_modifiers: egui::Modifiers,
    /// Which surface currently holds keyboard focus (for routing key events).
    focused: Option<Which>,
    popup: WinSurface,
    settings: WinSurface,
    quick: QuickModeApp,
    /// A surface that needs to be kicked back into its redraw loop after the
    /// current draw (it should spin but went quiet on a state transition).
    pending_bootstrap: Option<Which>,
    exit: bool,
}

impl App {
    /// Which surface a `wl_surface` belongs to (if any).
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

    /// Whether a surface should keep requesting frame callbacks (redrawing). The
    /// popup spins whenever it's shown OR settings is hidden (so it keeps `pump`
    /// alive for the idle/pricing cases); settings spins only while shown. So
    /// exactly one spins at a time and the two never compete for the vsync swap.
    fn should_spin(&self, which: Which) -> bool {
        match which {
            Which::Popup => self.popup.shown || !self.settings.shown,
            Which::Settings => self.settings.shown,
        }
    }

    /// Drain the channels (`pump`) and apply the resulting show/quit requests —
    /// the single per-frame tick, run by whichever surface is currently drawing.
    /// If a transition leaves a should-spin surface idle, schedule it for a kick.
    /// Re-activate the POE2 window after hiding a surface — the compositor won't
    /// hand keyboard focus back on its own, so it would otherwise stay on the
    /// now-invisible overlay (forcing an Alt-Tab). Best-effort, off the UI thread
    /// so the `xdotool` spawn never stalls the frame.
    fn refocus_game() {
        std::thread::spawn(|| {
            platform_linux::focus_poe2();
        });
    }

    fn tick(&mut self, current: Which) {
        // Always pump on the popup's egui context: that's where price results
        // render and where the watcher / background tasks request repaints.
        self.quick.pump(&self.popup.egui_ctx);
        // Keep the overlay perf log in sync with the Settings toggle (off by
        // default); cheap enough to set every tick.
        surface::set_perf_metrics_enabled(self.quick.perf_metrics_enabled());
        if self.quick.take_pop_request() {
            // A Ctrl+C takes over: show the popup, leave settings.
            self.popup.shown = true;
            self.settings.shown = false;
        }
        if self.quick.take_close_request() {
            // Escape / click-outside / Alt+Tab — dismiss whichever surface is up.
            let was_shown = self.popup.shown || self.settings.shown;
            self.popup.shown = false;
            self.settings.shown = false;
            // Hiding the layer surface drops its keyboard grab, but the
            // compositor won't hand focus back to POE2 on its own — it would
            // leave focus on the now-invisible overlay, so the game stays
            // unselected (X / Escape / "Open on trade site" all hit this).
            // Re-activate the game (best-effort, via xdotool). Off-thread so the
            // process spawn never stalls the frame. For the trade-site case the
            // browser still raises itself over the game once it finishes opening.
            // Only when something was actually open, so a stray Alt+Tab with no
            // overlay up doesn't yank focus back to POE2.
            if was_shown {
                Self::refocus_game();
            }
        }
        if self.quick.take_settings_request() {
            // Open settings and hide the popup so the two don't overlap.
            self.settings.shown = true;
            self.popup.shown = false;
        }
        if self.quick.take_settings_close_request() {
            self.settings.shown = false;
            // Same as the popup close: the compositor leaves keyboard focus on
            // the now-hidden settings surface, so hand it back to the game.
            Self::refocus_game();
        }
        if self.quick.take_quit_request() {
            self.exit = true;
        }
        // If the OTHER surface now needs to spin but has gone quiet (a transition
        // just changed which one is active), kick it after the current draw.
        let other = current.other();
        if self.should_spin(other) && !self.surf(other).spinning {
            self.pending_bootstrap = Some(other);
        }
    }

    /// Draw `which`, then kick any surface that a transition left needing a
    /// redraw (so the active surface always keeps spinning). Entry point for
    /// both frame callbacks and configures.
    fn render(&mut self, which: Which, qh: &QueueHandle<Self>) {
        self.draw_surface(which, qh);
        if let Some(boot) = self.pending_bootstrap.take() {
            if boot != which {
                self.draw_surface(boot, qh);
            }
        }
    }

    /// Draw one surface: the price-check popup (`QuickModeApp::content`) or the
    /// settings panel (`QuickModeApp::settings_content`). The two differ only in
    /// their width, placement, and content closure — everything else (pumping,
    /// the shared Wayland/GL state, error handling) is identical.
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
                    // center + at-cursor (Phase 7 stub) both center for now.
                    _ => Placement::Center,
                };
                (self.quick.surface_width(), place)
            }
            Which::Settings => (SETTINGS_WIDTH, Placement::Center),
        };
        // Themeable popup colours (opacity baked into the alpha), read fresh each
        // frame so a settings/hot-reload change shows up immediately.
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
