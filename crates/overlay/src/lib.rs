//! The price-check popup and the settings panel, each rendered onto its own
//! `wlr-layer-shell` *overlay* surface (PRD §4.5, §4.8).
//!
//! Both surfaces share one Wayland event loop and one EGL display, but each has
//! its own GL context + `egui::Context` so they lay out and paint
//! independently. [`WinSurface`] is the per-window bundle; [`App`] owns the two
//! (popup + settings) plus the shared [`ui::QuickModeApp`] that holds all the
//! state. Pointer / keyboard / configure / frame events are routed to whichever
//! surface they belong to.
//!
//! The price popup pops on a valid Ctrl+C and is pinned until Esc / the X
//! button; drag it with Alt held. The settings surface is opened from the gear
//! button or the tray (PRD §4.9) and closed by its own X / the tray Quit.
//! Neither surface takes keyboard focus while hidden, so POE2 keeps it.
//!
//! Entry point is [`run`], shared by the `poe2ddd` binary (`cargo run`) and the
//! `poe2-overlay` binary (`cargo run -p overlay`). The league/config come from
//! `~/.config/poe2ddd/config.json`; set `POE_LEAGUE` only to override one run.

use std::num::NonZeroU32;
use std::ptr::NonNull;
use std::sync::mpsc::channel;
use std::sync::Arc;

use anyhow::{anyhow, Context as _, Result};
use glutin::config::{ConfigTemplateBuilder, GlConfig};
use glutin::context::{
    ContextAttributesBuilder, NotCurrentGlContext, PossiblyCurrentContext,
    PossiblyCurrentGlContext,
};
use glutin::display::{Display, DisplayApiPreference, GlDisplay};
use glutin::surface::{GlSurface, Surface, SurfaceAttributesBuilder, WindowSurface};
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer,
    delegate_registry, delegate_relative_pointer, delegate_seat,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers as SctkModifiers},
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
        relative_pointer::{RelativeMotionEvent, RelativePointerHandler, RelativePointerState},
        Capability, SeatHandler, SeatState,
    },
    shell::{
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
        WaylandSurface,
    },
};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_surface},
    Connection, Proxy, QueueHandle,
};
use wayland_protocols::wp::relative_pointer::zv1::client::zwp_relative_pointer_v1;

use ui::{Hotkey, QuickModeApp};

/// Initial popup width before the content measurement grows it to
/// [`QuickModeApp::surface_width`]; the settings surface is a fixed width.
const POPUP_INIT_WIDTH: u32 = 470;
const SETTINGS_WIDTH: u32 = 540;
/// Starting height before the first content measurement.
const INITIAL_HEIGHT: u32 = 200;
/// Vertical space egui lays the content out in while we measure it. The surface
/// is then shrunk to the measured height (clamped to `MAX_HEIGHT`).
const LAYOUT_HEIGHT: f32 = 1600.0;
const MIN_HEIGHT: u32 = 80;
const MAX_HEIGHT: u32 = 1300;
/// Don't shrink the surface for height drops smaller than this — a deadband that
/// stops measurement jitter from thrashing `set_size` (see [`WinSurface::draw`]).
const HEIGHT_DEADBAND: u32 = 8;
/// Corner radius of the popup card.
const CORNER_RADIUS: f32 = 14.0;
/// Popup backing: a solid (opaque) grey card. Only the rounded corners let the
/// game show through.
const OVERLAY_FILL: egui::Color32 = egui::Color32::from_rgb(0x2c, 0x2e, 0x36);
const OVERLAY_STROKE: egui::Color32 = egui::Color32::from_rgb(0x50, 0x52, 0x5e);

