//! Building a [`SearchRequest`] from a parsed [`parser::Item`].

use std::collections::BTreeMap;

use parser::{Item, Rarity};

use crate::definitions::{ItemDefinitions, StatDefinitions};
use crate::model::{
    EquipmentFilters, Filters, MapFilterFields, MapFilters, MiscFilters, OptionFilter, PriceRange,
    Query, SearchRequest, Sort, StatFilter, StatGroup, StatValue, Status, TradeFilterFields,
    TradeFilters, TypeFilterFields, TypeFilters,
};

/// Which listings a search should return, by seller/trade availability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ListingStatus {
    #[default]
    Online,
    /// Instant-buyout: listings buyable through GGG's automated secure trade.
    Securable,
    Available,
    Any,
}

impl ListingStatus {
    pub fn as_option(self) -> &'static str {
        match self {
            ListingStatus::Online => "online",
            ListingStatus::Securable => "securable",
            ListingStatus::Available => "available",
            ListingStatus::Any => "any",
        }
    }
}

/// How Fire/Cold/Lightning resistance rolls become trade stat filters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResistanceMode {
    /// Each element searched as its own literal stat, no grouping.
    Specific,
    /// Elements interchangeable; emits cumulative `count` groups.
    #[default]
    Fungible,
    /// Collapse all three into one "+#% total Elemental Resistance" pseudo.
    Total,
}

/// The discriminant doubles as an index into a per-element accumulator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResElement {
    Fire = 0,
    Cold = 1,
    Lightning = 2,
}

/// Stat-id suffixes for the three plain single-element resistances.
const RES_FIRE_SUFFIX: &str = "stat_3372524247";
const RES_COLD_SUFFIX: &str = "stat_4220027924";
const RES_LIGHTNING_SUFFIX: &str = "stat_1671376347";

/// Per-element resistance-total pseudo ids; members of the fungible groups.
const PSEUDO_FIRE: &str = "pseudo.pseudo_total_fire_resistance";
const PSEUDO_COLD: &str = "pseudo.pseudo_total_cold_resistance";
const PSEUDO_LIGHTNING: &str = "pseudo.pseudo_total_lightning_resistance";
const PSEUDO_TOTAL_ELE: &str = "pseudo.pseudo_total_elemental_resistance";

fn res_element(id: &str) -> Option<ResElement> {
    match id.rsplit('.').next().unwrap_or(id) {
        RES_FIRE_SUFFIX => Some(ResElement::Fire),
        RES_COLD_SUFFIX => Some(ResElement::Cold),
        RES_LIGHTNING_SUFFIX => Some(ResElement::Lightning),
        _ => None,
    }
}

/// Whether `id` is a plain Fire/Cold/Lightning resistance.
pub fn is_elemental_resistance(id: &str) -> bool {
    res_element(id).is_some()
}

/// Knobs for query construction.
#[derive(Debug, Clone, Copy)]
pub struct QueryOptions {
    pub status: ListingStatus,
    pub include_stats: bool,
    /// Emit stat filters disabled (toggleable later) rather than active.
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
        map_filters: None,
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

/// One per-stat filter the user can toggle in detailed mode.
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
    pub currency: Option<String>,
}

impl PriceFilter {
    pub fn is_empty(&self) -> bool {
        self.min.is_none() && self.max.is_none()
    }
}

/// One equipment-property filter in detailed mode (defence/offence stat).
#[derive(Debug, Clone, PartialEq)]
pub struct EquipmentSelection {
    /// Trade filter id, e.g. `ev` for Evasion.
    pub key: String,
    pub enabled: bool,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

/// Three-state misc filter: ignore the attribute, require it, or forbid it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MiscState {
    #[default]
    Any,
    Include,
    Exclude,
}

/// A three-state misc filter (corrupted, mirrored, …); see [`MiscState`].
#[derive(Debug, Clone, PartialEq)]
pub struct MiscSelection {
    pub key: String,
    pub state: MiscState,
}

