//! The `trade2/data/stats` and `trade2/data/items` snapshots, and the lookups the query builder needs.

use std::collections::{HashMap, HashSet};

use parser::{Item, ModKind, ModSource, Modifier};
use serde::Deserialize;

use crate::error::Error;
use crate::stat_text::{self, canonical_ggg};

#[derive(Debug, Deserialize)]
struct RawDoc<E> {
    result: Vec<RawGroup<E>>,
}

#[derive(Debug, Deserialize)]
struct RawGroup<E> {
    entries: Vec<E>,
}

#[derive(Debug, Deserialize)]
struct RawStat {
    id: String,
    text: String,
    #[serde(rename = "type")]
    stat_type: String,
}

#[derive(Debug, Deserialize)]
struct RawItem {
    #[serde(rename = "type")]
    type_line: String,
    name: Option<String>,
}

/// A resolved stat filter: the GGG id plus the rolled value(s) from the item.
#[derive(Debug, Clone, PartialEq)]
pub struct MappedStat {
    pub id: String,
    pub stat_type: String,
    pub values: Vec<f64>,
    pub template: String,
}

impl MappedStat {
    pub fn filter_value(&self) -> Option<f64> {
        self.values.first().copied()
    }
}

/// The `trade2/data/stats` snapshot, indexed for lookup by affix type + text.
#[derive(Debug, Default)]
pub struct StatDefinitions {
    by_type_text: HashMap<(String, String), String>,
    by_text: HashMap<String, String>,
}

impl StatDefinitions {
    pub fn from_json(json: &str) -> Result<Self, Error> {
        let doc: RawDoc<RawStat> =
            serde_json::from_str(json).map_err(|e| Error::decode("data/stats", e))?;
        let mut defs = StatDefinitions::default();
        for group in doc.result {
            for stat in group.entries {
                let key = canonical_ggg(&stat.text);
                defs.by_type_text
                    .entry((stat.stat_type.clone(), key.clone()))
                    .or_insert_with(|| stat.id.clone());
                defs.by_text.entry(key).or_insert(stat.id);
            }
        }
        Ok(defs)
    }

    pub fn len(&self) -> usize {
        self.by_type_text.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_type_text.is_empty()
    }

    /// Map all of an item's modifiers to GGG stat filters, choosing local-vs-global per stat.
    pub fn map_item(&self, item: &Item) -> Vec<MappedStat> {
        let ctx = LocalContext::for_item(item);
        item.modifiers
            .iter()
            .flat_map(|m| self.map_modifier(m, ctx))
            .collect()
    }

    /// Resolve one [`Modifier`]'s stat lines to GGG stat filters; unmappable lines skipped.
    pub fn map_modifier(&self, modifier: &Modifier, ctx: LocalContext) -> Vec<MappedStat> {
        modifier
            .stats
            .iter()
            .filter_map(|line| {
                let prefer_local = ctx.prefer_local(line);
                self.map_stat_line(&modifier.kind, modifier.source.as_ref(), line, prefer_local)
            })
            .collect()
    }

    /// Resolve a single stat line; when `prefer_local`, the `(Local)` variant is tried first.
    pub fn map_stat_line(
        &self,
        kind: &ModKind,
        source: Option<&ModSource>,
        line: &str,
        prefer_local: bool,
    ) -> Option<MappedStat> {
        let types = preferred_types(kind, source);
        for cand in stat_text::candidates(line) {
            if prefer_local {
                let local = format!("{} (Local)", cand.template);
                if let Some(mapped) = self.lookup(&types, &local, &cand.values) {
                    return Some(mapped);
                }
            }
            if let Some(mapped) = self.lookup(&types, &cand.template, &cand.values) {
                return Some(mapped);
            }
            // Some count mods only expose a singular "an additional X" presence filter.
            if let Some(singular) = singular_additional_variant(&cand.template) {
                if let Some(mapped) = self.lookup(&types, &singular, &[]) {
                    return Some(mapped);
                }
            }
        }
        None
    }

    /// Explicit (prefix/suffix) stat lines on `item` that matched no trade filter, de-duplicated.
    pub fn unmapped_explicit_lines(&self, item: &Item) -> Vec<String> {
        let ctx = LocalContext::for_item(item);
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for m in item
            .modifiers
            .iter()
            .filter(|m| m.kind != ModKind::Implicit)
        {
            for line in &m.stats {
                if is_unrevealed_placeholder(line) {
                    continue;
                }
                let unmapped = self
                    .map_stat_line(&m.kind, m.source.as_ref(), line, ctx.prefer_local(line))
                    .is_none();
                if unmapped && seen.insert(line.clone()) {
                    out.push(line.clone());
                }
            }
        }
        out
    }

