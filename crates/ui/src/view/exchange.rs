//! The pricing view for stackables (currency/runes/fragments/…), priced from
//! poe2scout with the official bulk exchange kept only as a fallback.

use std::fmt::Write as _;

use egui::{Color32, RichText};
use egui_phosphor::regular as ph;
use trade_api::{ExchangeOffer, ScoutPrice};

use crate::model::{fmt_amount, ExchangePhase, ScoutPhase};
use crate::{QuickModeApp, SHOWN};

use super::listings::{results_table, RowData};
use super::percent_encode;
use super::theme::accent_gold;

const WARN_GOLD: Color32 = Color32::from_rgb(0xff, 0xc8, 0x4b);
const DIVINE_BLUE: Color32 = Color32::from_rgb(0x9f, 0xb4, 0xff);

const PAY_CURRENCIES: &[(&str, &str)] = &[
    ("exalted", "Exalted"),
    ("divine", "Divine"),
    ("chaos", "Chaos"),
];

impl QuickModeApp {
    /// Render the stackable pricing view: poe2scout card, else the exchange fallback.
    pub(super) fn exchange_content(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        copied: &mut Option<String>,
        open_trade: &mut Option<String>,
    ) {
        match &self.scout_phase {
            ScoutPhase::Idle | ScoutPhase::Loading => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("checking poe2scout economy…");
                });
            }
            ScoutPhase::Done(price) => {
                self.scout_card(ui, price, open_trade);
            }
            ScoutPhase::NotFound => {
                ui.colored_label(
                    WARN_GOLD,
                    format!(
                        "{} No poe2scout economy data for this item — showing the \
                         official exchange instead.",
                        ph::WARNING
                    ),
                );
                ui.separator();
                self.exchange_fallback(ui, ctx, copied, open_trade);
            }
            ScoutPhase::Failed(cause) => {
                ui.colored_label(
                    WARN_GOLD,
                    format!(
                        "{} poe2scout is unavailable — showing the official exchange \
                         instead.",
                        ph::WARNING
                    ),
                )
                .on_hover_text(cause.clone());
                ui.separator();
                self.exchange_fallback(ui, ctx, copied, open_trade);
            }
        }
    }

    /// The poe2scout info card: value, recent range, freshness, exchange link.
    fn scout_card(&self, ui: &mut egui::Ui, price: &ScoutPrice, open_trade: &mut Option<String>) {
        if let Some(name) = price.text.as_deref() {
            ui.label(RichText::new(name).strong());
        }
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("{} ex", fmt_amount(price.exalted)))
                    .size(20.0)
                    .strong()
                    .color(accent_gold()),
            );
            if let Some(div) = price.divine {
                ui.label(
                    RichText::new(format!("≈ {} div", fmt_amount(div)))
                        .size(15.0)
                        .color(DIVINE_BLUE),
                );
            }
        });

        if let (Some(lo), Some(hi)) = (price.low, price.high) {
            if hi > lo {
                ui.label(
                    RichText::new(format!(
                        "recent range {} – {} ex",
                        fmt_amount(lo),
                        fmt_amount(hi)
                    ))
                    .weak(),
                );
            }
        }
        if let Some(vol) = price.volume {
            ui.label(
                RichText::new(format!("recent listed qty ~{}", fmt_amount(vol)))
                    .weak()
                    .small(),
            );
        }

        let mut meta = String::from("via poe2scout economy");
        if let Some(rate) = price.divine_price {
            let _ = write!(meta, " · {} ex / div", fmt_amount(rate));
        }
        if let Some(secs) = self.last_query_at.map(|t| t.elapsed().as_secs()) {
            let _ = write!(meta, " · updated {}", fmt_ago(secs));
        }
        ui.label(RichText::new(meta).weak().small());
        if let Some(as_of) = &price.as_of {
            ui.label(
                RichText::new(format!("latest sample: {as_of}"))
                    .weak()
                    .small(),
            );
        }

        ui.add_space(6.0);
        ui.separator();
        if ui
            .button(format!("{} Open on trade site", ph::GLOBE))
            .on_hover_text("Opens the official currency exchange for this league")
            .clicked()
        {
            *open_trade = Some(exchange_page_url(&self.config.league));
        }
    }

    /// The official bulk-exchange listings, shown only when poe2scout has nothing.
    fn exchange_fallback(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        copied: &mut Option<String>,
        open_trade: &mut Option<String>,
    ) {
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
        ui.colored_label(
            WARN_GOLD,
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
                                .color(accent_gold()),
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

fn fmt_ago(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s ago")
    } else {
        format!("{}m ago", secs / 60)
    }
}

fn pay_label(id: &str) -> &str {
    PAY_CURRENCIES
        .iter()
        .find(|(i, _)| *i == id)
        .map_or("Exalted", |(_, l)| *l)
}

/// Link to the official currency exchange page for a league.
fn exchange_page_url(league: &str) -> String {
    format!(
        "https://www.pathofexile.com/trade2/exchange/poe2/{}",
        percent_encode(league)
    )
}

fn exchange_url(league: &str, id: &str) -> String {
    format!(
        "https://www.pathofexile.com/trade2/exchange/poe2/{}/{}",
        percent_encode(league),
        id
    )
}

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
