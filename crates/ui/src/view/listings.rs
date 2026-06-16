//! The shared results table (item listings and exchange offers) and the
//! per-row chat-action buttons.

use egui::{Color32, RichText};
use egui_phosphor::regular as ph;
use trade_api::PriceCheck;

use crate::model::fmt_amount;
use crate::SHOWN;

use super::actions::send_chat_to_poe2;
use super::item_card::{item_preview, show_item_preview_at_cursor, ItemPreview};
use super::theme::{ACCENT_GOLD, ONLINE_DOT};

/// Height of a results-table row.
const ROW_H: f32 = 26.0;

/// One row of the results table (an item listing or an exchange offer).
pub(super) struct RowData {
    pub(super) price: String,
    pub(super) online: bool,
    pub(super) seller: String,
    pub(super) seller_hover: Option<String>,
    pub(super) whisper: Option<String>,
    pub(super) character: Option<String>,
    /// Instant Buyout teleport token (authenticated fetch only). When present the
    /// Hideout button becomes a one-click teleport into the seller's hideout.
    pub(super) hideout_token: Option<String>,
    /// The actual item for this listing (icon + mods), for the hover preview.
    /// `None` for currency-exchange offers.
    pub(super) item: Option<ItemPreview>,
}

pub(super) fn show_results(
    ui: &mut egui::Ui,
    pc: &PriceCheck,
    copied: &mut Option<String>,
    teleport: &mut Option<String>,
) {
    match pc.median_price() {
        Some(p) => {
            ui.label(
                RichText::new(format!("Median: {} {}", fmt_amount(p.amount), p.currency))
                    .size(18.0)
                    .strong()
                    .color(ACCENT_GOLD),
            );
        }
        None => {
            ui.label(RichText::new("No priced listings.").italics());
        }
    }
    ui.label(
        RichText::new(format!(
            "{} online listing(s) · showing cheapest {}",
            pc.total, SHOWN
        ))
        .weak(),
    );
    ui.add_space(6.0);

    let rows: Vec<RowData> = pc
        .cheapest(SHOWN)
        .into_iter()
        .map(|e| {
            let l = &e.listing;
            RowData {
                price: l.price.as_ref().map_or_else(
                    || "—".to_string(),
                    |p| format!("{} {}", fmt_amount(p.amount), p.currency),
                ),
                online: l.is_online(),
                seller: l.account.name.clone(),
                seller_hover: l.indexed.as_ref().map(|i| format!("listed {i}")),
                whisper: l.whisper.clone(),
                character: l.account.last_character_name.clone(),
                hideout_token: l.hideout_token.clone(),
                item: Some(item_preview(&e.item)),
            }
        })
        .collect();
    results_table(ui, &rows, copied, teleport);
}

/// The shared results table (item listings and exchange offers): striped,
/// full-width columns — price (auto) · seller (fills, truncates) · actions
/// (auto). `vscroll(false)` so it sizes to content in the auto-height popup.
pub(super) fn results_table(
    ui: &mut egui::Ui,
    rows: &[RowData],
    copied: &mut Option<String>,
    teleport: &mut Option<String>,
) {
    use egui_extras::{Column, TableBuilder};
    if rows.is_empty() {
        return;
    }
    // Hovering anywhere on a row shows the item preview, anchored above-left of
    // the cursor so it doesn't cover the action buttons.
    let ctx = ui.ctx().clone();
    TableBuilder::new(ui)
        .striped(true)
        .vscroll(false)
        .sense(egui::Sense::hover())
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .column(Column::auto().at_least(70.0))
        .column(Column::remainder().clip(true))
        .column(Column::auto())
        .body(|mut body| {
            for r in rows {
                body.row(ROW_H, |mut row| {
                    row.col(|ui| {
                        ui.label(RichText::new(&r.price).strong());
                    });
                    row.col(|ui| {
                        online_dot(ui, r.online);
                        let lbl = ui.add(egui::Label::new(&r.seller).truncate());
                        // Seller hover (listed-date / stock) only when there's no
                        // item preview competing with it (exchange offers).
                        if r.item.is_none() {
                            if let Some(h) = &r.seller_hover {
                                lbl.on_hover_text(h);
                            }
                        }
                    });
                    row.col(|ui| {
                        action_buttons(
                            ui,
                            r.whisper.as_deref(),
                            r.character.as_deref(),
                            r.hideout_token.as_deref(),
                            &r.seller,
                            copied,
                            teleport,
                        );
                    });
                    if let Some(item) = &r.item {
                        // contains_pointer() triggers anywhere on the row;
                        // hovered() can be false when a child widget is the
                        // hover target.
                        if row.response().contains_pointer() {
                            show_item_preview_at_cursor(&ctx, item);
                        }
                    }
                });
            }
        });
}

/// A painted online-status dot (the glyph renders as tofu, so paint it
/// directly). Green = online, grey = offline.
fn online_dot(ui: &mut egui::Ui, online: bool) {
    let color = if online {
        ONLINE_DOT
    } else {
        Color32::from_gray(0x70)
    };
    let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, ROW_H), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

/// The four chat-action buttons (Whisper / Invite / Hideout / Trade) shared by
/// item listings and exchange offers — Phosphor icons with hover labels.
fn action_buttons(
    ui: &mut egui::Ui,
    whisper: Option<&str>,
    character: Option<&str>,
    hideout_token: Option<&str>,
    seller: &str,
    copied: &mut Option<String>,
    teleport: &mut Option<String>,
) {
    if let Some(w) = whisper {
        if ui
            .button(ph::CHAT_CIRCLE_DOTS)
            .on_hover_text("Whisper (sends in POE2)")
            .clicked()
        {
            send_chat_to_poe2(w.to_string());
            *copied = Some(format!("whisper to {seller}"));
        }
    } else {
        ui.add_enabled(false, egui::Button::new(ph::CHAT_CIRCLE_DOTS))
            .on_hover_text("Whisper (unavailable)");
    }
    chat_button(
        ui,
        ph::USER_PLUS,
        "Invite",
        character.map(|c| format!("/invite {c}")),
        copied,
    );
    // Hideout: an Instant Buyout listing carries a teleport token → one-click
    // travel into the seller's hideout. Otherwise fall back to the
    // `/hideout <char>` chat command.
    if let Some(token) = hideout_token {
        if ui
            .button(ph::HOUSE)
            .on_hover_text("Teleport to seller's hideout (Instant Buyout)")
            .clicked()
        {
            *teleport = Some(token.to_string());
            *copied = Some(format!("teleport to {seller}"));
        }
    } else {
        chat_button(
            ui,
            ph::HOUSE,
            "Hideout",
            character.map(|c| format!("/hideout {c}")),
            copied,
        );
    }
    chat_button(
        ui,
        ph::HANDSHAKE,
        "Trade",
        character.map(|c| format!("/tradewith {c}")),
        copied,
    );
}

/// An icon button that sends a chat `command` into POE2. `name` is the hover
/// label. Disabled (greyed) when we couldn't build a command (e.g. the listing
/// has no character name).
fn chat_button(
    ui: &mut egui::Ui,
    icon: &str,
    name: &str,
    command: Option<String>,
    copied: &mut Option<String>,
) {
    match command {
        Some(cmd) => {
            if ui
                .button(icon)
                .on_hover_text(format!("{name} (sends in POE2)"))
                .clicked()
            {
                send_chat_to_poe2(cmd.clone());
                *copied = Some(cmd);
            }
        }
        None => {
            ui.add_enabled(false, egui::Button::new(icon))
                .on_hover_text(format!("{name} (no character name)"));
        }
    }
}
