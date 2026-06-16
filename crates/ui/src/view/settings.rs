//! The settings surface. Edits write straight to `config` and persist on
//! change; startup-only fields (hotkeys, realm, focus gate) are flagged
//! "restart to apply" rather than pretending to take effect live.

use std::time::Instant;

use egui::{Color32, RichText};
use egui_phosphor::regular as ph;

use crate::config::Config;
use crate::model::SessionCheck;
use crate::{QuickModeApp, POESESSID_DEBOUNCE};

use super::theme::ONLINE_DOT;

/// Trade listing-status options for the settings dropdown (config id, label).
const TRADE_STATUSES: &[(&str, &str)] = &[
    ("securable", "Instant Buyout"),
    ("online", "Online (In Person)"),
    ("available", "Online + Buyout"),
    ("any", "Any"),
];

/// Popup position modes for the settings dropdown (config id, label).
const POSITION_MODES: &[(&str, &str)] = &[("center", "Center"), ("fixed", "Fixed")];

impl QuickModeApp {
    /// Render the settings surface body. Call [`pump`](Self::pump) first
    /// (shared with the popup surface).
    pub fn settings_content(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
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
                // League (live — the client switches without a rebuild).
                ui.horizontal(|ui| {
                    ui.label("League");
                    if self.leagues.is_empty() {
                        ui.label(RichText::new(&self.config.league).weak());
                    } else {
                        let before = self.config.league.clone();
                        egui::ComboBox::from_id_salt("settings-league")
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
                    let current = self.config.realm.clone().unwrap_or_else(|| "pc".into());
                    let mut chosen = current.clone();
                    egui::ComboBox::from_id_salt("settings-realm")
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
                    ui.label(RichText::new("(restart)").weak().small());
                });

                // Listing type / trade status (live — read per query).
                ui.horizontal(|ui| {
                    ui.label("Listings");
                    let before = self.config.trade_status.clone();
                    egui::ComboBox::from_id_salt("settings-status")
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
                // fetch for each listing's teleport token).
                ui.horizontal(|ui| {
                    ui.label("POESESSID");
                    let mut sid = self.config.poesessid.clone().unwrap_or_default();
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut sid)
                            .password(true)
                            .hint_text("trade-site session cookie")
                            .desired_width(220.0),
                    );
                    if resp.changed() {
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
                ui.horizontal(|ui| {
                    ui.label("Popup position");
                    let before = self.config.position_mode.clone();
                    egui::ComboBox::from_id_salt("settings-position")
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
                    ui.horizontal(|ui| {
                        ui.label("    x / y");
                        changed |= ui
                            .add(egui::DragValue::new(&mut self.config.fixed_x).speed(2))
                            .changed();
                        changed |= ui
                            .add(egui::DragValue::new(&mut self.config.fixed_y).speed(2))
                            .changed();
                        ui.label(RichText::new("px from top-left").weak().small());
                    });
                    ui.label(
                        RichText::new("    Tip: Alt+drag the popup to set this.")
                            .weak()
                            .small(),
                    );
                }

                ui.separator();

                // Filter defaults (live — re-seeds the loaded item immediately).
                ui.horizontal(|ui| {
                    ui.label("Min roll %").on_hover_text(
                        "Each mod filter's minimum starts at this share of the item's \
                         own roll. 100% = exact roll; lower = a looser search that also \
                         finds slightly worse copies. Applies to the item on screen now.",
                    );
                    let resp = ui.add(
                        egui::Slider::new(&mut self.config.filter_min_percent, 50..=100)
                            .suffix("%"),
                    );
                    // Commit (save + re-seed + re-price) only when adjusting
                    // finishes (drag release or a discrete step), so a 100→70
                    // drag fires one query, not one per value, sparing the
                    // rate-limited trade API.
                    if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
                        changed = true;
                        reseed = true;
                    }
                });
                if ui
                    .checkbox(
                        &mut self.config.implicits_off_by_default,
                        "Implicit mods off by default",
                    )
                    .changed()
                {
                    changed = true;
                    reseed = true;
                }

                // Chat macros (live — pump reads the command on press). Two
                // slots: F5 (default /hideout) and F2 (default /exit).
                ui.horizontal(|ui| {
                    let mut enabled = self.config.f5_command.is_some();
                    if ui.checkbox(&mut enabled, "Hideout macro").changed() {
                        self.config.f5_command = if enabled {
                            Some("/hideout".into())
                        } else {
                            None
                        };
                        changed = true;
                    }
                    if let Some(cmd) = &mut self.config.f5_command {
                        changed |= ui.text_edit_singleline(cmd).changed();
                    }
                });
                ui.horizontal(|ui| {
                    let mut enabled = self.config.macro2_command.is_some();
                    if ui.checkbox(&mut enabled, "Exit macro").changed() {
                        self.config.macro2_command =
                            if enabled { Some("/exit".into()) } else { None };
                        changed = true;
                    }
                    if let Some(cmd) = &mut self.config.macro2_command {
                        changed |= ui.text_edit_singleline(cmd).changed();
                    }
                });

                // POE2-focus gate (read once by the evdev watcher — restart).
                ui.horizontal(|ui| {
                    if ui
                        .checkbox(
                            &mut self.config.require_poe2_focus,
                            "Only fire hotkeys while POE2 is focused",
                        )
                        .changed()
                    {
                        changed = true;
                        restart = true;
                    }
                    ui.label(RichText::new("(restart)").weak().small());
                });

                ui.separator();

                // Hotkey bindings (parsed by the evdev watcher at startup —
                // restart-only). Free-text like "Ctrl+Alt+C", "F5", "Escape".
                ui.label(RichText::new("Hotkeys (restart to apply)").strong());
                restart |= hotkey_row(ui, "Quick", &mut self.config.hotkey_quick, &mut changed);
                restart |= hotkey_row(
                    ui,
                    "Detailed",
                    &mut self.config.hotkey_detailed,
                    &mut changed,
                );
                restart |= hotkey_row(
                    ui,
                    "Hideout macro",
                    &mut self.config.hotkey_macro,
                    &mut changed,
                );
                restart |= hotkey_row(
                    ui,
                    "Exit macro",
                    &mut self.config.hotkey_macro2,
                    &mut changed,
                );
                restart |= hotkey_row(ui, "Close", &mut self.config.hotkey_close, &mut changed);
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
                self.settings_note =
                    Some("Hotkeys / realm / focus-gate apply after a restart.".to_string());
            } else {
                self.settings_note = None;
            }
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
                ui.colored_label(ONLINE_DOT, ph::CHECK_CIRCLE)
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
            ui.colored_label(ONLINE_DOT, text);
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

/// A labelled, right-aligned hotkey-string text field. Sets `*changed` and
/// returns whether it changed (the caller folds that into the restart flag,
/// since bindings are only read at startup).
fn hotkey_row(ui: &mut egui::Ui, label: &str, value: &mut String, changed: &mut bool) -> bool {
    let mut row_changed = false;
    ui.horizontal(|ui| {
        ui.label(format!("  {label}"));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            row_changed = ui
                .add(egui::TextEdit::singleline(value).desired_width(140.0))
                .changed();
        });
    });
    if row_changed {
        *changed = true;
    }
    row_changed
}
