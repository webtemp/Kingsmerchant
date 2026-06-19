//! The small shared data types and pure (egui-free) helpers used across the
//! UI: the background-message and phase enums, the detailed-filter row structs
//! and their selection mappings, and the item-hashing / number-formatting /
//! status-parsing helpers. No rendering lives here.

use parser::{Item, ModKind, Rarity};
use trade_api::{
    EquipmentSelection, ExchangeCheck, ListingStatus, PriceCheck, PriceEstimate, PriceFilter,
    ScoutPrice, SessionStatus, StatSelection,
};

/// Standard rare-item affix cap: up to three prefixes and three suffixes.
const MAX_AFFIXES_PER_GROUP: usize = 3;

/// Tablets (Breach/Abyss/… map-device tablets) cap at two prefixes and two
/// suffixes — four mods total, not six like normal rare gear.
const MAX_TABLET_AFFIXES_PER_GROUP: usize = 2;

/// Open (empty) prefix and suffix slots on a rare item, as `(prefix, suffix)`.
/// Only rares can be crafted into, so non-rares always report `(false, false)`.
/// Counts each `{ Prefix }` / `{ Suffix }` descriptor as one filled slot.
///
/// Tablets use a lower cap (2/2) than normal gear (3/3), so a full tablet
/// correctly reports no open slots instead of phantom empty rows.
pub(crate) fn open_affix_slots(item: &Item) -> (bool, bool) {
    if item.rarity != Rarity::Rare {
        return (false, false);
    }
    let max = if item.item_class == "Tablet" {
        MAX_TABLET_AFFIXES_PER_GROUP
    } else {
        MAX_AFFIXES_PER_GROUP
    };
    let count = |kind: &ModKind| item.modifiers.iter().filter(|m| &m.kind == kind).count();
    (count(&ModKind::Prefix) < max, count(&ModKind::Suffix) < max)
}

/// A waystone's map tier, parsed from its base type (`Waystone (Tier 16)`).
/// `None` for non-waystones or when no tier is present.
pub(crate) fn waystone_tier(item: &Item) -> Option<u32> {
    if item.item_class != "Waystones" {
        return None;
    }
    let base = item.base_type.as_deref()?;
    let open = base.find('(')?;
    let inner = &base[open + 1..base.find(')')?];
    inner.split_whitespace().last()?.parse::<u32>().ok()
}

/// Result of a background price check, sent back to the UI thread.
pub(crate) enum Msg {
    Result(Box<Result<PriceCheck, String>>),
    /// poeprices.info ML estimate (rares). `None` = poeprices declined to price
    /// it; `Err` = it failed.
    Estimate(Box<Result<Option<PriceEstimate>, String>>),
    /// Bulk-exchange result for a stackable (currency/rune/fragment/…), used as
    /// the fallback when poe2scout has no economy data.
    Exchange(Box<Result<ExchangeCheck, String>>),
    /// poe2scout economy price for a stackable (the primary currency source).
    /// `Ok(None)` = poe2scout doesn't know it → fall back to the exchange.
    Scout(Box<Result<Option<ScoutPrice>, String>>),
    /// Outcome of an Instant Buyout hideout teleport (`Ok` = GGG accepted it).
    Teleport(Result<(), String>),
    /// Outcome of a live POESESSID validation (Settings panel).
    SessionChecked(SessionStatus),
}

/// What the Settings panel shows beside the POESESSID field, driven by the
/// instant format check and then the live server validation.
#[derive(Default, Clone)]
pub(crate) enum SessionCheck {
    /// Nothing entered, or a saved session not yet (re)validated this session.
    #[default]
    Idle,
    /// The entered value isn't a 32-hex POESESSID — never sent to the server.
    Malformed,
    /// Debounce elapsing / validation request in flight.
    Checking,
    /// The server accepted it (with the account name, when exposed).
    Valid(Option<String>),
    /// The server rejected it (401/403) — wrong or expired.
    Invalid,
    /// Couldn't confirm (offline, or an unexpected status); the cause is logged.
    Unknown,
}

/// Which pricing path the loaded item uses: a per-item search, or the bulk
/// currency exchange for stackables.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PriceMode {
    Item,
    Exchange,
}

/// Background state of a bulk-exchange price check (parallel to [`Phase`], which
/// covers the per-item search). Used only as the fallback path now that
/// poe2scout (see [`ScoutPhase`]) is the primary currency source.
#[derive(Default)]
pub(crate) enum ExchangePhase {
    #[default]
    Idle,
    Loading,
    Done(ExchangeCheck),
    Failed(String),
}

