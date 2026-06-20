//! All egui rendering for [`QuickModeApp`], split by surface. This module hosts
//! the popup body ([`QuickModeApp::content`]) that dispatches into those surfaces.

mod actions;
mod exchange;
mod filters;
mod item_card;
mod listings;
mod settings;
pub(crate) mod theme;

pub(crate) use actions::run_chat_macro;

use egui::{Color32, RichText};
use egui_phosphor::regular as ph;
use trade_api::build_detailed_query;

use crate::model::{Phase, PriceMode, View};
use crate::{QuickModeApp, FILTER_DEBOUNCE};

use item_card::item_card;
use listings::show_results;

impl QuickModeApp {
    /// Render the popup body into the given `Ui`. Call [`pump`](Self::pump) first.
    pub fn content(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        // Install the user's palette for this frame so the accent helpers read it.
        theme::set_active(self.theme);

        // Header: title (left) + league selector & close button (right).
        ui.horizontal(|ui| {
            ui.label(RichText::new("Kingsmerchant").strong());
            // Build version — confirms a fresh build runs.
            ui.label(
                RichText::new(concat!("v", env!("CARGO_PKG_VERSION")))
                    .weak()
                    .small(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("X")
                    .on_hover_text("Close (Esc / click outside)")
                    .clicked()
                {
                    self.close_requested = true;
                }
                if ui.button(ph::GEAR).on_hover_text("Open settings").clicked() {
                    self.settings_requested = true;
                }
                self.league_selector(ui, &ctx);
            });
        });
        ui.add_space(4.0);

        // View toggle (Item ⇄ Text); pricing is driven by Ctrl+C / the filters.
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.view, View::Item, format!("{} Item", ph::SHIELD));
            ui.selectable_value(
                &mut self.view,
                View::Text,
                format!("{} Text", ph::NOTE_PENCIL),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button(format!("{} Read clipboard", ph::CLIPBOARD))
                    .clicked()
                {
                    self.read_clipboard();
                    self.start_price_check(&ctx);
                }
            });
        });

        // Instant feedback while reading the clipboard after Ctrl+C.
        if self.awaiting_copy {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Reading item from POE2…").strong());
            });
        }

        if let Some(hint) = &self.hint {
            ui.add_space(4.0);
            ui.colored_label(
                Color32::from_rgb(0xff, 0xc8, 0x4b),
                format!("{} {hint}", ph::WARNING),
            );
        }

        ui.add_space(4.0);

        match self.view {
            View::Text => {
                ui.add(
                    egui::TextEdit::multiline(&mut self.item_text)
                        .desired_rows(8)
                        .desired_width(f32::INFINITY)
                        .font(egui::TextStyle::Monospace),
                );
            }
            View::Item => {
                // Render from the already-parsed item; re-parsing every frame lagged.
                if let Some(item) = &self.item {
                    item_card(ui, item, self.icon_url.as_deref());
                } else if self.item_text.trim().is_empty() {
                    ui.label(
                        RichText::new("Hover an item in POE2 and press Ctrl+C to price it.")
                            .weak()
                            .italics(),
                    );
                } else {
                    ui.label(
                        RichText::new("Not a POE2 item — switch to Text to edit.")
                            .weak()
                            .italics(),
                    );
                }
            }
        }

        ui.add_space(6.0);

        // Rate-limit feedback while waiting on the trade API's bucket.
        if let Some(wait) = self.client.retry_in() {
            let secs = (wait.as_millis() as u64).div_ceil(1000);
            ui.colored_label(
                Color32::from_rgb(0xff, 0xc8, 0x4b),
                format!("{} Rate limited — retrying in {secs}s", ph::HOURGLASS),
            );
        }

        let mut copied: Option<String> = None;
        let mut open_trade: Option<String> = None;
        let mut teleport: Option<String> = None;
        match self.mode {
            PriceMode::Exchange => self.exchange_content(ui, &ctx, &mut copied, &mut open_trade),
            PriceMode::Item => {
                // Filter panel; edits re-run the search after a debounce, "Apply now" is immediate.
                {
                    ui.add_space(6.0);
                    let apply_now = self.filter_panel(ui);
                    let debounced = self.filter_dirty
                        && self.filter_changed_at.elapsed() >= FILTER_DEBOUNCE
                        && !matches!(self.phase, Phase::Loading);
                    if apply_now || debounced {
                        self.filter_dirty = false;
                        self.rerun_query(&ctx);
                    }
                }
                ui.add_space(6.0);
                if matches!(self.phase, Phase::Loading) {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("searching…");
                    });
                }
                self.estimate_badge(ui);
                ui.separator();
                match &self.phase {
                    Phase::Idle => {
                        ui.label(RichText::new("Waiting for an item…").weak().italics());
                    }
                    Phase::Loading => {}
                    Phase::Failed(e) => {
                        ui.colored_label(Color32::from_rgb(0xff, 0x6b, 0x6b), e);
                    }
                    Phase::Done(pc) => {
                        show_results(ui, pc, &mut copied, &mut teleport);
                    }
                }
            }
        }
        if let Some(label) = copied {
            self.copy_status = Some(label);
        }
        if let Some(url) = open_trade {
            match platform_linux::open_url(&url) {
                // Hide the popup so the browser comes forward over our overlay.
                Ok(()) => self.close_requested = true,
                Err(e) => tracing::warn!(error = %e, "xdg-open failed"),
            }
        }
        if let Some(token) = teleport {
            self.spawn_teleport(token, &ctx);
        }

        if let Some(status) = &self.copy_status {
            ui.add_space(4.0);
            ui.colored_label(
                Color32::from_rgb(0x4c, 0xd1, 0x37),
                format!("{} Sent {status} to POE2", ph::CHECK_CIRCLE),
            );
        }
    }

    /// The league dropdown; switching re-prices the loaded item under the new league.
    fn league_selector(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if self.leagues.is_empty() {
            ui.label(RichText::new(&self.config.league).weak());
            return;
        }
        let current = self.config.league.clone();
        let mut chosen = current.clone();
        egui::ComboBox::from_id_salt("league-selector")
            .selected_text(&current)
            .show_ui(ui, |ui| {
                for lg in &self.leagues {
                    ui.selectable_value(&mut chosen, lg.id.clone(), &lg.text);
                }
            });
        if chosen != current {
            self.config.league.clone_from(&chosen);
            // An explicit pick pins the league (stops auto-resolve on restart).
            self.config.league_pinned = true;
            if let Err(e) = self.config.save() {
                tracing::warn!(error = %e, "could not save config");
            }
            self.client.set_league(chosen);
            // Re-price under the new league, keeping any detailed-mode filters.
            self.rerun_query(ctx);
        }
    }

    /// Deep link to Craft of Exile, pre-loaded with the item via the `eimport` param.
    fn craft_of_exile_url(&self) -> String {
        format!(
            "https://www.craftofexile.com/?game=poe2&eimport={}",
            percent_encode(&self.item_text)
        )
    }

    /// Deep link to the trade site, encoding the whole query in `?q=` so disabled
    /// filters show greyed rather than missing.
    fn trade_url(&self) -> String {
        let base = format!(
            "https://www.pathofexile.com/trade2/search/poe2/{}",
            percent_encode(&self.config.league)
        );
        let Some(item) = &self.item else {
            return base;
        };
        let request = build_detailed_query(item, self.client.items(), &self.detailed_filters());
        match serde_json::to_string(&request) {
            Ok(json) => format!("{base}?q={}", percent_encode(&json)),
            Err(e) => {
                tracing::warn!(error = %e, "could not encode trade query");
                base
            }
        }
    }
}

/// Percent-encode a string for use in a URL (RFC 3986 unreserved pass through).
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char);
            }
            _ => {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0xf) as usize] as char);
            }
        }
    }
    out
}
