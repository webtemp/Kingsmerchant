//! Phase 4: the quick-mode price popup rendered onto a `wlr-layer-shell`
//! *overlay* surface (PRD §4.5).
//!
//! The windowing layer (smithay-client-toolkit layer surface + glutin EGL +
//! egui_glow) is validated by `crates/overlay-spike`. Here we render the real
//! UI by reusing [`ui::QuickModeApp::ui`] — the exact egui draw code from the
//! Phase 3 window — so only the surface underneath changes.
//!
//! Increment 1 (this file): the overlay maps at a fixed position, takes NO
//! keyboard focus, and prices the item copied by the in-game Ctrl+C. Cursor
//! positioning, Alt-drag, and Esc-dismiss land in later increments.
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
    delegate_seat,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
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
/// Fixed placement for now (cursor-relative placement is a later increment).
const MARGIN_TOP: i32 = 120;
const MARGIN_LEFT: i32 = 120;
/// Popup backing: deliberately very translucent so the game shows through
/// (~80% transparent per the design). Text/widgets paint opaque on top.
const OVERLAY_FILL: egui::Color32 = egui::Color32::from_rgba_premultiplied(10, 11, 15, 52);
const OVERLAY_STROKE: egui::Color32 = egui::Color32::from_rgb(0x3a, 0x3a, 0x46);

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
    layer.set_anchor(Anchor::TOP | Anchor::LEFT);
    layer.set_margin(MARGIN_TOP, 0, 0, MARGIN_LEFT);
    layer.set_size(WIDTH, INITIAL_HEIGHT);
    layer.commit();

    let mut app = App {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        conn: conn.clone(),
        compositor,
        layer,
        pointer: None,
        gl: None,
        egui_ctx,
        quick,
        events: Vec::new(),
        pointer_pos: egui::pos2(-1.0, -1.0),
        width: WIDTH,
        height: INITIAL_HEIGHT,
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
    conn: Connection,
    compositor: CompositorState,
    layer: LayerSurface,
    pointer: Option<wl_pointer::WlPointer>,
    gl: Option<Gl>,
    egui_ctx: egui::Context,
    quick: QuickModeApp,
    events: Vec<egui::Event>,
    pointer_pos: egui::Pos2,
    width: u32,
    height: u32,
    /// Whether the popup is visible. Starts hidden; a valid Ctrl+C shows it,
    /// Esc/✕ hides it (Esc lands in a later increment).
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
        // Ctrl+C is noticed. A valid item flips us visible.
        self.quick.pump(&self.egui_ctx);
        if self.quick.take_pop_request() {
            self.shown = true;
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
                        .rounding(8.0)
                        .inner_margin(egui::Margin::same(10.0))
                        .show(ui, |ui| {
                            ui.set_width(width - 20.0);
                            self.quick.content(ui);
                        });
                });
            measured = resp.response.rect.height();
        });

        // Auto-height: resize the surface to the measured content (one-frame
        // settle). The configure that follows resizes the GL surface.
        if shown && measured > 0.0 {
            let want = (measured.ceil() as u32).clamp(MIN_HEIGHT, MAX_HEIGHT);
            if want != self.height {
                self.layer.set_size(self.width, want);
                self.layer.commit();
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
}

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.draw(qh);
    }
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
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
            self.pointer = self.seat_state.get_pointer(qh, &seat).ok();
        }
        // Deliberately never grab the keyboard — POE2 must keep focus.
    }
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Pointer {
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
                    self.pointer_pos = pos;
                    self.events.push(egui::Event::PointerMoved(pos));
                }
                PointerEventKind::Leave { .. } => {
                    self.events.push(egui::Event::PointerGone);
                }
                PointerEventKind::Press { button, .. } => {
                    if let Some(b) = map_button(button) {
                        self.events.push(egui::Event::PointerButton {
                            pos,
                            button: b,
                            pressed: true,
                            modifiers: egui::Modifiers::default(),
                        });
                    }
                }
                PointerEventKind::Release { button, .. } => {
                    if let Some(b) = map_button(button) {
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

/// Linux evdev button codes → egui buttons.
fn map_button(code: u32) -> Option<egui::PointerButton> {
    match code {
        0x110 => Some(egui::PointerButton::Primary),   // BTN_LEFT
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
delegate_layer!(App);
delegate_registry!(App);
