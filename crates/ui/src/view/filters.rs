//! The detailed-mode filter panel (price range + per-stat rows) and the
//! poeprices.info ML estimate badge.

use std::time::Instant;

use egui::{Color32, RichText};
use egui_phosphor::regular as ph;

use trade_api::{is_elemental_resistance, ResistanceMode};

use crate::model::{fmt_amount, MinFilter};
use crate::QuickModeApp;

use super::theme::affix_blue;

/// Resistance-mode options for the detailed-filter dropdown, with hover help.
const RESISTANCE_MODES: &[(ResistanceMode, &str, &str)] = &[
    (
        ResistanceMode::Fungible,
        "Fungible",
        "Any element counts: searches Fire/Cold/Lightning as interchangeable, \
         matched by value so you see the widest pool of comparable items.",
    ),
    (
        ResistanceMode::Specific,
        "Specific",
        "Each element must match exactly — a Fire roll only finds Fire.",
    ),
    (
        ResistanceMode::Total,
        "Total",
        "Search by the summed +#% total Elemental Resistance only.",
    ),
];

/// Width of each min/max filter field.
const FILTER_FIELD_W: f32 = 60.0;

/// Currencies offered in the price-range dropdown (id, label). Empty id = any.
const PRICE_CURRENCIES: &[(&str, &str)] = &[
    ("", "any"),
    ("exalted", "exalted"),
    ("divine", "divine"),
    ("chaos", "chaos"),
];

/// Rarity options for the detailed-filter dropdown (`type_filters.rarity` id).
/// `any` emits no rarity filter (search every rarity), matching the trade site.
const RARITIES: &[(&str, &str)] = &[
    ("any", "Any"),
    ("normal", "Normal"),
    ("magic", "Magic"),
    ("rare", "Rare"),
    ("unique", "Unique"),
];

