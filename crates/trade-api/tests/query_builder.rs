//! Request-builder tests: parsed item + definition subsets → the search body we POST.

use parser::parse_item;
use trade_api::{
    build_detailed_query, build_search_query, category_for, DetailedFilters, EquipmentSelection,
    ItemDefinitions, ListingStatus, MiscSelection, MiscState, PriceFilter, QueryOptions,
    ResistanceMode, StatDefinitions, StatSelection, StatValue,
};

const FIRE_RES: &str = "explicit.stat_3372524247";
const COLD_RES: &str = "explicit.stat_4220027924";
const LIGHTNING_RES: &str = "explicit.stat_1671376347";
const PSEUDO_TOTAL_ELE: &str = "pseudo.pseudo_total_elemental_resistance";

fn sel(id: &str, min: f64) -> StatSelection {
    StatSelection {
        id: id.to_string(),
        enabled: true,
        min: Some(min),
        max: None,
    }
}

fn min_i64(value: Option<&StatValue>) -> Option<i64> {
    value?.min.as_ref()?.as_i64()
}

fn stats() -> StatDefinitions {
    StatDefinitions::from_json(include_str!("fixtures/api/data_stats.json")).unwrap()
}

fn items() -> ItemDefinitions {
    ItemDefinitions::from_json(include_str!("fixtures/api/data_items.json")).unwrap()
}

