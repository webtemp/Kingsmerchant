//! The price-check popup rendered onto a `wlr-layer-shell` *overlay* surface
//! (PRD §4.5), in both quick (Ctrl+C) and detailed (Ctrl+Alt+C) modes.
//!
//! The windowing layer is a smithay-client-toolkit layer surface + glutin EGL +
//! egui_glow; the UI itself reuses [`ui::QuickModeApp`] so only the surface
//! underneath differs from a normal window. The surface takes NO keyboard focus
//! (POE2 stays focused), starts hidden until the first valid Ctrl+C, and is
//! centered on the output.
//!
//! Ctrl+C (or Ctrl+Alt+C) shows the popup; it's pinned open (with the filter
//! panel) until Esc or the X button. Drag it with Alt held.
//!
//! Entry point is [`run`], shared by the `poe2ddd` binary (`cargo run`) and the
//! `poe2-overlay` binary (`cargo run -p overlay`). The league comes from
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
    delegate_compositor, delegate_layer, delegate_output, delegate_pointer, delegate_registry,
    delegate_relative_pointer, delegate_seat,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
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
    protocol::{wl_output, wl_pointer, wl_seat, wl_surface},
    Connection, Proxy, QueueHandle,
};
use wayland_protocols::wp::relative_pointer::zv1::client::zwp_relative_pointer_v1;

use ui::{Hotkey, QuickModeApp};

/// Fixed popup width; height auto-fits the content (see [`App::draw`]).
const WIDTH: u32 = 470;
/// Starting height before the first content measurement.
const INITIAL_HEIGHT: u32 = 200;
/// Vertical space egui lays the content out in while we measure it. The surface
/// is then shrunk to the measured height (clamped to `MAX_HEIGHT`).
const LAYOUT_HEIGHT: f32 = 1600.0;
const MIN_HEIGHT: u32 = 80;
const MAX_HEIGHT: u32 = 1300;
/// Don't shrink the surface for height drops smaller than this — a deadband that
/// stops measurement jitter from thrashing `set_size` (see `App::draw`).
const HEIGHT_DEADBAND: u32 = 8;
/// Corner radius of the popup card.
const CORNER_RADIUS: f32 = 14.0;
/// Popup backing: a solid (opaque) grey card. Only the rounded corners let the
/// game show through.
const OVERLAY_FILL: egui::Color32 = egui::Color32::from_rgb(0x2c, 0x2e, 0x36);
const OVERLAY_STROKE: egui::Color32 = egui::Color32::from_rgb(0x50, 0x52, 0x5e);

/// Launch the price-check overlay: build the egui app, bind the layer surface,
/// and run the Wayland event loop until the popup is closed.
pub fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    // egui side. The watcher repaints this same context; build it before the
    // app so the hotkey thread and the price-check task target one context.
    let egui_ctx = egui::Context::default();
    ui::install_loaders(&egui_ctx);
    ui::configure_style(&egui_ctx);

    let (hk_tx, hk_rx) = channel::<Hotkey>();
    ui::spawn_hotkey_watcher(egui_ctx.clone(), hk_tx);
    let quick = ui::build_app(hk_rx).context("building price-check app")?;

    // Wayland side.
    let conn = Connection::connect_to_env().context("connect to Wayland")?;
    let (globals, mut event_queue) = registry_queue_init(&conn).context("registry init")?;
    let qh = event_queue.handle();

    let compositor =
        CompositorState::bind(&globals, &qh).map_err(|e| anyhow!("wl_compositor: {e}"))?;
    let layer_shell =
        LayerShell::bind(&globals, &qh).map_err(|e| anyhow!("wlr layer shell unavailable: {e}"))?;

    let surface = compositor.create_surface(&qh);
    let layer = layer_shell.create_layer_surface(
        &qh,
        surface,
        Layer::Overlay,
        Some("poe2ddd"),
        None,
    );
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
    // Anchor to the top-left corner; we center by computing margins from the
    // output size (KWin doesn't reliably center an unanchored surface). Margins
    // are (re)applied per frame while visible — see `App::draw`.
    layer.set_anchor(Anchor::TOP | Anchor::LEFT);
    layer.set_size(WIDTH, INITIAL_HEIGHT);
    layer.commit();

    let mut app = App {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        relative_pointer_state: RelativePointerState::bind(&globals, &qh),
        conn: conn.clone(),
        compositor,
        layer,
        pointer: None,
        relative_pointer: None,
        gl: None,
        egui_ctx,
        quick,
        events: Vec::new(),
        width: WIDTH,
        height: INITIAL_HEIGHT,
        desired_width: WIDTH,
        desired_height: INITIAL_HEIGHT,
        current_output: None,
        margin_left: 0,
        margin_top: 0,
        dragged: false,
        dragging: false,
        shown: false,
        input_region: None,
        applied_input: None,
        exit: false,
    };

    tracing::info!("overlay running (hidden) — Ctrl+C on an item in POE2 to pop it");
    while !app.exit {
        event_queue.blocking_dispatch(&mut app).context("dispatch")?;
    }
    Ok(())
}

