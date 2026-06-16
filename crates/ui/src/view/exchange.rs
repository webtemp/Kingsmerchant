//! The bulk-exchange view for stackables (currency/runes/…), priced via the
//! official currency exchange rather than per-item search.

use egui::{Color32, RichText};
use egui_phosphor::regular as ph;
use trade_api::ExchangeOffer;

use crate::model::{fmt_amount, ExchangePhase};
use crate::{QuickModeApp, SHOWN};

use super::listings::{results_table, RowData};
use super::percent_encode;
use super::theme::ACCENT_GOLD;

/// Currencies offered in the exchange "pay with" selector (id, label).
const PAY_CURRENCIES: &[(&str, &str)] = &[
    ("exalted", "Exalted"),
    ("divine", "Divine"),
    ("chaos", "Chaos"),
];

impl QuickModeApp {
    /// Render the bulk-exchange results for a stackable: a pay-with currency
    /// selector, the median + cheapest offers with whisper buttons, and a link
    /// to the exchange page. No stat filters (they don't apply).
    pub(super) fn exchange_content(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        copied: &mut Option<String>,
        open_trade: &mut Option<String>,
    ) {
        // Pick what to pay with. Offers are listed in the seller's currency, so
        // we query one currency at a time (default Exalted) and the list stays
        // sortable — sidestepping exalted-vs-divine normalisation.
        ui.horizontal(|ui| {
            ui.label("Pay with");
            let before = self.pay_currency.clone();
            egui::ComboBox::from_id_salt("pay-currency")
                .selected_text(pay_label(&self.pay_currency))
                .show_ui(ui, |ui| {
                    for (id, label) in PAY_CURRENCIES {
                        ui.selectable_value(&mut self.pay_currency, id.to_string(), *label);
                    }
                });
            if self.pay_currency != before {
                self.spawn_exchange_query(ctx);
            }
        });
        if matches!(self.exchange_phase, ExchangePhase::Loading) {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("checking exchange…");
            });
        }
        // Player exchange listings are often stale/unreliable for currency —
        // point the user at the in-game Currency Exchange.
        ui.colored_label(
            Color32::from_rgb(0xff, 0xc8, 0x4b),
            format!(
                "{} These player prices are often stale. For currency, the in-game \
                 Currency Exchange is more reliable.",
                ph::WARNING
            ),
        );
        ui.separator();

        match &self.exchange_phase {
            ExchangePhase::Idle => {
                ui.label(RichText::new("Waiting for the exchange…").weak().italics());
            }
            ExchangePhase::Loading => {}
            ExchangePhase::Failed(e) => {
                ui.colored_label(Color32::from_rgb(0xff, 0x6b, 0x6b), e);
            }
            ExchangePhase::Done(ex) => {
                let pay = pay_label(&ex.pay_currency);
                match ex.median_unit_price() {
                    Some(m) => {
                        ui.label(
                            RichText::new(format!("Median: {} {} each", fmt_amount(m), pay))
                                .size(18.0)
                                .strong()
                                .color(ACCENT_GOLD),
                        );
                    }
                    None => {
                        ui.label(
                            RichText::new(format!(
                                "No {pay} offers — try a different pay currency."
                            ))
                            .italics(),
                        );
                    }
                }
                ui.label(
                    RichText::new(format!(
                        "{} offer(s) · showing cheapest {}",
                        ex.offers.len(),
                        SHOWN
                    ))
                    .weak(),
                );
                ui.add_space(6.0);
                let rows = exchange_rows(ex.cheapest(SHOWN), pay);
                // Exchange offers never carry a hideout teleport token.
                results_table(ui, &rows, copied, &mut None);
                ui.add_space(6.0);
                if ui
                    .button(format!("{} Open exchange page", ph::GLOBE))
                    .on_hover_text("Opens the in-game-style currency exchange in your browser")
                    .clicked()
                {
                    *open_trade = Some(exchange_url(&self.config.league, &ex.id));
                }
            }
        }
    }
}

fn pay_label(id: &str) -> &str {
    PAY_CURRENCIES
        .iter()
        .find(|(i, _)| *i == id)
        .map_or("Exalted", |(_, l)| *l)
}

/// Deep link to the bulk-exchange page for a result.
fn exchange_url(league: &str, id: &str) -> String {
    format!(
        "https://www.pathofexile.com/trade2/exchange/poe2/{}/{}",
        percent_encode(league),
        id
    )
}

/// Build the results-table rows for bulk-exchange offers (no item preview).
fn exchange_rows(offers: &[ExchangeOffer], pay: &str) -> Vec<RowData> {
    offers
        .iter()
        .map(|o| RowData {
            price: format!("{} {}", fmt_amount(o.unit_price), pay),
            online: o.online,
            seller: o.account.clone(),
            seller_hover: o.stock.map(|s| format!("stock: {}", fmt_amount(s))),
            whisper: o.whisper.clone(),
            character: o.character.clone(),
            hideout_token: None,
            item: None,
        })
        .collect()
}
