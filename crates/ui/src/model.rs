//! Shared data types and pure (egui-free) helpers used across the UI.

use parser::{Item, ModKind, Rarity};
use trade_api::{
    EquipmentSelection, ExchangeCheck, ListingStatus, MiscState, PriceCheck, PriceEstimate,
    PriceFilter, ScoutPrice, SessionStatus, StatSelection,
};

const MAX_AFFIXES_PER_GROUP: usize = 3;

/// Tablets and jewels cap at two prefixes and two suffixes (not six like normal gear).
const MAX_LOW_AFFIXES_PER_GROUP: usize = 2;

/// Open prefix and suffix slots on a rare item, as `(prefix, suffix)`; non-rares are `(false, false)`.
pub(crate) fn open_affix_slots(item: &Item) -> (bool, bool) {
    if item.rarity != Rarity::Rare {
        return (false, false);
    }
    let max = if matches!(item.item_class.as_str(), "Tablet" | "Jewels") {
        MAX_LOW_AFFIXES_PER_GROUP
    } else {
        MAX_AFFIXES_PER_GROUP
    };
    let count = |kind: &ModKind| item.modifiers.iter().filter(|m| &m.kind == kind).count();
    (count(&ModKind::Prefix) < max, count(&ModKind::Suffix) < max)
}

/// A waystone's map tier, parsed from its base type (`Waystone (Tier 16)`).
pub(crate) fn waystone_tier(item: &Item) -> Option<u32> {
    if item.item_class != "Waystones" {
        return None;
    }
    let base = item.base_type.as_deref()?;
    let open = base.find('(')?;
    let close = base.find(')')?;
    // `get` (not slicing) so a malformed `)( ` order yields None instead of panicking.
    let inner = base.get(open + 1..close)?;
    inner.split_whitespace().last()?.parse::<u32>().ok()
}

/// Result of a background price check, sent back to the UI thread.
pub(crate) enum Msg {
    Result(Box<Result<PriceCheck, String>>),
    /// poeprices.info ML estimate (rares); `Ok(None)` = declined.
    Estimate(Box<Result<Option<PriceEstimate>, String>>),
    /// Bulk-exchange result for a stackable; fallback when poe2scout has no data.
    Exchange(Box<Result<ExchangeCheck, String>>),
    /// poe2scout economy price; `Ok(None)` = unknown → fall back to the exchange.
    Scout(Box<Result<Option<ScoutPrice>, String>>),
    Teleport(Result<(), String>),
    SessionChecked(SessionStatus),
}

/// What the Settings panel shows beside the POESESSID field.
#[derive(Default, Clone)]
pub(crate) enum SessionCheck {
    #[default]
    Idle,
    /// Not a 32-hex POESESSID — never sent to the server.
    Malformed,
    Checking,
    Valid(Option<String>),
    Invalid,
    /// Couldn't confirm (offline / unexpected status).
    Unknown,
}

/// Which pricing path the loaded item uses.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PriceMode {
    Item,
    Exchange,
}

/// Background state of a bulk-exchange price check (the fallback path).
#[derive(Default)]
pub(crate) enum ExchangePhase {
    #[default]
    Idle,
    Loading,
    Done(ExchangeCheck),
    Failed(String),
}

/// Background state of the poe2scout lookup (the primary stackable price source).
#[derive(Default)]
pub(crate) enum ScoutPhase {
    #[default]
    Idle,
    Loading,
    Done(ScoutPrice),
    /// Reachable but no entry for this currency.
    NotFound,
    Failed(String),
}

