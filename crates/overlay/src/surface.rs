//! The EGL/egui/glutin rendering layer: one [`WinSurface`] per overlay surface.

use std::num::NonZeroU32;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use anyhow::{anyhow, Context as _, Result};
use glutin::config::{ConfigTemplateBuilder, GlConfig};
use glutin::context::{
    ContextAttributesBuilder, NotCurrentGlContext, PossiblyCurrentContext, PossiblyCurrentGlContext,
};
use glutin::display::{Display, DisplayApiPreference, GlDisplay};
use glutin::surface::{GlSurface, Surface, SurfaceAttributesBuilder, WindowSurface};
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};
use smithay_client_toolkit::{
    compositor::{CompositorState, Region},
    output::OutputState,
    shell::{
        wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell, LayerSurface},
        WaylandSurface,
    },
};
use wayland_client::{protocol::wl_output, Connection, Proxy, QueueHandle};

use crate::App;

/// Initial popup width before content measurement grows it; settings is fixed.
pub(crate) const POPUP_INIT_WIDTH: u32 = 470;
pub(crate) const SETTINGS_WIDTH: u32 = 540;
const INITIAL_HEIGHT: u32 = 200;
/// Vertical space egui lays content out in while measuring; then shrunk to fit.
const LAYOUT_HEIGHT: f32 = 1600.0;
const MIN_HEIGHT: u32 = 80;
const MAX_HEIGHT: u32 = 1300;
/// Deadband so measurement jitter doesn't thrash `set_size`.
const HEIGHT_DEADBAND: u32 = 8;
const CORNER_RADIUS: f32 = 14.0;

struct Perf {
    frames: u32,
    shown_frames: u32,
    max_ms: f32,
    setsize: u32,
    since: Instant,
}

/// Whether to emit the per-second `perf` log; default off.
static PERF_ENABLED: AtomicBool = AtomicBool::new(false);

pub(crate) fn set_perf_metrics_enabled(on: bool) {
    PERF_ENABLED.store(on, Ordering::Relaxed);
}

fn perf_enabled() -> bool {
    PERF_ENABLED.load(Ordering::Relaxed)
}

fn perf() -> &'static std::sync::Mutex<Perf> {
    static P: OnceLock<std::sync::Mutex<Perf>> = OnceLock::new();
    P.get_or_init(|| {
        std::sync::Mutex::new(Perf {
            frames: 0,
            shown_frames: 0,
            max_ms: 0.0,
            setsize: 0,
            since: Instant::now(),
        })
    })
}

fn perf_note_setsize() {
    if !perf_enabled() {
        return;
    }
    if let Ok(mut p) = perf().lock() {
        p.setsize += 1;
    }
}

fn perf_note_frame(shown: bool, ms: f32) {
    if !perf_enabled() {
        return;
    }
    let Ok(mut p) = perf().lock() else { return };
    p.frames += 1;
    if shown {
        p.shown_frames += 1;
    }
    p.max_ms = p.max_ms.max(ms);
    if p.since.elapsed() >= std::time::Duration::from_secs(1) {
        tracing::info!(
            target: "perf",
            fps = p.frames,
            shown_fps = p.shown_frames,
            max_frame_ms = format!("{:.1}", p.max_ms),
            set_size = p.setsize,
            "PERF"
        );
        *p = Perf {
            frames: 0,
            shown_frames: 0,
            max_ms: 0.0,
            setsize: 0,
            since: Instant::now(),
        };
    }
}

/// Seconds since process start, fed to egui as `RawInput::time` for click
/// interval timing.
fn elapsed_seconds() -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

/// How many of a surface's first visible frames pre-warm the font atlas.
const WARMUP_FRAMES: u8 = 3;
/// Accented-Latin range baked into the font atlas up front to avoid tofu boxes.
const WARMUP_TEXT: &str = "\
\u{00A1}\u{00A2}\u{00A3}\u{00A4}\u{00A5}\u{00A6}\u{00A7}\u{00A8}\u{00A9}\u{00AA}\u{00AB}\u{00AC}\u{00AD}\u{00AE}\u{00AF}\u{00B0}\u{00B1}\u{00B2}\u{00B3}\u{00B4}\u{00B5}\u{00B6}\u{00B7}\u{00B8}\u{00B9}\u{00BA}\u{00BB}\u{00BC}\u{00BD}\u{00BE}\u{00BF}\
ÀÁÂÃÄÅÆÇÈÉÊËÌÍÎÏÐÑÒÓÔÕÖ×ØÙÚÛÜÝÞßàáâãäåæçèéêëìíîïðñòóôõö÷øùúûüýþÿ\
ĀāĂăĄąĆćĈĉĊċČčĎďĐđĒēĔĕĖėĘęĚěĜĝĞğĠġĢģĤĥĦħĨĩĪīĬĭĮįİıĲĳĴĵĶķĸĹĺĻļĽľĿŀŁłŃńŅņŇňŉŊŋŌōŎŏŐőŒœŔŕŖŗŘřŚśŜŝŞşŠšŢţŤťŦŧŨũŪūŬŭŮůŰűŲųŴŵŶŷŸŹźŻżŽž";