/// Everything the user can tune in detailed mode.
#[derive(Debug, Clone, Default)]
pub struct DetailedFilters {
    pub status: ListingStatus,
    pub stats: Vec<StatSelection>,
    pub equipment: Vec<EquipmentSelection>,
    pub misc: Vec<MiscSelection>,
    pub quality: Option<f64>,
    pub item_level: Option<f64>,
    pub waystone_tier: Option<f64>,
    /// `None` defaults to the item's rarity; `Some("any")` emits no filter.
    pub rarity: Option<String>,
    pub price: PriceFilter,
    pub resistance_mode: ResistanceMode,
}

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

    let quality = f.quality.map(StatValue::min);
    let ilvl = f.item_level.map(StatValue::min);
    let rarity = match f.rarity.as_deref() {
        Some("any") => None,
        Some(r) => Some(r),
        None => rarity_option(&item.rarity),
    }
    .map(OptionFilter::new);
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
    let map_filters = f.waystone_tier.map(|t| MapFilters {
        filters: MapFilterFields {
            map_tier: Some(StatValue::min(t)),
        },
    });
    let filters = Filters {
        type_filters,
        equipment_filters: build_equipment_filters(&f.equipment),
        misc_filters: build_misc_filters(&f.misc),
        map_filters,
        trade_filters,
    };

    let stat_groups = build_stat_groups(&f.stats, f.resistance_mode);

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

/// Turn the per-stat selections into trade stat groups, applying the resistance mode.
fn build_stat_groups(selections: &[StatSelection], mode: ResistanceMode) -> Vec<StatGroup> {
    // Sum fungible resistances by element; everything else stays an `and` filter.
    let mut res = [0f64; 3];
    let mut others: Vec<StatFilter> = Vec::new();
    for s in selections {
        if mode != ResistanceMode::Specific && s.enabled {
            if let (Some(el), Some(min)) = (res_element(&s.id), s.min) {
                res[el as usize] += min;
                continue;
            }
        }
        others.push(s.to_filter());
    }

    let mut groups = Vec::new();
    match mode {
        ResistanceMode::Specific => {}
        ResistanceMode::Fungible => groups.extend(fungible_resistance_groups(res)),
        ResistanceMode::Total => {
            if let Some(filter) = total_resistance_filter(res) {
                others.push(filter);
            }
        }
    }
    if !others.is_empty() {
        groups.push(StatGroup::and(others));
    }
    groups
}

/// Cumulative `count` groups: one per distinct value `v`, requiring at least `k`
/// elements ≥ `v` where `k` is how many of the item's own resistances reach `v`.
fn fungible_resistance_groups(res: [f64; 3]) -> Vec<StatGroup> {
    let mut thresholds: Vec<f64> = res.iter().copied().filter(|&v| v > 0.0).collect();
    thresholds.sort_by(|a, b| b.total_cmp(a));
    thresholds.dedup();

    thresholds
        .into_iter()
        .map(|v| {
            let count = res.iter().filter(|&&t| t >= v).count() as u32;
            let filters = [PSEUDO_FIRE, PSEUDO_COLD, PSEUDO_LIGHTNING]
                .into_iter()
                .map(|id| StatFilter {
                    id: id.to_string(),
                    value: Some(StatValue::min(v)),
                    disabled: false,
                })
                .collect();
            StatGroup::count(filters, count)
        })
        .collect()
}

/// The summed "+#% total Elemental Resistance" pseudo filter, or `None`.
fn total_resistance_filter(res: [f64; 3]) -> Option<StatFilter> {
    let sum: f64 = res.iter().sum();
    (sum > 0.0).then(|| StatFilter {
        id: PSEUDO_TOTAL_ELE.to_string(),
        value: Some(StatValue::min(sum)),
        disabled: false,
    })
}

/// Collect the constrained boolean attributes into the `misc_filters` group.
fn build_misc_filters(selections: &[MiscSelection]) -> Option<MiscFilters> {
    let mut filters = BTreeMap::new();
    for s in selections {
        let option = match s.state {
            MiscState::Any => continue,
            MiscState::Include => "true",
            MiscState::Exclude => "false",
        };
        filters.insert(s.key.clone(), OptionFilter::new(option));
    }
    if filters.is_empty() {
        None
    } else {
        Some(MiscFilters { filters })
    }
}

/// Collect the enabled equipment-property filters into the `equipment_filters` group.
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

/// A rare with a known category drops its exact base to search the whole
/// category; other rarities keep their exact base.
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

/// The `type_filters.rarity` option matching an item's rarity. Uniques are
/// pinned by name; gems/currency aren't rarity-filtered.
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
    // Resolve a possible display-tier prefix ("Exceptional Crude Bow") to a base.
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
        Rarity::Magic => {
            let base = resolved_base()
                .or_else(|| item.name.as_deref().and_then(|n| items.split_magic_base(n)));
            (None, base)
        }
        Rarity::Rare => (None, resolved_base()),
        Rarity::Normal => (None, resolved_base().or_else(|| item.base_type.clone())),
        Rarity::Gem | Rarity::Currency | Rarity::Other(_) => (None, item.name.clone()),
    }
}

/// Map a POE2 `Item Class:` to the trade2 `category` filter option.
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
