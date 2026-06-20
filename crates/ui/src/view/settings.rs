//! The settings surface. Edits write straight to `config` and persist on
//! change; startup-only fields (hotkeys, realm, focus gate) are flagged
//! "restart to apply" rather than pretending to take effect live.

use std::time::Instant;

use egui::{Color32, RichText};
use egui_phosphor::regular as ph;

use crate::config::Config;
use crate::model::SessionCheck;
use crate::{HotkeySlot, QuickModeApp, POESESSID_DEBOUNCE};

use super::theme::online_dot;

/// Trade listing-status options for the settings dropdown (config id, label).
const TRADE_STATUSES: &[(&str, &str)] = &[
    ("securable", "Instant Buyout"),
    ("online", "Online (In Person)"),
    ("available", "Online + Buyout"),
    ("any", "Any"),
];

/// Popup position modes for the settings dropdown (config id, label).
const POSITION_MODES: &[(&str, &str)] = &[("center", "Center"), ("fixed", "Fixed")];

/// Log-verbosity options for the settings dropdown (config id, label). `auto` is
/// `error` in release and `debug` in dev; the rest map straight to tracing levels.
const LOG_LEVELS: &[(&str, &str)] = &[
    ("auto", "Auto"),
    ("off", "Off"),
    ("error", "Error"),
    ("warn", "Warn"),
    ("info", "Info"),
    ("debug", "Debug"),
    ("trace", "Trace"),
];

/// Shared width for the right-edge button-like controls (the dropdowns and the
/// hotkey-record buttons) so they form one consistent column. The theme-preset
/// row is intentionally excluded — it's a multi-button row, not a single control.
const CONTROL_WIDTH: f32 = 140.0;

impl QuickModeApp {
    /// Render the settings surface body. Call [`pump`](Self::pump) first
    /// (shared with the popup surface).
    pub fn settings_content(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        // Same palette install as the popup, so any themed widgets here match.
        super::theme::set_active(self.theme);
        // Esc closes the settings panel when it has focus (it gets the key event
        // via Wayland; the popup's Esc is handled globally by the evdev watcher).
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.settings_close_requested = true;
        }
        // What kind of follow-up an edit needs.
        let mut changed = false; // any field → persist to disk
        let mut requery = false; // league / status → re-price now
        let mut reseed = false; // min-roll % / implicit default → re-seed + re-price
        let mut restart = false; // a startup-only field → show the restart note
        let mut retheme = false; // a colour/opacity edit → rebuild the live palette

