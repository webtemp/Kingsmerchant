//! The `trade2/data/stats` and `trade2/data/items` snapshots, and the lookups
//! the query builder needs from them (PRD §4.3, §4.4).
//!
//! These are refreshed on app start. This module turns the raw JSON into:
//!   * stat text → GGG stat id (+ rolled values), respecting affix type, and
//!   * magic-item name → base type (the parser leaves magic bases `None`
//!     because the base is fused with affixes on one line).

use std::collections::HashMap;

use parser::{Item, ModKind, ModSource, Modifier};
use serde::Deserialize;

use crate::error::Error;
use crate::stat_text::{self, canonical_ggg};

// ---- raw JSON shapes (verbatim from the API) -------------------------------

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
    /// Present only on uniques (the rolled name).
    name: Option<String>,
}

// ---- stat definitions ------------------------------------------------------

/// A resolved stat filter: the GGG id plus the rolled value(s) from the item.
#[derive(Debug, Clone, PartialEq)]
pub struct MappedStat {
    pub id: String,
    pub stat_type: String,
    pub values: Vec<f64>,
    /// The matched canonical template, for debugging / display.
    pub template: String,
}

impl MappedStat {
    /// The value to use as a search filter minimum (first rolled value).
    pub fn filter_value(&self) -> Option<f64> {
        self.values.first().copied()
    }
}

/// The `trade2/data/stats` snapshot, indexed for lookup by affix type + text.
#[derive(Debug, Default)]
pub struct StatDefinitions {
    /// `(stat type, canonical template)` → stat id.
    by_type_text: HashMap<(String, String), String>,
    /// canonical template → stat id (type-agnostic fallback).
    by_text: HashMap<String, String>,
}

impl StatDefinitions {
    /// Parse the raw `trade2/data/stats` JSON.
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

    /// Number of distinct `(type, text)` entries indexed.
    pub fn len(&self) -> usize {
        self.by_type_text.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_type_text.is_empty()
    }

    /// Map all of an item's modifiers to GGG stat filters, deciding the
    /// local-vs-global stat variant from the item's category (PRD §4.4).
    ///
    /// On armour/weapons, GGG suffixes the *local* variant's text with
    /// ` (Local)` — e.g. `#% increased Evasion Rating (Local)` is the armour
    /// mod, `#% increased Evasion Rating` is the global passive-tree one. They
    /// share display text but have different stat ids, so a search built from
    /// the wrong one returns nothing. We prefer the local variant on gear.
    pub fn map_item(&self, item: &Item) -> Vec<MappedStat> {
        let local = item_has_local_stats(&item.item_class);
        item.modifiers
            .iter()
            .flat_map(|m| self.map_modifier(m, local))
            .collect()
    }

    /// Resolve one parsed [`Modifier`]'s stat lines to GGG stat filters.
    ///
    /// A descriptor can grant several stat lines (a hybrid prefix), so this
    /// returns one [`MappedStat`] per line it could map; unmappable lines are
    /// silently skipped (mirroring the parser's best-effort stance).
    /// `prefer_local` selects the `(Local)` stat variant when one exists.
    pub fn map_modifier(&self, modifier: &Modifier, prefer_local: bool) -> Vec<MappedStat> {
        modifier
            .stats
            .iter()
            .filter_map(|line| {
                self.map_stat_line(&modifier.kind, modifier.source.as_ref(), line, prefer_local)
            })
            .collect()
    }