/// Background state of the poe2scout economy lookup — the primary price source
/// for stackables. On [`Failed`](ScoutPhase::Failed) or
/// [`NotFound`](ScoutPhase::NotFound) the UI falls back to the official
/// bulk exchange (see [`ExchangePhase`]).
#[derive(Default)]
pub(crate) enum ScoutPhase {
    #[default]
    Idle,
    Loading,
    Done(ScoutPrice),
    /// poe2scout was reachable but had no entry for this currency.
    NotFound,
    /// poe2scout failed (down / decode / HTTP) — cause carried for logging.
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

/// One toggleable stat filter in the detailed panel, built from the item's
/// mapped stats. `min`/`max` are text buffers (blank = unbounded) so they can be
/// cleared.
pub(crate) struct StatFilterRow {
    pub(crate) id: String,
    /// Human-ish label (the canonical stat template, e.g. `#% to Fire Resistance`).
    pub(crate) label: String,
    pub(crate) enabled: bool,
    pub(crate) min: String,
    pub(crate) max: String,
    /// This filter is an implicit mod — flagged with a pill and off by default.
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

/// A defence/offence equipment-property filter, built from the item's parsed
/// properties (e.g. `Evasion Rating: 1099`) rather than its affix mods.
pub(crate) struct EquipmentRow {
    /// Trade filter id (`ev`, `ar`, `es`, …).
    key: String,
    /// Display label (the property name, e.g. `Evasion Rating`).
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

/// Map a parsed item-property name to its trade equipment-filter id, for the
/// properties worth filtering on (defences + spirit).
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

/// Build equipment-property filter rows from the item's defences, prefilled with
/// the item's value and ticked — the key thing you search armour by.
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

    // Rune sockets (the "S S S" line). Usually not worth filtering, but on an
    // Exceptional base the extra socket is the whole value, so default it on
    // there (min = the item's own count); otherwise available but off.
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
    /// Currency id (`exalted`, …) or empty for "any".
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

/// A single-value "≥ min" filter with an enable toggle (item quality, item
/// level — both routed to `type_filters`).
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

/// Boolean item attributes for the Miscellaneous section (trade filter id,
/// label), sorted alphabetically by label. All off by default.
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

/// A boolean Miscellaneous toggle (e.g. Corrupted). Checked → require `true`.
pub(crate) struct MiscToggle {
    pub(crate) key: &'static str,
    pub(crate) label: &'static str,
    pub(crate) on: bool,
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

/// Whitespace-collapsed form of clipboard text. Two copies of the same item can
/// differ by line endings/spacing (the XWayland bridge isn't byte-stable), so we
/// collapse every whitespace run to one space before comparing/hashing.
pub(crate) fn normalize_item_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Stable hash identifying an item, for de-duplicating repeated Ctrl+C. Hashes
/// the *parsed* structure (name/base/class/mod lines) so it's invariant to
/// clipboard formatting; falls back to normalised text if it doesn't parse.
pub(crate) fn item_hash(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    match parser::parse_item(text) {
        Ok(item) => {
            item.name.hash(&mut h);
            item.base_type.hash(&mut h);
            item.item_class.hash(&mut h);
            for m in &item.modifiers {
                m.stats.hash(&mut h);
            }
        }
        Err(_) => normalize_item_text(text).hash(&mut h),
    }
    h.finish()
}

/// Map the configured trade-status string to a [`ListingStatus`] (defaults to
/// Instant Buyout / securable for anything unrecognised).
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
        // Up to 3 decimals, trailing zeros trimmed — so 0.17 stays "0.17" (not
        // rounded to "0.2", which over-tightens the search) and 2.5 stays "2.5".
        format!("{amount:.3}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}

/// Scale a rolled value to the configured filter-min percentage. Integer rolls
/// floor (90% of 132 → 118); fractional rolls keep their precision.
pub(crate) fn scaled_min(rolled: f64, percent: u32) -> f64 {
    let scaled = rolled * f64::from(percent) / 100.0;
    if rolled.fract() == 0.0 {
        scaled.floor()
    } else {
        scaled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a parsed item with `prefixes` prefix and `suffixes` suffix mods.
    fn item_with(rarity: &str, prefixes: usize, suffixes: usize) -> Item {
        item_with_class("Rings", rarity, prefixes, suffixes)
    }

    /// As [`item_with`], but with an explicit `Item Class:` (e.g. `Tablet`).
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
        // Rare with room in both groups.
        assert_eq!(open_affix_slots(&item_with("Rare", 1, 1)), (true, true));
        assert_eq!(open_affix_slots(&item_with("Rare", 0, 0)), (true, true));
        // One group full, the other free — reported independently.
        assert_eq!(open_affix_slots(&item_with("Rare", 3, 1)), (false, true));
        assert_eq!(open_affix_slots(&item_with("Rare", 1, 3)), (true, false));
        // All six slots filled → nothing to craft.
        assert_eq!(open_affix_slots(&item_with("Rare", 3, 3)), (false, false));
        // Non-rares never qualify, even with free slots.
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

        // A ring with parentheses elsewhere must not be mistaken for a tier.
        let ring = item_with_class("Rings", "Rare", 1, 0);
        assert_eq!(waystone_tier(&ring), None);
    }

    #[test]
    fn tablets_cap_affixes_at_two_per_group() {
        // A full tablet is 2 prefixes + 2 suffixes — no open slots (the bug:
        // it used to report a phantom 3rd prefix/suffix as craftable).
        assert_eq!(
            open_affix_slots(&item_with_class("Tablet", "Rare", 2, 2)),
            (false, false)
        );
        // One prefix free, suffixes full.
        assert_eq!(
            open_affix_slots(&item_with_class("Tablet", "Rare", 1, 2)),
            (true, false)
        );
        // Both groups have room.
        assert_eq!(
            open_affix_slots(&item_with_class("Tablet", "Rare", 1, 1)),
            (true, true)
        );
        // Normal gear still uses the 3/3 cap (regression guard).
        assert_eq!(
            open_affix_slots(&item_with_class("Rings", "Rare", 2, 2)),
            (true, true)
        );
    }
}