const RARE_RING: &str = "Item Class: Rings
Rarity: Rare
Honour Spiral
Topaz Ring
--------
Item Level: 79
--------
{ Implicit Modifier - Elemental, Lightning, Resistance }
+30(20-30)% to Lightning Resistance
--------
{ Prefix Modifier \"Adroit\" (Tier: 1) - Evasion }
+221(203-233) to Evasion Rating
{ Suffix Modifier \"of the Thunderhead\" (Tier: 5) - Elemental, Lightning, Resistance }
+23(21-25)% to Lightning Resistance";

const MAGIC_WAND: &str = "Item Class: Wands
Rarity: Magic
Professor's Volatile Wand of Expertise
--------
Requires: Level 58, 103 Int
--------
{ Prefix Modifier \"Professor's\" (Tier: 5) - Damage, Caster }
63(55-64)% increased Spell Damage
{ Suffix Modifier \"of Expertise\" (Tier: 5) - Caster, Speed }
17(17-20)% increased Cast Speed";

const UNIQUE_BELT: &str = "Item Class: Belts
Rarity: Unique
Mageblood
Utility Belt
--------
Item Level: 84";

const NORMAL_RING: &str = "Item Class: Rings
Rarity: Normal
Prismatic Ring
--------
Item Level: 80
--------
{ Implicit Modifier }
+8(7-10)% to all Elemental Resistances";

#[test]
fn normal_item_keeps_exact_base_and_normal_rarity() {
    let item = parse_item(NORMAL_RING).unwrap();
    let req = build_search_query(&item, &stats(), &items(), QueryOptions::default());
    assert_eq!(req.query.type_.as_deref(), Some("Prismatic Ring"));
    assert_eq!(req.query.name, None);
    let tf = &req.query.filters.type_filters.as_ref().unwrap().filters;
    assert_eq!(tf.category.as_ref().unwrap().option, "accessory.ring");
    assert_eq!(tf.rarity.as_ref().unwrap().option, "normal");
}

#[test]
fn category_mapping_covers_common_classes() {
    assert_eq!(category_for("Rings"), Some("accessory.ring"));
    assert_eq!(category_for("Wands"), Some("weapon.wand"));
    assert_eq!(category_for("Quarterstaves"), Some("weapon.warstaff"));
    assert_eq!(category_for("Body Armours"), Some("armour.chest"));
    assert_eq!(category_for("Waystones"), Some("map.waystone"));
    assert_eq!(category_for("Totally Made Up Class"), None);
}

#[test]
fn rare_ring_query_searches_by_category_not_base_type() {
    let item = parse_item(RARE_RING).unwrap();
    let req = build_search_query(&item, &stats(), &items(), QueryOptions::default());

    let query = &req.query;
    assert_eq!(query.type_, None);
    assert_eq!(query.name, None);
    let category = &query
        .filters
        .type_filters
        .as_ref()
        .unwrap()
        .filters
        .category;
    assert_eq!(category.as_ref().unwrap().option, "accessory.ring");

    let filters = &query.stats[0].filters;
    assert_eq!(filters.len(), 3);
    assert!(filters.iter().all(|f| f.disabled));
    let ids: Vec<&str> = filters.iter().map(|f| f.id.as_str()).collect();
    assert!(ids.contains(&"implicit.stat_1671376347"));
    assert!(ids.contains(&"explicit.stat_2144192055"));
}

#[test]
fn rare_ring_query_snapshot() {
    let item = parse_item(RARE_RING).unwrap();
    let req = build_search_query(&item, &stats(), &items(), QueryOptions::default());
    insta::assert_json_snapshot!(req);
}

#[test]
fn magic_wand_base_is_split_out_of_the_fused_name() {
    let item = parse_item(MAGIC_WAND).unwrap();
    assert_eq!(item.base_type, None);
    let req = build_search_query(&item, &stats(), &items(), QueryOptions::default());
    assert_eq!(req.query.type_.as_deref(), Some("Volatile Wand"));
    assert_eq!(req.query.name, None);
}

#[test]
fn magic_wand_query_snapshot() {
    let item = parse_item(MAGIC_WAND).unwrap();
    let req = build_search_query(&item, &stats(), &items(), QueryOptions::default());
    insta::assert_json_snapshot!(req);
}

#[test]
fn unique_query_sets_both_name_and_type() {
    let item = parse_item(UNIQUE_BELT).unwrap();
    let req = build_search_query(&item, &stats(), &items(), QueryOptions::default());
    assert_eq!(req.query.name.as_deref(), Some("Mageblood"));
    assert_eq!(req.query.type_.as_deref(), Some("Utility Belt"));
    let category = &req
        .query
        .filters
        .type_filters
        .as_ref()
        .unwrap()
        .filters
        .category;
    assert_eq!(category.as_ref().unwrap().option, "accessory.belt");
}

#[test]
fn enabled_stat_filters_carry_min_values() {
    let item = parse_item(RARE_RING).unwrap();
    let opts = QueryOptions {
        include_stats: true,
        stats_disabled: false,
        ..QueryOptions::default()
    };
    let req = build_search_query(&item, &stats(), &items(), opts);
    let filters = &req.query.stats[0].filters;
    assert!(filters.iter().all(|f| !f.disabled));
    let res = filters
        .iter()
        .find(|f| f.id == "implicit.stat_1671376347")
        .unwrap();
    let min = res.value.as_ref().unwrap().min.as_ref().unwrap();
    assert_eq!(min.as_i64(), Some(30));
}

#[test]
fn securable_status_selects_instant_buyout_listings() {
    let item = parse_item(RARE_RING).unwrap();
    let opts = QueryOptions {
        status: ListingStatus::Securable,
        ..QueryOptions::default()
    };
    let req = build_search_query(&item, &stats(), &items(), opts);
    assert_eq!(req.query.status.option, "securable");
    let online = build_search_query(&item, &stats(), &items(), QueryOptions::default());
    assert_eq!(online.query.status.option, "online");
}

#[test]
fn detailed_query_emits_selections_with_disabled_reflecting_the_toggle() {
    let item = parse_item(RARE_RING).unwrap();
    let selections = vec![
        StatSelection {
            id: "implicit.stat_1671376347".to_string(),
            enabled: true,
            min: Some(25.0),
            max: None,
        },
        StatSelection {
            id: "explicit.stat_2144192055".to_string(),
            enabled: false,
            min: None,
            max: None,
        },
    ];
    let req = build_detailed_query(
        &item,
        &items(),
        &DetailedFilters {
            stats: selections,
            resistance_mode: ResistanceMode::Specific,
            ..Default::default()
        },
    );

    assert_eq!(req.query.type_, None);
    let category = &req
        .query
        .filters
        .type_filters
        .as_ref()
        .unwrap()
        .filters
        .category;
    assert_eq!(category.as_ref().unwrap().option, "accessory.ring");

    let filters = &req.query.stats[0].filters;
    assert_eq!(filters.len(), 2);
    let enabled = filters
        .iter()
        .find(|f| f.id == "implicit.stat_1671376347")
        .unwrap();
    assert!(!enabled.disabled);
    assert_eq!(
        enabled
            .value
            .as_ref()
            .unwrap()
            .min
            .as_ref()
            .unwrap()
            .as_i64(),
        Some(25)
    );
    let disabled = filters
        .iter()
        .find(|f| f.id == "explicit.stat_2144192055")
        .unwrap();
    assert!(disabled.disabled);
    assert!(disabled.value.is_none());

    assert!(req.query.filters.trade_filters.is_none());
}

#[test]
fn detailed_query_attaches_price_range_filter() {
    let item = parse_item(RARE_RING).unwrap();
    let price = PriceFilter {
        min: Some(1.0),
        max: Some(20.0),
        currency: Some("exalted".to_string()),
    };
    let req = build_detailed_query(
        &item,
        &items(),
        &DetailedFilters {
            price,
            ..Default::default()
        },
    );

    let price = req
        .query
        .filters
        .trade_filters
        .as_ref()
        .unwrap()
        .filters
        .price
        .as_ref()
        .unwrap();
    assert_eq!(price.min.as_ref().unwrap().as_i64(), Some(1));
    assert_eq!(price.max.as_ref().unwrap().as_i64(), Some(20));
    assert_eq!(price.option.as_deref(), Some("exalted"));
    assert!(req.query.stats.is_empty());
}

#[test]
fn detailed_query_with_nothing_active_is_a_bare_base_search() {
    let item = parse_item(RARE_RING).unwrap();
    let req = build_detailed_query(&item, &items(), &DetailedFilters::default());
    assert!(req.query.stats.is_empty());
    assert!(req.query.filters.trade_filters.is_none());
    assert!(req.query.filters.equipment_filters.is_none());
    assert_eq!(req.query.type_, None);
    let category = &req
        .query
        .filters
        .type_filters
        .as_ref()
        .unwrap()
        .filters
        .category;
    assert_eq!(category.as_ref().unwrap().option, "accessory.ring");
}

#[test]
fn detailed_query_rarity_defaults_to_item_then_honours_override() {
    let item = parse_item(RARE_RING).unwrap();

    let req = build_detailed_query(&item, &items(), &DetailedFilters::default());
    let tf = &req.query.filters.type_filters.as_ref().unwrap().filters;
    assert_eq!(tf.rarity.as_ref().unwrap().option, "rare");

    let req = build_detailed_query(
        &item,
        &items(),
        &DetailedFilters {
            rarity: Some("magic".to_string()),
            ..Default::default()
        },
    );
    let tf = &req.query.filters.type_filters.as_ref().unwrap().filters;
    assert_eq!(tf.rarity.as_ref().unwrap().option, "magic");

    let req = build_detailed_query(
        &item,
        &items(),
        &DetailedFilters {
            rarity: Some("any".to_string()),
            ..Default::default()
        },
    );
    let tf = &req.query.filters.type_filters.as_ref().unwrap().filters;
    assert!(tf.rarity.is_none());
}

#[test]
fn detailed_query_attaches_enabled_equipment_filters() {
    let item = parse_item(RARE_RING).unwrap();
    let req = build_detailed_query(
        &item,
        &items(),
        &DetailedFilters {
            equipment: vec![
                EquipmentSelection {
                    key: "ev".to_string(),
                    enabled: true,
                    min: Some(1099.0),
                    max: None,
                },
                EquipmentSelection {
                    key: "ar".to_string(),
                    enabled: false,
                    min: Some(50.0),
                    max: None,
                },
            ],
            ..Default::default()
        },
    );
    let eq = &req
        .query
        .filters
        .equipment_filters
        .as_ref()
        .unwrap()
        .filters;
    assert_eq!(eq.len(), 1);
    assert_eq!(eq["ev"].min.as_ref().unwrap().as_i64(), Some(1099));
    assert!(!eq.contains_key("ar"));
}

#[test]
fn detailed_query_attaches_waystone_tier_map_filter() {
    let item = parse_item(RARE_RING).unwrap();
    let req = build_detailed_query(
        &item,
        &items(),
        &DetailedFilters {
            waystone_tier: Some(16.0),
            ..Default::default()
        },
    );
    let map = &req.query.filters.map_filters.as_ref().unwrap().filters;
    assert_eq!(min_i64(map.map_tier.as_ref()), Some(16));

    let bare = build_detailed_query(&item, &items(), &DetailedFilters::default());
    assert!(bare.query.filters.map_filters.is_none());
}

#[test]
fn detailed_query_carries_sockets_and_quality() {
    let item = parse_item(RARE_RING).unwrap();
    let req = build_detailed_query(
        &item,
        &items(),
        &DetailedFilters {
            equipment: vec![EquipmentSelection {
                key: "rune_sockets".to_string(),
                enabled: true,
                min: Some(3.0),
                max: None,
            }],
            quality: Some(23.0),
            item_level: Some(82.0),
            ..Default::default()
        },
    );
    let eq = &req
        .query
        .filters
        .equipment_filters
        .as_ref()
        .unwrap()
        .filters;
    assert_eq!(eq["rune_sockets"].min.as_ref().unwrap().as_i64(), Some(3));
    let tf = &req.query.filters.type_filters.as_ref().unwrap().filters;
    assert_eq!(
        tf.quality.as_ref().unwrap().min.as_ref().unwrap().as_i64(),
        Some(23)
    );
    assert_eq!(
        tf.ilvl.as_ref().unwrap().min.as_ref().unwrap().as_i64(),
        Some(82)
    );
}

#[test]
fn detailed_query_carries_checked_misc_filters() {
    let item = parse_item(RARE_RING).unwrap();
    let req = build_detailed_query(
        &item,
        &items(),
        &DetailedFilters {
            misc: vec![
                MiscSelection {
                    key: "corrupted".to_string(),
                    state: MiscState::Include,
                },
                MiscSelection {
                    key: "mirrored".to_string(),
                    state: MiscState::Any,
                },
                MiscSelection {
                    key: "identified".to_string(),
                    state: MiscState::Exclude,
                },
            ],
            ..Default::default()
        },
    );
    let misc = &req.query.filters.misc_filters.as_ref().unwrap().filters;
    assert_eq!(misc.len(), 2);
    assert_eq!(misc["corrupted"].option, "true");
    assert_eq!(misc["identified"].option, "false");
    assert!(!misc.contains_key("mirrored"));
}

#[test]
fn detailed_query_snapshot() {
    let item = parse_item(RARE_RING).unwrap();
    let selections = vec![StatSelection {
        id: "implicit.stat_1671376347".to_string(),
        enabled: true,
        min: Some(25.0),
        max: Some(30.0),
    }];
    let price = PriceFilter {
        min: Some(5.0),
        max: None,
        currency: Some("exalted".to_string()),
    };
    let req = build_detailed_query(
        &item,
        &items(),
        &DetailedFilters {
            stats: selections,
            equipment: vec![EquipmentSelection {
                key: "ev".to_string(),
                enabled: true,
                min: Some(1099.0),
                max: None,
            }],
            price,
            resistance_mode: ResistanceMode::Specific,
            ..Default::default()
        },
    );
    insta::assert_json_snapshot!(req);
}

fn fungible_query(stats: Vec<StatSelection>) -> trade_api::SearchRequest {
    let item = parse_item(RARE_RING).unwrap();
    build_detailed_query(
        &item,
        &items(),
        &DetailedFilters {
            stats,
            ..Default::default()
        },
    )
}

#[test]
fn fungible_single_resistance_becomes_one_count_group() {
    let req = fungible_query(vec![sel(FIRE_RES, 30.0)]);

    assert_eq!(req.query.stats.len(), 1);
    let group = &req.query.stats[0];
    assert_eq!(group.type_, "count");
    assert_eq!(min_i64(group.value.as_ref()), Some(1));
    let ids: Vec<&str> = group.filters.iter().map(|f| f.id.as_str()).collect();
    assert!(ids.contains(&"pseudo.pseudo_total_fire_resistance"));
    assert!(ids.contains(&"pseudo.pseudo_total_cold_resistance"));
    assert!(ids.contains(&"pseudo.pseudo_total_lightning_resistance"));
    for f in &group.filters {
        assert_eq!(min_i64(f.value.as_ref()), Some(30));
        assert!(!f.disabled);
    }
}

#[test]
fn fungible_two_resistances_use_cumulative_counts() {
    let req = fungible_query(vec![sel(FIRE_RES, 42.0), sel(COLD_RES, 22.0)]);

    assert_eq!(req.query.stats.len(), 2);
    let group_at = |threshold| {
        req.query
            .stats
            .iter()
            .find(|g| min_i64(g.filters[0].value.as_ref()) == Some(threshold))
            .unwrap()
    };
    assert_eq!(group_at(42).type_, "count");
    assert_eq!(min_i64(group_at(42).value.as_ref()), Some(1));
    assert_eq!(min_i64(group_at(22).value.as_ref()), Some(2));
}

#[test]
fn fungible_same_value_resistances_collapse_to_one_group() {
    let req = fungible_query(vec![sel(FIRE_RES, 30.0), sel(LIGHTNING_RES, 30.0)]);

    assert_eq!(req.query.stats.len(), 1);
    assert_eq!(min_i64(req.query.stats[0].value.as_ref()), Some(2));
}

#[test]
fn total_mode_sums_resistances_into_one_pseudo_filter() {
    let item = parse_item(RARE_RING).unwrap();
    let req = build_detailed_query(
        &item,
        &items(),
        &DetailedFilters {
            stats: vec![sel(FIRE_RES, 42.0), sel(COLD_RES, 22.0)],
            resistance_mode: ResistanceMode::Total,
            ..Default::default()
        },
    );

    assert_eq!(req.query.stats.len(), 1);
    let group = &req.query.stats[0];
    assert_eq!(group.type_, "and");
    assert_eq!(group.filters.len(), 1);
    assert_eq!(group.filters[0].id, PSEUDO_TOTAL_ELE);
    assert_eq!(min_i64(group.filters[0].value.as_ref()), Some(64));
}

#[test]
fn fungible_keeps_non_resistance_stats_in_an_and_group() {
    let req = fungible_query(vec![
        sel(FIRE_RES, 30.0),
        sel("explicit.stat_2144192055", 200.0),
    ]);

    assert_eq!(req.query.stats.len(), 2);
    let and = req.query.stats.iter().find(|g| g.type_ == "and").unwrap();
    assert_eq!(and.filters.len(), 1);
    assert_eq!(and.filters[0].id, "explicit.stat_2144192055");
    assert!(req.query.stats.iter().any(|g| g.type_ == "count"));
}

#[test]
fn stats_can_be_omitted_entirely() {
    let item = parse_item(RARE_RING).unwrap();
    let opts = QueryOptions {
        include_stats: false,
        stats_disabled: true,
        ..QueryOptions::default()
    };
    let req = build_search_query(&item, &stats(), &items(), opts);
    assert!(req.query.stats.is_empty());
}