impl QuickModeApp {
    /// The detailed-mode filter panel: a price range plus a toggleable row per
    /// mapped stat. Returns `true` when the user asked to re-run the search.
    pub(super) fn filter_panel(&mut self, ui: &mut egui::Ui) -> bool {
        let mut requery = false;
        let mut changed = false;
        egui::CollapsingHeader::new(RichText::new(format!("{} Filters", ph::FUNNEL)).strong())
            .default_open(true)
            .show(ui, |ui| {
                // Price range, right-aligned.
                ui.horizontal(|ui| {
                    ui.label("Price");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let before = self.price_filter.currency.clone();
                        egui::ComboBox::from_id_salt("price-currency")
                            .selected_text(currency_label(&self.price_filter.currency))
                            .show_ui(ui, |ui| {
                                for (id, label) in PRICE_CURRENCIES {
                                    ui.selectable_value(
                                        &mut self.price_filter.currency,
                                        id.to_string(),
                                        *label,
                                    );
                                }
                            });
                        changed |= self.price_filter.currency != before;
                        changed |= ui
                            .add(
                                egui::TextEdit::singleline(&mut self.price_filter.max)
                                    .hint_text("max")
                                    .desired_width(FILTER_FIELD_W),
                            )
                            .changed();
                        ui.label("–");
                        changed |= ui
                            .add(
                                egui::TextEdit::singleline(&mut self.price_filter.min)
                                    .hint_text("min")
                                    .desired_width(FILTER_FIELD_W),
                            )
                            .changed();
                    });
                });

                // Item level (type_filters.ilvl) — default-on for Normal bases
                // only. And item quality (type_filters.quality).
                changed |= min_filter_row(ui, "Item level ≥", &mut self.ilvl_filter);
                changed |= min_filter_row(ui, "Quality ≥", &mut self.quality_filter);

                // Rarity (type_filters.rarity), defaulting to the item's own.
                ui.horizontal(|ui| {
                    ui.label("Rarity");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let before = self.rarity_filter.clone();
                        egui::ComboBox::from_id_salt("rarity-filter")
                            .selected_text(rarity_label(&self.rarity_filter))
                            .show_ui(ui, |ui| {
                                for (id, label) in RARITIES {
                                    ui.selectable_value(
                                        &mut self.rarity_filter,
                                        (*id).to_string(),
                                        *label,
                                    );
                                }
                            });
                        changed |= self.rarity_filter != before;
                    });
                });

                // Defences / equipment properties (armour / evasion / ES / …),
                // built from the item's stats block, not its affix mods.
                if !self.equipment.is_empty() {
                    ui.add_space(6.0);
                    ui.label(RichText::new("Defences").strong());
                    for row in &mut self.equipment {
                        ui.horizontal(|ui| {
                            changed |= ui.checkbox(&mut row.enabled, "").changed();
                            ui.label(RichText::new(&row.label).strong());
                            changed |= min_max_fields(ui, &mut row.min, &mut row.max);
                        });
                    }
                }

                ui.add_space(6.0);
                if !self.equipment.is_empty() {
                    ui.label(RichText::new("Modifiers").strong());
                }

                // Resistance-search mode — only meaningful when the item carries
                // a Fire/Cold/Lightning roll, so hide it otherwise.
                if self.filters.iter().any(|r| is_elemental_resistance(&r.id)) {
                    ui.horizontal(|ui| {
                        ui.label("Resistances");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let before = self.resistance_mode;
                            egui::ComboBox::from_id_salt("resistance-mode")
                                .selected_text(resistance_label(self.resistance_mode))
                                .show_ui(ui, |ui| {
                                    for (mode, label, help) in RESISTANCE_MODES {
                                        ui.selectable_value(
                                            &mut self.resistance_mode,
                                            *mode,
                                            *label,
                                        )
                                        .on_hover_text(*help);
                                    }
                                });
                            changed |= self.resistance_mode != before;
                        });
                    });
                }

                if self.filters.is_empty() {
                    ui.label(
                        RichText::new("No mapped stats to filter on this item.")
                            .weak()
                            .italics(),
                    );
                } else {
                    egui::ScrollArea::vertical()
                        .max_height(240.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for row in &mut self.filters {
                                ui.horizontal(|ui| {
                                    changed |= ui.checkbox(&mut row.enabled, "").changed();
                                    if row.is_implicit {
                                        implicit_pill(ui);
                                    }
                                    stat_filter_label(ui, &row.label);
                                    changed |= min_max_fields(ui, &mut row.min, &mut row.max);
                                });
                            }
                        });
                }

                // Mods with no GGG trade filter (e.g. a tablet's "Map contains N
                // additional Rare Chests" — GGG has no searchable variant).
                // Shown read-only so they don't silently vanish from the panel.
                if !self.unfilterable_mods.is_empty() {
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new("Not searchable on trade")
                            .strong()
                            .color(Color32::from_rgb(0xb0, 0xb0, 0xb0)),
                    )
                    .on_hover_text(
                        "These mods have no trade-site filter, so they can't narrow the search.",
                    );
                    for line in &self.unfilterable_mods {
                        ui.label(RichText::new(format!("• {line}")).weak().italics());
                    }
                }

                // Miscellaneous: boolean attribute filters, collapsed by default.
                ui.add_space(6.0);
                egui::CollapsingHeader::new(RichText::new("Miscellaneous").strong())
                    .default_open(false)
                    .show(ui, |ui| {
                        // Two even columns of four (4 + 4), evenly spaced.
                        ui.columns(2, |cols| {
                            for (i, m) in self.misc.iter_mut().enumerate() {
                                let col = usize::from(i >= 4);
                                changed |= cols[col].checkbox(&mut m.on, m.label).changed();
                            }
                        });
                    });

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui
                        .button(format!("{} Apply now", ph::ARROW_CLOCKWISE))
                        .clicked()
                    {
                        requery = true;
                    }
                    // External links, pinned to the right edge of the window. In
                    // this right-to-left layout the first added sits rightmost, so
                    // the trade-site link leads and Craft of Exile follows to its left.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .button(format!("{} Open on trade site", ph::MAGNIFYING_GLASS))
                            .on_hover_text("Opens your browser with this exact search")
                            .clicked()
                        {
                            // Built only on click — encoding the full query is too
                            // costly to rebuild every frame.
                            let url = self.trade_url();
                            match platform_linux::open_url(&url) {
                                // Hide the popup so the browser comes forward — we're
                                // an always-on-top overlay that would cover it.
                                Ok(()) => self.close_requested = true,
                                Err(e) => tracing::warn!(error = %e, "xdg-open failed"),
                            }
                        }
                        // Craft of Exile crafting simulator, pre-loaded with the
                        // opened item (only meaningful when one is parsed).
                        if self.item.is_some()
                            && ui
                                .button(format!("{} Craft of Exile", ph::HAMMER))
                                .on_hover_text(
                                    "Open this item in Craft of Exile's crafting simulator",
                                )
                                .clicked()
                        {
                            let url = self.craft_of_exile_url();
                            match platform_linux::open_url(&url) {
                                Ok(()) => self.close_requested = true,
                                Err(e) => tracing::warn!(error = %e, "xdg-open failed"),
                            }
                        }
                    });
                });
            });

        // Any edit (re)starts the debounce timer; the caller fires the re-query
        // once it elapses.
        if changed {
            self.filter_dirty = true;
            self.filter_changed_at = Instant::now();
        }
        requery
    }

    /// The poeprices.info ML estimate badge: a spinner while it loads, then the
    /// predicted range + confidence, or nothing if poeprices declined.
    pub(super) fn estimate_badge(&self, ui: &mut egui::Ui) {
        if self.estimate_loading {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("poeprices.info ML estimate…").weak().small());
            });
            return;
        }
        let Some(est) = &self.estimate else {
            return;
        };
        let conf = est
            .confidence
            .map(|c| format!("  ·  {c:.0}% confidence"))
            .unwrap_or_default();
        let text = format!(
            "{} poeprices ML: {}-{} {}{}",
            ph::ROBOT,
            fmt_amount(est.min),
            fmt_amount(est.max),
            est.currency,
            conf
        );
        egui::Frame::none()
            .fill(Color32::from_rgb(0x23, 0x2a, 0x3a))
            .stroke(egui::Stroke::new(1.0, Color32::from_rgb(0x3c, 0x55, 0x7a)))
            .rounding(6.0)
            .inner_margin(egui::Margin::symmetric(8.0, 4.0))
            .show(ui, |ui| {
                ui.label(RichText::new(text).color(Color32::from_rgb(0x7e, 0xc8, 0xff)));
            });
    }
}

