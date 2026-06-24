//! Background-task plumbing for [`QuickModeApp`] and the detailed-filter state.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use parser::{Item, Rarity};
use trade_api::{DetailedFilters, MiscSelection};

use crate::model::{
    build_equipment_rows, fmt_amount, open_affix_slots, parse_status, scaled_min, waystone_tier,
    EquipmentRow, ExchangePhase, MinFilter, Msg, Phase, PriceFilterState, PriceMode, ScoutPhase,
    SessionCheck, StatFilterRow,
};
use crate::{QuickModeApp, SAMPLE};

/// Pseudo stat ids for open prefix / suffix slots on craftable rares.
const EMPTY_PREFIX_STAT: &str = "pseudo.pseudo_number_of_empty_prefix_mods";
const EMPTY_SUFFIX_STAT: &str = "pseudo.pseudo_number_of_empty_suffix_mods";

/// Pseudo stat ids for unrevealed (hidden) desecrated prefix / suffix counts.
const UNREVEALED_PREFIX_STAT: &str = "pseudo.pseudo_number_of_unrevealed_prefix_mods";
const UNREVEALED_SUFFIX_STAT: &str = "pseudo.pseudo_number_of_unrevealed_suffix_mods";

/// Pseudo stat id for a tablet's remaining uses.
const USES_REMAINING_STAT: &str = "pseudo.pseudo_number_of_uses_remaining";

/// A tablet's current "N uses remaining" count; `None` for non-tablets or no line.
fn tablet_uses_remaining(item: &Item) -> Option<u32> {
    if item.item_class != "Tablet" {
        return None;
    }
    item.modifiers
        .iter()
        .flat_map(|m| &m.stats)
        .find_map(|line| {
            let n = line
                .strip_suffix(" uses remaining")
                .or_else(|| line.strip_suffix(" use remaining"))?;
            n.trim().parse::<u32>().ok()
        })
}

impl QuickModeApp {
    /// Start a fresh price check from `item_text`, rebuilding the filter panel from the item.
    pub(crate) fn start_price_check(&mut self, ctx: &egui::Context) {
        let item = match parser::parse_item(&self.item_text) {
            Ok(item) => item,
            Err(e) => {
                self.phase = Phase::Failed(format!("Not a POE2 item: {e}"));
                self.item = None;
                return;
            }
        };
        self.query_gen = self.query_gen.wrapping_add(1);
        self.icon_url = None;
        self.estimate = None;
        self.estimate_loading = false;
        self.exchange_phase = ExchangePhase::Idle;
        self.scout_phase = ScoutPhase::Idle;

        // Stackables: detected via the bulk-exchange catalogue but priced from poe2scout.
        if let Some(want_id) = self.exchange_id_for(&item) {
            self.mode = PriceMode::Exchange;
            self.exchange_want_id = want_id;
            self.item = Some(item);
            self.spawn_scout_query(ctx);
            return;
        }
        self.mode = PriceMode::Item;

        let exceptional = self.is_exceptional_base(&item);
        self.filter_body_h = 0.0;
        self.filters = self.build_filter_rows(&item);
        self.unfilterable_mods = self.client.stats().unmapped_explicit_lines(&item);
        self.equipment = build_equipment_rows(&item, self.config.filter_min_percent, exceptional);
        // Quality: on when above the normal 20% cap (bonus quality).
        let quality = item.quality.unwrap_or(0);
        self.quality_filter = MinFilter::new(quality > 20, (quality > 0).then_some(quality as u32));
        // Item level: default-on only for Normal items (crafting bases).
        let ilvl_on = item.rarity == Rarity::Normal && item.item_level.is_some();
        self.ilvl_filter = MinFilter::new(ilvl_on, item.item_level);
        let tier = waystone_tier(&item);
        self.waystone_filter = MinFilter::new(tier.is_some(), tier);
        self.rarity_filter = match item.rarity {
            Rarity::Normal => "normal",
            Rarity::Magic => "magic",
            Rarity::Rare => "rare",
            Rarity::Unique => "unique",
            _ => "",
        }
        .to_string();
        self.price_filter = PriceFilterState::default();
        self.resistance_mode = trade_api::ResistanceMode::default();
        self.filter_dirty = false;
        // Unidentified items must search the unidentified pool — the trade API
        // returns identified items by default. Reset to Any when identified.
        if let Some(m) = self.misc.iter_mut().find(|m| m.key == "identified") {
            m.state = if item.unidentified {
                trade_api::MiscState::Exclude
            } else {
                trade_api::MiscState::Any
            };
        }
        // poeprices ML estimate is rares-only and filter-independent: fetch once per check.
        if item.rarity == Rarity::Rare {
            self.spawn_estimate(ctx);
        }
        self.item = Some(item.clone());
        self.spawn_query(ctx, item);
    }