    /// Map a `Grants Skill: Level N <Skill>` property to its `skill.*` id, with level as the value.
    pub fn map_granted_skill(&self, value: &str) -> Option<MappedStat> {
        let rest = value.trim().strip_prefix("Level ")?;
        let (level, skill) = rest.split_once(' ')?;
        let level: f64 = level.trim().parse().ok()?;
        let template = format!("Grants Skill: Level # {}", skill.trim());
        let id = self
            .by_text
            .get(&stat_text::canonical_ggg(&template))?
            .clone();
        Some(MappedStat {
            id,
            stat_type: "skill".to_string(),
            values: vec![level],
            template,
        })
    }

    fn lookup(&self, types: &[&str], template: &str, values: &[f64]) -> Option<MappedStat> {
        for ty in types {
            if let Some(id) = self
                .by_type_text
                .get(&(ty.to_string(), template.to_string()))
            {
                return Some(MappedStat {
                    id: id.clone(),
                    stat_type: ty.to_string(),
                    values: values.to_vec(),
                    template: template.to_string(),
                });
            }
        }
        if let Some(id) = self.by_text.get(template) {
            let stat_type = id.split('.').next().unwrap_or("explicit").to_string();
            return Some(MappedStat {
                id: id.clone(),
                stat_type,
                values: values.to_vec(),
                template: template.to_string(),
            });
        }
        None
    }
}

/// Item context for choosing local-vs-global stat variants.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalContext {
    is_weapon: bool,
    is_armour_piece: bool,
}

impl LocalContext {
    pub fn for_item(item: &Item) -> Self {
        match crate::query::category_for(&item.item_class) {
            Some(c) => LocalContext {
                is_weapon: c.starts_with("weapon."),
                is_armour_piece: c.starts_with("armour.") && c != "armour.quiver",
            },
            None => LocalContext::default(),
        }
    }

    fn prefer_local(self, line: &str) -> bool {
        if is_defence_stat(line) {
            self.is_armour_piece
        } else {
            self.is_weapon
        }
    }
}

fn is_defence_stat(line: &str) -> bool {
    line.contains("Armour")
        || line.contains("Evasion Rating")
        || line.contains("Energy Shield")
        || line.contains("Block")
        || line.contains("Ward")
}

/// Placeholder stat lines POE2 prints for an unrevealed desecrated affix.
const UNREVEALED_PREFIX_LINE: &str = "Desecrated Prefix";
const UNREVEALED_SUFFIX_LINE: &str = "Desecrated Suffix";

pub fn is_unrevealed_placeholder(line: &str) -> bool {
    line == UNREVEALED_PREFIX_LINE || line == UNREVEALED_SUFFIX_LINE
}

/// Count an item's unrevealed desecrated modifiers as `(prefixes, suffixes)`.
pub fn unrevealed_affix_counts(item: &Item) -> (usize, usize) {
    let mut prefixes = 0;
    let mut suffixes = 0;
    for m in &item.modifiers {
        if m.stats.iter().any(|s| s == UNREVEALED_PREFIX_LINE) {
            prefixes += 1;
        } else if m.stats.iter().any(|s| s == UNREVEALED_SUFFIX_LINE) {
            suffixes += 1;
        }
    }
    (prefixes, suffixes)
}

/// The GGG stat-id prefixes to try for a parsed affix, most-specific first.
fn preferred_types(kind: &ModKind, source: Option<&ModSource>) -> Vec<&'static str> {
    match source {
        Some(ModSource::Fractured) => vec!["fractured", "explicit"],
        Some(ModSource::Crafted) => vec!["crafted", "explicit"],
        // `desecrated.*` matches almost nothing on the market; prefer plain explicit.
        Some(ModSource::Desecrated) => vec!["explicit", "desecrated"],
        None => match kind {
            ModKind::Implicit => vec!["implicit", "explicit"],
            ModKind::Prefix | ModKind::Suffix | ModKind::Unique | ModKind::Other(_) => {
                vec!["explicit", "implicit"]
            }
        },
    }
}

/// Turn a `"… # additional <plural>"` count template into the singular `"… an additional <singular>"` form.
fn singular_additional_variant(template: &str) -> Option<String> {
    let idx = template.find("# additional ")?;
    let head = &template[..idx];
    let plural_noun = &template[idx + "# additional ".len()..];
    if plural_noun.is_empty() {
        return None;
    }
    Some(format!(
        "{head}an additional {}",
        singularize_last_word(plural_noun)
    ))
}

/// Singularise the last whitespace-separated word of `s` with simple English rules.
fn singularize_last_word(s: &str) -> String {
    let (head, last) = match s.rsplit_once(' ') {
        Some((h, l)) => (Some(h), l),
        None => (None, s),
    };
    let singular = if let Some(stem) = last.strip_suffix("ies") {
        format!("{stem}y")
    } else if last.ends_with("ses")
        || last.ends_with("xes")
        || last.ends_with("zes")
        || last.ends_with("ches")
        || last.ends_with("shes")
    {
        last[..last.len() - 2].to_string()
    } else if let Some(stem) = last.strip_suffix('s') {
        stem.to_string()
    } else {
        last.to_string()
    };
    match head {
        Some(h) => format!("{h} {singular}"),
        None => singular,
    }
}