/// Where to place a surface on its output.
#[derive(Clone, Copy)]
pub(crate) enum Placement {
    Center,
    /// Fixed top-left position in output-logical pixels.
    Fixed {
        x: i32,
        y: i32,
    },
}

/// GL state for one surface, created lazily on the first configure.
pub(crate) struct Gl {
    pub(crate) context: PossiblyCurrentContext,
    pub(crate) gl_surface: Surface<WindowSurface>,
    painter: egui_glow::Painter,
}

impl Drop for Gl {
    /// Free the painter's GL objects; context must be current first. Best-effort.
    fn drop(&mut self) {
        if self.context.make_current(&self.gl_surface).is_ok() {
            self.painter.destroy();
        }
    }
}

/// Shared, per-frame resources a surface needs to draw, borrowed from [`App`].
pub(crate) struct Shared<'a> {
    pub(crate) conn: &'a Connection,
    pub(crate) compositor: &'a CompositorState,
    pub(crate) output_state: &'a OutputState,
    pub(crate) display: &'a mut Option<Display>,
    pub(crate) kbd_modifiers: egui::Modifiers,
}

/// One `wlr-layer-shell` overlay surface (popup or settings panel) with its own
/// GL context and egui context.
pub(crate) struct WinSurface {
    /// Short identity for logs ("popup" / "settings").
    label: &'static str,
    /// Layer-shell namespace, kept so the surface can be rebuilt on a pinned output.
    namespace: &'static str,
    pub(crate) layer: LayerSurface,
    pub(crate) egui_ctx: egui::Context,
    pub(crate) gl: Option<Gl>,
    pub(crate) events: Vec<egui::Event>,
    pub(crate) kbd_focus: bool,
    pub(crate) width: u32,
    pub(crate) height: u32,
    desired_width: u32,
    desired_height: u32,
    /// The output the surface is on (from `surface_enter`), used to center it.
    pub(crate) current_output: Option<wl_output::WlOutput>,
    pub(crate) margin_left: i32,
    pub(crate) margin_top: i32,
    /// User has Alt-dragged this show (suppresses re-placement).
    pub(crate) dragged: bool,
    /// An Alt drag is in progress (left button held).
    pub(crate) dragging: bool,
    pub(crate) shown: bool,
    /// Currently in its redraw loop; goes quiet → needs a kick (see `App::tick`).
    pub(crate) spinning: bool,
    /// Kept alive until the next commit so the compositor reads the region.
    input_region: Option<Region>,
    /// Last applied (shown, width, height) to skip no-op input region updates.
    applied_input: Option<(bool, u32, u32)>,
    applied_kbd: Option<bool>,
    /// Remaining frames to pre-warm the font atlas (see [`WARMUP_FRAMES`]).
    warm_frames: u8,
    /// Last frame's measured target height; resize only once it has settled.
    last_want_h: u32,
}

impl WinSurface {
    /// Create a hidden, click-through layer surface, anchored top-left.
    pub(crate) fn new(
        compositor: &CompositorState,
        layer_shell: &LayerShell,
        qh: &QueueHandle<App>,
        egui_ctx: egui::Context,
        namespace: &'static str,
        label: &'static str,
        width: u32,
    ) -> Self {
        let surface = compositor.create_surface(qh);
        let layer =
            layer_shell.create_layer_surface(qh, surface, Layer::Overlay, Some(namespace), None);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        // Anchor top-left; we center via computed margins.
        layer.set_anchor(Anchor::TOP | Anchor::LEFT);
        layer.set_size(width, INITIAL_HEIGHT);
        layer.commit();
        WinSurface {
            label,
            namespace,
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
            last_want_h: INITIAL_HEIGHT,
        }
    }

