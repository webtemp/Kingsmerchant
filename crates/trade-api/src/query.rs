//! Building a [`SearchRequest`] from a parsed [`parser::Item`] (PRD §4.4).
//!
//! What we can pin down exactly we put in `type`/`name`; the affix rolls become
//! stat filters via the stat-definition snapshot. By default the mapped stat
//! filters are emitted *disabled* — the base/name search is the broad query a
//! price check wants, and detailed mode (Phase 5) flips individual mods on.

use parser::{Item, Rarity};

use crate::definitions::{ItemDefinitions, StatDefinitions};
use crate::model::{
    Filters, OptionFilter, Query, SearchRequest, Sort, StatFilter, StatGroup, StatValue, Status,
    TypeFilterFields, TypeFilters,
};

/// Knobs for query construction.
#[derive(Debug, Clone, Copy)]
pub struct QueryOptions {
    /// Emit mapped affix rolls as stat filters at all.
    pub include_stats: bool,
    /// Emit those stat filters disabled (toggleable later) rather than active.
    pub stats_disabled: bool,
}

impl Default for QueryOptions {
    fn default() -> Self {
        QueryOptions {
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
    let (name, type_) = name_and_type(item, items);

    let category = category_for(&item.item_class).map(OptionFilter::new);
    let filters = Filters {
        type_filters: category.map(|category| TypeFilters {
            filters: TypeFilterFields {
                category: Some(category),
                rarity: None,
            },
        }),
    };

    let stat_filters = if opts.include_stats {
        item.modifiers
            .iter()
            .flat_map(|m| stats.map_modifier(m))
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
            status: Status::online(),
            type_,
            name,
            stats: stat_groups,
            filters,
        },
        sort: Some(Sort::price_asc()),
    }
}

/// Derive the exact-match `(name, type)` pair for the query.
fn name_and_type(item: &Item, items: &ItemDefinitions) -> (Option<String>, Option<String>) {
    match item.rarity {
        Rarity::Unique => {
            let base = item.base_type.clone().or_else(|| {
                item.name
                    .as_deref()
                    .and_then(|n| items.unique_base(n))
                    .map(str::to_string)
            });
            (item.name.clone(), base)
        }
        // The parser leaves a magic item's base fused in `name`; split it out.
        Rarity::Magic => {
            let base = item
                .base_type
                .clone()
                .or_else(|| item.name.as_deref().and_then(|n| items.split_magic_base(n)));
            (None, base)
        }
        Rarity::Rare | Rarity::Normal => (None, item.base_type.clone()),
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
