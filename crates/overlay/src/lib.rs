//! The price-check popup and settings panel, each on its own `wlr-layer-shell`
//! overlay surface. [`run`] builds both, shares one Wayland loop and EGL
//! display, and routes events to whichever surface owns them.

#![allow(unsafe_code)]

use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context as _, Result};
use glutin::display::Display;
use smithay_client_toolkit::{
    compositor::CompositorState,
    output::OutputState,
    reexports::calloop::{
        ping::make_ping,
        timer::{TimeoutAction, Timer},
        EventLoop, LoopHandle, RegistrationToken,
    },
    reexports::calloop_wayland_source::WaylandSource,
    registry::RegistryState,
    seat::{relative_pointer::RelativePointerState, SeatState},
    shell::{wlr_layer::LayerShell, WaylandSurface},
};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_surface},
    Connection,
};
use wayland_protocols::wp::relative_pointer::zv1::client::zwp_relative_pointer_v1;

use ui::{Hotkey, QuickModeApp};

use crate::surface::{Placement, Shared, WinSurface, POPUP_INIT_WIDTH, SETTINGS_WIDTH};

mod handlers;
mod input_map;
mod surface;

/// Minimum spacing between egui-driven repaints (~60fps). egui animations like
/// spinners and smooth scrolling ask to repaint "immediately" every frame;
/// without this clamp that runs at the monitor refresh rate (100–150fps here),
/// pegging the GPU during a long-lived spinner. Frames are ~0.4–2ms now, so
/// ~90fps costs only a few percent of GPU while keeping scroll/animation smooth
/// (egui eases mouse-wheel scroll over ~0.1s, so the easing wants a decent
/// frame rate to look fluid; 30 felt choppy when scrolling Settings).
const ANIMATION_FRAME: Duration = Duration::from_millis(11);

