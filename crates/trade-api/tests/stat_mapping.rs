//! Stat-id mapping + base-type splitting against the recorded snapshot subsets.

use parser::{parse_item, ModKind, ModSource, Modifier};
use trade_api::{ItemDefinitions, StatDefinitions};

fn stats() -> StatDefinitions {
    let json = include_str!("fixtures/api/data_stats.json");
    StatDefinitions::from_json(json).expect("real stats subset parses")
}

fn items() -> ItemDefinitions {
    let json = include_str!("fixtures/api/data_items.json");
    ItemDefinitions::from_json(json).expect("real items subset parses")
}

fn modifier(kind: ModKind, source: Option<ModSource>, stat: &str) -> Modifier {
    Modifier {
        kind,
        source,
        name: None,
        tier: None,
        tags: Vec::new(),
        stats: vec![stat.to_string()],
    }
}

#[test]
fn implicit_resistance_maps_to_implicit_id() {
    let defs = stats();
    let m = defs
        .map_stat_line(
            &ModKind::Implicit,
            None,
            "+30(20-30)% to Lightning Resistance",
            false,
        )
        .expect("lightning resistance maps");
    assert_eq!(m.id, "implicit.stat_1671376347");
    assert_eq!(m.stat_type, "implicit");
    assert_eq!(m.values, [30.0]);
    assert_eq!(m.filter_value(), Some(30.0));
}

#[test]
fn explicit_prefix_evasion_maps_to_explicit_id() {
    let defs = stats();
    let m = defs
        .map_stat_line(
            &ModKind::Prefix,
            None,
            "+221(203-233) to Evasion Rating",
            false,
        )
        .expect("evasion maps");
    assert_eq!(m.id, "explicit.stat_2144192055");
    assert_eq!(m.values, [221.0]);
}

#[test]
fn same_text_resolves_to_different_id_by_affix_type() {
    let defs = stats();
    let implicit = defs
        .map_stat_line(
            &ModKind::Implicit,
            None,
            "+30% to Lightning Resistance",
            false,
        )
        .unwrap();
    let explicit = defs
        .map_stat_line(
            &ModKind::Suffix,
            None,
            "+23(21-25)% to Lightning Resistance",
            false,
        )
        .unwrap();
    assert_eq!(implicit.id, "implicit.stat_1671376347");
    assert_eq!(explicit.id, "explicit.stat_1671376347");
}

#[test]
fn plural_count_mod_falls_back_to_singular_presence_filter() {
    let defs = stats();
    let m = defs
        .map_stat_line(
            &ModKind::Prefix,
            None,
            "Map contains 3(2-3) additional Rare Chests",
            false,
        )
        .expect("plural Rare Chests maps to the singular presence stat");
    assert_eq!(m.id, "explicit.stat_3650769924");
    assert_eq!(m.template, "Map contains an additional Rare Chest");
    assert!(
        m.values.is_empty(),
        "a presence filter sends no value, got {:?}",
        m.values
    );
    assert_eq!(m.filter_value(), None);
}

#[test]
fn fractured_source_prefers_fractured_id() {
    let defs = stats();
    let m = defs
        .map_stat_line(
            &ModKind::Prefix,
            Some(&ModSource::Fractured),
            "+45(40-50)% increased maximum Life",
            false,
        )
        .unwrap();
    assert_eq!(m.id, "fractured.stat_983749596");
    assert_eq!(m.stat_type, "fractured");
}

#[test]
fn spell_damage_and_cast_speed_map() {
    let defs = stats();
    let sd = defs
        .map_stat_line(
            &ModKind::Prefix,
            None,
            "63(55-64)% increased Spell Damage",
            false,
        )
        .unwrap();
    let cs = defs
        .map_stat_line(
            &ModKind::Suffix,
            None,
            "17(17-20)% increased Cast Speed",
            false,
        )
        .unwrap();
    assert_eq!(sd.id, "explicit.stat_2974417149");
    assert_eq!(cs.id, "explicit.stat_2891184298");
}

#[test]
fn hybrid_modifier_maps_each_stat_line() {
    let defs = stats();
    let m = Modifier {
        stats: vec![
            "+118(100-119) to maximum Life".to_string(),
            "+25(20-30) to maximum Mana".to_string(),
        ],
        ..modifier(ModKind::Prefix, None, "")
    };
    let mapped = defs.map_modifier(&m, trade_api::LocalContext::default());
    assert_eq!(mapped.len(), 2);
    assert_eq!(mapped[0].id, "explicit.stat_3299347043");
    assert_eq!(mapped[0].values, [118.0]);
    assert_eq!(mapped[1].id, "explicit.stat_1050105434");
}

#[test]
fn prefer_local_picks_the_local_stat_variant() {
    let json = r##"{"result":[{"id":"explicit","label":"Explicit","entries":[
        {"id":"explicit.stat_124859000","text":"#% increased Evasion Rating (Local)","type":"explicit"},
        {"id":"explicit.stat_2106365538","text":"#% increased Evasion Rating","type":"explicit"}
    ]}]}"##;
    let defs = StatDefinitions::from_json(json).unwrap();

    let local = defs
        .map_stat_line(
            &ModKind::Prefix,
            None,
            "103(101-110)% increased Evasion Rating",
            true,
        )
        .unwrap();
    assert_eq!(local.id, "explicit.stat_124859000");

    let global = defs
        .map_stat_line(
            &ModKind::Prefix,
            None,
            "103(101-110)% increased Evasion Rating",
            false,
        )
        .unwrap();
    assert_eq!(global.id, "explicit.stat_2106365538");
}

