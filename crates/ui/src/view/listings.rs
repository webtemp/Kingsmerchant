//! The shared results table and its per-row chat-action buttons.

use egui::{Color32, RichText};
use egui_phosphor::regular as ph;
use trade_api::{Presence, PriceCheck};

use crate::model::fmt_amount;
use crate::SHOWN;

use super::actions::send_chat_to_poe2;
use super::item_card::{item_preview, show_item_preview_at_cursor, ItemPreview};
use super::theme::{accent_gold, online_dot as online_dot_color};

const ROW_H: f32 = 26.0;

/// One row of the results table (an item listing or an exchange offer).
pub(super) struct RowData {
    pub(super) price: String,
    pub(super) presence: Presence,
    pub(super) seller: String,
    pub(super) seller_hover: Option<String>,
    pub(super) whisper: Option<String>,
    pub(super) character: Option<String>,
    /// Instant Buyout teleport token; turns Hideout into a one-click teleport.
    pub(super) hideout_token: Option<String>,
    /// The listing's item for the hover preview; `None` for exchange offers.
    pub(super) item: Option<ItemPreview>,
    /// Corrupted listing — flagged prominently so its cheap price doesn't mislead.
    pub(super) corrupted: bool,
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
                    .color(accent_gold()),
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
                presence: l.account.presence(),
                seller: l.account.name.clone(),
                seller_hover: l.indexed.as_ref().map(|i| format!("listed {i}")),
                whisper: l.whisper.clone(),
                character: l.account.last_character_name.clone(),
                hideout_token: l.hideout_token.clone(),
                corrupted: e
                    .item
                    .get("corrupted")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                item: Some(item_preview(&e.item)),
            }
        })
        .collect();
    results_table(ui, &rows, copied, teleport);
}

/// The shared results table: striped price · seller · actions columns,
/// `vscroll(false)` so it sizes to content.
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
    // Hovering a row shows the item preview, anchored above the cursor.
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
                        presence_badge(ui, r.presence);
                        if r.corrupted {
                            ui.label(
                                RichText::new("CORRUPTED")
                                    .small()
                                    .strong()
                                    .color(Color32::from_rgb(0xff, 0x5a, 0x5a)),
                            );
                        }
                        let lbl = ui.add(egui::Label::new(&r.seller).truncate());
                        // Seller hover only when no item preview competes with it.
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
                        // contains_pointer() triggers anywhere on the row, unlike hovered().
                        if row.response().contains_pointer() {
                            show_item_preview_at_cursor(&ctx, item);
                        }
                    }
                });
            }
        });
}

/// A presence dot plus an explicit "Online" / "AFK" / "Offline" label, so a
/// grayed row reads as what it is instead of just looking dimmed.
fn presence_badge(ui: &mut egui::Ui, presence: Presence) {
    let (color, text) = match presence {
        Presence::Online => (online_dot_color(), "Online"),
        Presence::Afk => (Color32::from_rgb(0xff, 0xc8, 0x4b), "AFK"),
        Presence::Offline => (Color32::from_gray(0x70), "Offline"),
    };
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, ROW_H), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
    ui.label(RichText::new(text).small().color(color));
}

/// The four chat-action buttons (Whisper / Invite / Hideout / Trade).
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
    // Only Instant-Buyout listings carry a teleport token; show the button only
    // then. `/hideout <stranger>` is rejected by the game ("You cannot currently
    // access this player's area"), so we never offer that dead-end fallback.
    if let Some(token) = hideout_token {
        if ui
            .button(ph::HOUSE)
            .on_hover_text("Teleport to seller's hideout (Instant Buyout)")
            .clicked()
        {
            *teleport = Some(token.to_string());
            *copied = Some(format!("teleport to {seller}"));
        }
    }
    chat_button(
        ui,
        ph::HANDSHAKE,
        "Trade",
        character.map(|c| format!("/tradewith {c}")),
        copied,
    );
}

/// An icon button that sends a chat `command` into POE2; disabled when `command` is None.
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