        ui.horizontal(|ui| {
            ui.label(RichText::new("Settings").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("X").on_hover_text("Close (Esc)").clicked() {
                    self.settings_close_requested = true;
                }
            });
        });
        ui.label(
            RichText::new("Changes save automatically — no save button.")
                .weak()
                .small(),
        );
        ui.separator();

        egui::ScrollArea::vertical()
            .max_height(560.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                // egui's default scrollbar is *floating* — it allocates no space
                // and paints on top of content, so the right-edge controls
                // (combos/sliders, laid out right-to-left) would sit under it and
                // get clipped. Reserve the bar's width on the right so they clear
                // it. `bar_width` is the fully-expanded (hover/scroll) width.
                let bar = ui.spacing().scroll.bar_width;
                ui.set_max_width((ui.available_width() - bar).max(0.0));

                // League (live — the client switches without a rebuild).
                setting_row(ui, "League", |ui| {
                    if self.leagues.is_empty() {
                        ui.label(RichText::new(&self.config.league).weak());
                    } else {
                        let before = self.config.league.clone();
                        egui::ComboBox::from_id_salt("settings-league")
                            .width(CONTROL_WIDTH)
                            .selected_text(&self.config.league)
                            .show_ui(ui, |ui| {
                                for lg in &self.leagues {
                                    ui.selectable_value(
                                        &mut self.config.league,
                                        lg.id.clone(),
                                        &lg.text,
                                    );
                                }
                            });
                        if self.config.league != before {
                            // An explicit pick pins the league (stops auto-resolve).
                            self.config.league_pinned = true;
                            self.client.set_league(self.config.league.clone());
                            changed = true;
                            requery = true;
                        }
                    }
                });

                // Realm (read into the request URL at startup — restart-only).
                // The "(restart)" hint sits next to the label, not out by the
                // combo, so it doesn't break the right-edge control column.
                ui.horizontal(|ui| {
                    ui.label("Realm");
                    ui.label(RichText::new("(restart)").weak().small());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let current = self.config.realm.clone().unwrap_or_else(|| "pc".into());
                        let mut chosen = current.clone();
                        egui::ComboBox::from_id_salt("settings-realm")
                            .width(CONTROL_WIDTH)
                            .selected_text(&current)
                            .show_ui(ui, |ui| {
                                for r in ["pc", "sony", "xbox"] {
                                    ui.selectable_value(&mut chosen, r.to_string(), r);
                                }
                            });
                        if chosen != current {
                            self.config.realm = if chosen == "pc" { None } else { Some(chosen) };
                            changed = true;
                            restart = true;
                        }
                    });
                });

                // Listing type / trade status (live — read per query).
                setting_row(ui, "Listings", |ui| {
                    let before = self.config.trade_status.clone();
                    egui::ComboBox::from_id_salt("settings-status")
                        .width(CONTROL_WIDTH)
                        .selected_text(trade_status_label(&self.config.trade_status))
                        .show_ui(ui, |ui| {
                            for (id, label) in TRADE_STATUSES {
                                ui.selectable_value(
                                    &mut self.config.trade_status,
                                    id.to_string(),
                                    *label,
                                );
                            }
                        });
                    if self.config.trade_status != before {
                        changed = true;
                        requery = true;
                    }
                });

                // POESESSID session cookie (live — unlocks the Instant Buyout
                // "Teleport to hideout" button, which needs an authenticated
                // fetch for each listing's teleport token). A wide, multi-control
                // row, so it keeps its own left-label / controls-after layout.
                ui.horizontal(|ui| {
                    ui.label("POESESSID");
                    let mut sid = self.config.poesessid.clone().unwrap_or_default();
                    // Masked by default (account access); the eye toggle reveals it.
                    let show_id = egui::Id::new("show-poesessid");
                    let mut show = ui.data_mut(|d| d.get_temp::<bool>(show_id).unwrap_or(false));
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut sid)
                            .password(!show)
                            .hint_text("trade-site session cookie")
                            .desired_width(220.0),
                    );
                    let mut edited = resp.changed();
                    let eye = if show { ph::EYE_SLASH } else { ph::EYE };
                    if ui
                        .button(eye)
                        .on_hover_text(if show { "Hide" } else { "Show" })
                        .clicked()
                    {
                        show = !show;
                        ui.data_mut(|d| d.insert_temp(show_id, show));
                    }
                    // One-click Copy / Paste on the whole value — no select-then-
                    // right-click needed (opening a menu defocuses the field and
                    // clears the visible selection anyway).
                    if ui
                        .button(ph::COPY)
                        .on_hover_text("Copy to clipboard")
                        .clicked()
                    {
                        let _ = platform_linux::write_clipboard_text(&sid);
                    }
                    if ui
                        .button(ph::CLIPBOARD)
                        .on_hover_text("Paste from clipboard")
                        .clicked()
                    {
                        if let Ok(Some(text)) = platform_linux::read_paste_text() {
                            sid = text.trim().to_string();
                            edited = true;
                        }
                    }
                    // egui 0.29 has no built-in TextEdit context menu, so also
                    // provide one. The actions operate on the whole value — it's a
                    // single short cookie — and read/write the real OS clipboard.
                    resp.context_menu(|ui| {
                        if ui.button("Copy").clicked() {
                            let _ = platform_linux::write_clipboard_text(&sid);
                            ui.close_menu();
                        }
                        if ui.button("Cut").clicked() {
                            let _ = platform_linux::write_clipboard_text(&sid);
                            sid.clear();
                            edited = true;
                            ui.close_menu();
                        }
                        if ui.button("Paste").clicked() {
                            if let Ok(Some(text)) = platform_linux::read_paste_text() {
                                sid = text.trim().to_string();
                                edited = true;
                            }
                            ui.close_menu();
                        }
                    });
                    if edited {
                        let trimmed = sid.trim().to_string();
                        self.config.poesessid = (!trimmed.is_empty()).then(|| trimmed.clone());
                        // Push live so the next search authenticates immediately.
                        // `set_poesessid` drops a malformed value, so this can't
                        // brick requests even mid-edit.
                        self.client.set_poesessid(self.config.poesessid.clone());
                        changed = true;
                        // Instant format feedback; a well-formed value also
                        // schedules a debounced live validation.
                        match trade_api::poesessid_format(&trimmed) {
                            trade_api::SessionIdFormat::Empty => {
                                self.session_status = SessionCheck::Idle;
                                self.session_check_at = None;
                            }
                            trade_api::SessionIdFormat::Malformed => {
                                self.session_status = SessionCheck::Malformed;
                                self.session_check_at = None;
                            }
                            trade_api::SessionIdFormat::WellFormed => {
                                self.session_status = SessionCheck::Checking;
                                self.session_check_at = Some(Instant::now());
                            }
                        }
                    }
                    session_status_label(ui, &self.session_status, self.config.poesessid.is_some());
                });
                ui.label(
                    RichText::new(
                        "Optional — only the Instant Buyout Teleport button needs it. \
                         Browser DevTools → Application → Cookies → pathofexile.com → \
                         POESESSID. Sent only to pathofexile.com; treat it like a password.",
                    )
                    .weak()
                    .small(),
                );

                ui.separator();

                // Position mode + fixed coordinates (live — the overlay reads
                // these every frame to place the popup).
                setting_row(ui, "Popup position", |ui| {
                    let before = self.config.position_mode.clone();
                    egui::ComboBox::from_id_salt("settings-position")
                        .width(CONTROL_WIDTH)
                        .selected_text(position_label(&self.config.position_mode))
                        .show_ui(ui, |ui| {
                            for (id, label) in POSITION_MODES {
                                ui.selectable_value(
                                    &mut self.config.position_mode,
                                    id.to_string(),
                                    *label,
                                );
                            }
                        });
                    if self.config.position_mode != before {
                        changed = true;
                    }
                });
                if self.config.position_mode == "fixed" {
                    setting_row(ui, "Fixed position (x, y)", |ui| {
                        // Right-to-left: the "px" hint sits furthest right, then
                        // y, then x — so they read x, y, hint left-to-right.
                        ui.label(RichText::new("px from top-left").weak().small());
                        changed |= ui
                            .add(egui::DragValue::new(&mut self.config.fixed_y).speed(2))
                            .changed();
                        changed |= ui
                            .add(egui::DragValue::new(&mut self.config.fixed_x).speed(2))
                            .changed();
                    });
                    ui.label(
                        RichText::new("Tip: Alt+drag the popup to set this.")
                            .weak()
                            .small(),
                    );
                }

                ui.separator();

                // Filter defaults (live — re-seeds the loaded item immediately).
                // Stored as `filter_min_percent` (the seeded minimum as a share of
                // the item's roll); the slider shows it as a tolerance *below* the
                // roll (0% = exact, up to 20% looser), which reads more naturally.
                ui.horizontal(|ui| {
                    ui.label("Roll tolerance").on_hover_text(
                        "How far below each mod's rolled value the filter minimum is \
                         seeded. 0% = exact roll; higher widens the search to also \
                         catch slightly worse copies. Applies to the item on screen now.",
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let mut tolerance = 100u32.saturating_sub(self.config.filter_min_percent);
                        let resp = ui.add(egui::Slider::new(&mut tolerance, 0..=20).suffix("%"));
                        // Track the slider live so its handle follows the drag.
                        self.config.filter_min_percent = 100 - tolerance.min(100);
                        // Commit (save + re-seed + re-price) only when adjusting
                        // finishes (drag release or a discrete step), so a 0→15
                        // drag fires one query, not one per value, sparing the
                        // rate-limited trade API.
                        if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
                            changed = true;
                            reseed = true;
                        }
                    });
                });
                if setting_row(ui, "Implicit mods off by default", |ui| {
                    ui.checkbox(&mut self.config.implicits_off_by_default, "")
                        .changed()
                }) {
                    changed = true;
                    reseed = true;
                }

                // Chat macros (live — pump reads the command on press). Two
                // slots: F5 (default /hideout) and F2 (default /exit).
                setting_row(ui, "Hideout macro", |ui| {
                    // Right-to-left: command field first (rightmost), then the
                    // enable toggle to its left.
                    if let Some(cmd) = &mut self.config.f5_command {
                        changed |= ui
                            .add(egui::TextEdit::singleline(cmd).desired_width(160.0))
                            .changed();
                    }
                    let mut enabled = self.config.f5_command.is_some();
                    if ui.checkbox(&mut enabled, "").changed() {
                        self.config.f5_command = if enabled {
                            Some("/hideout".into())
                        } else {
                            None
                        };
                        changed = true;
                    }
                });
                setting_row(ui, "Exit macro", |ui| {
                    if let Some(cmd) = &mut self.config.macro2_command {
                        changed |= ui
                            .add(egui::TextEdit::singleline(cmd).desired_width(160.0))
                            .changed();
                    }
                    let mut enabled = self.config.macro2_command.is_some();
                    if ui.checkbox(&mut enabled, "").changed() {
                        self.config.macro2_command =
                            if enabled { Some("/exit".into()) } else { None };
                        changed = true;
                    }
                });

                // POE2-focus gate (pushed live to the evdev watcher).
                if setting_row(ui, "Only fire hotkeys while POE2 is focused", |ui| {
                    ui.checkbox(&mut self.config.require_poe2_focus, "")
                        .changed()
                }) {
                    changed = true;
                    // Apply to the running watcher at once (no restart).
                    self.hotkeys.apply_config(&self.config);
                }

                // Per-second overlay performance log (off by default — a
                // diagnostic aid on the `perf` tracing target).
                if setting_row(ui, "Performance metrics (log)", |ui| {
                    ui.checkbox(&mut self.config.perf_metrics, "")
                        .on_hover_text(
                            "Log frame rate / max frame time / resize count once a \
                             second to the `perf` tracing target. For diagnosing lag; \
                             leave off for normal use.",
                        )
                        .changed()
                }) {
                    changed = true;
                }

                // Log verbosity (read once at startup, so a change needs a
                // restart). `auto` = error in release, debug in development;
                // `RUST_LOG` overrides it regardless.
                ui.horizontal(|ui| {
                    ui.label("Log level");
                    ui.label(RichText::new("(restart)").weak().small());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let before = self.config.log_level.clone();
                        egui::ComboBox::from_id_salt("settings-log-level")
                            .width(CONTROL_WIDTH)
                            .selected_text(log_level_label(&self.config.log_level))
                            .show_ui(ui, |ui| {
                                for (id, label) in LOG_LEVELS {
                                    ui.selectable_value(
                                        &mut self.config.log_level,
                                        id.to_string(),
                                        *label,
                                    );
                                }
                            });
                        if self.config.log_level != before {
                            changed = true;
                            restart = true;
                        }
                    });
                });

                ui.separator();

                // Hotkey bindings (pushed live to the evdev watcher by
                // `commit_hotkey`). Click a row, then press the new combo.
                ui.label(RichText::new("Hotkeys").strong());
                ui.label(
                    RichText::new("Click a hotkey, then press the new combo (Esc cancels).")
                        .weak()
                        .small(),
                );
                let rows = [
                    (
                        HotkeySlot::Quick,
                        "Price check",
                        self.config.hotkey_quick.clone(),
                    ),
                    (
                        HotkeySlot::Macro,
                        "Hideout macro",
                        self.config.hotkey_macro.clone(),
                    ),
                    (
                        HotkeySlot::Macro2,
                        "Exit macro",
                        self.config.hotkey_macro2.clone(),
                    ),
                    (
                        HotkeySlot::Close,
                        "Close popup",
                        self.config.hotkey_close.clone(),
                    ),
                    (
                        HotkeySlot::Settings,
                        "Open settings",
                        self.config.hotkey_settings.clone(),
                    ),
                ];
                for (slot, label, current) in rows {
                    let recording = self.recording_hotkey == Some(slot);
                    if hotkey_record_row(ui, label, &current, recording) {
                        // Toggle: clicking the active row cancels, otherwise it
                        // (re)starts recording for that row.
                        self.recording_hotkey = if recording { None } else { Some(slot) };
                    }
                }

                ui.separator();

                // Appearance / theme (live — the overlay reads the resolved
                // palette each frame, so colour & opacity changes are instant).
                ui.label(RichText::new("Appearance").strong());
                ui.label(
                    RichText::new(
                        "Pick a preset, then fine-tune. Colours are also \
                                   hand-editable as #rrggbb in config.json.",
                    )
                    .weak()
                    .small(),
                );
                ui.horizontal(|ui| {
                    ui.label("Preset");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Right-to-left layout, so push in reverse to read L→R.
                        let presets = super::theme::presets();
                        for preset in presets.into_iter().rev() {
                            if ui.button(preset.name).clicked() {
                                self.config.theme = preset.theme;
                                changed = true;
                                retheme = true;
                            }
                        }
                    });
                });
                // Opacity (the popup background's alpha — lower = more
                // see-through to the game). 30% floor so it can't vanish entirely.
                ui.horizontal(|ui| {
                    ui.label("Opacity").on_hover_text(
                        "How solid the popup background is. Lower lets the game \
                         show through; the text and item cards stay readable.",
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let mut pct = (self.config.theme.opacity.clamp(0.0, 1.0) * 100.0).round();
                        let resp = ui.add(egui::Slider::new(&mut pct, 30.0..=100.0).suffix("%"));
                        self.config.theme.opacity = pct / 100.0;
                        // Live-preview while dragging; persist on release/step.
                        retheme |= resp.changed();
                        if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
                            changed = true;
                        }
                    });
                });
                // Per-accent colour pickers.
                let theme = &mut self.config.theme;
                let mut c = false;
                c |= color_row(ui, "Accent (price)", &mut theme.accent_gold);
                c |= color_row(ui, "Mod text", &mut theme.affix_blue);
                c |= color_row(ui, "Online dot", &mut theme.online_dot);
                c |= color_row(ui, "Card background", &mut theme.header_bg);
                c |= color_row(ui, "Popup background", &mut theme.overlay_fill);
                c |= color_row(ui, "Popup border", &mut theme.overlay_stroke);
                if c {
                    changed = true;
                    retheme = true;
                }
            });

        ui.separator();
        ui.label(
            RichText::new(format!("config.json: {}", Config::path().display()))
                .weak()
                .small(),
        );
        // A note only when there's something to say: a restart-required field
        // changed, or a save failed. Plain saves are silent.
        if let Some(note) = &self.settings_note {
            ui.colored_label(Color32::from_rgb(0xff, 0xc8, 0x4b), note);
        }

        if changed {
            if let Err(e) = self.config.save() {
                tracing::warn!(error = %e, "could not save config");
                self.settings_note = Some(format!("Could not save: {e}"));
            } else if restart {
                self.settings_note = Some("Realm / log level apply after a restart.".to_string());
            } else {
                self.settings_note = None;
            }
        }
        // Rebuild the live palette from the (possibly just-edited) config so the
        // overlay paints the new colours/opacity on the next frame. Also fires
        // mid-drag for the opacity slider, giving a live preview without writes.
        if retheme {
            self.theme = super::theme::Theme::from_config(&self.config.theme);
        }
        // Re-seed (min-roll % / implicit default) re-prices on its own; a plain
        // `requery` (league / status) re-prices with the existing filters.
        if reseed {
            self.reseed_filters(&ctx);
        } else if requery {
            self.rerun_query(&ctx);
        }

        // Fire the debounced POESESSID live check once edits settle. Requesting
        // a repaint at the deadline guarantees a frame even if the panel is idle.
        if let Some(at) = self.session_check_at {
            let waited = at.elapsed();
            if waited >= POESESSID_DEBOUNCE {
                self.session_check_at = None;
                self.spawn_session_check(&ctx);
            } else {
                ctx.request_repaint_after(POESESSID_DEBOUNCE.saturating_sub(waited));
            }
        }
    }
}

