//! The settings surface. Edits persist on change; startup-only fields are
//! flagged "restart to apply".

use std::time::Instant;

use egui::{Color32, RichText};
use egui_phosphor::regular as ph;

use crate::config::Config;
use crate::model::SessionCheck;
use crate::{HotkeySlot, QuickModeApp, POESESSID_DEBOUNCE};

use super::theme::online_dot;

/// Copy the POESESSID to the OS clipboard, logging any failure.
fn copy_session_id(sid: &str) {
    if let Err(e) = platform::write_clipboard_text(sid) {
        tracing::warn!(error = %e, "could not copy POESESSID to clipboard");
    }
}

const TRADE_STATUSES: &[(&str, &str)] = &[
    ("securable", "Instant Buyout"),
    ("online", "Online (In Person)"),
    ("available", "Online + Buyout"),
    ("any", "Any"),
];

const POSITION_MODES: &[(&str, &str)] = &[("center", "Center"), ("fixed", "Fixed")];

/// `auto` = error in release, debug in dev; the rest map to tracing levels.
const LOG_LEVELS: &[(&str, &str)] = &[
    ("auto", "Auto"),
    ("off", "Off"),
    ("error", "Error"),
    ("warn", "Warn"),
    ("info", "Info"),
    ("debug", "Debug"),
    ("trace", "Trace"),
];

/// Shared width for the right-edge button-like controls so they form one column.
const CONTROL_WIDTH: f32 = 140.0;