    /// Rebuild the surface pinned to `output`, so the compositor can't drift it to
    /// another monitor (a layer surface's output is fixed at creation). Must run
    /// before any draw — the `debug_assert` enforces `gl` is unset, so it never
    /// tears a live painter from egui's texture state (a `GL_INVALID_OPERATION`).
    pub(crate) fn pin_to_output(
        &mut self,
        compositor: &CompositorState,
        layer_shell: &LayerShell,
        qh: &QueueHandle<App>,
        output: Option<&wl_output::WlOutput>,
    ) {
        debug_assert!(self.gl.is_none(), "pin_to_output must run before GL init");

        let surface = compositor.create_surface(qh);
        let layer = layer_shell.create_layer_surface(
            qh,
            surface,
            Layer::Overlay,
            Some(self.namespace),
            output,
        );
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_anchor(Anchor::TOP | Anchor::LEFT);
        layer.set_size(self.desired_width, self.desired_height);
        layer.commit();

        self.layer = layer;
        // Seed the centring output so the first frame lands right, before `surface_enter`.
        self.current_output = output.cloned();
        // Fresh surface: force cached applied-state to re-apply, re-warm the atlas.
        self.applied_input = None;
        self.applied_kbd = None;
        self.input_region = None;
        self.warm_frames = WARMUP_FRAMES;
    }

