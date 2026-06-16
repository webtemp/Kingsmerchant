//! The parser itself: POE2 clipboard text → [`Item`].
//!
//! Item text is dash-separated (`--------`) sections; the first is always a
//! header (`Item Class:`, `Rarity:`, name lines). Remaining sections are
//! classified by content, not position, since which appear and in what order
//! varies by item type.

use std::sync::OnceLock;

use regex::Regex;

use crate::model::{
    Item, ModKind, ModSource, Modifier, ParseError, Property, Rarity, Requirements, StackSize,
};

/// Standalone marker lines captured into [`Item::flags`] (those not already
/// modelled as a dedicated bool). Allowlisted so we don't mistake description
/// prose for a flag.
const STANDALONE_FLAGS: &[&str] = &[
    "Sanctified",
    "Hinekora's Lock",
    "Synthesised",
    "Synthesised Item",
    "Split",
    "Unmodifiable",
];

/// Parse a POE2 clipboard item string.
pub fn parse_item(input: &str) -> Result<Item, ParseError> {
    if input.trim().is_empty() {
        return Err(ParseError::Empty);
    }

    let sections = split_sections(input);
    let header = sections.first().ok_or(ParseError::Empty)?;

    let mut item_class = None;
    let mut rarity = None;
    let mut name_lines: Vec<&str> = Vec::new();
    let mut unusable = false;
    for &line in header {
        if let Some(v) = line.strip_prefix("Item Class:") {
            item_class = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("Rarity:") {
            rarity = Some(Rarity::parse(v.trim()));
        } else if line.trim().starts_with("You cannot use this item") {
            // Usability note + a separator land in the header, pushing the real
            // name/base into the next section. Not the base type.
            unusable = true;
        } else if !line.trim().is_empty() {
            name_lines.push(line.trim());
        }
    }

    // `Rarity:` is the reliable "this is a POE2 item" marker. `Item Class:` is
    // optional — a few copies (e.g. meta gems from the trade site) omit it.
    let rarity = rarity.ok_or(ParseError::NotAnItem)?;
    let item_class = item_class.unwrap_or_default();

    // Usability note in the header means name/base were split into the next
    // section: adopt it as the name lines (and skip re-parsing it below). An
    // empty header-name only happens in this case.
    let mut name_section = None;
    if name_lines.is_empty() && unusable {
        if let Some(section) = sections.get(1) {
            name_lines = section
                .iter()
                .map(|l| l.trim())
                .filter(|l| !l.is_empty())
                .collect();
            name_section = Some(1);
        }
    }

    let (name, base_type) = split_name(&rarity, &name_lines);

    let mut item = Item {
        item_class,
        rarity,
        name,
        base_type,
        item_level: None,
        quality: None,
        requirements: Requirements::default(),
        sockets: None,
        stack_size: None,
        properties: Vec::new(),
        rune_mods: Vec::new(),
        modifiers: Vec::new(),
        flavour_text: Vec::new(),
        notes: Vec::new(),
        corrupted: false,
        mirrored: false,
        unidentified: false,
        fractured: false,
        flags: Vec::new(),
    };
    if unusable {
        item.flags.push("Unusable".to_string());
    }

    for (i, section) in sections.iter().enumerate().skip(1) {
        // Skip the section we adopted as the name/base above.
        if Some(i) == name_section {
            continue;
        }
        classify_section(section, &mut item);
    }

    Ok(item)
}

/// Split into sections on dash-only separator lines, normalizing `\r\n`.
/// Sections that are entirely blank are dropped.
fn split_sections(input: &str) -> Vec<Vec<&str>> {
    let mut sections = Vec::new();
    let mut current = Vec::new();
    for raw in input.lines() {
        let line = raw.trim_end_matches('\r');
        if is_separator(line) {
            sections.push(std::mem::take(&mut current));
        } else {
            current.push(line);
        }
    }
    if !current.is_empty() {
        sections.push(current);
    }
    sections
        .into_iter()
        .filter(|s| s.iter().any(|l| !l.trim().is_empty()))
        .collect()
}

/// A separator is a line of three or more dashes and nothing else.
fn is_separator(line: &str) -> bool {
    let t = line.trim();
    t.len() >= 3 && t.chars().all(|c| c == '-')
}

/// Assign the header's name line(s) to `name` / `base_type` per rarity.
// Arms are kept separate per rarity for documentation even where bodies match.
#[allow(clippy::match_same_arms)]
fn split_name(rarity: &Rarity, lines: &[&str]) -> (Option<String>, Option<String>) {
    let get = |i: usize| lines.get(i).map(std::string::ToString::to_string);
    match rarity {
        // Identified: title then base type. Unidentified rares/uniques show
        // only the base type (no rolled name), so a single line is the base.
        Rarity::Rare | Rarity::Unique if lines.len() >= 2 => (get(0), get(1)),
        Rarity::Rare | Rarity::Unique => (None, get(0)),
        // One line that is just the base type.
        Rarity::Normal => (None, get(0)),
        // One line fusing base + affixes; splitting needs the item snapshot.
        Rarity::Magic => (get(0), None),
        // Gems / currency / unknowns: a single name line.
        _ => (get(0), None),
    }
}

/// Route one (non-header) section into the right field(s) of `item`.
fn classify_section(section: &[&str], item: &mut Item) {
    let nonempty: Vec<&str> = section
        .iter()
        .copied()
        .filter(|l| !l.trim().is_empty())
        .collect();
    let Some(first) = nonempty.first() else {
        return;
    };

    // Modifier section: advanced-format descriptor blocks.
    if first.trim_start().starts_with('{') {
        parse_modifiers(&nonempty, item);
        return;
    }

    // Flavour text block: a quoted (often multi-line) section.
    if first.trim_start().starts_with('"') {
        let joined = nonempty.join("\n");
        item.flavour_text.push(joined.trim_matches('"').to_string());
        return;
    }

    // Rune section: every line is a `(rune)`-granted stat.
    if nonempty.iter().all(|l| l.trim_end().ends_with("(rune)")) {
        item.rune_mods
            .extend(nonempty.iter().map(|l| l.trim().to_string()));
        return;
    }

    // Otherwise a miscellaneous section of typed keys / flags / properties.
    for &line in &nonempty {
        let t = line.trim();
        if let Some(v) = t.strip_prefix("Item Level:") {
            item.item_level = v.trim().parse().ok();
        } else if let Some(v) = t.strip_prefix("Requires:") {
            // Items can have several `Requires:` lines; merge so a later line
            // doesn't clobber attributes from an earlier one.
            merge_requirements(&mut item.requirements, &parse_requirements(v));
        } else if let Some(v) = t.strip_prefix("Sockets:") {
            item.sockets = Some(v.trim().to_string());
        } else if let Some(v) = t.strip_prefix("Quality:") {
            item.quality = parse_quality(v);
        } else if let Some(v) = t.strip_prefix("Stack Size:") {
            item.stack_size = parse_stack_size(v);
        } else if let Some(v) = t.strip_prefix("Note:") {
            item.notes.push(v.trim().to_string());
        } else if t == "Corrupted" {
            item.corrupted = true;
        } else if t == "Twice Corrupted" {
            item.corrupted = true;
            item.flags.push(t.to_string());
        } else if t == "Mirrored" {
            item.mirrored = true;
        } else if t == "Unidentified" {
            item.unidentified = true;
        } else if t == "Fractured Item" {
            item.fractured = true;
        } else if STANDALONE_FLAGS.contains(&t) {
            item.flags.push(t.to_string());
        } else if let Some((key, value)) = t.split_once(": ") {
            item.properties.push(Property {
                name: key.to_string(),
                value: value.to_string(),
            });
        }
        // Unrecognized prose (gem/skill descriptions, etc.) is ignored.
    }
}

/// Parse `Level 65, 86 Dex` / `Level 78 (unmet), 163 (unmet) Dex` /
/// `113 Intelligence` into [`Requirements`]. Parenthetical annotations like
/// `(unmet)` are stripped, and both abbreviated and full attribute names work.
fn parse_requirements(value: &str) -> Requirements {
    let mut req = Requirements::default();
    for token in value.split(',') {
        let token = normalize(&strip_annotations(token));
        if let Some(n) = token.strip_prefix("Level ") {
            req.level = n.trim().parse().ok();
        } else if let Some(n) = attr_value(&token, "Str", "Strength") {
            req.strength = Some(n);
        } else if let Some(n) = attr_value(&token, "Dex", "Dexterity") {
            req.dexterity = Some(n);
        } else if let Some(n) = attr_value(&token, "Int", "Intelligence") {
            req.intelligence = Some(n);
        }
    }
    req
}

/// Fold non-`None` fields of `from` into `into`, keeping existing values.
fn merge_requirements(into: &mut Requirements, from: &Requirements) {
    into.level = from.level.or(into.level);
    into.strength = from.strength.or(into.strength);
    into.dexterity = from.dexterity.or(into.dexterity);
    into.intelligence = from.intelligence.or(into.intelligence);
}

/// Parse the number from an attribute requirement token, accepting both the
/// abbreviated (`86 Dex`) and full (`113 Dexterity`) forms POE2 emits.
fn attr_value(token: &str, abbr: &str, full: &str) -> Option<u32> {
    let num = token
        .strip_suffix(&format!(" {full}"))
        .or_else(|| token.strip_suffix(&format!(" {abbr}")))?;
    num.trim().parse().ok()
}

/// Remove `(...)` segments (e.g. `(unmet)`, `(augmented)`) from a string.
fn strip_annotations(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0u32;
    for c in s.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// Collapse runs of whitespace to single spaces and trim the ends.
fn normalize(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Parse the signed integer out of `+20% (augmented)`.
fn parse_quality(value: &str) -> Option<i32> {
    value.trim().split('%').next()?.trim().parse().ok()
}

/// Parse `23/10` (or `41,941/1,000`) into [`StackSize`].
fn parse_stack_size(value: &str) -> Option<StackSize> {
    let (count, max) = value.trim().split_once('/')?;
    Some(StackSize {
        count: parse_count(count)?,
        max: parse_count(max)?,
    })
}

/// Parse an unsigned count, tolerating `,` thousands separators.
fn parse_count(s: &str) -> Option<u32> {
    s.trim().replace(',', "").parse().ok()
}

/// Parse a section of `{ ... }` descriptors plus their following stat lines,
/// appending each [`Modifier`] to `item.modifiers` in order.
fn parse_modifiers(lines: &[&str], item: &mut Item) {
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        let Some(mut modifier) = parse_descriptor(line) else {
            // Not a descriptor (basic-format copy, or a line we can't read):
            // skip it rather than misattribute it.
            i += 1;
            continue;
        };
        i += 1;
        while i < lines.len() && !lines[i].trim_start().starts_with('{') {
            modifier.stats.push(lines[i].trim().to_string());
            i += 1;
        }
        if modifier.source == Some(ModSource::Fractured) {
            item.fractured = true;
        }
        item.modifiers.push(modifier);
    }
}

/// `{ [<qualifier>] <slot> Modifier ["name"] [(Tier: N)] [<dash> tags] }`, or a
/// non-`Modifier` label like `{ Corruption Enhancement - tags }`.
///
/// The label is whatever precedes the optional quoted name / tier / tags. Name,
/// tier, and tags are each optional; the tag separator may be a hyphen or an em
/// dash (POE2 emits both).
fn descriptor_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"^\{\s*(.*?)\s*(?:"([^"]*)")?\s*(?:\(Tier:\s*(\d+)\))?\s*(?:[-—]\s*([^}]+?))?\s*\}$"#,
        )
        .expect("descriptor regex is valid")
    })
}

