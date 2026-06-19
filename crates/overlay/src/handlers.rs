//! The Wayland trait impls for [`App`] (compositor / layer-shell / seat /
//! pointer / relative-pointer / keyboard / output / registry) plus the
//! `delegate_*!` and `registry_handlers!` macro wiring that routes events into
//! them. Each event is dispatched to whichever surface owns it.

use std::num::NonZeroU32;

use glutin::surface::GlSurface;
use smithay_client_toolkit::{
    compositor::CompositorHandler,
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer,
    delegate_registry, delegate_relative_pointer, delegate_seat,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers as SctkModifiers},
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
        relative_pointer::{RelativeMotionEvent, RelativePointerHandler},
        Capability, SeatHandler, SeatState,
    },
    shell::{
        wlr_layer::{LayerShellHandler, LayerSurface, LayerSurfaceConfigure},
        WaylandSurface,
    },
};
use wayland_client::{
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_surface},
    Connection, QueueHandle,
};
use wayland_protocols::wp::relative_pointer::zv1::client::zwp_relative_pointer_v1;

use crate::input_map::{format_binding, keysym_to_binding_key, map_button, map_keysym, BTN_LEFT};
use crate::{App, Which};

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: i32,
    ) {
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        if let Some(which) = self.which(surface) {
            self.render(which, qh);
        }
    }
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        output: &wl_output::WlOutput,
    ) {
        match self.which(surface) {
            Some(Which::Popup) => self.popup.current_output = Some(output.clone()),
            Some(Which::Settings) => self.settings.current_output = Some(output.clone()),
            None => {}
        }
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
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
                // `width`/`height` are seeded non-zero in `WinSurface::new` and a
                // zero `new_size` is ignored above, so both stay non-zero.
                gl.gl_surface.resize(
                    &gl.context,
                    NonZeroU32::new(surf.width).expect("surface width is non-zero"),
                    NonZeroU32::new(surf.height).expect("surface height is non-zero"),
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
    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
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
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
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
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        // Where the popup ended an Alt-drag this frame — persisted as the fixed
        // position after the loop (avoids borrowing `self.quick` while a surface
        // is borrowed).
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
                        surf.dragging = false;
                        // Remember where the popup was dropped → fixed position.
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
        // The popup auto-closes when focus leaves it (user clicked back into
        // POE2), so no Esc needed. Settings stays open — close it via its X /
        // the tray.
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
        // Click-to-record hotkey capture (Settings): grab the whole combo for the
        // row being recorded instead of routing the key into egui. Esc cancels;
        // modifier-only presses (Ctrl/Alt/Shift) yield no key name, so we keep
        // listening until a real key lands.
        if self.quick.is_recording_hotkey() {
            if event.keysym == Keysym::Escape {
                self.quick.cancel_hotkey_recording();
            } else if let Some(key) = keysym_to_binding_key(event.keysym) {
                let binding =
                    format_binding(modifiers.ctrl, modifiers.alt, modifiers.shift, &key);
                self.quick.commit_hotkey(binding);
            }
            return;
        }
        let Some(which) = self.focused else {
            return;
        };
        // Ctrl+V: egui never reads the system clipboard itself — we must turn
        // the shortcut into an `Event::Paste` so text fields can be pasted into.
        // Use the Wayland-first paste read (not the X11 POE2-item read), so
        // pasting e.g. a POESESSID copied from a browser doesn't yield the last
        // item left in POE2's X11 clipboard.
        if modifiers.ctrl
            && !modifiers.alt
            && (event.keysym == Keysym::v || event.keysym == Keysym::V)
        {
            match platform_linux::read_paste_text() {
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
        // Printable text — not while Ctrl/Alt are held (shortcuts) and not
        // control chars (Backspace etc. also arrive as utf8).
        if !modifiers.ctrl && !modifiers.alt {
            if let Some(text) = event.utf8 {
                if !text.is_empty() && !text.chars().any(char::is_control) {
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
