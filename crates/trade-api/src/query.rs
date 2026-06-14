//! Building a [`SearchRequest`] from a parsed [`parser::Item`] (PRD §4.4).
//!
//! What we can pin down exactly we put in `type`/`name`; the affix rolls become
//! stat filters via the stat-definition snapshot. By default the mapped stat
//! filters are emitted *disabled* — the base/name search is the broad query a
//! price check wants, and detailed mode (Phase 5) flips individual mods on.

use parser::{Item, Rarity};

use crate::definitions::{ItemDefinitions, StatDefinitions};
use crate::model::{
    Filters, OptionFilter, PriceRange, Query, SearchRequest, Sort, StatFilter, StatGroup, StatValue,
    Status, TradeFilterFields, TradeFilters, TypeFilterFields, TypeFilters,
};

/// Which listings a search should return, by seller/trade availability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ListingStatus {
    /// Seller online (default).
    #[default]
    Online,
    /// "Instant buyout" — listings buyable through GGG's automated secure
    /// trade, the ones the trade site shows a Teleport button for.
    Securable,
    /// Available to trade (online or securable).
    Available,
    /// Any listing, online or not.
    Any,
}

impl ListingStatus {
    /// The trade API `status.option` string.
    pub fn as_option(self) -> &'static str {
        match self {
            ListingStatus::Online => "online",
            ListingStatus::Securable => "securable",
            ListingStatus::Available => "available",
            ListingStatus::Any => "any",
        }
    }
}

/// Knobs for query construction.
#[derive(Debug, Clone, Copy)]
pub struct QueryOptions {
    /// Which listings to return (online / instant-buyout / …).
    pub status: ListingStatus,
    /// Emit mapped affix rolls as stat filters at all.
    pub include_stats: bool,
    /// Emit those stat filters disabled (toggleable later) rather than active.
    pub stats_disabled: bool,
}

impl Default for QueryOptions {
    fn default() -> Self {
        QueryOptions {
            status: ListingStatus::Online,
            include_stats: true,
            stats_disabled: true,
        }
    }
}