/// How many of a surface's first visible frames pre-warm the font atlas. A
/// couple is enough: glyphs laid out here join the early atlas upload. See
/// [`WinSurface::warm_frames`] and [`WARMUP_TEXT`].
const WARMUP_FRAMES: u8 = 3;
/// Characters laid out (invisibly) during warm-up so they're baked into the
/// font atlas up front: printable Latin-1 Supplement + Latin Extended-A. Covers
/// the accented letters POE2 item and player names use (ö, é, ü, å, ñ, ł, …)
/// which otherwise rendered as boxes the first time they appeared.
const WARMUP_TEXT: &str = "\
\u{00A1}\u{00A2}\u{00A3}\u{00A4}\u{00A5}\u{00A6}\u{00A7}\u{00A8}\u{00A9}\u{00AA}\u{00AB}\u{00AC}\u{00AD}\u{00AE}\u{00AF}\u{00B0}\u{00B1}\u{00B2}\u{00B3}\u{00B4}\u{00B5}\u{00B6}\u{00B7}\u{00B8}\u{00B9}\u{00BA}\u{00BB}\u{00BC}\u{00BD}\u{00BE}\u{00BF}\
ÀÁÂÃÄÅÆÇÈÉÊËÌÍÎÏÐÑÒÓÔÕÖ×ØÙÚÛÜÝÞßàáâãäåæçèéêëìíîïðñòóôõö÷øùúûüýþÿ\
ĀāĂăĄąĆćĈĉĊċČčĎďĐđĒēĔĕĖėĘęĚěĜĝĞğĠġĢģĤĥĦħĨĩĪīĬĭĮįİıĲĳĴĵĶķĸĹĺĻļĽľĿŀŁłŃńŅņŇňŉŊŋŌōŎŏŐőŒœŔŕŖŗŘřŚśŜŝŞşŠšŢţŤťŦŧŨũŪūŬŭŮůŰűŲųŴŵŶŷŸŹźŻżŽž";

/// Which of the two surfaces an event belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Which {
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

/// Where to place a surface on its output.
#[derive(Clone, Copy)]
enum Placement {
    /// Centered on the output (default; also the `at-cursor` fallback for now).
    Center,
    /// Fixed top-left position in output-logical pixels.
    Fixed { x: i32, y: i32 },
}

/// Launch the overlay: build the egui app + tray, bind the two layer surfaces,
/// and run the Wayland event loop until the app is closed.
pub fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    // One egui context per surface (independent layout + GL). The popup context
    // is the one the hotkey watcher and price-check tasks repaint.
    let popup_ctx = egui::Context::default();
    ui::install_loaders(&popup_ctx);
    ui::configure_style(&popup_ctx);
    let settings_ctx = egui::Context::default();
    ui::install_loaders(&settings_ctx);
    ui::configure_style(&settings_ctx);

    let (hk_tx, hk_rx) = channel::<Hotkey>();
    ui::spawn_hotkey_watcher(popup_ctx.clone(), hk_tx.clone());

    // Tray (PRD §4.9): runs on its own thread; menu clicks (Open Settings /
    // Quit) are forwarded into the same hotkey channel that `pump()` drains
    // every frame, so no extra wake-up of the Wayland loop is needed (it
    // redraws continuously). The handle lets the UI push tooltip state.
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
    let quick = ui::build_app(hk_rx, tray).context("building price-check app")?;

    // Config hot-reload (PRD §4.8): watch config.json and push reloaded configs
    // down the same channel. Started after `build_app` so the startup backfill
    // write doesn't trigger a spurious reload. Best-effort — a watcher failure
    // just disables it.
    ui::spawn_config_watcher(popup_ctx.clone(), hk_tx);

    // Wayland side.
    let conn = Connection::connect_to_env().context("connect to Wayland")?;
    let (globals, mut event_queue) = registry_queue_init(&conn).context("registry init")?;
    let qh = event_queue.handle();

    let compositor =
        CompositorState::bind(&globals, &qh).map_err(|e| anyhow!("wl_compositor: {e}"))?;
    let layer_shell =
        LayerShell::bind(&globals, &qh).map_err(|e| anyhow!("wlr layer shell unavailable: {e}"))?;

    let popup = WinSurface::new(&compositor, &layer_shell, &qh, popup_ctx, "poe2ddd", POPUP_INIT_WIDTH);
    let settings = WinSurface::new(
        &compositor,
        &layer_shell,
        &qh,
        settings_ctx,
        "poe2ddd-settings",
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
        event_queue.blocking_dispatch(&mut app).context("dispatch")?;
    }
    Ok(())
}

/// GL state for one surface, created lazily on the first configure (once
/// mapped). The EGL [`Display`] is shared and lives on [`App`].
struct Gl {
    context: PossiblyCurrentContext,
    gl_surface: Surface<WindowSurface>,
    painter: egui_glow::Painter,
}