/// A modifier-row label, width-capped so it truncates with an ellipsis instead
/// of eating the row and shoving the right-pinned min/max fields out of their
/// column (long unique mods overflowed and overlapped the fields). The cap
/// reserves room for the two fields; the full text is available on hover.
fn stat_filter_label(ui: &mut egui::Ui, text: &str) {
    // Two fields plus the gaps around them — keep in sync with `min_max_fields`.
    let reserved = 2.0 * FILTER_FIELD_W + 3.0 * ui.spacing().item_spacing.x;
    let label_w = (ui.available_width() - reserved).max(40.0);
    ui.allocate_ui_with_layout(
        egui::vec2(label_w, ui.spacing().interact_size.y),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.add(egui::Label::new(RichText::new(text).color(affix_blue())).truncate())
                .on_hover_text(text);
        },
    );
}

/// Right-aligned min + max fields (they hug the right edge of the row so the
/// columns line up and the row fills the width). Returns whether either changed.
fn min_max_fields(ui: &mut egui::Ui, min: &mut String, max: &mut String) -> bool {
    let mut changed = false;
    // Fixed size (not just `desired_width`, which egui shrinks under pressure):
    // on rows with long, truncated labels the leftmost field used to get
    // squeezed narrower than the right one. `add_sized` pins both to one width.
    let size = egui::vec2(FILTER_FIELD_W, ui.spacing().interact_size.y);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        // In a right-to-left layout the first item is rightmost, so max first.
        changed |= ui
            .add_sized(size, egui::TextEdit::singleline(max).hint_text("max"))
            .changed();
        changed |= ui
            .add_sized(size, egui::TextEdit::singleline(min).hint_text("min"))
            .changed();
    });
    changed
}

/// A checkbox + label for a single-value (min-only) filter, with the min field
/// right-aligned to fill the row width. Returns whether it changed.
fn min_filter_row(ui: &mut egui::Ui, label: &str, filter: &mut MinFilter) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        changed |= ui.checkbox(&mut filter.enabled, "").changed();
        ui.label(label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            changed |= ui
                .add(
                    egui::TextEdit::singleline(&mut filter.min)
                        .hint_text("min")
                        .desired_width(FILTER_FIELD_W),
                )
                .changed();
        });
    });
    changed
}

/// A small green "implicit" pill, drawn before an implicit filter's label.
fn implicit_pill(ui: &mut egui::Ui) {
    egui::Frame::none()
        .fill(Color32::from_rgb(0x2e, 0x7d, 0x32))
        .rounding(7.0)
        .inner_margin(egui::Margin::symmetric(5.0, 1.0))
        .show(ui, |ui| {
            ui.label(
                RichText::new("implicit")
                    .color(Color32::from_rgb(0xe6, 0xff, 0xe6))
                    .small(),
            );
        });
}

/// Label for the price-currency dropdown's current id.
fn currency_label(id: &str) -> &str {
    PRICE_CURRENCIES
        .iter()
        .find(|(cid, _)| *cid == id)
        .map_or("any", |(_, label)| *label)
}

/// Display label for a resistance-search mode.
fn resistance_label(mode: ResistanceMode) -> &'static str {
    RESISTANCE_MODES
        .iter()
        .find(|(m, _, _)| *m == mode)
        .map_or("Fungible", |(_, label, _)| *label)
}

/// Label for the rarity dropdown's current id (empty id = the item's own rarity).
fn rarity_label(id: &str) -> &str {
    RARITIES
        .iter()
        .find(|(rid, _)| *rid == id)
        .map_or("Any", |(_, label)| *label)
}
