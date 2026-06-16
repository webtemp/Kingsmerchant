//! Background-task plumbing for [`QuickModeApp`]: spawning the per-item search,
//! the bulk-exchange query, the poeprices ML estimate and the hideout teleport
//! onto the tokio runtime, plus building/snapshotting the detailed-filter state
//! that drives those requests. No rendering lives here.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use parser::{Item, Rarity};
use trade_api::{DetailedFilters, MiscSelection};

use crate::model::{
    build_equipment_rows, fmt_amount, parse_status, scaled_min, EquipmentRow, ExchangePhase,
    MinFilter, Msg, Phase, PriceFilterState, PriceMode, StatFilterRow,
};
use crate::{QuickModeApp, SAMPLE};

impl QuickModeApp {
    /// Start a *fresh* price check from `item_text` (a new Ctrl+C, manual button,
    /// or paste). Rebuilds the filter panel from the item and resets the price
    /// filter; a filter-driven re-query uses [`rerun_query`](Self::rerun_query)
    /// instead so toggles survive.
    pub(crate) fn start_price_check(&mut self, ctx: &egui::Context) {
        let item = match parser::parse_item(&self.item_text) {
            Ok(item) => item,
            Err(e) => {
                self.phase = Phase::Failed(format!("Not a POE2 item: {e}"));
                self.item = None;
                return;
            }
        };
        self.icon_url = None;
        self.estimate = None;
        self.estimate_loading = false;
        self.exchange_phase = ExchangePhase::Idle;

        // Stackables (currency, runes, fragments, …) aren't sold as individual
        // listings — they trade via the bulk exchange. Route them there.
        if let Some(want_id) = self.exchange_id_for(&item) {
            self.mode = PriceMode::Exchange;
            self.exchange_want_id = want_id;
            self.item = Some(item);
            self.spawn_exchange_query(ctx);
            return;
        }
        self.mode = PriceMode::Item;

        // "Exceptional" bases carry a tier prefix resolve_base strips; on those
        // the extra sockets/quality are the value, so default those filters on.
        let exceptional = self.is_exceptional_base(&item);
        self.filters = self.build_filter_rows(&item);
        self.equipment = build_equipment_rows(&item, self.config.filter_min_percent, exceptional);
        // Quality: on when above the normal 20% cap (bonus quality).
        let quality = item.quality.unwrap_or(0);
        self.quality_filter = MinFilter::new(quality > 20, (quality > 0).then_some(quality as u32));
        // Item level: on for any item that has one (a major price driver).
        self.ilvl_filter = MinFilter::new(item.item_level.is_some(), item.item_level);
        self.price_filter = PriceFilterState::default();
        self.filter_dirty = false; // no stale debounce from the previous item
                                   // poeprices ML estimate is rares-only and
                                   // filter-independent, so fetch it once per check.
        if item.rarity == Rarity::Rare {
            self.spawn_estimate(ctx);
        }
        self.item = Some(item.clone());
        self.spawn_query(ctx, item);
    }

    /// Fetch the poeprices.info ML estimate for the current `item_text` on a
    /// background task (rares).
    fn spawn_estimate(&mut self, ctx: &egui::Context) {
        self.estimate_loading = true;
        let client = Arc::clone(&self.client);
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        let text = self.item_text.clone();
        self.rt.spawn(async move {
            let result = client
                .price_estimate(&text)
                .await
                .map_err(|e| e.to_string());
            let _ = tx.send(Msg::Estimate(Box::new(result)));
            ctx.request_repaint();
        });
    }

    /// Teleport into an Instant Buyout seller's hideout via the trade API.
    /// `token` is the listing's short-lived `hideout_token`. Runs off the UI
    /// thread; on failure the error surfaces in the status line. POE2 pulls the
    /// character in the moment GGG accepts it, so only fire it on a button click.
    pub(crate) fn spawn_teleport(&mut self, token: String, ctx: &egui::Context) {
        self.copy_status = Some("teleport".to_string());
        let client = Arc::clone(&self.client);
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        self.rt.spawn(async move {
            let result = client
                .teleport_to_hideout(&token)
                .await
                .map_err(|e| e.to_string());
            let _ = tx.send(Msg::Teleport(result));
            ctx.request_repaint();
        });
    }

    /// The bulk-exchange currency id for an item, if it's a stackable that
    /// prices via the exchange rather than the per-item search. Tries the item
    /// name then the base-type line against the `data/static` catalogue.
    fn exchange_id_for(&self, item: &Item) -> Option<String> {
        [item.name.as_deref(), item.base_type.as_deref()]
            .into_iter()
            .flatten()
            .find_map(|name| self.client.currencies().lookup(name).map(|e| e.id.clone()))
    }

    /// Price the loaded stackable via the bulk exchange on a background task,
    /// paying in the currently-selected `pay_currency`.
    pub(crate) fn spawn_exchange_query(&mut self, ctx: &egui::Context) {
        self.exchange_phase = ExchangePhase::Loading;
        self.last_query_at = Some(Instant::now());
        let client = Arc::clone(&self.client);
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        let want = self.exchange_want_id.clone();
        let pay = self.pay_currency.clone();
        tracing::info!(want = %want, pay = %pay, "exchange check");
        self.rt.spawn(async move {
            let result = client
                .price_check_exchange(&want, &pay)
                .await
                .map_err(|e| e.to_string());
            if let Err(ref e) = result {
                tracing::error!(error = %e, "exchange price check failed");
            }
            let _ = tx.send(Msg::Exchange(Box::new(result)));
            ctx.request_repaint();
        });
    }