/// Minimum spacing between input-driven repaints (~120fps). Pointer/keyboard
/// input renders at this snappier rate so interaction (hover, scroll, drag)
/// isn't bound to the animation cap; it only exists to coalesce high-polling-
/// rate mice. Frames are ~1ms, so this is essentially free.
const INPUT_FRAME: Duration = Duration::from_millis(8);

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
    let tray = match platform::spawn_tray() {
        Ok((handle, actions)) => {
            let tx = hk_tx.clone();
            let ctx = popup_ctx.clone();
            std::thread::spawn(move || {
                for action in actions {
                    let hk = match action {
                        platform::TrayAction::OpenSettings => Hotkey::OpenSettings,
                        platform::TrayAction::Quit => Hotkey::Quit,
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

    // calloop drives the loop so it can sleep when idle and be woken by a repaint
    // ping (egui asked to repaint) or a timer (a delayed egui animation), instead
    // of busy-rendering via perpetual frame callbacks.
    let mut event_loop: EventLoop<'static, App> =
        EventLoop::try_new().context("create event loop")?;
    let loop_handle = event_loop.handle();

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
        layer_shell,
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
        bootstrapping: true,
        exit: false,
        loop_handle: loop_handle.clone(),
        repaint_timer: None,
        last_render_at: None,
    };

    // Pin the surfaces to POE2's monitor before any draw, so the compositor can't
    // drift them to another output mid-session. While `bootstrapping`, configure
    // events don't draw, so no GL exists yet and re-homing the surface is safe.
    // Two roundtrips: bind + receive each output's logical geometry.
    event_queue
        .roundtrip(&mut app)
        .context("output roundtrip")?;
    event_queue
        .roundtrip(&mut app)
        .context("output roundtrip")?;
    if let Some(output) = poe2_output(&app.output_state) {
        app.popup
            .pin_to_output(&app.compositor, &app.layer_shell, &qh, Some(&output));
        app.settings
            .pin_to_output(&app.compositor, &app.layer_shell, &qh, Some(&output));
        tracing::info!("overlay pinned to POE2's monitor");
    } else {
        tracing::info!(
            "POE2 monitor not resolved (game not running?) — overlay uses compositor default"
        );
    }
    app.bootstrapping = false;

    // Wake the idle loop whenever egui requests a repaint (price-check results,
    // hotkeys, session checks — all land on a worker thread that calls
    // `request_repaint`). The ping coalesces, so a burst is one wake-up. Wired to
    // both contexts since the popup and settings panels repaint independently.
    let (repaint_ping, repaint_source) = make_ping().context("create repaint ping")?;
    loop_handle
        .insert_source(repaint_source, |(), (), app| app.on_repaint_request())
        .map_err(|e| anyhow!("insert repaint ping: {e}"))?;
    for ctx in [&app.popup.egui_ctx, &app.settings.egui_ctx] {
        let ping = repaint_ping.clone();
        ctx.set_request_repaint_callback(move |_| ping.ping());
    }

    WaylandSource::new(conn.clone(), event_queue)
        .insert(loop_handle)
        .map_err(|e| anyhow!("insert wayland source: {e}"))?;

    tracing::info!("overlay running (hidden) — Ctrl+C on an item in POE2 to pop it");
    while !app.exit {
        event_loop.dispatch(None, &mut app).context("dispatch")?;
        // Flush requests queued by renders driven from ping/timer wake-ups (the
        // Wayland source only flushes when it itself runs).
        app.conn.flush().context("flush wayland")?;
    }
    Ok(())
}

/// The `wl_output` whose logical rect contains POE2's window centre, so the
/// surfaces can be pinned to the monitor the game is on. `None` if POE2 isn't
/// found or no output's geometry covers it (e.g. mixed-DPI coordinate mismatch).
fn poe2_output(output_state: &OutputState) -> Option<wl_output::WlOutput> {
    let (x, y, w, h) = platform::poe2_window_geometry()?;
    let (cx, cy) = (x + w / 2, y + h / 2);
    for output in output_state.outputs() {
        let Some(info) = output_state.info(&output) else {
            continue;
        };
        if let (Some((ox, oy)), Some((ow, oh))) = (info.logical_position, info.logical_size) {
            if cx >= ox && cx < ox + ow && cy >= oy && cy < oy + oh {
                return Some(output);
            }
        }
    }
    None
}

struct App {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    relative_pointer_state: RelativePointerState,
    conn: Connection,
    compositor: CompositorState,
    layer_shell: LayerShell,
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
    /// True until the surfaces are pinned to their output at startup; suppresses
    /// drawing (hence GL init) so pinning happens before any painter exists.
    bootstrapping: bool,
    exit: bool,
    /// calloop handle, used to arm one-shot repaint timers for egui animations.
    loop_handle: LoopHandle<'static, App>,
    /// The pending egui-driven repaint timer, replaced (not stacked) each frame.
    repaint_timer: Option<RegistrationToken>,
    /// When the last paint happened, to throttle the repaint rate (see
    /// [`App::render_throttled`]).
    last_render_at: Option<Instant>,
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

    /// Whether a surface currently needs drawing — i.e. it is visible. A hidden
    /// surface is left fully idle; it is woken on demand by input, a repaint
    /// ping, or a timer (see [`App::on_repaint_request`]). This replaces the old
    /// rule that kept the popup spinning whenever Settings was closed, which
    /// busy-rendered ~100fps doing nothing.
    fn should_spin(&self, which: Which) -> bool {
        match which {
            Which::Popup => self.popup.shown,
            Which::Settings => self.settings.shown,
        }
    }

    /// Paint now after an input/state change in a Wayland handler (pointer,
    /// keyboard, drag), at the snappier [`INPUT_FRAME`] rate so interaction isn't
    /// bound to the 30fps animation cap. Renders whichever surface is visible.
    /// Replaces the old perpetual spin that absorbed input.
    fn repaint_input(&mut self) {
        self.render_throttled(self.visible(), INPUT_FRAME);
    }

    /// Render in response to a wake-up (repaint ping from a worker thread, or a
    /// repaint timer continuing an animation), at the [`ANIMATION_FRAME`] rate.
    /// `tick` pumps the channels either way, so a hidden popup still processes
    /// hotkeys / results and may pop.
    fn on_repaint_request(&mut self) {
        self.render_throttled(self.visible(), ANIMATION_FRAME);
    }

    /// The surface currently on screen (popup unless Settings is open).
    fn visible(&self) -> Which {
        if self.settings.shown {
            Which::Settings
        } else {
            Which::Popup
        }
    }

    /// Paint `which`, but no more than once per `min_interval`. egui's spinner
    /// (and other animations) call `request_repaint` every frame, which pings us
    /// every frame; without this throttle that would render at the monitor
    /// refresh rate. If we painted too recently, defer to a timer — collapsing
    /// any number of repaint requests into one capped paint. Input passes a small
    /// `min_interval` for responsiveness; egui's own animations always re-arm at
    /// the slower [`ANIMATION_FRAME`] below.
    fn render_throttled(&mut self, which: Which, min_interval: Duration) {
        let now = Instant::now();
        if let Some(last) = self.last_render_at {
            let since = now.saturating_duration_since(last);
            if since < min_interval {
                self.arm_repaint(min_interval.saturating_sub(since));
                return;
            }
        }
        self.last_render_at = Some(now);
        let delay = self.render(which);
        // egui wants to keep animating → schedule the next paint (capped to 30fps,
        // independent of how snappily input rendered).
        if delay < Duration::MAX {
            self.arm_repaint(delay.max(ANIMATION_FRAME));
        }
    }

    /// Arm a one-shot timer to repaint after `delay` (egui requested a delayed
    /// repaint, e.g. an animation or input debounce). Replaces any pending timer
    /// so they can't accumulate.
    fn arm_repaint(&mut self, delay: Duration) {
        if let Some(token) = self.repaint_timer.take() {
            self.loop_handle.remove(token);
        }
        let token =
            self.loop_handle
                .insert_source(Timer::from_duration(delay), move |_, (), app| {
                    app.repaint_timer = None;
                    app.on_repaint_request();
                    TimeoutAction::Drop
                });
        match token {
            Ok(token) => self.repaint_timer = Some(token),
            Err(e) => tracing::warn!(error = %e, "could not arm repaint timer"),
        }
    }

    /// Re-activate the POE2 window after hiding a surface (the compositor won't
    /// hand keyboard focus back on its own). Best-effort, off the UI thread.
    fn refocus_game() {
        std::thread::spawn(|| {
            platform::focus_poe2();
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
        // Kick the other surface if a transition left it visible and needing a draw.
        let other = current.other();
        if self.should_spin(other) {
            self.pending_bootstrap = Some(other);
        }
    }

    /// Draw `which` (plus any surface a transition left needing a redraw) and
    /// return the soonest delay egui wants before the next paint. Scheduling and
    /// rate-capping are the caller's job (see [`App::render_throttled`]).
    fn render(&mut self, which: Which) -> Duration {
        let mut delay = self.draw_surface(which);
        if let Some(boot) = self.pending_bootstrap.take() {
            if boot != which {
                delay = delay.min(self.draw_surface(boot));
            }
        }
        delay
    }

    /// Draw one surface (popup or settings); they differ only in width,
    /// placement, and content closure. Returns the delay egui wants before its
    /// next paint (see [`WinSurface::draw`]).
    fn draw_surface(&mut self, which: Which) -> Duration {
        self.tick(which);
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
        let result = surf.draw(&mut shared, want_w, place, fill, stroke, |ui| match which {
            Which::Popup => quick.content(ui),
            Which::Settings => quick.settings_content(ui),
        });
        match result {
            Ok(delay) => delay,
            Err(e) => {
                tracing::error!(error = %format!("{e:#}"), which = ?which, "surface draw failed");
                *exit = true;
                Duration::MAX
            }
        }
    }
}