/// Shared, per-frame resources a surface needs to draw, borrowed from [`App`]
/// (so the two surfaces can be drawn without aliasing the whole `App`).
struct Shared<'a> {
    conn: &'a Connection,
    compositor: &'a CompositorState,
    output_state: &'a OutputState,
    display: &'a mut Option<Display>,
    kbd_modifiers: egui::Modifiers,
}

/// One `wlr-layer-shell` overlay surface (the popup or the settings panel) with
/// its own GL context and egui context.
struct WinSurface {
    layer: LayerSurface,
    egui_ctx: egui::Context,
    gl: Option<Gl>,
    events: Vec<egui::Event>,
    /// Whether this surface currently holds keyboard focus.
    kbd_focus: bool,
    width: u32,
    height: u32,
    desired_width: u32,
    desired_height: u32,
    /// The output the surface is on (from `surface_enter`), used to center it.
    current_output: Option<wl_output::WlOutput>,
    margin_left: i32,
    margin_top: i32,
    /// Whether the user has Alt-dragged this show (suppresses re-placement).
    dragged: bool,
    /// Whether an Alt drag is in progress (left button held).
    dragging: bool,
    /// Whether the surface is visible.
    shown: bool,
    /// Whether this surface is currently in its redraw loop (requested the next
    /// frame callback on its last draw). Used to know when a surface that should
    /// be redrawing has gone quiet and needs a kick (see `App::tick`).
    spinning: bool,
    /// Kept alive until the next commit so the compositor reads the region
    /// before it's destroyed.
    input_region: Option<Region>,
    /// Last applied (shown, width, height) so we only touch the input region on
    /// change.
    applied_input: Option<(bool, u32, u32)>,
    /// Last applied keyboard-interactivity `shown` state (toggle on change).
    applied_kbd: Option<bool>,
    /// Remaining frames to pre-warm the font atlas (see [`WARMUP_FRAMES`]). egui
    /// adds glyphs to its atlas lazily, and in this custom egui_glow integration
    /// glyphs uploaded *after* the initial atlas (rare ones like the `ö` in an
    /// item name) ended up rendering as tofu boxes. Laying out the common
    /// accented-Latin range on the first visible frames bakes those glyphs into
    /// the early atlas upload so they render correctly.
    warm_frames: u8,
}

impl WinSurface {
    /// Create a hidden layer surface (no keyboard focus, empty input region →
    /// click-through), anchored top-left and committed so it maps.
    fn new(
        compositor: &CompositorState,
        layer_shell: &LayerShell,
        qh: &QueueHandle<App>,
        egui_ctx: egui::Context,
        namespace: &str,
        width: u32,
    ) -> Self {
        let surface = compositor.create_surface(qh);
        let layer =
            layer_shell.create_layer_surface(qh, surface, Layer::Overlay, Some(namespace), None);
        // Keyboard focus is taken on-demand while shown (so text fields are
        // editable); None while hidden so POE2 keeps the keyboard.
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        // Anchor to top-left; we center via computed margins (KWin doesn't
        // reliably center an unanchored surface).
        layer.set_anchor(Anchor::TOP | Anchor::LEFT);
        layer.set_size(width, INITIAL_HEIGHT);
        layer.commit();
        WinSurface {
            layer,
            egui_ctx,
            gl: None,
            events: Vec::new(),
            kbd_focus: false,
            width,
            height: INITIAL_HEIGHT,
            desired_width: width,
            desired_height: INITIAL_HEIGHT,
            current_output: None,
            margin_left: 0,
            margin_top: 0,
            dragged: false,
            dragging: false,
            shown: false,
            spinning: false,
            input_region: None,
            applied_input: None,
            applied_kbd: None,
            warm_frames: WARMUP_FRAMES,
        }
    }

