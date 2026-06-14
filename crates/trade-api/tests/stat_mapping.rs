//! Stat-id mapping + base-type splitting against the recorded (real) snapshot
//! subsets in `tests/fixtures/api/` (PRD §4.3, §4.4, §7).

use parser::{ModKind, ModSource, Modifier};
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
        .map_stat_line(&ModKind::Implicit, None, "+30(20-30)% to Lightning Resistance")
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
        .map_stat_line(&ModKind::Prefix, None, "+221(203-233) to Evasion Rating")
        .expect("evasion maps");
    assert_eq!(m.id, "explicit.stat_2144192055");
    assert_eq!(m.values, [221.0]);
}

#[test]
fn same_text_resolves_to_different_id_by_affix_type() {
    let defs = stats();
    // The exact same template, mapped as an implicit vs a normal explicit,
    // must resolve to the implicit. and explicit. ids respectively.
    let implicit = defs
        .map_stat_line(&ModKind::Implicit, None, "+30% to Lightning Resistance")
        .unwrap();
    let explicit = defs
        .map_stat_line(&ModKind::Suffix, None, "+23(21-25)% to Lightning Resistance")
        .unwrap();
    assert_eq!(implicit.id, "implicit.stat_1671376347");
    assert_eq!(explicit.id, "explicit.stat_1671376347");
}

#[test]
fn fractured_source_prefers_fractured_id() {
    let defs = stats();
    let m = defs
        .map_stat_line(
            &ModKind::Prefix,
            Some(&ModSource::Fractured),
            "+45(40-50)% increased maximum Life",
        )
        .unwrap();
    assert_eq!(m.id, "fractured.stat_983749596");
    assert_eq!(m.stat_type, "fractured");
}

#[test]
fn spell_damage_and_cast_speed_map() {
    let defs = stats();
    let sd = defs
        .map_stat_line(&ModKind::Prefix, None, "63(55-64)% increased Spell Damage")
        .unwrap();
    let cs = defs
        .map_stat_line(&ModKind::Suffix, None, "17(17-20)% increased Cast Speed")
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
    let mapped = defs.map_modifier(&m);
    assert_eq!(mapped.len(), 2);
    assert_eq!(mapped[0].id, "explicit.stat_3299347043"); // flat life
    assert_eq!(mapped[0].values, [118.0]);
    assert_eq!(mapped[1].id, "explicit.stat_1050105434"); // flat mana
}

#[test]
fn unmappable_stat_line_is_dropped_not_panicking() {
    let defs = stats();
    assert!(defs
        .map_stat_line(&ModKind::Prefix, None, "Grants Eternal Youth and Free Snacks")
        .is_none());
}

#[test]
fn real_subset_indexes_many_entries() {
    let defs = stats();
    assert!(!defs.is_empty());
    assert!(defs.len() >= 20, "got {}", defs.len());
}

// ---- base-type splitting (PRD §4.3: magic bases left None by the parser) ----

#[test]
fn splits_magic_base_from_fused_name() {
    let defs = items();
    assert_eq!(
        defs.split_magic_base("Professor's Volatile Wand of Expertise").as_deref(),
        Some("Volatile Wand"),
    );
    assert_eq!(
        defs.split_magic_base("Glinting Sapphire Ring of the Drake").as_deref(),
        Some("Sapphire Ring"),
    );
}

#[test]
fn prefers_the_longest_matching_base() {
    let defs = items();
    // "Topaz Ring" must win over a hypothetical bare "Ring" base.
    assert_eq!(
        defs.split_magic_base("Hale Topaz Ring of Grounding").as_deref(),
        Some("Topaz Ring"),
    );
}

#[test]
fn magic_base_split_returns_none_when_unknown() {
    let defs = items();
    assert_eq!(defs.split_magic_base("Whirling Nonsense of Madeupness"), None);
}

#[test]
fn unique_name_resolves_to_base_type() {
    let defs = items();
    assert_eq!(defs.unique_base("Mageblood"), Some("Utility Belt"));
    assert_eq!(defs.unique_base("Headhunter"), Some("Heavy Belt"));
    assert_eq!(defs.unique_base("Andvarius"), Some("Gold Ring"));
    assert_eq!(defs.unique_base("Not A Real Unique"), None);
}