/// Render the POESESSID validation indicator. `has_session` is whether a
/// (well-formed) session is currently stored, so a saved-but-unchecked session
/// still shows as set.
fn session_status_label(ui: &mut egui::Ui, status: &SessionCheck, has_session: bool) {
    let warn = Color32::from_rgb(0xff, 0xc8, 0x4b);
    let bad = Color32::from_rgb(0xff, 0x6b, 0x6b);
    match status {
        SessionCheck::Idle => {
            if has_session {
                ui.colored_label(online_dot(), ph::CHECK_CIRCLE)
                    .on_hover_text("Session set");
            }
        }
        SessionCheck::Checking => {
            ui.spinner();
            ui.label(RichText::new("checking…").weak().small());
        }
        SessionCheck::Valid(account) => {
            let text = match account {
                Some(name) => format!("{} valid: {name}", ph::CHECK_CIRCLE),
                None => format!("{} valid", ph::CHECK_CIRCLE),
            };
            ui.colored_label(online_dot(), text);
        }
        SessionCheck::Invalid => {
            ui.colored_label(bad, format!("{} invalid or expired", ph::X_CIRCLE));
        }
        SessionCheck::Malformed => {
            ui.colored_label(
                bad,
                format!("{} not a POESESSID (32 hex chars)", ph::X_CIRCLE),
            )
            .on_hover_text(
                "Paste only the cookie value — no \"POESESSID=\" prefix, quotes, or spaces.",
            );
        }
        SessionCheck::Unknown => {
            ui.colored_label(warn, format!("{} couldn't verify", ph::WARNING))
                .on_hover_text("Network error — the session may still be fine.");
        }
    }
}