/// The `trade2/data/items` snapshot: base types and unique → base lookups.
#[derive(Debug, Default)]
pub struct ItemDefinitions {
    bases: Vec<String>,
    unique_base: HashMap<String, String>,
}

impl ItemDefinitions {
    pub fn from_json(json: &str) -> Result<Self, Error> {
        let doc: RawDoc<RawItem> =
            serde_json::from_str(json).map_err(|e| Error::decode("data/items", e))?;
        let mut bases: Vec<String> = Vec::new();
        let mut seen = HashSet::new();
        let mut unique_base = HashMap::new();
        for group in doc.result {
            for item in group.entries {
                match item.name {
                    Some(name) => {
                        unique_base.entry(name).or_insert(item.type_line);
                    }
                    None => {
                        if seen.insert(item.type_line.clone()) {
                            bases.push(item.type_line);
                        }
                    }
                }
            }
        }
        bases.sort_by(|a, b| {
            word_count(b)
                .cmp(&word_count(a))
                .then_with(|| b.len().cmp(&a.len()))
        });
        Ok(ItemDefinitions { bases, unique_base })
    }

    pub fn base_count(&self) -> usize {
        self.bases.len()
    }

    /// The base type of a unique by its rolled name.
    pub fn unique_base(&self, name: &str) -> Option<&str> {
        self.unique_base.get(name).map(String::as_str)
    }

    /// Resolve a raw base-type line to a base the trade API recognises.
    pub fn resolve_base(&self, raw: &str) -> Option<String> {
        if self.bases.iter().any(|b| b == raw) {
            return Some(raw.to_string());
        }
        self.split_magic_base(raw)
    }

    /// Split a magic item's fused name into its base type via the longest known whole-word run.
    pub fn split_magic_base(&self, magic_name: &str) -> Option<String> {
        let words: Vec<&str> = magic_name.split_whitespace().collect();
        for base in &self.bases {
            if contains_word_run(&words, base) {
                return Some(base.clone());
            }
        }
        None
    }
}

fn word_count(s: &str) -> usize {
    s.split_whitespace().count()
}

fn contains_word_run(words: &[&str], base: &str) -> bool {
    let base_words: Vec<&str> = base.split_whitespace().collect();
    if base_words.is_empty() || base_words.len() > words.len() {
        return false;
    }
    words
        .windows(base_words.len())
        .any(|w| w == base_words.as_slice())
}

#[cfg(test)]
mod tests {
    use super::{
        is_unrevealed_placeholder, singular_additional_variant, singularize_last_word,
        unrevealed_affix_counts,
    };

    #[test]
    fn counts_unrevealed_desecrated_affixes() {
        let text = "Item Class: Jewels\nRarity: Unique\nHeart of the Well\nDiamond\n--------\n\
             Item Level: 82\n--------\n\
             { Prefix Modifier \"\" }\nDesecrated Prefix\n\
             { Prefix Modifier \"\" }\nDesecrated Prefix\n\
             { Suffix Modifier \"\" }\nDesecrated Suffix\n\
             { Suffix Modifier \"\" }\nDesecrated Suffix\n";
        let item = parser::parse_item(text).expect("fixture parses");
        assert_eq!(unrevealed_affix_counts(&item), (2, 2));
        assert!(is_unrevealed_placeholder("Desecrated Prefix"));
        assert!(is_unrevealed_placeholder("Desecrated Suffix"));
        assert!(!is_unrevealed_placeholder("#% to Fire Resistance"));
    }

    #[test]
    fn singularises_trade_count_nouns() {
        assert_eq!(singularize_last_word("Rare Chests"), "Rare Chest");
        assert_eq!(singularize_last_word("Strongboxes"), "Strongbox");
        assert_eq!(singularize_last_word("Abysses"), "Abyss");
        assert_eq!(
            singularize_last_word("additional Abysses"),
            "additional Abyss"
        );
        assert_eq!(singularize_last_word("Shrine"), "Shrine");
    }

    #[test]
    fn builds_singular_additional_variant() {
        assert_eq!(
            singular_additional_variant("Map contains # additional Rare Chests").as_deref(),
            Some("Map contains an additional Rare Chest")
        );
        assert_eq!(
            singular_additional_variant("Map contains # additional Strongboxes").as_deref(),
            Some("Map contains an additional Strongbox")
        );
        assert_eq!(
            singular_additional_variant("#% increased Magic Monsters"),
            None
        );
    }
}