    /// Resolve a single stat line for a given affix slot/source. When
    /// `prefer_local` is set, the `(Local)` variant of the stat is tried first.
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
        }
        None
    }

    /// Look a canonical `template` up under the preferred stat types, then
    /// type-agnostically (e.g. an enchant on a slot we mapped as explicit).
    fn lookup(&self, types: &[&str], template: &str, values: &[f64]) -> Option<MappedStat> {
        for ty in types {
            if let Some(id) = self.by_type_text.get(&(ty.to_string(), template.to_string())) {
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

/// Whether an item class can carry *local* stats (armour defences, weapon
/// damage/speed/crit). Used to prefer the `(Local)` stat variant.
fn item_has_local_stats(item_class: &str) -> bool {
    crate::query::category_for(item_class)
        .map(|c| c.starts_with("armour.") || c.starts_with("weapon."))
        .unwrap_or(false)
}

/// The GGG stat-id prefixes to try for a parsed affix, most-specific first.
fn preferred_types(kind: &ModKind, source: Option<&ModSource>) -> Vec<&'static str> {
    match source {
        Some(ModSource::Fractured) => vec!["fractured", "explicit"],
        Some(ModSource::Crafted) => vec!["crafted", "explicit"],
        // Desecrated: the `desecrated.*` stat id matches only items whose mod
        // came from desecration (almost nothing on the market), so a price
        // check finds no comparables. Prefer the plain explicit variant (same
        // stat number) — it's the same affix for pricing purposes.
        Some(ModSource::Desecrated) => vec!["explicit", "desecrated"],
        None => match kind {
            ModKind::Implicit => vec!["implicit", "explicit"],
            ModKind::Prefix | ModKind::Suffix | ModKind::Unique | ModKind::Other(_) => {
                vec!["explicit", "implicit"]
            }
        },
    }
}

// ---- item definitions ------------------------------------------------------

/// The `trade2/data/items` snapshot: base types and unique → base lookups.
#[derive(Debug, Default)]
pub struct ItemDefinitions {
    /// All distinct base types, longest (by word count) first, so substring
    /// matching prefers the most specific base.
    bases: Vec<String>,
    /// Unique item name → its base type.
    unique_base: HashMap<String, String>,
}

impl ItemDefinitions {
    pub fn from_json(json: &str) -> Result<Self, Error> {
        let doc: RawDoc<RawItem> =
            serde_json::from_str(json).map_err(|e| Error::decode("data/items", e))?;
        let mut bases: Vec<String> = Vec::new();
        let mut seen = HashMap::new();
        let mut unique_base = HashMap::new();
        for group in doc.result {
            for item in group.entries {
                match item.name {
                    Some(name) => {
                        unique_base.entry(name).or_insert(item.type_line);
                    }
                    None => {
                        if seen.insert(item.type_line.clone(), ()).is_none() {
                            bases.push(item.type_line);
                        }
                    }
                }
            }
        }
        // Longest base name (by word count, then chars) first.
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

    /// The base type of a unique by its rolled name (e.g. `Mageblood` →
    /// `Utility Belt`).
    pub fn unique_base(&self, name: &str) -> Option<&str> {
        self.unique_base.get(name).map(String::as_str)
    }

    /// Resolve a raw base-type line to a base GGG's trade API recognises.
    ///
    /// POE2 shows higher-tier weapon/armour bases with a display prefix the
    /// trade `type` omits — e.g. the clipboard says `Exceptional Crude Bow` but
    /// GGG only knows `Crude Bow`. An exact known base wins (so `Runeforged
    /// Crude Bow` stays intact); otherwise we strip the prefix by finding the
    /// longest known base that appears as a whole-word run. `None` if nothing
    /// matches (the caller then falls back to the category filter).
    pub fn resolve_base(&self, raw: &str) -> Option<String> {
        if self.bases.iter().any(|b| b == raw) {
            return Some(raw.to_string());
        }
        self.split_magic_base(raw)
    }

    /// Split a magic item's fused name (`Professor's Volatile Wand of
    /// Expertise`) into its base type (`Volatile Wand`) by finding the longest
    /// known base that appears as a whole-word run inside the name.
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

/// Does `base` (as a sequence of whole words) appear contiguously in `words`?
fn contains_word_run(words: &[&str], base: &str) -> bool {
    let base_words: Vec<&str> = base.split_whitespace().collect();
    if base_words.is_empty() || base_words.len() > words.len() {
        return false;
    }
    words
        .windows(base_words.len())
        .any(|w| w == base_words.as_slice())
}