impl QuickModeApp {
    /// Render the settings surface body. Call [`pump`](Self::pump) first.
    pub fn settings_content(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        super::theme::set_active(self.theme);
        // Esc closes the settings panel when it has focus (via Wayland).
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.settings_close_requested = true;
        }
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
                // egui's scrollbar floats over content, so reserve its width on the
                // right or the right-edge controls get clipped under it.
                let bar = ui.spacing().scroll.bar_width;
                ui.set_max_width((ui.available_width() - bar).max(0.0));

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
                // teleport button, which needs an authenticated fetch).
                ui.horizontal(|ui| {
                    ui.label("POESESSID");
                    let mut sid = self.config.poesessid.clone().unwrap_or_default();
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
                    // One-click Copy / Paste on the whole value.
                    if ui
                        .button(ph::COPY)
                        .on_hover_text("Copy to clipboard")
                        .clicked()
                    {
                        copy_session_id(&sid);
                    }
                    if ui
                        .button(ph::CLIPBOARD)
                        .on_hover_text("Paste from clipboard")
                        .clicked()
                    {
                        if let Ok(Some(text)) = platform::read_paste_text() {
                            sid = text.trim().to_string();
                            edited = true;
                        }
                    }
                    // egui 0.29 has no built-in TextEdit context menu, so provide one.
                    resp.context_menu(|ui| {
                        if ui.button("Copy").clicked() {
                            copy_session_id(&sid);
                            ui.close_menu();
                        }
                        if ui.button("Cut").clicked() {
                            copy_session_id(&sid);
                            sid.clear();
                            edited = true;
                            ui.close_menu();
                        }
                        if ui.button("Paste").clicked() {
                            if let Ok(Some(text)) = platform::read_paste_text() {
                                sid = text.trim().to_string();
                                edited = true;
                            }
                            ui.close_menu();
                        }
                    });
                    if edited {
                        let trimmed = sid.trim().to_string();
                        self.config.poesessid = (!trimmed.is_empty()).then(|| trimmed.clone());
                        // Push live; `set_poesessid` drops a malformed value safely.
                        self.client.set_poesessid(self.config.poesessid.clone());
                        changed = true;
                        // Instant format feedback; well-formed also schedules a live check.
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

                // Cloudflare bypass: route requests through a Chrome-emulating client.
                let cf_toggle = ui.checkbox(
                    &mut self.config.impersonate,
                    "Cloudflare bypass (impersonate Chrome)",
                );
                let mut impersonate_changed = cf_toggle
                    .on_hover_text(
                        "Sends requests with a Chrome TLS fingerprint to pass Cloudflare's \
                         bot-check. Needs a cf_clearance cookie below.",
                    )
                    .changed();
                if self.config.impersonate {
                    ui.horizontal(|ui| {
                        ui.label("cf_clearance");
                        let mut cf = self.config.cf_clearance.clone().unwrap_or_default();
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut cf)
                                .password(true)
                                .hint_text("cf_clearance cookie value")
                                .desired_width(220.0),
                        );
                        if resp.changed() {
                            let trimmed = cf.trim().to_string();
                            self.config.cf_clearance = (!trimmed.is_empty()).then_some(trimmed);
                            impersonate_changed = true;
                        }
                    });
                    ui.label(
                        RichText::new(
                            "Browser DevTools → Application → Cookies → pathofexile.com → \
                             cf_clearance. Bound to your IP + browser and expires in a few \
                             hours, so it needs re-pasting periodically.",
                        )
                        .weak()
                        .small(),
                    );
                }
                if impersonate_changed {
                    changed = true;
                    self.push_impersonate_settings();
                }

                ui.separator();

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
                        // RTL: add hint, then y, then x so they read x, y, hint.
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

                // Filter defaults (live — re-seeds the loaded item). Stored as
                // `filter_min_percent`; shown as a tolerance below the roll.
                ui.horizontal(|ui| {
                    ui.label("Roll tolerance").on_hover_text(
                        "How far below each mod's rolled value the filter minimum is \
                         seeded. 0% = exact roll; higher widens the search to also \
                         catch slightly worse copies. Applies to the item on screen now.",
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let mut tolerance = 100u32.saturating_sub(self.config.filter_min_percent);
                        let resp = ui.add(egui::Slider::new(&mut tolerance, 0..=20).suffix("%"));
                        self.config.filter_min_percent = 100 - tolerance.min(100);
                        // Commit only when adjusting finishes, so a drag fires one query.
                        if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
                            changed = true;
                            reseed = true;
                        }
                    });
                });
                ui.horizontal(|ui| {
                    ui.label("Cache lifetime").on_hover_text(
                        "How long a price result is reused when you re-check the \
                         same, unchanged item, instead of querying the trade API \
                         again. 0 = always re-query; up to 120 s. Crafting changes \
                         the item, so a crafted item always re-queries.",
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let resp = ui.add(
                            egui::Slider::new(
                                &mut self.config.cache_ttl_secs,
                                0..=crate::config::MAX_CACHE_TTL_SECS,
                            )
                            .suffix(" s"),
                        );
                        // Persist only when the adjustment settles, like the other sliders.
                        if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
                            changed = true;
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

                // Chat macros (live — pump reads the command on press).
                setting_row(ui, "Hideout macro", |ui| {
                    // RTL: command field first, then the enable toggle to its left.
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

                if setting_row(ui, "Only fire hotkeys while POE2 is focused", |ui| {
                    ui.checkbox(&mut self.config.require_poe2_focus, "")
                        .changed()
                }) {
                    changed = true;
                    self.hotkeys.apply_config(&self.config);
                }

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

                // Log verbosity (read once at startup, so a change needs a restart).
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

                // Hotkey bindings (pushed live by `commit_hotkey`).
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
                        // Toggle: clicking the active row cancels, else (re)starts recording.
                        self.recording_hotkey = if recording { None } else { Some(slot) };
                    }
                }

                ui.separator();

                // Appearance / theme (live — colour & opacity changes are instant).
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
                        // RTL: push in reverse to read L→R.
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
                // Opacity (popup background alpha; 30% floor so it can't vanish).
                ui.horizontal(|ui| {
                    ui.label("Opacity").on_hover_text(
                        "How solid the popup background is. Lower lets the game \
                         show through; the text and item cards stay readable.",
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let mut pct = (self.config.theme.opacity.clamp(0.0, 1.0) * 100.0).round();
                        let resp = ui.add(egui::Slider::new(&mut pct, 30.0..=100.0).suffix("%"));
                        self.config.theme.opacity = pct / 100.0;
                        retheme |= resp.changed();
                        if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
                            changed = true;
                        }
                    });
                });
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
        // A note only on restart-required change or save failure; plain saves are silent.
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
        // Rebuild the live palette so the overlay paints new colours next frame.
        if retheme {
            self.theme = super::theme::Theme::from_config(&self.config.theme);
        }
        // Re-seed re-prices on its own; a plain `requery` keeps the existing filters.
        if reseed {
            self.reseed_filters(&ctx);
        } else if requery {
            self.rerun_query(&ctx);
        }

        // Fire the debounced POESESSID live check once edits settle, requesting
        // a repaint at the deadline so it runs even if the panel is idle.
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

/// Render the POESESSID validation indicator. `has_session`: a session is stored.
fn session_status_label(ui: &mut egui::Ui, status: &SessionCheck, has_session: bool) {
    let warn = Color32::from_rgb(0xff, 0xc8, 0x4b);
    let bad = Color32::from_rgb(0xff, 0x6b, 0x6b);
    match status {
        SessionCheck::Idle => {
            if has_session {
                // A stored session is NOT a verified one — don't show a green
                // tick until it actually validates (which happens automatically).
                ui.label(RichText::new("set — verifying…").weak())
                    .on_hover_text("Validates automatically when you price an item.");
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

/// Lay out one settings row: `label` flush-left, control(s) flush-right in a
/// right-to-left layout (add the rightmost widget first).
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

/// One labelled colour-picker row editing `hex` as `#rrggbb`. Returns whether it changed.
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

/// A click-to-record hotkey row; returns whether the button was clicked so the
/// caller can toggle recording. Capture happens in the overlay's keyboard handler.
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