    /// Re-run the price check for the already-loaded item, keeping current
    /// state (used by the detailed panel, the pay-currency selector, and a
    /// league switch). Routes to the search or the exchange per the mode.
    pub(crate) fn rerun_query(&mut self, ctx: &egui::Context) {
        match self.mode {
            PriceMode::Item => {
                if let Some(item) = self.item.clone() {
                    self.spawn_query(ctx, item);
                }
            }
            PriceMode::Exchange => self.spawn_exchange_query(ctx),
        }
    }

    /// Re-seed the stat + equipment filter minimums for the loaded item from the
    /// live "min roll %" (and noise/implicit defaults), then re-price — so those
    /// settings take effect on the item on screen without re-copying. Discards
    /// manual filter tweaks, but leaves quality/ilvl/price as set.
    pub(crate) fn reseed_filters(&mut self, ctx: &egui::Context) {
        if self.mode != PriceMode::Item {
            return;
        }
        let Some(item) = self.item.clone() else {
            return;
        };
        let exceptional = self.is_exceptional_base(&item);
        self.filters = self.build_filter_rows(&item);
        self.equipment = build_equipment_rows(&item, self.config.filter_min_percent, exceptional);
        self.filter_dirty = false;
        self.rerun_query(ctx);
    }

    /// Whether the item's base carries a tier prefix (Exceptional/Advanced/…)
    /// that the trade `type` omits — i.e. a high-tier base where the extra
    /// sockets / quality drive the price.
    fn is_exceptional_base(&self, item: &Item) -> bool {
        item.base_type.as_deref().is_some_and(|raw| {
            self.client
                .items()
                .resolve_base(raw)
                .is_some_and(|resolved| resolved != raw)
        })
    }

    /// Snapshot the current panel state into the trade-api filter struct.
    pub(crate) fn detailed_filters(&self) -> DetailedFilters {
        DetailedFilters {
            status: parse_status(&self.config.trade_status),
            stats: self.filters.iter().map(StatFilterRow::selection).collect(),
            equipment: self.equipment.iter().map(EquipmentRow::selection).collect(),
            misc: self
                .misc
                .iter()
                .map(|m| MiscSelection {
                    key: m.key.to_string(),
                    on: m.on,
                })
                .collect(),
            quality: self.quality_filter.value(),
            item_level: self.ilvl_filter.value(),
            price: self.price_filter.to_filter(),
        }
    }

    /// Spawn the background search/fetch for `item` using the current filters.
    fn spawn_query(&mut self, ctx: &egui::Context, item: Item) {
        tracing::info!(
            item = item
                .name
                .as_deref()
                .or(item.base_type.as_deref())
                .unwrap_or("?"),
            "search"
        );
        self.phase = Phase::Loading;
        self.last_query_at = Some(Instant::now());
        let client = Arc::clone(&self.client);
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        let filters = self.detailed_filters();
        self.rt.spawn(async move {
            let result = client
                .price_check_detailed(&item, &filters, SAMPLE)
                .await
                .map_err(|e| e.to_string());
            if let Err(ref e) = result {
                // Log so it's easy to copy out of the terminal, not just the popup.
                tracing::error!(error = %e, "price check failed");
            }
            let _ = tx.send(Msg::Result(Box::new(result)));
            ctx.request_repaint();
        });
    }

    /// Enumerate the item's mapped stats into toggleable filter rows, deduped by
    /// stat id, with the rolled value pre-filled as the min (blank max).
    fn build_filter_rows(&self, item: &Item) -> Vec<StatFilterRow> {
        let mut rows = Vec::new();
        let mut seen = HashSet::new();
        // Mapped with the item's local/global context (e.g. local evasion on
        // body armour). Most mods start ticked so the first search matches;
        // implicits and the configured noise mods start unticked.
        for mapped in self.client.stats().map_item(item) {
            if !seen.insert(mapped.id.clone()) {
                continue;
            }
            let rolled = mapped.filter_value();
            let is_implicit = mapped.stat_type == "implicit";
            let off = self
                .config
                .filter_off_by_default(&mapped.template, is_implicit);
            let pct = self.config.filter_min_percent;
            rows.push(StatFilterRow {
                id: mapped.id,
                label: mapped.template,
                enabled: !off,
                min: rolled
                    .map(|v| fmt_amount(scaled_min(v, pct)))
                    .unwrap_or_default(),
                max: String::new(),
                rolled,
                is_implicit,
            });
        }

        // Granted skills (e.g. "Grants Skill: Level 19 Discipline"): the level is
        // the price driver, so add a row per skill with the exact level as the
        // min — not scaled, since you want at-least-this-level.
        for prop in &item.properties {
            if prop.name != "Grants Skill" {
                continue;
            }
            let Some(mapped) = self.client.stats().map_granted_skill(&prop.value) else {
                continue;
            };
            if !seen.insert(mapped.id.clone()) {
                continue;
            }
            let level = mapped.filter_value();
            rows.push(StatFilterRow {
                id: mapped.id,
                label: mapped.template,
                enabled: true,
                min: level.map(fmt_amount).unwrap_or_default(),
                max: String::new(),
                rolled: level,
                is_implicit: false,
            });
        }
        rows
    }
}
