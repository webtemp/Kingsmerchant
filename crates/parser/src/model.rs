//! Data model for a parsed POE2 item.

/// Item rarity, as written on the `Rarity:` header line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rarity {
    Normal,
    Magic,
    Rare,
    Unique,
    Gem,
    Currency,
    /// Any rarity string we don't model yet (e.g. `Quest`, `Relic`).
    Other(String),
}

impl Rarity {
    pub fn parse(s: &str) -> Rarity {
        match s {
            "Normal" => Rarity::Normal,
            "Magic" => Rarity::Magic,
            "Rare" => Rarity::Rare,
            "Unique" => Rarity::Unique,
            "Gem" => Rarity::Gem,
            "Currency" => Rarity::Currency,
            other => Rarity::Other(other.to_string()),
        }
    }
}

/// Which kind of modifier a descriptor declares.
///
/// POE2 labels modifiers with the affix slot (`Prefix`/`Suffix`/`Implicit`),
/// `Unique Modifier`, or others like `Corruption Enhancement`. Unknown labels
/// are preserved verbatim in [`Other`](ModKind::Other).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModKind {
    Implicit,
    Prefix,
    Suffix,
    /// A unique item's intrinsic modifier (`Unique Modifier`).
    Unique,
    /// Any other descriptor label, e.g. `Corruption Enhancement`.
    Other(String),
}

/// Origin qualifier prefixed to a modifier's slot in the descriptor, e.g. the
/// `Fractured` in `{ Fractured Suffix Modifier … }`. Mutually exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModSource {
    Desecrated,
    Fractured,
    Crafted,
}

/// A single modifier: an advanced-format `{ ... }` descriptor plus the stat
/// line(s) that follow. One descriptor can grant several stats (e.g. a hybrid
/// prefix), so [`stats`](Modifier::stats) is a list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Modifier {
    pub kind: ModKind,
    /// Origin qualifier (`Desecrated`/`Fractured`/`Crafted`), if any.
    pub source: Option<ModSource>,
    /// Affix name, e.g. `Hellion's` / `of the Ice`. `None` for implicits, for
    /// descriptors without a quoted name, and for empty `""` names.
    pub name: Option<String>,
    /// Mod tier, where the game reports one. Some affixes have no tier.
    pub tier: Option<u32>,
    /// Stat-group tags, e.g. `["Elemental", "Cold", "Resistance"]`.
    pub tags: Vec<String>,
    /// The human-readable stat line(s) this modifier produced.
    pub stats: Vec<String>,
}

/// A generic `Key: Value` property from a non-modifier section
/// (e.g. `Evasion Rating: 391 (augmented)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Property {
    pub name: String,
    pub value: String,
}

/// Stack size for stackable items, from `Stack Size: 23/10`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StackSize {
    pub count: u32,
    pub max: u32,
}

/// Use requirements, from the `Requires:` line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Requirements {
    pub level: Option<u32>,
    pub strength: Option<u32>,
    pub dexterity: Option<u32>,
    pub intelligence: Option<u32>,
}

/// A fully parsed item.
///
/// Best-effort: unrecognized input is left at default rather than failing, so
/// unfamiliar item types still yield a usable header. Some fields (`stack_size`,
/// `rune_mods`, `flavour_text`, `notes`, `mirrored`, `unidentified`, `flags`)
/// are parsed but not yet read by any consumer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// Raw `Item Class:` value, e.g. `Body Armours`, `Rings`, `Support Gems`.
    /// Empty when the copy omits the header line (some non-game copies do).
    pub item_class: String,
    pub rarity: Rarity,
    /// Title line for rares/uniques (and the single name line for
    /// gems/currency). `None` for normal items and unidentified rares, which
    /// only have a base type.
    pub name: Option<String>,
    /// Base type. `None` for magic items (the base is fused with affixes on one
    /// line and can't be split without the item-definition snapshot) and for
    /// gems/currency.
    pub base_type: Option<String>,
    pub item_level: Option<u32>,
    /// Quality percentage, signed (e.g. `20` from `Quality: +20%`).
    pub quality: Option<i32>,
    pub requirements: Requirements,
    /// Raw sockets/runes string, e.g. `S S` or `G G G` or `S J`.
    pub sockets: Option<String>,
    pub stack_size: Option<StackSize>,
    pub properties: Vec<Property>,
    /// Stat lines granted by socketed runes (the `(rune)`-suffixed lines).
    pub rune_mods: Vec<String>,
    /// All `{ ... }`-descriptor modifiers in document order (implicits first,
    /// then explicits, then any unique/other mods). Partition by
    /// [`Modifier::kind`] when a specific slot is needed.
    pub modifiers: Vec<Modifier>,
    pub flavour_text: Vec<String>,
    /// `Note:` lines (players often append a price note).
    pub notes: Vec<String>,
    pub corrupted: bool,
    pub mirrored: bool,
    pub unidentified: bool,
    pub fractured: bool,
    /// Other recognized standalone marker lines we don't model as bools
    /// (e.g. `Sanctified`, `Hinekora's Lock`, `Twice Corrupted`).
    pub flags: Vec<String>,
}

/// Why [`parse_item`](crate::parse_item) rejected the input.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("clipboard text is empty")]
    Empty,
    #[error("not a Path of Exile item (missing `Rarity:` header)")]
    NotAnItem,
}
