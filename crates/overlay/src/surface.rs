//! The EGL/egui/glutin rendering layer: one [`WinSurface`] per `wlr-layer-shell`
//! overlay surface, with its own GL context and `egui::Context`, plus the
//! per-frame [`Shared`] borrow and the rendering constants. [`App`] owns the two
//! surfaces and routes events to them.

use std::num::NonZeroU32;
use std::ptr::NonNull;
use std::sync::Arc;

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

/// Initial popup width before the content measurement grows it to
/// [`QuickModeApp::surface_width`]; the settings surface is a fixed width.
pub(crate) const POPUP_INIT_WIDTH: u32 = 470;
pub(crate) const SETTINGS_WIDTH: u32 = 540;
/// Starting height before the first content measurement.
const INITIAL_HEIGHT: u32 = 200;
/// Vertical space egui lays the content out in while we measure it. The surface
/// is then shrunk to the measured height (clamped to `MAX_HEIGHT`).
const LAYOUT_HEIGHT: f32 = 1600.0;
const MIN_HEIGHT: u32 = 80;
const MAX_HEIGHT: u32 = 1300;
/// Don't shrink for height drops smaller than this — a deadband so measurement
/// jitter doesn't thrash `set_size`.
const HEIGHT_DEADBAND: u32 = 8;
/// Corner radius of the popup card.
const CORNER_RADIUS: f32 = 14.0;
/// Popup backing: a solid (opaque) grey card. Only the rounded corners let the
/// game show through.
const OVERLAY_FILL: egui::Color32 = egui::Color32::from_rgb(0x2c, 0x2e, 0x36);
const OVERLAY_STROKE: egui::Color32 = egui::Color32::from_rgb(0x50, 0x52, 0x5e);

/// How many of a surface's first visible frames pre-warm the font atlas.
const WARMUP_FRAMES: u8 = 3;
/// Characters laid out invisibly during warm-up to bake them into the font atlas
/// up front: printable Latin-1 Supplement + Latin Extended-A. Covers the
/// accented letters in POE2 item/player names (ö, é, ü, …) that otherwise
/// rendered as boxes on first appearance.
const WARMUP_TEXT: &str = "\
\u{00A1}\u{00A2}\u{00A3}\u{00A4}\u{00A5}\u{00A6}\u{00A7}\u{00A8}\u{00A9}\u{00AA}\u{00AB}\u{00AC}\u{00AD}\u{00AE}\u{00AF}\u{00B0}\u{00B1}\u{00B2}\u{00B3}\u{00B4}\u{00B5}\u{00B6}\u{00B7}\u{00B8}\u{00B9}\u{00BA}\u{00BB}\u{00BC}\u{00BD}\u{00BE}\u{00BF}\
ÀÁÂÃÄÅÆÇÈÉÊËÌÍÎÏÐÑÒÓÔÕÖ×ØÙÚÛÜÝÞßàáâãäåæçèéêëìíîïðñòóôõö÷øùúûüýþÿ\
ĀāĂăĄąĆćĈĉĊċČčĎďĐđĒēĔĕĖėĘęĚěĜĝĞğĠġĢģĤĥĦħĨĩĪīĬĭĮįİıĲĳĴĵĶķĸĹĺĻļĽľĿŀŁłŃńŅņŇňŉŊŋŌōŎŏŐőŒœŔŕŖŗŘřŚśŜŝŞşŠšŢţŤťŦŧŨũŪūŬŭŮůŰűŲųŴŵŶŷŸŹźŻżŽž";

/// Where to place a surface on its output.
#[derive(Clone, Copy)]
pub(crate) enum Placement {
    /// Centered on the output (default; also the `at-cursor` fallback for now).
    Center,
    /// Fixed top-left position in output-logical pixels.
    Fixed { x: i32, y: i32 },
}

/// GL state for one surface, created lazily on the first configure (once
/// mapped). The EGL [`Display`] is shared and lives on [`App`].
pub(crate) struct Gl {
    pub(crate) context: PossiblyCurrentContext,
    pub(crate) gl_surface: Surface<WindowSurface>,
    painter: egui_glow::Painter,
}

/// Shared, per-frame resources a surface needs to draw, borrowed from [`App`]
/// (so the two surfaces can be drawn without aliasing the whole `App`).
pub(crate) struct Shared<'a> {
    pub(crate) conn: &'a Connection,
    pub(crate) compositor: &'a CompositorState,
    pub(crate) output_state: &'a OutputState,
    pub(crate) display: &'a mut Option<Display>,
    pub(crate) kbd_modifiers: egui::Modifiers,
}