    /// Build the EGL context + egui painter for this surface, creating the
    /// shared EGL display on first use. glutin turns the `wl_surface` pointer
    /// into a `wl_egl_window` internally.
    fn init_gl(&mut self, shared: &mut Shared) -> Result<()> {
        let surface_ptr = self.layer.wl_surface().id().as_ptr() as *mut std::ffi::c_void;
        let raw_window = RawWindowHandle::Wayland(WaylandWindowHandle::new(
            NonNull::new(surface_ptr).context("null wl_surface ptr")?,
        ));

        if shared.display.is_none() {
            let display_ptr = shared.conn.backend().display_ptr() as *mut std::ffi::c_void;
            let raw_display = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
                NonNull::new(display_ptr).context("null wl_display ptr")?,
            ));
            let display = unsafe { Display::new(raw_display, DisplayApiPreference::Egl) }
                .context("create EGL display")?;
            *shared.display = Some(display);
        }
        let display = shared.display.as_ref().unwrap();

        let template = ConfigTemplateBuilder::new()
            .compatible_with_native_window(raw_window)
            .with_alpha_size(8)
            .build();
        // Prefer a plain 8-bit RGBA config (over deep/float ones) for a normal
        // translucent overlay.
        let config = unsafe { display.find_configs(template) }
            .context("find_configs")?
            .filter(|c| c.alpha_size() == 8)
            .min_by_key(|c| c.num_samples())
            .context("no RGBA8 EGL config")?;

        let context_attrs = ContextAttributesBuilder::new().build(Some(raw_window));
        let not_current = unsafe { display.create_context(&config, &context_attrs) }
            .context("create GL context")?;

        let surf_attrs = SurfaceAttributesBuilder::<WindowSurface>::new().build(
            raw_window,
            NonZeroU32::new(self.width).unwrap(),
            NonZeroU32::new(self.height).unwrap(),
        );
        let gl_surface = unsafe { display.create_window_surface(&config, &surf_attrs) }
            .context("create window surface")?;

        let context = not_current
            .make_current(&gl_surface)
            .context("make_current")?;

        let glow = unsafe {
            glow::Context::from_loader_function_cstr(|s| display.get_proc_address(s).cast())
        };
        let painter = egui_glow::Painter::new(Arc::new(glow), "", None, false)
            .map_err(|e| anyhow!("egui_glow painter: {e}"))?;

        self.gl = Some(Gl {
            context,
            gl_surface,
            painter,
        });
        tracing::info!("overlay GL surface ready");
        Ok(())
    }

    /// Lay out, size, place, and paint this surface for one frame, rendering
    /// `render` into the framed card when shown.
    fn draw(
        &mut self,
        shared: &mut Shared,
        qh: &QueueHandle<App>,
        want_width: u32,
        place: Placement,
        request_next: bool,
        render: impl FnOnce(&mut egui::Ui),
    ) -> Result<()> {
        if self.gl.is_none() {
            self.init_gl(shared)?;
        }

        self.apply_keyboard_interactivity();
        if !self.shown {
            // Forget any drag so the next show re-places it.
            self.dragged = false;
            self.dragging = false;
        }

        // Lay the content out in a tall space so we can measure its natural
        // height, then shrink the surface to fit (PRD §4.5 "small popup").
        let raw_input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(self.width as f32, LAYOUT_HEIGHT),
            )),
            events: std::mem::take(&mut self.events),
            focused: self.kbd_focus,
            modifiers: shared.kbd_modifiers,
            ..Default::default()
        };

        let ctx = self.egui_ctx.clone();
        let shown = self.shown;
        let width = self.width as f32;
        let warm = shown && self.warm_frames > 0;
        if warm {
            self.warm_frames -= 1;
        }
        let mut measured = 0.0_f32;
        // `ctx.run` wants an `FnMut`, but `render` is `FnOnce`; hand it out via
        // an `Option::take` so it's consumed at most once (run calls us once).
        let mut render = Some(render);
        let full = ctx.run(raw_input, |c| {
            if !shown {
                return;
            }
            // Pre-warm the font atlas: lay the accented-Latin range out in a
            // throwaway, fully-transparent, non-interactable Area so the glyphs
            // enter this frame's atlas upload without affecting the visible
            // layout or the measured height (see WARMUP_TEXT).
            if warm {
                egui::Area::new(egui::Id::new("atlas-warmup"))
                    .fixed_pos(egui::pos2(0.0, 0.0))
                    .interactable(false)
                    .show(c, |ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(WARMUP_TEXT).color(egui::Color32::TRANSPARENT),
                            )
                            .selectable(false),
                        );
                    });
            }
            let render = render.take();
            let resp = egui::Area::new(egui::Id::new("surface"))
                .fixed_pos(egui::pos2(0.0, 0.0))
                .show(c, |ui| {
                    ui.set_max_width(width);
                    egui::Frame::none()
                        .fill(OVERLAY_FILL)
                        .stroke(egui::Stroke::new(1.0, OVERLAY_STROKE))
                        .rounding(CORNER_RADIUS)
                        .inner_margin(egui::Margin::same(12.0))
                        .show(ui, |ui| {
                            ui.set_width(width - 24.0);
                            if let Some(render) = render {
                                render(ui);
                            }
                        });
                });
            measured = resp.response.rect.height();
        });

        // Auto-height: resize the surface to the measured content. NOT while
        // dragging, and with a deadband on shrink — every `set_size` triggers a
        // configure → draw → maybe set_size again (an un-throttled loop), so a
        // 1px jitter must not fire it (it pegs a core and makes drag lag).
        if shown && measured > 0.0 && !self.dragging {
            let want_h = (measured.ceil() as u32).clamp(MIN_HEIGHT, MAX_HEIGHT);
            let grow = want_h > self.desired_height; // grow at once (no clipping)
            let shrink = self.desired_height.saturating_sub(want_h) > HEIGHT_DEADBAND;
            if want_width != self.desired_width || grow || shrink {
                self.desired_height = want_h;
                self.desired_width = want_width;
                self.layer.set_size(want_width, want_h);
            }
        }

        // Placement every visible frame (incl. during a drag, so the surface
        // tracks the cursor): follow a drag, else apply the configured place.
        if shown {
            if self.dragged {
                self.apply_margin();
            } else {
                match place {
                    Placement::Center => self.center(shared, self.desired_width, self.desired_height),
                    Placement::Fixed { x, y } => {
                        self.margin_left = x.max(0);
                        self.margin_top = y.max(0);
                        self.apply_margin();
                    }
                }
            }
        }

        self.apply_input_region(shared);

        let ppp = full.pixels_per_point;
        let primitives = ctx.tessellate(full.shapes, ppp);
        let size = [self.width, self.height];

        let gl = self.gl.as_mut().unwrap();
        gl.context
            .make_current(&gl.gl_surface)
            .expect("make_current in draw");
        gl.painter.clear(size, [0.0, 0.0, 0.0, 0.0]);
        gl.painter
            .paint_and_update_textures(size, ppp, &primitives, &full.textures_delta);
        gl.gl_surface
            .swap_buffers(&gl.context)
            .expect("swap_buffers");

        // Schedule the next frame only if this surface should keep redrawing
        // (`App::should_spin`). EXACTLY ONE surface spins at a time — the active
        // one — so two overlays never compete for the vsync swap (that was the
        // settings-window lag). A surface going quiet still presents this frame
        // (the transparent clear hides it); `App::tick` kicks the other surface
        // back into its loop when the active one changes.
        self.spinning = request_next;
        if request_next {
            let surface = self.layer.wl_surface().clone();
            surface.frame(qh, surface.clone());
        }
        self.layer.commit();
        Ok(())
    }

    /// Take keyboard focus on-demand while shown (so text fields are editable),
    /// and drop it when hidden so POE2 gets the keyboard back.
    fn apply_keyboard_interactivity(&mut self) {
        if self.applied_kbd == Some(self.shown) {
            return;
        }
        self.layer.set_keyboard_interactivity(if self.shown {
            KeyboardInteractivity::OnDemand
        } else {
            KeyboardInteractivity::None
        });
        if !self.shown {
            self.kbd_focus = false;
        }
        self.applied_kbd = Some(self.shown);
    }

    /// Set the input region to the surface bounds when visible (so it catches
    /// clicks) and to nothing when hidden (so clicks pass through to POE2).
    fn apply_input_region(&mut self, shared: &Shared) {
        let state = (self.shown, self.width, self.height);
        if self.applied_input == Some(state) {
            return;
        }
        if let Ok(region) = Region::new(shared.compositor) {
            if self.shown {
                region.add(0, 0, self.width as i32, self.height as i32);
            }
            // An empty region = no input = clicks pass through to the game.
            self.layer.set_input_region(Some(region.wl_region()));
            self.input_region = Some(region); // keep alive until the next commit
            self.applied_input = Some(state);
        }
    }

    /// Center a `w`×`h` surface on its output by setting top/left margins.
    fn center(&mut self, shared: &Shared, w: u32, h: u32) {
        let (ow, oh) = self.output_size(shared);
        self.margin_left = ((ow - w as i32) / 2).max(0);
        self.margin_top = ((oh - h as i32) / 2).max(0);
        self.apply_margin();
    }

    /// Push the current margins to the surface (committed at end of frame).
    fn apply_margin(&self) {
        self.layer.set_margin(self.margin_top, 0, 0, self.margin_left);
    }

    /// Logical size of the output the surface is on (falls back to the first
    /// output, then a 1080p guess if nothing is known yet).
    fn output_size(&self, shared: &Shared) -> (i32, i32) {
        let output = self
            .current_output
            .clone()
            .or_else(|| shared.output_state.outputs().next());
        if let Some(output) = output {
            if let Some(info) = shared.output_state.info(&output) {
                if let Some(size) = info.logical_size {
                    return size;
                }
                if let Some(mode) = info.modes.iter().find(|m| m.current) {
                    return mode.dimensions;
                }
            }
        }
        (1920, 1080)
    }
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
    fn tick(&mut self, current: Which) {
        // Always pump on the popup's egui context: that's where price results
        // render and where the watcher / background tasks request repaints.
        self.quick.pump(&self.popup.egui_ctx);
        if self.quick.take_pop_request() {
            // A Ctrl+C takes over: show the popup, leave settings.
            self.popup.shown = true;
            self.settings.shown = false;
        }
        if self.quick.take_close_request() {
            self.popup.shown = false;
        }
        if self.quick.take_settings_request() {
            // Open settings and hide the popup so the two don't overlap.
            self.settings.shown = true;
            self.popup.shown = false;
        }
        if self.quick.take_settings_close_request() {
            self.settings.shown = false;
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
        match which {
            Which::Popup => self.draw_popup(qh),
            Which::Settings => self.draw_settings(qh),
        }
        if let Some(boot) = self.pending_bootstrap.take() {
            if boot != which {
                match boot {
                    Which::Popup => self.draw_popup(qh),
                    Which::Settings => self.draw_settings(qh),
                }
            }
        }
    }

    /// Draw the price-check popup (`QuickModeApp::content`).
    fn draw_popup(&mut self, qh: &QueueHandle<Self>) {
        self.tick(Which::Popup);
        let request_next = self.should_spin(Which::Popup);
        let place = match self.quick.position_mode() {
            "fixed" => {
                let (x, y) = self.quick.fixed_pos();
                Placement::Fixed { x, y }
            }
            // center + at-cursor (Phase 7 stub) both center for now.
            _ => Placement::Center,
        };
        let want_w = self.quick.surface_width();

        let App {
            popup,
            quick,
            conn,
            compositor,
            output_state,
            display,
            kbd_modifiers,
            exit,
            ..
        } = self;
        let mut shared = Shared {
            conn,
            compositor,
            output_state,
            display,
            kbd_modifiers: *kbd_modifiers,
        };
        if let Err(e) = popup.draw(&mut shared, qh, want_w, place, request_next, |ui| {
            quick.content(ui)
        }) {
            tracing::error!(error = %format!("{e:#}"), "popup draw failed");
            *exit = true;
        }
    }

    /// Draw the settings panel surface.
    fn draw_settings(&mut self, qh: &QueueHandle<Self>) {
        self.tick(Which::Settings);
        let request_next = self.should_spin(Which::Settings);
        let App {
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
        let mut shared = Shared {
            conn,
            compositor,
            output_state,
            display,
            kbd_modifiers: *kbd_modifiers,
        };
        if let Err(e) = settings.draw(
            &mut shared,
            qh,
            SETTINGS_WIDTH,
            Placement::Center,
            request_next,
            |ui| quick.settings_content(ui),
        ) {
            tracing::error!(error = %format!("{e:#}"), "settings draw failed");
            *exit = true;
        }
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, qh: &QueueHandle<Self>, surface: &wl_surface::WlSurface, _: u32) {
        if let Some(which) = self.which(surface) {
            self.render(which, qh);
        }
    }
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &wl_surface::WlSurface, output: &wl_output::WlOutput) {
        match self.which(surface) {
            Some(Which::Popup) => self.popup.current_output = Some(output.clone()),
            Some(Which::Settings) => self.settings.current_output = Some(output.clone()),
            None => {}
        }
    }
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.exit = true;
    }
    fn configure(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        let Some(which) = self.which(layer.wl_surface()) else {
            return;
        };
        {
            let surf = self.surf_mut(which);
            if configure.new_size.0 != 0 && configure.new_size.1 != 0 {
                surf.width = configure.new_size.0;
                surf.height = configure.new_size.1;
            }
            if let Some(gl) = surf.gl.as_ref() {
                gl.gl_surface.resize(
                    &gl.context,
                    NonZeroU32::new(surf.width).unwrap(),
                    NonZeroU32::new(surf.height).unwrap(),
                );
            }
        }
        self.render(which, qh);
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Pointer && self.pointer.is_none() {
            if let Ok(pointer) = self.seat_state.get_pointer(qh, &seat) {
                // Raw motion deltas for Alt drag (immune to the surface moving
                // under the cursor).
                self.relative_pointer = self
                    .relative_pointer_state
                    .get_relative_pointer(&pointer, qh)
                    .ok();
                self.pointer = Some(pointer);
            }
        }
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            if let Ok(kbd) = self.seat_state.get_keyboard(qh, &seat, None) {
                self.keyboard = Some(kbd);
            }
        }
    }
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Keyboard {
            if let Some(k) = self.keyboard.take() {
                k.release();
            }
        }
        if capability == Capability::Pointer {
            self.relative_pointer = None;
            if let Some(p) = self.pointer.take() {
                p.release();
            }
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for App {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        // Where the popup ended an Alt-drag this frame — persisted as the fixed
        // position so it stays put (handled after the event loop to avoid
        // borrowing `self.quick` while a surface is borrowed).
        let mut popup_dropped: Option<(i32, i32)> = None;
        for event in events {
            let Some(which) = self.which(&event.surface) else {
                continue;
            };
            let pos = egui::pos2(event.position.0 as f32, event.position.1 as f32);
            let alt = self.quick.alt_held();
            let surf = self.surf_mut(which);
            match event.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    // While dragging, the relative pointer drives the move; don't
                    // also feed motion to egui.
                    if !surf.dragging {
                        surf.events.push(egui::Event::PointerMoved(pos));
                    }
                }
                PointerEventKind::Leave { .. } => {
                    surf.events.push(egui::Event::PointerGone);
                }
                PointerEventKind::Press { button, .. } => {
                    // Alt + left button starts a window drag (consumed, not
                    // forwarded to egui).
                    if button == BTN_LEFT && alt {
                        surf.dragging = true;
                        surf.dragged = true; // stop re-placement, keep current pos
                    } else if let Some(b) = map_button(button) {
                        surf.events.push(egui::Event::PointerButton {
                            pos,
                            button: b,
                            pressed: true,
                            modifiers: egui::Modifiers::default(),
                        });
                    }
                }
                PointerEventKind::Release { button, .. } => {
                    if button == BTN_LEFT && surf.dragging {
                        surf.dragging = false; // end drag (position kept)
                        // Remember where the POPUP was dropped → fixed position.
                        if which == Which::Popup {
                            popup_dropped = Some((surf.margin_left, surf.margin_top));
                        }
                    } else if let Some(b) = map_button(button) {
                        surf.events.push(egui::Event::PointerButton {
                            pos,
                            button: b,
                            pressed: false,
                            modifiers: egui::Modifiers::default(),
                        });
                    }
                }
                PointerEventKind::Axis { vertical, .. } => {
                    let dy = -vertical.absolute as f32;
                    if dy != 0.0 {
                        surf.events.push(egui::Event::MouseWheel {
                            unit: egui::MouseWheelUnit::Point,
                            delta: egui::vec2(0.0, dy),
                            modifiers: egui::Modifiers::default(),
                        });
                    }
                }
            }
        }
        // Persist the dropped popup position (switches Settings to Fixed at x/y).
        if let Some((x, y)) = popup_dropped {
            self.quick.set_fixed_position(x, y);
        }
    }
}