/// Build the search request body for `item`.
pub fn build_search_query(
    item: &Item,
    stats: &StatDefinitions,
    items: &ItemDefinitions,
    opts: QueryOptions,
) -> SearchRequest {
    let category_opt = category_for(&item.item_class);
    let (name, type_) = query_name_and_type(item, items, category_opt);

    let filters = Filters {
        type_filters: category_opt.map(|c| TypeFilters {
            filters: TypeFilterFields {
                category: Some(OptionFilter::new(c)),
                rarity: None,
            },
        }),
        trade_filters: None,
    };

    let stat_filters = if opts.include_stats {
        stats
            .map_item(item)
            .into_iter()
            .map(|mapped| StatFilter {
                value: mapped.filter_value().map(StatValue::min),
                id: mapped.id,
                disabled: opts.stats_disabled,
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let stat_groups = if stat_filters.is_empty() {
        Vec::new()
    } else {
        vec![StatGroup::and(stat_filters)]
    };

    SearchRequest {
        query: Query {
            status: Status::new(opts.status.as_option()),
            type_,
            name,
            stats: stat_groups,
            filters,
        },
        sort: Some(Sort::price_asc()),
    }
}

/// One per-stat filter the user can toggle in detailed mode (PRD §4.7), built
/// from the item's mapped stats. `min`/`max` are the active range bounds.
#[derive(Debug, Clone, PartialEq)]
pub struct StatSelection {
    pub id: String,
    pub enabled: bool,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

impl StatSelection {
    fn to_filter(&self) -> StatFilter {
        let value = if self.min.is_none() && self.max.is_none() {
            None
        } else {
            Some(StatValue::range(self.min, self.max))
        };
        StatFilter {
            id: self.id.clone(),
            value,
            disabled: !self.enabled,
        }
    }
}

/// Detailed-mode price-range filter (PRD §4.7).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PriceFilter {
    pub min: Option<f64>,
    pub max: Option<f64>,
    /// Currency the bounds are in (`exalted`, `divine`, …); `None` = any.
    pub currency: Option<String>,
}

impl PriceFilter {
    pub fn is_empty(&self) -> bool {
        self.min.is_none() && self.max.is_none()
    }
}

/// Build the detailed-mode search request (PRD §4.7): same exact name / type /
/// category as the quick query, but the stat filters come from explicit
/// per-stat `selections` (each emitted with `disabled = !enabled`, so the
/// resulting trade-site link mirrors exactly what the user sees), plus an
/// optional price-range filter.
pub fn build_detailed_query(
    item: &Item,
    items: &ItemDefinitions,
    status: ListingStatus,
    selections: &[StatSelection],
    price: &PriceFilter,
) -> SearchRequest {
    let category_opt = category_for(&item.item_class);
    let (name, type_) = query_name_and_type(item, items, category_opt);

    let trade_filters = PriceRange::new(price.min, price.max, price.currency.clone())
        .map(|price| TradeFilters {
            filters: TradeFilterFields { price: Some(price) },
        });

    let filters = Filters {
        type_filters: category_opt.map(|c| TypeFilters {
            filters: TypeFilterFields {
                category: Some(OptionFilter::new(c)),
                rarity: None,
            },
        }),
        trade_filters,
    };

    let stat_filters: Vec<StatFilter> = selections.iter().map(StatSelection::to_filter).collect();
    let stat_groups = if stat_filters.is_empty() {
        Vec::new()
    } else {
        vec![StatGroup::and(stat_filters)]
    };

    SearchRequest {
        query: Query {
            status: Status::new(status.as_option()),
            type_,
            name,
            stats: stat_groups,
            filters,
        },
        sort: Some(Sort::price_asc()),
    }
}

/// Name/type for the query. For rares/normals with a known category we drop the
/// exact base type and search the whole *category* instead (e.g. "body armour",
/// not "Corsair Coat") — we want comparable items across bases, not just the
/// same base. Uniques/magic keep their type (the base is the point there).
fn query_name_and_type(
    item: &Item,
    items: &ItemDefinitions,
    category: Option<&str>,
) -> (Option<String>, Option<String>) {
    let (name, type_) = name_and_type(item, items);
    if category.is_some() && matches!(item.rarity, Rarity::Rare | Rarity::Normal) {
        (name, None)
    } else {
        (name, type_)
    }
}

/// Derive the exact-match `(name, type)` pair for the query.
fn name_and_type(item: &Item, items: &ItemDefinitions) -> (Option<String>, Option<String>) {
    // The parsed base line can carry a display-tier prefix GGG's trade `type`
    // omits ("Exceptional Crude Bow" → "Crude Bow"); resolve it to a known base.
    let resolved_base = || {
        item.base_type
            .as_deref()
            .and_then(|b| items.resolve_base(b))
    };
    match item.rarity {
        Rarity::Unique => {
            let base = resolved_base().or_else(|| {
                item.name
                    .as_deref()
                    .and_then(|n| items.unique_base(n))
                    .map(str::to_string)
            });
            (item.name.clone(), base)
        }
        // The parser leaves a magic item's base fused in `name`; split it out.
        Rarity::Magic => {
            let base =
                resolved_base().or_else(|| item.name.as_deref().and_then(|n| items.split_magic_base(n)));
            (None, base)
        }
        Rarity::Rare | Rarity::Normal => (None, resolved_base()),
        // Gems / currency: the single name line *is* the trade `type`.
        Rarity::Gem | Rarity::Currency | Rarity::Other(_) => (None, item.name.clone()),
    }
}

/// Map a POE2 `Item Class:` to the trade2 `category` filter option.
/// Returns `None` for classes we don't filter on (the `type` match suffices).
pub fn category_for(item_class: &str) -> Option<&'static str> {
    let c = match item_class {
        "Rings" => "accessory.ring",
        "Amulets" => "accessory.amulet",
        "Belts" => "accessory.belt",
        "Wands" => "weapon.wand",
        "Sceptres" => "weapon.sceptre",
        "Staves" => "weapon.staff",
        "Quarterstaves" => "weapon.warstaff",
        "Bows" => "weapon.bow",
        "Crossbows" => "weapon.crossbow",
        "Spears" => "weapon.spear",
        "Flails" => "weapon.flail",
        "Daggers" => "weapon.dagger",
        "Claws" => "weapon.claw",
        "One Hand Maces" => "weapon.onemace",
        "Two Hand Maces" => "weapon.twomace",
        "One Hand Swords" => "weapon.onesword",
        "Two Hand Swords" => "weapon.twosword",
        "One Hand Axes" => "weapon.oneaxe",
        "Two Hand Axes" => "weapon.twoaxe",
        "Body Armours" => "armour.chest",
        "Helmets" => "armour.helmet",
        "Gloves" => "armour.gloves",
        "Boots" => "armour.boots",
        "Quivers" => "armour.quiver",
        "Shields" => "armour.shield",
        "Foci" => "armour.focus",
        "Bucklers" => "armour.buckler",
        "Jewels" => "jewel",
        "Life Flasks" => "flask.life",
        "Mana Flasks" => "flask.mana",
        "Charms" => "flask.charm",
        "Waystones" => "map.waystone",
        "Relics" => "sanctum.relic",
        "Skill Gems" => "gem.activegem",
        "Support Gems" => "gem.supportgem",
        "Meta Gems" => "gem.metagem",
        _ => return None,
    };
    Some(c)
}
