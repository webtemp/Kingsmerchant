//! Phase 4 spike (throwaway). Goal: prove the riskiest unknown before any
//! rewrite — can we render egui onto a `wlr-layer-shell` *overlay* surface that
//! NEVER takes keyboard focus, while mouse input still works?
//!
//! Stack: smithay-client-toolkit (layer shell + seat/pointer) for the surface,
//! glutin (EGL) for the GL context — glutin builds the `wl_egl_window` itself
//! from our `wl_surface` pointer — and egui_glow as the painter.
//!
//! What to look for when it runs:
//!   * a small panel appears ~near the top-left, ABOVE everything (overlay layer);
//!   * the window/game underneath keeps keyboard focus — typing goes to it, the
//!     spike receives no key events (KeyboardInteractivity::None);
//!   * moving the mouse over the panel highlights the button; clicking it bumps
//!     the counter → egui is receiving pointer input;
//!   * clicking OUTSIDE the panel hits whatever is underneath (the surface is
//!     sized to the panel, so there is no "outside" to swallow).
//!
//! Run: `cargo run -p overlay-spike`. Close: click the ✕ button (no Esc here —
//! that needs keyboard focus we deliberately don't take; in the real overlay
//! Esc comes from the evdev watcher).

use std::num::NonZeroU32;
use std::ptr::NonNull;
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
    compositor::{CompositorHandler, CompositorState},
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

const WIDTH: u32 = 360;
const HEIGHT: u32 = 220;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let conn = Connection::connect_to_env().context("connect to Wayland (is this a WL session?)")?;
    let (globals, mut event_queue) = registry_queue_init(&conn).context("registry init")?;
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh).map_err(|e| anyhow!("wl_compositor: {e}"))?;
    let layer_shell = LayerShell::bind(&globals, &qh)
        .map_err(|e| anyhow!("wlr layer shell unavailable: {e}"))?;

    let surface = compositor.create_surface(&qh);
    let layer = layer_shell.create_layer_surface(
        &qh,
        surface,
        Layer::Overlay,
        Some("poe2ddd-spike"),
        None,
    );
    // The crux of Phase 4: never take keyboard focus → the game stays focused.
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
    // Anchor to a corner and offset with margins — that is how we will place the
    // popup at the cursor later (margins = cursor pos within the monitor).
    layer.set_anchor(Anchor::TOP | Anchor::LEFT);
    layer.set_margin(200, 0, 0, 200);
    layer.set_size(WIDTH, HEIGHT);
    // Initial commit with no buffer → compositor sends the first configure.
    layer.commit();

    let mut app = App {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        conn: conn.clone(),
        layer,
        pointer: None,
        gl: None,
        egui_ctx: egui::Context::default(),
        events: Vec::new(),
        pointer_pos: egui::pos2(-1.0, -1.0),
        width: WIDTH,
        height: HEIGHT,
        clicks: 0,
        exit: false,
    };

    tracing::info!("spike running — overlay should be visible; the game keeps focus");
    while !app.exit {
        event_queue.blocking_dispatch(&mut app).context("dispatch")?;
    }
    tracing::info!("spike exited cleanly");
    Ok(())
}

/// GL state, created lazily on the first configure (once we know we are mapped).
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
    layer: LayerSurface,
    pointer: Option<wl_pointer::WlPointer>,
    gl: Option<Gl>,
    egui_ctx: egui::Context,
    events: Vec<egui::Event>,
    pointer_pos: egui::Pos2,
    width: u32,
    height: u32,
    clicks: u32,
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
        let config = unsafe { display.find_configs(template) }
            .context("find_configs")?
            .reduce(|best, c| {
                if c.alpha_size() > best.alpha_size() {
                    c
                } else {
                    best
                }
            })
            .context("no EGL config")?;
        tracing::info!(alpha = config.alpha_size(), "picked EGL config");

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
        let gl = self.gl.as_mut().unwrap();

        let raw_input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(self.width as f32, self.height as f32),
            )),
            events: std::mem::take(&mut self.events),
            focused: false,
            ..Default::default()
        };

        let mut clicked_close = false;
        let clicks = self.clicks;
        let pos = self.pointer_pos;
        let full = self.egui_ctx.run(raw_input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.heading("poe2ddd overlay spike");
                ui.label("layer: overlay · keyboard focus: none");
                ui.separator();
                ui.label(format!("pointer (surface-local): {:.0}, {:.0}", pos.x, pos.y));
                ui.label(format!("primary clicks seen: {clicks}"));
                ui.add_space(8.0);
                // Proves egui hit-testing: only a click ON this button closes us.
                if ui.button("✕ close").clicked() {
                    clicked_close = true;
                }
            });
        });

        if clicked_close {
            self.exit = true;
        }

        let ppp = full.pixels_per_point;
        let primitives = self.egui_ctx.tessellate(full.shapes, ppp);
        let size = [self.width, self.height];

        gl.context
            .make_current(&gl.gl_surface)
            .expect("make_current in draw");
        gl.painter.clear(size, [0.0, 0.0, 0.0, 0.0]);
        gl.painter
            .paint_and_update_textures(size, ppp, &primitives, &full.textures_delta);
        gl.gl_surface
            .swap_buffers(&gl.context)
            .expect("swap_buffers");

        static PAINTED: std::sync::Once = std::sync::Once::new();
        PAINTED.call_once(|| tracing::info!("first egui frame painted + swapped (GL path works)"));

        // Schedule the next frame so hover/redraw stays live for the spike.
        let surface = self.layer.wl_surface().clone();
        surface.frame(qh, surface.clone());
        self.layer.commit();
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
        tracing::warn!("compositor closed the layer surface");
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
        // Deliberately do NOT grab the keyboard capability.
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
                        if b == egui::PointerButton::Primary {
                            self.clicks += 1;
                        }
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
                PointerEventKind::Axis { .. } => {}
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