impl RelativePointerHandler for App {
    fn relative_pointer_motion(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &zwp_relative_pointer_v1::ZwpRelativePointerV1,
        _: &wl_pointer::WlPointer,
        event: RelativeMotionEvent,
    ) {
        // Apply the raw cursor delta to whichever surface is being dragged.
        let dx = event.delta.0.round() as i32;
        let dy = event.delta.1.round() as i32;
        let surf = if self.popup.dragging {
            Some(&mut self.popup)
        } else if self.settings.dragging {
            Some(&mut self.settings)
        } else {
            None
        };
        if let Some(surf) = surf {
            surf.margin_left = (surf.margin_left + dx).max(0);
            surf.margin_top = (surf.margin_top + dy).max(0);
        }
    }
}

impl KeyboardHandler for App {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
        if let Some(which) = self.which(surface) {
            self.focused = Some(which);
            self.surf_mut(which).kbd_focus = true;
            tracing::info!(?which, "keyboard focus GAINED");
        }
    }
    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        let Some(which) = self.which(surface) else {
            return;
        };
        self.surf_mut(which).kbd_focus = false;
        if self.focused == Some(which) {
            self.focused = None;
        }
        // The popup auto-closes when focus leaves it (the user clicked back into
        // POE2) so they don't have to press Esc. The settings panel stays open
        // (you may alt-tab to check something) — close it with its X / the tray.
        if which == Which::Popup && self.popup.shown {
            tracing::info!("popup keyboard focus lost → closing");
            self.popup.shown = false;
        }
    }
    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        let modifiers = self.kbd_modifiers;
        let Some(which) = self.focused else {
            return;
        };
        // Ctrl+V: egui never reads the system clipboard itself — the windowing
        // layer must turn the shortcut into an `Event::Paste`. (Without this,
        // text fields like the POESESSID setting can't be pasted into.)
        if modifiers.ctrl && !modifiers.alt && (event.keysym == Keysym::v || event.keysym == Keysym::V) {
            match platform_linux::read_clipboard_text() {
                Ok(Some(text)) if !text.is_empty() => {
                    self.surf_mut(which).events.push(egui::Event::Paste(text));
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "clipboard read for paste failed"),
            }
            return;
        }
        if let Some(key) = map_keysym(event.keysym) {
            self.surf_mut(which).events.push(egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            });
        }
        // Printable text — but not while Ctrl/Alt are held (those are
        // shortcuts), and not control chars (Backspace etc. arrive as utf8 too).
        if !modifiers.ctrl && !modifiers.alt {
            if let Some(text) = event.utf8 {
                if !text.is_empty() && !text.chars().any(|c| c.is_control()) {
                    self.surf_mut(which).events.push(egui::Event::Text(text));
                }
            }
        }
    }
    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        let modifiers = self.kbd_modifiers;
        let Some(which) = self.focused else {
            return;
        };
        if let Some(key) = map_keysym(event.keysym) {
            self.surf_mut(which).events.push(egui::Event::Key {
                key,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers,
            });
        }
    }
    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        modifiers: SctkModifiers,
        _: u32,
    ) {
        self.kbd_modifiers = egui::Modifiers {
            alt: modifiers.alt,
            ctrl: modifiers.ctrl,
            shift: modifiers.shift,
            mac_cmd: false,
            command: modifiers.ctrl,
        };
    }
}

/// Map a keysym to an egui [`Key`] for the editing/navigation keys a text field
/// needs (printable characters go through `Event::Text` instead).
fn map_keysym(k: Keysym) -> Option<egui::Key> {
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
const BTN_LEFT: u32 = 0x110;

/// Linux evdev button codes → egui buttons.
fn map_button(code: u32) -> Option<egui::PointerButton> {
    match code {
        BTN_LEFT => Some(egui::PointerButton::Primary),
        0x111 => Some(egui::PointerButton::Secondary), // BTN_RIGHT
        0x112 => Some(egui::PointerButton::Middle),    // BTN_MIDDLE
        _ => None,
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(App);
delegate_output!(App);
delegate_seat!(App);
delegate_pointer!(App);
delegate_relative_pointer!(App);
delegate_keyboard!(App);
delegate_layer!(App);
delegate_registry!(App);