    /// Fetch the poeprices.info ML estimate for the current `item_text` (rares).
    fn spawn_estimate(&mut self, ctx: &egui::Context) {
        self.estimate_loading = true;
        let client = Arc::clone(&self.client);
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        let text = self.item_text.clone();
        let gen = self.query_gen;
        self.rt.spawn(async move {
            let result = client
                .price_estimate(&text)
                .await
                .map_err(|e| e.to_string());
            let _ = tx.send((gen, Msg::Estimate(Box::new(result))));
            ctx.request_repaint();
        });
    }

    /// Teleport into an Instant Buyout seller's hideout via the trade API. Button-only.
    pub(crate) fn spawn_teleport(&mut self, token: String, ctx: &egui::Context) {
        self.copy_status = Some("teleport".to_string());
        let client = Arc::clone(&self.client);
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        let gen = self.query_gen;
        self.rt.spawn(async move {
            let result = client
                .teleport_to_hideout(&token)
                .await
                .map_err(|e| e.to_string());
            let _ = tx.send((gen, Msg::Teleport(result)));
            ctx.request_repaint();
        });
    }

    /// Validate the configured POESESSID against the server, verdict to the Settings panel.
    pub(crate) fn spawn_session_check(&mut self, ctx: &egui::Context) {
        self.session_status = SessionCheck::Checking;
        self.last_session_check = Some(Instant::now());
        let client = Arc::clone(&self.client);
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        let gen = self.query_gen;
        self.rt.spawn(async move {
            let status = client.validate_session().await;
            let _ = tx.send((gen, Msg::SessionChecked(status)));
            ctx.request_repaint();
        });
    }

    /// The bulk-exchange currency id for a stackable item, else `None`.
    fn exchange_id_for(&self, item: &Item) -> Option<String> {
        // Only fungible commodities trade on the bulk exchange; rolled items are per-item.
        if !exchange_eligible_rarity(&item.rarity) {
            return None;
        }
        [item.name.as_deref(), item.base_type.as_deref()]
            .into_iter()
            .flatten()
            .find_map(|name| self.client.currencies().lookup(name).map(|e| e.id.clone()))
    }

    /// Price the loaded stackable from poe2scout (the primary source); falls back on failure.
    pub(crate) fn spawn_scout_query(&mut self, ctx: &egui::Context) {
        self.scout_phase = ScoutPhase::Loading;
        self.exchange_phase = ExchangePhase::Idle;
        self.last_query_at = Some(Instant::now());
        // poe2scout keys on the exchange id; the name is the search-fallback hint.
        let exchange_id = self.exchange_want_id.clone();
        let name = self
            .item
            .as_ref()
            .and_then(|i| i.name.clone().or_else(|| i.base_type.clone()))
            .unwrap_or_default();
        if exchange_id.is_empty() && name.is_empty() {
            self.scout_phase = ScoutPhase::Failed("item has no id to price".to_string());
            return;
        }
        let client = Arc::clone(&self.client);
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        tracing::info!(id = %exchange_id, currency = %name, "poe2scout price check");
        let gen = self.query_gen;
        self.rt.spawn(async move {
            let result = client
                .scout_price(&exchange_id, &name)
                .await
                .map_err(|e| e.to_string());
            if let Err(ref e) = result {
                tracing::error!(error = %e, "poe2scout price check failed");
            }
            let _ = tx.send((gen, Msg::Scout(Box::new(result))));
            ctx.request_repaint();
        });
    }

    /// Price the loaded stackable via the bulk exchange (the poe2scout fallback).
    pub(crate) fn spawn_exchange_query(&mut self, ctx: &egui::Context) {
        self.exchange_phase = ExchangePhase::Loading;
        self.last_query_at = Some(Instant::now());
        let client = Arc::clone(&self.client);
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        let want = self.exchange_want_id.clone();
        let pay = self.pay_currency.clone();
        tracing::info!(want = %want, pay = %pay, "exchange check");
        let gen = self.query_gen;
        self.rt.spawn(async move {
            let result = client
                .price_check_exchange(&want, &pay)
                .await
                .map_err(|e| e.to_string());
            if let Err(ref e) = result {
                tracing::error!(error = %e, "exchange price check failed");
            }
            let _ = tx.send((gen, Msg::Exchange(Box::new(result))));
            ctx.request_repaint();
        });
    }

    /// Re-run the price check for the already-loaded item, keeping current state.
    pub(crate) fn rerun_query(&mut self, ctx: &egui::Context) {
        self.query_gen = self.query_gen.wrapping_add(1);
        match self.mode {
            PriceMode::Item => {
                if let Some(item) = self.item.clone() {
                    self.spawn_query(ctx, item);
                }
            }
            PriceMode::Exchange => self.spawn_scout_query(ctx),
        }
    }

    /// Re-seed the stat + equipment filter minimums from the live "min roll %", then re-price.
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