/// GL state, created lazily on the first configure (once mapped).
struct Gl {
    _display: Display,
    context: PossiblyCurrentContext,
    gl_surface: Surface<WindowSurface>,
    painter: egui_glow::Painter,
}

struct App {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    relative_pointer_state: RelativePointerState,
    conn: Connection,
    compositor: CompositorState,
    layer: LayerSurface,
    pointer: Option<wl_pointer::WlPointer>,
    relative_pointer: Option<zwp_relative_pointer_v1::ZwpRelativePointerV1>,
    gl: Option<Gl>,
    egui_ctx: egui::Context,
    quick: QuickModeApp,
    events: Vec<egui::Event>,
    width: u32,
    height: u32,
    /// Width we last asked the surface to be (mode-dependent: detailed is wider).
    desired_width: u32,
    /// Height we last asked the surface to be (auto-height target).
    desired_height: u32,
    /// The output the surface is on (from `surface_enter`), used to center it.
    current_output: Option<wl_output::WlOutput>,
    /// Current top/left margins (the surface position).
    margin_left: i32,
    margin_top: i32,
    /// Whether the user has Ctrl+Alt-dragged this show (suppresses centering).
    dragged: bool,
    /// Whether a Ctrl+Alt drag is in progress (left button held).
    dragging: bool,
    /// Whether the popup is visible. Starts hidden; a valid Ctrl+C shows it,
    /// Escape hides it.
    shown: bool,
    /// Kept alive until the next commit so the compositor reads the region
    /// before it's destroyed.
    input_region: Option<Region>,
    /// Last applied (shown, width, height) so we only touch the input region on
    /// change.
    applied_input: Option<(bool, u32, u32)>,
    exit: bool,
}