#[derive(Default)]
pub(crate) enum Phase {
    #[default]
    Idle,
    Loading,
    Done(PriceCheck),
    Failed(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum View {
    Item,
    Text,
}

/// One toggleable stat filter in the detailed panel. `min`/`max` are text buffers.
pub(crate) struct StatFilterRow {
    pub(crate) id: String,
    /// Canonical stat template, e.g. `#% to Fire Resistance`.
    pub(crate) label: String,
    pub(crate) enabled: bool,
    pub(crate) min: String,
    pub(crate) max: String,
    pub(crate) is_implicit: bool,
}

impl StatFilterRow {
    pub(crate) fn selection(&self) -> StatSelection {
        StatSelection {
            id: self.id.clone(),
            enabled: self.enabled,
            min: parse_num(&self.min),
            max: parse_num(&self.max),
        }
    }
}

/// A defence/offence equipment-property filter, built from parsed properties.
pub(crate) struct EquipmentRow {
    /// Trade filter id (`ev`, `ar`, `es`, …).
    key: String,
    pub(crate) label: String,
    pub(crate) enabled: bool,
    pub(crate) min: String,
    pub(crate) max: String,
}

impl EquipmentRow {
    pub(crate) fn selection(&self) -> EquipmentSelection {
        EquipmentSelection {
            key: self.key.clone(),
            enabled: self.enabled,
            min: parse_num(&self.min),
            max: parse_num(&self.max),
        }
    }
}

/// Map a parsed item-property name to its trade equipment-filter id.
fn equipment_key(property_name: &str) -> Option<&'static str> {
    match property_name {
        "Armour" => Some("ar"),
        "Evasion Rating" => Some("ev"),
        "Energy Shield" => Some("es"),
        "Spirit" => Some("spirit"),
        "Ward" => Some("ward"),
        "Block" | "Block chance" => Some("block"),
        _ => None,
    }
}

/// First numeric run in a property value (`"1099 (augmented)"` → `1099`).
fn first_number(s: &str) -> Option<f64> {
    let start = s.find(|c: char| c.is_ascii_digit())?;
    let rest = &s[start..];
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// Build equipment-property filter rows from the item's defences, prefilled and ticked.
pub(crate) fn build_equipment_rows(
    item: &Item,
    percent: u32,
    exceptional: bool,
) -> Vec<EquipmentRow> {
    let mut rows: Vec<EquipmentRow> = item
        .properties
        .iter()
        .filter_map(|prop| {
            let key = equipment_key(&prop.name)?;
            let value = first_number(&prop.value)?;
            Some(EquipmentRow {
                key: key.to_string(),
                label: prop.name.clone(),
                enabled: true,
                min: fmt_amount(scaled_min(value, percent)),
                max: String::new(),
            })
        })
        .collect();

    // Rune sockets: default-on only on Exceptional bases, where the extra socket is the value.
    let sockets = socket_count(item);
    if sockets > 0 {
        rows.push(EquipmentRow {
            key: "rune_sockets".to_string(),
            label: "Sockets".to_string(),
            enabled: exceptional,
            min: sockets.to_string(),
            max: String::new(),
        });
    }
    rows
}

/// Number of rune sockets (count of `S` on the parsed `Sockets:` line).
fn socket_count(item: &Item) -> usize {
    item.sockets
        .as_deref()
        .map_or(0, |s| s.chars().filter(|c| *c == 'S').count())
}

/// The detailed-mode price-range filter inputs.
#[derive(Default)]
pub(crate) struct PriceFilterState {
    pub(crate) min: String,
    pub(crate) max: String,
    /// Currency id, or empty for "any".
    pub(crate) currency: String,
}

impl PriceFilterState {
    pub(crate) fn to_filter(&self) -> PriceFilter {
        PriceFilter {
            min: parse_num(&self.min),
            max: parse_num(&self.max),
            currency: if self.currency.is_empty() {
                None
            } else {
                Some(self.currency.clone())
            },
        }
    }
}

/// Which tab of the detailed-filter panel is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum FilterTab {
    #[default]
    General,
    Misc,
}

/// A single-value "≥ min" filter with an enable toggle.
#[derive(Default)]
pub(crate) struct MinFilter {
    pub(crate) enabled: bool,
    pub(crate) min: String,
}

impl MinFilter {
    pub(crate) fn new(enabled: bool, min: Option<u32>) -> Self {
        MinFilter {
            enabled,
            min: min
                .filter(|v| *v > 0)
                .map(|v| v.to_string())
                .unwrap_or_default(),
        }
    }

    pub(crate) fn value(&self) -> Option<f64> {
        if self.enabled {
            parse_num(&self.min)
        } else {
            None
        }
    }
}

/// Boolean item attributes for the Miscellaneous section (trade filter id, label).
pub(crate) const MISC_OPTIONS: &[(&str, &str)] = &[
    ("corrupted", "Corrupted"),
    ("crafted", "Crafted"),
    ("desecrated", "Desecrated"),
    ("fractured_item", "Fractured"),
    ("identified", "Identified"),
    ("mirrored", "Mirrored"),
    ("sanctified", "Sanctified"),
    ("twice_corrupted", "Twice Corrupted"),
];

/// A three-state Miscellaneous toggle: Any / Yes (require) / No (forbid).
pub(crate) struct MiscToggle {
    pub(crate) key: &'static str,
    pub(crate) label: &'static str,
    pub(crate) state: MiscState,
}

/// Parse a numeric filter buffer; blank or unparseable → no bound.
fn parse_num(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        t.parse().ok()
    }
}