/// Lay out one settings row with the `label` flush-left and its control(s)
/// flush-right, so every row's controls line up in a column down the right edge
/// (checkboxes, combos, sliders and values all share the same right margin).
///
/// Controls added inside `add` run in a right-to-left layout, so when a row has
/// more than one widget, add the *rightmost* one first.
fn setting_row<R>(
    ui: &mut egui::Ui,
    label: impl Into<egui::WidgetText>,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), add)
            .inner
    })
    .inner
}

/// One labelled colour-picker row. `hex` is edited in place as `#rrggbb`
/// (alpha is the separate opacity slider, so only RGB is picked here). Returns
/// whether the colour changed.
fn color_row(ui: &mut egui::Ui, label: &str, hex: &mut String) -> bool {
    setting_row(ui, label, |ui| {
        let current = super::theme::parse_hex(hex).unwrap_or(Color32::GRAY);
        let mut rgb = [current.r(), current.g(), current.b()];
        if ui.color_edit_button_srgb(&mut rgb).changed() {
            *hex = super::theme::to_hex(Color32::from_rgb(rgb[0], rgb[1], rgb[2]));
            true
        } else {
            false
        }
    })
}

fn trade_status_label(id: &str) -> &str {
    TRADE_STATUSES
        .iter()
        .find(|(i, _)| *i == id)
        .map_or("Instant Buyout", |(_, l)| *l)
}