fn parse_descriptor(line: &str) -> Option<Modifier> {
    let caps = descriptor_re().captures(line.trim())?;
    let label = caps.get(1)?.as_str().trim();
    if label.is_empty() {
        return None;
    }
    let (kind, source) = classify_label(label);
    let tags = caps
        .get(4)
        .map(|m| {
            m.as_str()
                .split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect()
        })
        .unwrap_or_default();
    Some(Modifier {
        kind,
        source,
        // Treat an empty `""` name (desecrated/hidden mods) as no name.
        name: caps
            .get(2)
            .map(|m| m.as_str().to_string())
            .filter(|s| !s.is_empty()),
        tier: caps.get(3).and_then(|m| m.as_str().parse().ok()),
        tags,
        stats: Vec::new(),
    })
}

/// Split a descriptor label into its slot [`ModKind`] and origin [`ModSource`].
///
/// A label ending in `Modifier` is `[<qualifier>...] <slot> Modifier`; anything
/// else (e.g. `Corruption Enhancement`) is preserved as [`ModKind::Other`].
fn classify_label(label: &str) -> (ModKind, Option<ModSource>) {
    let words: Vec<&str> = label.split_whitespace().collect();
    if words.len() >= 2 && words.last() == Some(&"Modifier") {
        let slot = words[words.len() - 2];
        let kind = match slot {
            "Prefix" => ModKind::Prefix,
            "Suffix" => ModKind::Suffix,
            "Implicit" => ModKind::Implicit,
            "Unique" => ModKind::Unique,
            _ => return (ModKind::Other(label.to_string()), None),
        };
        let source = words[..words.len() - 2].iter().find_map(|w| match *w {
            "Desecrated" => Some(ModSource::Desecrated),
            "Fractured" => Some(ModSource::Fractured),
            "Crafted" => Some(ModSource::Crafted),
            _ => None,
        });
        (kind, source)
    } else {
        (ModKind::Other(label.to_string()), None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_and_non_items() {
        assert_eq!(parse_item(""), Err(ParseError::Empty));
        assert_eq!(parse_item("   \n  "), Err(ParseError::Empty));
        assert_eq!(
            parse_item("just some text\nnot an item"),
            Err(ParseError::NotAnItem)
        );
    }

    #[test]
    fn parses_item_without_item_class_line() {
        // Meta gems copied from the trade site omit `Item Class:`.
        let item = parse_item("Rarity: Gem\nAnimus Splinters\n--------\nLevel: 20 (Max)").unwrap();
        assert_eq!(item.item_class, "");
        assert_eq!(item.rarity, Rarity::Gem);
        assert_eq!(item.name.as_deref(), Some("Animus Splinters"));
    }

    #[test]
    fn unusable_item_keeps_its_name_and_base() {
        // "You cannot use this item" + a separator land in the header; the
        // name/base must still be read from the next section.
        let text = "Item Class: Sceptres\n\
            Rarity: Unique\n\
            You cannot use this item. Its stats will be ignored\n\
            --------\n\
            Sylvan's Effigy\n\
            Stoic Sceptre\n\
            --------\n\
            Item Level: 84\n\
            --------\n\
            Grants Skill: Level 19 Discipline\n\
            Grants Skill: Level 19 Azmerian Wolf";
        let item = parse_item(text).unwrap();
        assert_eq!(item.name.as_deref(), Some("Sylvan's Effigy"));
        assert_eq!(item.base_type.as_deref(), Some("Stoic Sceptre"));
        assert!(item.flags.iter().any(|f| f == "Unusable"));
        assert_eq!(item.item_level, Some(84));
        // The granted skills are still captured (as properties).
        assert_eq!(
            item.properties
                .iter()
                .filter(|p| p.name == "Grants Skill")
                .count(),
            2
        );
    }

    #[test]
    fn requirements_level_and_attributes() {
        let r = parse_requirements("Level 52, 29 Str, 72 Int");
        assert_eq!(r.level, Some(52));
        assert_eq!(r.strength, Some(29));
        assert_eq!(r.intelligence, Some(72));
        assert_eq!(r.dexterity, None);
    }

    #[test]
    fn requirements_strip_unmet_annotations() {
        let r = parse_requirements("Level 78 (unmet), 163 (unmet) Dex");
        assert_eq!(r.level, Some(78));
        assert_eq!(r.dexterity, Some(163));
    }

    #[test]
    fn requirements_full_attribute_name_without_level() {
        let r = parse_requirements("113 Intelligence");
        assert_eq!(r.level, None);
        assert_eq!(r.intelligence, Some(113));
    }

    #[test]
    fn second_requires_line_does_not_clobber_first() {
        // Gems can require a level/attributes AND a weapon type on two lines.
        let item = parse_item(
            "Rarity: Gem\nWhirling Assault\n--------\nRequires: Level 90, 86 Dex, 86 Int\nRequires: Quarterstaff",
        )
        .unwrap();
        assert_eq!(item.requirements.level, Some(90));
        assert_eq!(item.requirements.dexterity, Some(86));
        assert_eq!(item.requirements.intelligence, Some(86));
    }

    #[test]
    fn quality_and_stack_size() {
        assert_eq!(parse_quality("+20% (augmented)"), Some(20));
        assert_eq!(parse_quality(" +0%"), Some(0));
        assert_eq!(
            parse_stack_size(" 23/10"),
            Some(StackSize { count: 23, max: 10 })
        );
        assert_eq!(parse_stack_size("nonsense"), None);
        // Thousands separators (e.g. Verisium `41,941/1,000`).
        assert_eq!(
            parse_stack_size("41,941/1,000"),
            Some(StackSize {
                count: 41941,
                max: 1000
            })
        );
    }

    #[test]
    fn descriptor_variants() {
        // Full: name + tier + multiple tags.
        let m = parse_descriptor(
            r#"{ Suffix Modifier "of the Ice" (Tier: 2) - Elemental, Cold, Resistance }"#,
        )
        .unwrap();
        assert_eq!(m.kind, ModKind::Suffix);
        assert_eq!(m.source, None);
        assert_eq!(m.name.as_deref(), Some("of the Ice"));
        assert_eq!(m.tier, Some(2));
        assert_eq!(m.tags, ["Elemental", "Cold", "Resistance"]);

        // Bare implicit: nothing but the label.
        let m = parse_descriptor("{ Implicit Modifier }").unwrap();
        assert_eq!(m.kind, ModKind::Implicit);
        assert!(m.tags.is_empty());

        // No tier, tags present.
        let m = parse_descriptor(r#"{ Prefix Modifier "Exploiter's" - Damage, Minion }"#).unwrap();
        assert_eq!(m.tier, None);
        assert_eq!(m.tags, ["Damage", "Minion"]);

        // Name only (waystone suffix).
        let m = parse_descriptor(r#"{ Suffix Modifier "Sleet" }"#).unwrap();
        assert_eq!(m.name.as_deref(), Some("Sleet"));

        // Unique modifier.
        let m = parse_descriptor("{ Unique Modifier — Elemental, Cold, Resistance }").unwrap();
        assert_eq!(m.kind, ModKind::Unique);

        // Unknown label is preserved verbatim, not dropped.
        let m = parse_descriptor("{ Corruption Enhancement — Attack }").unwrap();
        assert_eq!(m.kind, ModKind::Other("Corruption Enhancement".to_string()));
        assert_eq!(m.tags, ["Attack"]);

        assert!(parse_descriptor("not a descriptor").is_none());
    }

    #[test]
    fn descriptor_source_qualifiers() {
        let desecrated = parse_descriptor(
            r#"{ Desecrated Suffix Modifier "of Mastery" (Tier: 2) — Attack, Speed }"#,
        )
        .unwrap();
        assert_eq!(desecrated.kind, ModKind::Suffix);
        assert_eq!(desecrated.source, Some(ModSource::Desecrated));

        let fractured = parse_descriptor(
            r#"{ Fractured Suffix Modifier "of Unmaking" (Tier: 1) — Attack, Critical }"#,
        )
        .unwrap();
        assert_eq!(fractured.kind, ModKind::Suffix);
        assert_eq!(fractured.source, Some(ModSource::Fractured));

        let crafted =
            parse_descriptor(r#"{ Crafted Prefix Modifier "Motivating" (Tier: 3) — Damage }"#)
                .unwrap();
        assert_eq!(crafted.kind, ModKind::Prefix);
        assert_eq!(crafted.source, Some(ModSource::Crafted));
    }

    #[test]
    fn descriptor_empty_name_is_none() {
        let m = parse_descriptor(r#"{ Prefix Modifier "" }"#).unwrap();
        assert_eq!(m.kind, ModKind::Prefix);
        assert_eq!(m.name, None);
    }

    #[test]
    fn unidentified_rare_single_header_line_is_the_base() {
        let item = parse_item(
            "Item Class: Sceptres\nRarity: Rare\nSuperior Rattling Sceptre\n--------\nUnidentified",
        )
        .unwrap();
        assert_eq!(item.name, None);
        assert_eq!(item.base_type.as_deref(), Some("Superior Rattling Sceptre"));
        assert!(item.unidentified);
    }

    #[test]
    fn hybrid_modifier_keeps_both_stat_lines() {
        let mut item = bare_item();
        let lines = [
            r#"{ Prefix Modifier "Wanderer's" (Tier: 1) - Mana, Evasion }"#,
            "39(39-42)% increased Evasion Rating",
            "+35(33-39) to maximum Mana",
        ];
        parse_modifiers(&lines, &mut item);
        assert_eq!(item.modifiers.len(), 1);
        assert_eq!(
            item.modifiers[0].stats,
            [
                "39(39-42)% increased Evasion Rating",
                "+35(33-39) to maximum Mana"
            ]
        );
    }

    fn bare_item() -> Item {
        Item {
            item_class: String::new(),
            rarity: Rarity::Rare,
            name: None,
            base_type: None,
            item_level: None,
            quality: None,
            requirements: Requirements::default(),
            sockets: None,
            stack_size: None,
            properties: Vec::new(),
            rune_mods: Vec::new(),
            modifiers: Vec::new(),
            flavour_text: Vec::new(),
            notes: Vec::new(),
            corrupted: false,
            mirrored: false,
            unidentified: false,
            fractured: false,
            flags: Vec::new(),
        }
    }
}
