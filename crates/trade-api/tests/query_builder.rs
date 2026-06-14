//! Request-builder tests (PRD §7): a parsed item + the real definition snapshot
//! subsets → the search body we'd POST. Bodies are snapshotted as JSON with
//! `insta` (run `cargo insta review` on a diff), and key fields asserted
//! directly.

use parser::parse_item;
use trade_api::{build_search_query, category_for, ItemDefinitions, QueryOptions, StatDefinitions};

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
fn rare_ring_query_has_base_type_category_and_disabled_stat_filters() {
    let item = parse_item(RARE_RING).unwrap();
    let req = build_search_query(&item, &stats(), &items(), QueryOptions::default());

    let query = &req.query;
    assert_eq!(query.type_.as_deref(), Some("Topaz Ring"));
    assert_eq!(query.name, None);
    let category = &query.filters.type_filters.as_ref().unwrap().filters.category;
    assert_eq!(category.as_ref().unwrap().option, "accessory.ring");

    // Three stat lines all map; default options emit them disabled.
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
    // The parser leaves a magic base as None; the builder splits it.
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
    let category = &req.query.filters.type_filters.as_ref().unwrap().filters.category;
    assert_eq!(category.as_ref().unwrap().option, "accessory.belt");
}

#[test]
fn enabled_stat_filters_carry_min_values() {
    let item = parse_item(RARE_RING).unwrap();
    let opts = QueryOptions {
        include_stats: true,
        stats_disabled: false,
    };
    let req = build_search_query(&item, &stats(), &items(), opts);
    let filters = &req.query.stats[0].filters;
    assert!(filters.iter().all(|f| !f.disabled));
    // The implicit lightning-res roll (30) becomes a min filter.
    let res = filters
        .iter()
        .find(|f| f.id == "implicit.stat_1671376347")
        .unwrap();
    let min = res.value.as_ref().unwrap().min.as_ref().unwrap();
    assert_eq!(min.as_i64(), Some(30));
}

#[test]
fn stats_can_be_omitted_entirely() {
    let item = parse_item(RARE_RING).unwrap();
    let opts = QueryOptions {
        include_stats: false,
        stats_disabled: true,
    };
    let req = build_search_query(&item, &stats(), &items(), opts);
    assert!(req.query.stats.is_empty());
}
