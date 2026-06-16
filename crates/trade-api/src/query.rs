//! Building a [`SearchRequest`] from a parsed [`parser::Item`].
//!
//! Exact bits go in `type`/`name`; affix rolls become stat filters via the
//! stat-definition snapshot. Mapped stat filters are emitted *disabled* by
//! default — the broad base/name search is what a price check wants; detailed
//! mode flips individual mods on.

use std::collections::BTreeMap;

use parser::{Item, Rarity};

use crate::definitions::{ItemDefinitions, StatDefinitions};
use crate::model::{
    EquipmentFilters, Filters, MiscFilters, OptionFilter, PriceRange, Query, SearchRequest, Sort,
    StatFilter, StatGroup, StatValue, Status, TradeFilterFields, TradeFilters, TypeFilterFields,
    TypeFilters,
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

    let rarity = rarity_option(&item.rarity).map(OptionFilter::new);
    let type_filters = if category_opt.is_some() || rarity.is_some() {
        Some(TypeFilters {
            filters: TypeFilterFields {
                category: category_opt.map(OptionFilter::new),
                rarity,
                quality: None,
                ilvl: None,
            },
        })
    } else {
        None
    };
    let filters = Filters {
        type_filters,
        equipment_filters: None,
        misc_filters: None,
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

/// One per-stat filter the user can toggle in detailed mode, built from the
/// item's mapped stats. `min`/`max` are the active range bounds.
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

/// Detailed-mode price-range filter.
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

/// One equipment-property filter in detailed mode: an item's defence or offence
/// stat (`ar`, `ev`, `es`, `block`, `spirit`, …), built from the item's parsed
/// properties rather than its affix mods.
#[derive(Debug, Clone, PartialEq)]
pub struct EquipmentSelection {
    /// Trade filter id, e.g. `ev` for Evasion.
    pub key: String,
    pub enabled: bool,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

/// A boolean misc filter (corrupted, mirrored, …); `on` → require `true`.
#[derive(Debug, Clone, PartialEq)]
pub struct MiscSelection {
    pub key: String,
    pub on: bool,
}

/// Everything the user can tune in detailed mode: listing status, per-stat
/// affix filters, equipment-property filters, and a price range.
#[derive(Debug, Clone, Default)]
pub struct DetailedFilters {
    pub status: ListingStatus,
    pub stats: Vec<StatSelection>,
    pub equipment: Vec<EquipmentSelection>,
    /// Boolean attribute filters (corrupted, identified, …).
    pub misc: Vec<MiscSelection>,
    /// Minimum item quality (goes in `type_filters.quality`); `None` = no filter.
    pub quality: Option<f64>,
    /// Minimum item level (goes in `type_filters.ilvl`); `None` = no filter.
    pub item_level: Option<f64>,
    pub price: PriceFilter,
}

/// Build the detailed-mode search request: same name / type / category as the
/// quick query, but stat filters come from explicit per-stat selections (each
/// `disabled = !enabled`, so the trade-site link mirrors what the user sees),
/// plus equipment-property and optional price-range filters.
pub fn build_detailed_query(
    item: &Item,
    items: &ItemDefinitions,
    f: &DetailedFilters,
) -> SearchRequest {
    let category_opt = category_for(&item.item_class);
    let (name, type_) = query_name_and_type(item, items, category_opt);

    let trade_filters =
        PriceRange::new(f.price.min, f.price.max, f.price.currency.clone()).map(|price| {
            TradeFilters {
                filters: TradeFilterFields { price: Some(price) },
            }
        });

    // type_filters holds the category, the rarity, and the quality/ilvl filters,
    // so emit it if any is present.
    let quality = f.quality.map(StatValue::min);
    let ilvl = f.item_level.map(StatValue::min);
    let rarity = rarity_option(&item.rarity).map(OptionFilter::new);
    let type_filters =
        if category_opt.is_some() || rarity.is_some() || quality.is_some() || ilvl.is_some() {
            Some(TypeFilters {
                filters: TypeFilterFields {
                    category: category_opt.map(OptionFilter::new),
                    rarity,
                    quality,
                    ilvl,
                },
            })
        } else {
            None
        };
    let filters = Filters {
        type_filters,
        equipment_filters: build_equipment_filters(&f.equipment),
        misc_filters: build_misc_filters(&f.misc),
        trade_filters,
    };

    let stat_filters: Vec<StatFilter> = f.stats.iter().map(StatSelection::to_filter).collect();
    let stat_groups = if stat_filters.is_empty() {
        Vec::new()
    } else {
        vec![StatGroup::and(stat_filters)]
    };

    SearchRequest {
        query: Query {
            status: Status::new(f.status.as_option()),
            type_,
            name,
            stats: stat_groups,
            filters,
        },
        sort: Some(Sort::price_asc()),
    }
}

/// Collect the checked boolean attributes into the `misc_filters` group (each
/// `{ "option": "true" }`); unchecked ones are omitted (not filtered).
fn build_misc_filters(selections: &[MiscSelection]) -> Option<MiscFilters> {
    let mut filters = BTreeMap::new();
    for s in selections {
        if s.on {
            filters.insert(s.key.clone(), OptionFilter::new("true"));
        }
    }
    if filters.is_empty() {
        None
    } else {
        Some(MiscFilters { filters })
    }
}

/// Collect the enabled equipment-property filters into the `equipment_filters`
/// group (omitting disabled / empty ones — these are plain min/max inputs on the
/// trade site, with no "disabled" state).
fn build_equipment_filters(selections: &[EquipmentSelection]) -> Option<EquipmentFilters> {
    let mut filters = BTreeMap::new();
    for s in selections {
        if !s.enabled || (s.min.is_none() && s.max.is_none()) {
            continue;
        }
        filters.insert(s.key.clone(), StatValue::range(s.min, s.max));
    }
    if filters.is_empty() {
        None
    } else {
        Some(EquipmentFilters { filters })
    }
}

/// Name/type for the query. A *rare* with a known category drops its exact base
/// and searches the whole category (comparable rares across bases). Other
/// rarities keep their exact base — a white "Prismatic Ring" must stay that
/// base (you're buying the base + implicit), not become "all rings".
fn query_name_and_type(
    item: &Item,
    items: &ItemDefinitions,
    category: Option<&str>,
) -> (Option<String>, Option<String>) {
    let (name, type_) = name_and_type(item, items);
    if category.is_some() && matches!(item.rarity, Rarity::Rare) {
        (name, None)
    } else {
        (name, type_)
    }
}

/// The trade `type_filters.rarity` option matching an item's rarity, so a search
/// returns the SAME rarity (a white-base search must not return magic items).
/// Uniques are pinned by name; gems/currency aren't rarity-filtered.
fn rarity_option(rarity: &Rarity) -> Option<&'static str> {
    match rarity {
        Rarity::Normal => Some("normal"),
        Rarity::Magic => Some("magic"),
        Rarity::Rare => Some("rare"),
        _ => None,
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
            let base = resolved_base()
                .or_else(|| item.name.as_deref().and_then(|n| items.split_magic_base(n)));
            (None, base)
        }
        // Rare drops to the category in `query_name_and_type`, so its type is
        // discarded anyway. Normal keeps its exact base — fall back to the raw
        // base line if it isn't in our snapshot, so an uncommon white base still
        // searches by its literal name rather than collapsing to "any base".
        Rarity::Rare => (None, resolved_base()),
        Rarity::Normal => (None, resolved_base().or_else(|| item.base_type.clone())),
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