#[test]
fn quiver_accuracy_is_global_not_local() {
    let json = r##"{"result":[{"id":"explicit","entries":[
        {"id":"explicit.stat_803737631","text":"# to Accuracy Rating","type":"explicit"},
        {"id":"explicit.stat_691932474","text":"# to Accuracy Rating (Local)","type":"explicit"}
    ]}]}"##;
    let defs = StatDefinitions::from_json(json).unwrap();
    let quiver = parse_item(
        "Item Class: Quivers\nRarity: Rare\nCorpse Bolt\nToxic Quiver\n--------\n\
         { Prefix Modifier \"Hunter's\" — Attack }\n+257 to Accuracy Rating",
    )
    .unwrap();

    let mapped = defs.map_item(&quiver);
    let acc = mapped
        .iter()
        .find(|m| m.id.contains("803737631") || m.id.contains("691932474"))
        .expect("accuracy maps");
    assert_eq!(
        acc.id, "explicit.stat_803737631",
        "quiver accuracy must be global"
    );
}

#[test]
fn unmappable_stat_line_is_dropped_not_panicking() {
    let defs = stats();
    assert!(defs
        .map_stat_line(
            &ModKind::Prefix,
            None,
            "Grants Eternal Youth and Free Snacks",
            false
        )
        .is_none());
}

#[test]
fn real_subset_indexes_many_entries() {
    let defs = stats();
    assert!(!defs.is_empty());
    assert!(defs.len() >= 20, "got {}", defs.len());
}

#[test]
fn splits_magic_base_from_fused_name() {
    let defs = items();
    assert_eq!(
        defs.split_magic_base("Professor's Volatile Wand of Expertise")
            .as_deref(),
        Some("Volatile Wand"),
    );
    assert_eq!(
        defs.split_magic_base("Glinting Sapphire Ring of the Drake")
            .as_deref(),
        Some("Sapphire Ring"),
    );
}

#[test]
fn prefers_the_longest_matching_base() {
    let defs = items();
    assert_eq!(
        defs.split_magic_base("Hale Topaz Ring of Grounding")
            .as_deref(),
        Some("Topaz Ring"),
    );
}

#[test]
fn magic_base_split_returns_none_when_unknown() {
    let defs = items();
    assert_eq!(
        defs.split_magic_base("Whirling Nonsense of Madeupness"),
        None
    );
}

#[test]
fn resolve_base_strips_display_tier_prefix() {
    let json = r#"{"result":[{"id":"weapon","entries":[
        {"type":"Crude Bow"},{"type":"Runeforged Crude Bow"},{"type":"Heavy Bow"}
    ]}]}"#;
    let defs = ItemDefinitions::from_json(json).unwrap();

    assert_eq!(
        defs.resolve_base("Exceptional Crude Bow").as_deref(),
        Some("Crude Bow")
    );
    assert_eq!(
        defs.resolve_base("Runeforged Crude Bow").as_deref(),
        Some("Runeforged Crude Bow")
    );
    assert_eq!(defs.resolve_base("Crude Bow").as_deref(), Some("Crude Bow"));
    assert_eq!(defs.resolve_base("Totally Made Up Base"), None);
}

#[test]
fn unique_name_resolves_to_base_type() {
    let defs = items();
    assert_eq!(defs.unique_base("Mageblood"), Some("Utility Belt"));
    assert_eq!(defs.unique_base("Headhunter"), Some("Heavy Belt"));
    assert_eq!(defs.unique_base("Andvarius"), Some("Gold Ring"));
    assert_eq!(defs.unique_base("Not A Real Unique"), None);
}

#[test]
fn granted_skill_maps_to_skill_id_with_level_as_value() {
    let json = r#"{"result":[{"id":"skill","label":"Skill","entries":[
        {"id":"skill.discipline","text":"Grants Skill: Level # Discipline","type":"skill"},
        {"id":"skill.summon_azmerian_wolf","text":"Grants Skill: Level # Azmerian Wolf","type":"skill"}
    ]}]}"#;
    let defs = StatDefinitions::from_json(json).expect("inline skill stats parse");

    let d = defs
        .map_granted_skill("Level 19 Discipline")
        .expect("discipline maps");
    assert_eq!(d.id, "skill.discipline");
    assert_eq!(d.filter_value(), Some(19.0));

    let w = defs
        .map_granted_skill("Level 20 Azmerian Wolf")
        .expect("azmerian wolf maps");
    assert_eq!(w.id, "skill.summon_azmerian_wolf");
    assert_eq!(w.filter_value(), Some(20.0));

    assert!(defs
        .map_granted_skill("Level 19 Not A Real Skill")
        .is_none());
    assert!(defs.map_granted_skill("garbage").is_none());
}

#[test]
fn trailing_qualifier_is_stripped_so_ingame_text_maps() {
    // GGG publishes this stat only as "… (Gold Piles)", but the in-game mod text
    // omits the qualifier — it must still map to the filter (regression: tablet
    // gold-find showed as unsearchable). Two ids share the stripped form across
    // types, so it resolves via the type-scoped table, not the global one.
    let json = r##"{"result":[{"label":"Explicit","entries":[
        {"id":"explicit.stat_1276056105","text":"#% increased Gold found in Map (Gold Piles)","type":"explicit"},
        {"id":"fractured.stat_1276056105","text":"#% increased Gold found in Map (Gold Piles)","type":"fractured"}
    ]}]}"##;
    let defs = StatDefinitions::from_json(json).expect("parse");
    let m = defs
        .map_stat_line(
            &ModKind::Prefix,
            None,
            "28(25-35)% increased Gold found in Map",
            false,
        )
        .expect("gold-find maps via stripped qualifier");
    assert_eq!(m.id, "explicit.stat_1276056105");
    assert_eq!(m.values, [28.0]);
}