    /// Whether the item's base carries a tier prefix (Exceptional/Advanced/…) the trade `type` omits.
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
                    state: m.state,
                })
                .collect(),
            quality: self.quality_filter.value(),
            item_level: self.ilvl_filter.value(),
            waystone_tier: self.waystone_filter.value(),
            rarity: (!self.rarity_filter.is_empty()).then(|| self.rarity_filter.clone()),
            price: self.price_filter.to_filter(),
            resistance_mode: self.resistance_mode,
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
        let gen = self.query_gen;
        self.rt.spawn(async move {
            let result = client
                .price_check_detailed(&item, &filters, SAMPLE)
                .await
                .map_err(|e| e.to_string());
            if let Err(ref e) = result {
                tracing::error!(error = %e, "price check failed");
            }
            let _ = tx.send((gen, Msg::Result(Box::new(result))));
            ctx.request_repaint();
        });
    }

    /// Enumerate the item's mapped stats into toggleable filter rows, deduped by stat id.
    fn build_filter_rows(&self, item: &Item) -> Vec<StatFilterRow> {
        let mut rows = Vec::new();
        let mut seen = HashSet::new();
        // Most mods start ticked; implicits and configured noise mods start unticked.
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
                is_implicit,
            });
        }

        // Granted skills: one row per skill with the exact level as min (not scaled).
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
                is_implicit: false,
            });
        }

        let count_row = |id: &str, label: &str, min: String, enabled: bool| StatFilterRow {
            id: id.to_string(),
            label: label.to_string(),
            enabled,
            min,
            max: String::new(),
            is_implicit: false,
        };

        // Tablet uses remaining: seed the min with the current count, ticked on (exact count).
        if let Some(uses) = tablet_uses_remaining(item) {
            rows.push(count_row(
                USES_REMAINING_STAT,
                "# uses remaining (Tablets)",
                uses.to_string(),
                true,
            ));
        }

        // Unrevealed desecrated mods: searchable by the count of hidden slots, exact.
        let (unrev_prefix, unrev_suffix) = trade_api::unrevealed_affix_counts(item);
        if unrev_prefix > 0 {
            rows.push(count_row(
                UNREVEALED_PREFIX_STAT,
                "# Unrevealed Prefix Modifiers",
                unrev_prefix.to_string(),
                true,
            ));
        }
        if unrev_suffix > 0 {
            rows.push(count_row(
                UNREVEALED_SUFFIX_STAT,
                "# Unrevealed Suffix Modifiers",
                unrev_suffix.to_string(),
                true,
            ));
        }

        // Open affix slots on a craftable rare: prefix/suffix row (min 1, off by default).
        let (prefix_open, suffix_open) = open_affix_slots(item);
        if prefix_open {
            rows.push(count_row(
                EMPTY_PREFIX_STAT,
                "# Empty Prefix Modifiers",
                "1".to_string(),
                false,
            ));
        }
        if suffix_open {
            rows.push(count_row(
                EMPTY_SUFFIX_STAT,
                "# Empty Suffix Modifiers",
                "1".to_string(),
                false,
            ));
        }
        rows
    }
}

/// Whether an item's rarity makes it a fungible bulk-exchange commodity.
fn exchange_eligible_rarity(rarity: &Rarity) -> bool {
    !matches!(rarity, Rarity::Magic | Rarity::Rare | Rarity::Unique)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rolled_items_are_not_bulk_exchangeable() {
        assert!(!exchange_eligible_rarity(&Rarity::Magic));
        assert!(!exchange_eligible_rarity(&Rarity::Rare));
        assert!(!exchange_eligible_rarity(&Rarity::Unique));
        assert!(exchange_eligible_rarity(&Rarity::Currency));
        assert!(exchange_eligible_rarity(&Rarity::Normal));
        assert!(exchange_eligible_rarity(&Rarity::Gem));
    }

    #[test]
    fn tablet_uses_remaining_is_extracted_for_tablets_only() {
        let tablet = parser::parse_item(
            "Item Class: Tablet\nRarity: Rare\nPhoenix Myth\nAbyss Tablet\n--------\n\
             Item Level: 82\n--------\n{ Implicit Modifier }\nAdds Abysses to a Map\n\
             10 uses remaining\n",
        )
        .expect("parses");
        assert_eq!(tablet_uses_remaining(&tablet), Some(10));

        let one = parser::parse_item(
            "Item Class: Tablet\nRarity: Rare\nX\nAbyss Tablet\n--------\n\
             { Implicit Modifier }\nAdds Abysses to a Map\n1 use remaining\n",
        )
        .expect("parses");
        assert_eq!(tablet_uses_remaining(&one), Some(1));

        let ring = parser::parse_item(
            "Item Class: Rings\nRarity: Rare\nX\nSapphire Ring\n--------\n\
             { Prefix Modifier \"P\" }\n+50 to maximum Life\n",
        )
        .expect("parses");
        assert_eq!(tablet_uses_remaining(&ring), None);
    }
}