impl App {
    /// Build the EGL context + egui painter against our `wl_surface`. glutin
    /// turns the `wl_surface` pointer into a `wl_egl_window` internally.
    fn init_gl(&mut self) -> Result<()> {
        let wl_surface = self.layer.wl_surface();

        let display_ptr = self.conn.backend().display_ptr() as *mut std::ffi::c_void;
        let raw_display = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
            NonNull::new(display_ptr).context("null wl_display ptr")?,
        ));
        let surface_ptr = wl_surface.id().as_ptr() as *mut std::ffi::c_void;
        let raw_window = RawWindowHandle::Wayland(WaylandWindowHandle::new(
            NonNull::new(surface_ptr).context("null wl_surface ptr")?,
        ));

        let display = unsafe { Display::new(raw_display, DisplayApiPreference::Egl) }
            .context("create EGL display")?;

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
            _display: display,
            context,
            gl_surface,
            painter,
        });
        tracing::info!("overlay GL surface ready");
        Ok(())
    }

    fn draw(&mut self, qh: &QueueHandle<Self>) {
        if self.gl.is_none() {
            if let Err(e) = self.init_gl() {
                tracing::error!(error = %format!("{e:#}"), "GL init failed");
                self.exit = true;
                return;
            }
        }

        // Drain hotkeys + results every frame, even while hidden, so a fresh
        // Ctrl+C is noticed. A valid item flips us visible; Escape hides it.
        self.quick.pump(&self.egui_ctx);
        if self.quick.take_pop_request() {
            self.shown = true;
        }
        if self.quick.take_close_request() {
            self.shown = false;
        }
        // The popup is pinned: once shown it stays until Esc or the X button
        // (handled above via take_close_request).
        if !self.shown {
            // Forget any drag so the next pop re-centers (no position memory).
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
            focused: false,
            ..Default::default()
        };

        let ctx = self.egui_ctx.clone();
        let shown = self.shown;
        let width = self.width as f32;
        let mut measured = 0.0_f32;
        let full = ctx.run(raw_input, |c| {
            if !shown {
                return;
            }
            let resp = egui::Area::new(egui::Id::new("popup"))
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
                            self.quick.content(ui);
                        });
                });
            measured = resp.response.rect.height();
        });

        // Auto-height: resize the surface to the measured content. Crucially NOT
        // while dragging, and with a deadband on shrink: every `set_size`
        // triggers a configure → draw → maybe set_size again, an UN-throttled
        // loop (configure isn't vsync-gated). Letting a 1px measurement jitter
        // fire it pegs a core and makes the whole UI — and Alt-drag — lag.
        if shown && measured > 0.0 && !self.dragging {
            let want_h = (measured.ceil() as u32).clamp(MIN_HEIGHT, MAX_HEIGHT);
            // Width is mode-dependent (detailed mode is wider for the filters).
            let want_w = self.quick.surface_width();
            let grow = want_h > self.desired_height; // grow at once (no clipping)
            let shrink = self.desired_height.saturating_sub(want_h) > HEIGHT_DEADBAND;
            if want_w != self.desired_width || grow || shrink {
                self.desired_height = want_h;
                self.desired_width = want_w;
                self.layer.set_size(want_w, want_h);
            }
        }
        // Placement every visible frame (incl. during a drag, so the surface
        // tracks the cursor): follow a drag, else keep dragged position, else
        // center.
        if shown {
            if self.dragged {
                self.apply_margin();
            } else {
                self.center(self.desired_width, self.desired_height);
            }
        }

        self.apply_input_region();

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

        // Keep redrawing so async price-check results and hover states appear.
        // (Continuous at vsync for now; on-demand wake-ups are a later polish.)
        let surface = self.layer.wl_surface().clone();
        surface.frame(qh, surface.clone());
        self.layer.commit();
    }

    /// Set the surface input region to the popup bounds when visible (so it
    /// catches clicks) and to nothing when hidden (so clicks pass through to
    /// POE2). Only re-applied when the visibility or size changes.
    fn apply_input_region(&mut self) {
        let state = (self.shown, self.width, self.height);
        if self.applied_input == Some(state) {
            return;
        }
        if let Ok(region) = Region::new(&self.compositor) {
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
    fn center(&mut self, w: u32, h: u32) {
        let (ow, oh) = self.output_size();
        self.margin_left = ((ow - w as i32) / 2).max(0);
        self.margin_top = ((oh - h as i32) / 2).max(0);
        self.apply_margin();
    }

    /// Push the current margins to the surface (committed at end of frame).
    fn apply_margin(&self) {
        self.layer.set_margin(self.margin_top, 0, 0, self.margin_left);
    }

    /// Logical size of the output the surface is on (falls back to the first
    /// output, then to a 1080p guess if nothing is known yet).
    fn output_size(&self) -> (i32, i32) {
        let output = self
            .current_output
            .clone()
            .or_else(|| self.output_state.outputs().next());
        if let Some(output) = output {
            if let Some(info) = self.output_state.info(&output) {
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

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.draw(qh);
    }
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, output: &wl_output::WlOutput) {
        self.current_output = Some(output.clone());
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
        _: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        if configure.new_size.0 != 0 && configure.new_size.1 != 0 {
            self.width = configure.new_size.0;
            self.height = configure.new_size.1;
        }
        if let Some(gl) = self.gl.as_ref() {
            gl.gl_surface.resize(
                &gl.context,
                NonZeroU32::new(self.width).unwrap(),
                NonZeroU32::new(self.height).unwrap(),
            );
        }
        self.draw(qh);
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
                // Raw motion deltas for Ctrl+Alt drag (immune to the surface
                // moving under the cursor).
                self.relative_pointer = self
                    .relative_pointer_state
                    .get_relative_pointer(&pointer, qh)
                    .ok();
                self.pointer = Some(pointer);
            }
        }
        // Deliberately never grab the keyboard — POE2 must keep focus.
    }
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, capability: Capability) {
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
        for event in events {
            if &event.surface != self.layer.wl_surface() {
                continue;
            }
            let pos = egui::pos2(event.position.0 as f32, event.position.1 as f32);
            match event.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    // While dragging, the relative pointer drives the move; don't
                    // also feed motion to egui.
                    if !self.dragging {
                        self.events.push(egui::Event::PointerMoved(pos));
                    }
                }
                PointerEventKind::Leave { .. } => {
                    self.events.push(egui::Event::PointerGone);
                }
                PointerEventKind::Press { button, .. } => {
                    // Alt + left button starts a window drag (consumed, not
                    // forwarded to egui).
                    if button == BTN_LEFT && self.quick.alt_held() {
                        self.dragging = true;
                        self.dragged = true; // stop centering, keep current pos
                    } else if let Some(b) = map_button(button) {
                        self.events.push(egui::Event::PointerButton {
                            pos,
                            button: b,
                            pressed: true,
                            modifiers: egui::Modifiers::default(),
                        });
                    }
                }
                PointerEventKind::Release { button, .. } => {
                    if button == BTN_LEFT && self.dragging {
                        self.dragging = false; // end drag (position kept)
                    } else if let Some(b) = map_button(button) {
                        self.events.push(egui::Event::PointerButton {
                            pos,
                            button: b,
                            pressed: false,
                            modifiers: egui::Modifiers::default(),
                        });
                    }
                }
                PointerEventKind::Axis { vertical, .. } => {
                    // egui scrolls on raw delta; surface axis is in "discrete"
                    // logical pixels.
                    let dy = -vertical.absolute as f32;
                    if dy != 0.0 {
                        self.events.push(egui::Event::MouseWheel {
                            unit: egui::MouseWheelUnit::Point,
                            delta: egui::vec2(0.0, dy),
                            modifiers: egui::Modifiers::default(),
                        });
                    }
                }
            }
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
        if self.dragging {
            // Raw cursor delta → margin shift. Moving the surface by exactly the
            // cursor delta keeps the cursor over it, so events keep flowing.
            self.margin_left = (self.margin_left + event.delta.0.round() as i32).max(0);
            self.margin_top = (self.margin_top + event.delta.1.round() as i32).max(0);
        }
    }
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
delegate_layer!(App);
delegate_registry!(App);