    /// Build the EGL context + egui painter, creating the shared display on
    /// first use.
    fn init_gl(&mut self, shared: &mut Shared) -> Result<()> {
        let surface_ptr = self
            .layer
            .wl_surface()
            .id()
            .as_ptr()
            .cast::<std::ffi::c_void>();
        let raw_window = RawWindowHandle::Wayland(WaylandWindowHandle::new(
            NonNull::new(surface_ptr).context("null wl_surface ptr")?,
        ));

        if shared.display.is_none() {
            let display_ptr = shared
                .conn
                .backend()
                .display_ptr()
                .cast::<std::ffi::c_void>();
            let raw_display = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
                NonNull::new(display_ptr).context("null wl_display ptr")?,
            ));
            // SAFETY: `raw_display` wraps the live `wl_display` from `App`'s
            // `Connection`, which outlives every GL object derived from it.
            let display = unsafe { Display::new(raw_display, DisplayApiPreference::Egl) }
                .context("create EGL display")?;
            *shared.display = Some(display);
        }
        let display = shared
            .display
            .as_ref()
            .expect("EGL display populated above");

        let template = ConfigTemplateBuilder::new()
            .compatible_with_native_window(raw_window)
            .with_alpha_size(8)
            .build();
        // SAFETY: `display` is the valid EGL display from above; `template`
        // carries only the live `wl_surface` handle for this surface.
        let config = unsafe { display.find_configs(template) }
            .context("find_configs")?
            .filter(|c| c.alpha_size() == 8)
            .min_by_key(glutin::config::GlConfig::num_samples)
            .context("no RGBA8 EGL config")?;

        let context_attrs = ContextAttributesBuilder::new().build(Some(raw_window));
        // SAFETY: `config` came from this `display`; `raw_window` is the live
        // `wl_surface`.
        let not_current = unsafe { display.create_context(&config, &context_attrs) }
            .context("create GL context")?;

        let surf_attrs = SurfaceAttributesBuilder::<WindowSurface>::new().build(
            raw_window,
            NonZeroU32::new(self.width).expect("surface width is non-zero"),
            NonZeroU32::new(self.height).expect("surface height is non-zero"),
        );
        // SAFETY: `config` belongs to `display`; `surf_attrs` carries the live
        // `raw_window` plus non-zero width/height.
        let gl_surface = unsafe { display.create_window_surface(&config, &surf_attrs) }
            .context("create window surface")?;

        let context = not_current
            .make_current(&gl_surface)
            .context("make_current")?;

        // SAFETY: the context is current, so `get_proc_address` resolves valid
        // GL function pointers; glow only invokes the loader during construction.
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
        tracing::info!(surface = self.label, "overlay GL surface ready");
        Ok(())
    }

    /// Lay out, size, place, and paint this surface for one frame.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw(
        &mut self,
        shared: &mut Shared,
        qh: &QueueHandle<App>,
        want_width: u32,
        place: Placement,
        request_next: bool,
        fill: egui::Color32,
        stroke: egui::Color32,
        render: impl FnOnce(&mut egui::Ui),
    ) -> Result<()> {
        let perf_t0 = Instant::now();
        if self.gl.is_none() {
            self.init_gl(shared)?;
        }

        self.apply_keyboard_interactivity();
        if !self.shown {
            self.dragged = false;
            self.dragging = false;
        }

        // Lay content out in a tall space to measure natural height, then shrink.
        let raw_input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(self.width as f32, LAYOUT_HEIGHT),
            )),
            events: std::mem::take(&mut self.events),
            focused: self.kbd_focus,
            modifiers: shared.kbd_modifiers,
            // Real wall-clock time, else double/triple-click never registers.
            time: Some(elapsed_seconds()),
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
        // `ctx.run` wants `FnMut` but `render` is `FnOnce`; `take` hands it out once.
        let mut render = Some(render);
        let full = ctx.run(raw_input, |c| {
            if !shown {
                return;
            }
            // Pre-warm the font atlas in a throwaway transparent Area.
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
                        .fill(fill)
                        .stroke(egui::Stroke::new(1.0, stroke))
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

        // Bridge egui's copy/cut out to the system clipboard.
        if !full.platform_output.copied_text.is_empty() {
            if let Err(e) = platform::write_clipboard_text(&full.platform_output.copied_text) {
                tracing::warn!(error = %e, "clipboard write (copy/cut) failed");
            }
        }

        // Auto-height: resize to measured content (not while dragging, with a
        // shrink deadband), and only once the height has settled across two
        // frames so reflow wobble can't drive a configure↔set_size feedback loop.
        if shown && measured > 0.0 && !self.dragging {
            let want_h = (measured.ceil() as u32).clamp(MIN_HEIGHT, MAX_HEIGHT);
            let settled = want_h == self.last_want_h;
            self.last_want_h = want_h;
            let height_changed = want_h.abs_diff(self.desired_height) > HEIGHT_DEADBAND;
            if want_width != self.desired_width || (settled && height_changed) {
                self.desired_height = want_h;
                self.desired_width = want_width;
                self.layer.set_size(want_width, want_h);
                perf_note_setsize();
            }
        }

        // Placement every visible frame: follow a drag, else apply the place.
        if shown {
            if self.dragged {
                self.apply_margin();
            } else {
                match place {
                    Placement::Center => {
                        self.center(shared, self.desired_width, self.desired_height);
                    }
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

        let gl = self.gl.as_mut().expect("gl initialised at the top of draw");
        gl.context
            .make_current(&gl.gl_surface)
            .context("make_current in draw")?;
        gl.painter.clear(size, [0.0, 0.0, 0.0, 0.0]);
        gl.painter
            .paint_and_update_textures(size, ppp, &primitives, &full.textures_delta);
        gl.gl_surface
            .swap_buffers(&gl.context)
            .context("swap_buffers")?;

        // Schedule the next frame only if this surface should keep redrawing.
        self.spinning = request_next;
        if request_next {
            let surface = self.layer.wl_surface();
            surface.frame(qh, surface.clone());
        }
        self.layer.commit();
        perf_note_frame(shown, perf_t0.elapsed().as_secs_f32() * 1000.0);
        Ok(())
    }

    /// Take keyboard focus on-demand while shown, drop it when hidden.
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

    /// Input region = surface bounds when visible, empty when hidden (clicks
    /// pass through).
    fn apply_input_region(&mut self, shared: &Shared) {
        let state = (self.shown, self.width, self.height);
        if self.applied_input == Some(state) {
            return;
        }
        if let Ok(region) = Region::new(shared.compositor) {
            if self.shown {
                region.add(0, 0, self.width as i32, self.height as i32);
            }
            self.layer.set_input_region(Some(region.wl_region()));
            self.input_region = Some(region);
            self.applied_input = Some(state);
        }
    }

    /// Center a `w`×`h` surface on its output via top/left margins.
    fn center(&mut self, shared: &Shared, w: u32, h: u32) {
        let (ow, oh) = self.output_size(shared);
        self.margin_left = ((ow - w as i32) / 2).max(0);
        self.margin_top = ((oh - h as i32) / 2).max(0);
        self.apply_margin();
    }

    fn apply_margin(&self) {
        self.layer
            .set_margin(self.margin_top, 0, 0, self.margin_left);
    }

    /// Logical size of the output the surface is on (falls back to 1080p).
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