fn position_label(id: &str) -> &str {
    POSITION_MODES
        .iter()
        .find(|(i, _)| *i == id)
        .map_or("Center", |(_, l)| *l)
}

fn log_level_label(id: &str) -> &str {
    LOG_LEVELS
        .iter()
        .find(|(i, _)| *i == id)
        .map_or("Auto", |(_, l)| *l)
}

/// A labelled, right-aligned click-to-record hotkey row. Shows the current
/// binding (or a "press keys" prompt while recording) on a button; returns
/// whether that button was clicked so the caller can toggle recording. The
/// actual capture happens in the overlay's keyboard handler, which calls
/// [`QuickModeApp::commit_hotkey`](crate::QuickModeApp::commit_hotkey).
fn hotkey_record_row(ui: &mut egui::Ui, label: &str, current: &str, recording: bool) -> bool {
    let mut clicked = false;
    ui.horizontal(|ui| {
        ui.label(format!("  {label}"));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let text = if recording {
                RichText::new("Press keys…").strong()
            } else {
                RichText::new(current)
            };
            let mut button = egui::Button::new(text).min_size(egui::vec2(CONTROL_WIDTH, 0.0));
            // Tint the active row so it's clear which one is listening.
            if recording {
                button = button.fill(Color32::from_rgb(0x4b, 0x6b, 0xff));
            }
            let resp = ui.add(button).on_hover_text(if recording {
                "Press the new key combo, or Esc to cancel"
            } else {
                "Click, then press the new key combo"
            });
            clicked = resp.clicked();
        });
    });
    clicked
}