/// One `wlr-layer-shell` overlay surface (the popup or the settings panel) with
/// its own GL context and egui context.
pub(crate) struct WinSurface {
    pub(crate) layer: LayerSurface,
    pub(crate) egui_ctx: egui::Context,
    pub(crate) gl: Option<Gl>,
    pub(crate) events: Vec<egui::Event>,
    /// Whether this surface currently holds keyboard focus.
    pub(crate) kbd_focus: bool,
    pub(crate) width: u32,
    pub(crate) height: u32,
    desired_width: u32,
    desired_height: u32,
    /// The output the surface is on (from `surface_enter`), used to center it.
    pub(crate) current_output: Option<wl_output::WlOutput>,
    pub(crate) margin_left: i32,
    pub(crate) margin_top: i32,
    /// Whether the user has Alt-dragged this show (suppresses re-placement).
    pub(crate) dragged: bool,
    /// Whether an Alt drag is in progress (left button held).
    pub(crate) dragging: bool,
    /// Whether the surface is visible.
    pub(crate) shown: bool,
    /// Whether this surface is currently in its redraw loop (requested the next
    /// frame callback on its last draw). Used to know when a surface that should
    /// be redrawing has gone quiet and needs a kick (see `App::tick`).
    pub(crate) spinning: bool,
    /// Kept alive until the next commit so the compositor reads the region
    /// before it's destroyed.
    input_region: Option<Region>,
    /// Last applied (shown, width, height) so we only touch the input region on
    /// change.
    applied_input: Option<(bool, u32, u32)>,
    /// Last applied keyboard-interactivity `shown` state (toggle on change).
    applied_kbd: Option<bool>,
    /// Remaining frames to pre-warm the font atlas (see [`WARMUP_FRAMES`]). egui
    /// adds glyphs lazily, and in this custom egui_glow integration glyphs
    /// uploaded after the initial atlas rendered as tofu boxes; warming the
    /// accented-Latin range up front avoids that.
    warm_frames: u8,
}

impl WinSurface {
    /// Create a hidden layer surface (no keyboard focus, empty input region →
    /// click-through), anchored top-left and committed so it maps.
    pub(crate) fn new(
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
        // Keyboard focus is taken on-demand while shown; None while hidden so
        // POE2 keeps the keyboard.
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        // Anchor top-left; we center via computed margins (KWin doesn't reliably
        // center an unanchored surface).
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
            .min_by_key(glutin::config::GlConfig::num_samples)
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
    pub(crate) fn draw(
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

        // Lay the content out in a tall space to measure its natural height,
        // then shrink the surface to fit.
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
        // `ctx.run` wants an `FnMut` but `render` is `FnOnce`; `Option::take`
        // hands it out at most once.
        let mut render = Some(render);
        let full = ctx.run(raw_input, |c| {
            if !shown {
                return;
            }
            // Pre-warm the font atlas: lay the accented-Latin range out in a
            // throwaway, transparent, non-interactable Area so the glyphs enter
            // this frame's atlas upload without affecting layout or measurement.
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

        // Auto-height: resize the surface to the measured content — not while
        // dragging, with a deadband on shrink. Every `set_size` triggers a
        // configure → draw → maybe set_size again, so a 1px jitter must not fire
        // it (pegs a core and lags the drag).
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

        // Placement every visible frame (incl. during a drag, so it tracks the
        // cursor): follow a drag, else apply the configured place.
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

        let gl = self.gl.as_mut().unwrap();
        gl.context
            .make_current(&gl.gl_surface)
            .context("make_current in draw")?;
        gl.painter.clear(size, [0.0, 0.0, 0.0, 0.0]);
        gl.painter
            .paint_and_update_textures(size, ppp, &primitives, &full.textures_delta);
        gl.gl_surface
            .swap_buffers(&gl.context)
            .context("swap_buffers")?;

        // Schedule the next frame only if this surface should keep redrawing
        // (`App::should_spin`). Exactly one surface spins at a time so the two
        // never compete for the vsync swap. A surface going quiet still presents
        // this frame; `App::tick` kicks the other back into its loop on a switch.
        self.spinning = request_next;
        if request_next {
            let surface = self.layer.wl_surface();
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
            self.input_region = Some(region);
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
        self.layer
            .set_margin(self.margin_top, 0, 0, self.margin_left);
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