/// Whitespace-collapsed clipboard text (the XWayland bridge isn't byte-stable).
pub(crate) fn normalize_item_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Stable hash of the whitespace-normalised item text, for de-duplicating repeated Ctrl+C.
pub(crate) fn item_hash(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    normalize_item_text(text).hash(&mut h);
    h.finish()
}

/// Map the configured trade-status string to a [`ListingStatus`] (default securable).
pub(crate) fn parse_status(s: &str) -> ListingStatus {
    match s.trim().to_ascii_lowercase().as_str() {
        "online" => ListingStatus::Online,
        "available" => ListingStatus::Available,
        "any" => ListingStatus::Any,
        _ => ListingStatus::Securable,
    }
}

pub(crate) fn fmt_amount(amount: f64) -> String {
    if amount.fract() == 0.0 {
        format!("{}", amount as i64)
    } else {
        // Up to 3 decimals, trailing zeros trimmed (no rounding, which would over-tighten).
        format!("{amount:.3}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}

/// Scale a rolled value to the filter-min percentage; integer rolls floor, fractions keep precision.
///
/// For a negative (downside) roll — e.g. `-72` from "reduced Amount Recovered"
/// stored as "increased" — scaling toward zero would make the filter *stricter*
/// than the item itself, so the item wouldn't match its own search. Clamp so the
/// min is never tighter than the roll.
pub(crate) fn scaled_min(rolled: f64, percent: u32) -> f64 {
    let mut scaled = rolled * f64::from(percent) / 100.0;
    if rolled < 0.0 {
        scaled = scaled.min(rolled);
    }
    if rolled.fract() == 0.0 {
        scaled.floor()
    } else {
        scaled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item_with(rarity: &str, prefixes: usize, suffixes: usize) -> Item {
        item_with_class("Rings", rarity, prefixes, suffixes)
    }

    fn item_with_class(class: &str, rarity: &str, prefixes: usize, suffixes: usize) -> Item {
        use std::fmt::Write as _;
        let mut text = format!(
            "Item Class: {class}\nRarity: {rarity}\nTest Name\nSapphire Ring\n--------\n\
             Item Level: 80\n--------\n"
        );
        for i in 0..prefixes {
            let _ = write!(
                text,
                "{{ Prefix Modifier \"P{i}\" }}\n+50 to maximum Life\n"
            );
        }
        for i in 0..suffixes {
            let _ = write!(
                text,
                "{{ Suffix Modifier \"S{i}\" }}\n+30% to Cold Resistance\n"
            );
        }
        parser::parse_item(&text).expect("fixture parses")
    }

    #[test]
    fn open_affix_slots_reports_prefix_and_suffix_separately() {
        assert_eq!(open_affix_slots(&item_with("Rare", 1, 1)), (true, true));
        assert_eq!(open_affix_slots(&item_with("Rare", 0, 0)), (true, true));
        assert_eq!(open_affix_slots(&item_with("Rare", 3, 1)), (false, true));
        assert_eq!(open_affix_slots(&item_with("Rare", 1, 3)), (true, false));
        assert_eq!(open_affix_slots(&item_with("Rare", 3, 3)), (false, false));
        assert_eq!(open_affix_slots(&item_with("Magic", 1, 0)), (false, false));
        assert_eq!(open_affix_slots(&item_with("Normal", 0, 0)), (false, false));
    }

    #[test]
    fn waystone_tier_is_parsed_for_waystones_only() {
        let ws = parser::parse_item(
            "Item Class: Waystones\nRarity: Rare\nEvil Bearings\nWaystone (Tier 16)\n--------\n\
             Item Level: 82\n",
        )
        .expect("parses");
        assert_eq!(waystone_tier(&ws), Some(16));

        let ring = item_with_class("Rings", "Rare", 1, 0);
        assert_eq!(waystone_tier(&ring), None);

        let weird = parser::parse_item(
            "Item Class: Waystones\nRarity: Rare\nName\nWeird )( Name\n--------\nItem Level: 82\n",
        )
        .expect("parses");
        assert_eq!(waystone_tier(&weird), None);
    }

    #[test]
    fn fmt_amount_trims_decimals_without_rounding() {
        assert_eq!(fmt_amount(5.0), "5");
        assert_eq!(fmt_amount(2.5), "2.5");
        assert_eq!(fmt_amount(0.17), "0.17");
        assert_eq!(fmt_amount(1.250), "1.25");
    }

    #[test]
    fn scaled_min_floors_integers_but_keeps_fractions() {
        assert_eq!(scaled_min(132.0, 90), 118.0);
        assert_eq!(scaled_min(132.0, 100), 132.0);
        assert_eq!(scaled_min(2.5, 100), 2.5);
        assert!((scaled_min(2.5, 90) - 2.25).abs() < 1e-9);
    }

    #[test]
    fn scaled_min_does_not_tighten_negative_downside_rolls() {
        // A "-72" roll scaled to 90% must not become -65 (stricter than the
        // item itself); it stays at the roll so the item matches its own filter.
        assert_eq!(scaled_min(-72.0, 90), -72.0);
        assert_eq!(scaled_min(-72.0, 100), -72.0);
    }

    #[test]
    fn item_hash_is_whitespace_stable_but_craft_sensitive() {
        let base = "Item Class: Rings\nRarity: Rare\nHonour Spiral\nTopaz Ring\n\
                    --------\n+30% to Lightning Resistance";
        // Whitespace-only noise must NOT change the hash.
        let noisy = "Item Class: Rings\r\nRarity: Rare\nHonour Spiral\nTopaz Ring\n\
                     --------\n+30% to Lightning Resistance   ";
        assert_eq!(item_hash(base), item_hash(noisy));

        // Crafts (quality, socketed rune, added mod) must bust the cache.
        let quality = "Item Class: Rings\nRarity: Rare\nHonour Spiral\nTopaz Ring\n\
                       --------\nQuality: +20%\n--------\n+30% to Lightning Resistance";
        let runed = "Item Class: Rings\nRarity: Rare\nHonour Spiral\nTopaz Ring\n\
                     --------\n+30% to Lightning Resistance\n+10 to Strength (rune)";
        let exalted = "Item Class: Rings\nRarity: Rare\nHonour Spiral\nTopaz Ring\n\
                       --------\n+30% to Lightning Resistance\n+50 to maximum Life";
        assert_ne!(item_hash(base), item_hash(quality));
        assert_ne!(item_hash(base), item_hash(runed));
        assert_ne!(item_hash(base), item_hash(exalted));
    }

    #[test]
    fn tablets_cap_affixes_at_two_per_group() {
        assert_eq!(
            open_affix_slots(&item_with_class("Tablet", "Rare", 2, 2)),
            (false, false)
        );
        assert_eq!(
            open_affix_slots(&item_with_class("Tablet", "Rare", 1, 2)),
            (true, false)
        );
        assert_eq!(
            open_affix_slots(&item_with_class("Tablet", "Rare", 1, 1)),
            (true, true)
        );
        assert_eq!(
            open_affix_slots(&item_with_class("Rings", "Rare", 2, 2)),
            (true, true)
        );
    }

    #[test]
    fn jewels_cap_affixes_at_two_per_group() {
        assert_eq!(
            open_affix_slots(&item_with_class("Jewels", "Rare", 2, 1)),
            (false, true)
        );
        assert_eq!(
            open_affix_slots(&item_with_class("Jewels", "Rare", 2, 2)),
            (false, false)
        );
    }

    #[test]
    fn magic_only_classes_never_report_open_slots() {
        for class in ["Life Flasks", "Mana Flasks", "Charms", "Relics"] {
            assert_eq!(
                open_affix_slots(&item_with_class(class, "Magic", 1, 1)),
                (false, false),
                "{class} is Magic-only and must report no open slots"
            );
        }
    }

    #[test]
    fn waystones_use_the_standard_three_per_group_cap() {
        assert_eq!(
            open_affix_slots(&item_with_class("Waystones", "Rare", 2, 2)),
            (true, true)
        );
        assert_eq!(
            open_affix_slots(&item_with_class("Waystones", "Rare", 3, 3)),
            (false, false)
        );
    }
}
