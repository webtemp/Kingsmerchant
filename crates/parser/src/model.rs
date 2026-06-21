//! Data model for a parsed POE2 item.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rarity {
    Normal,
    Magic,
    Rare,
    Unique,
    Gem,
    Currency,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModKind {
    Implicit,
    Prefix,
    Suffix,
    Unique,
    Other(String),
}

/// Origin qualifier on a modifier's slot, e.g. `Fractured` in `{ Fractured Suffix Modifier … }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModSource {
    Desecrated,
    Fractured,
    Crafted,
}

/// An advanced-format `{ ... }` descriptor plus the stat line(s) that follow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Modifier {
    pub kind: ModKind,
    pub source: Option<ModSource>,
    pub name: Option<String>,
    pub tier: Option<u32>,
    pub tags: Vec<String>,
    pub stats: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Property {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StackSize {
    pub count: u32,
    pub max: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Requirements {
    pub level: Option<u32>,
    pub strength: Option<u32>,
    pub dexterity: Option<u32>,
    pub intelligence: Option<u32>,
}

/// A fully parsed item. Best-effort: unrecognized input is left at default rather than failing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub item_class: String,
    pub rarity: Rarity,
    pub name: Option<String>,
    pub base_type: Option<String>,
    pub item_level: Option<u32>,
    pub quality: Option<i32>,
    pub requirements: Requirements,
    pub sockets: Option<String>,
    pub stack_size: Option<StackSize>,
    pub properties: Vec<Property>,
    pub rune_mods: Vec<String>,
    pub modifiers: Vec<Modifier>,
    pub flavour_text: Vec<String>,
    /// Effect/usage prose on currency & stackables, e.g. "Desecrates a Rare Jewel".
    pub description: Vec<String>,
    pub notes: Vec<String>,
    pub corrupted: bool,
    pub mirrored: bool,
    pub unidentified: bool,
    pub fractured: bool,
    pub flags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("clipboard text is empty")]
    Empty,
    #[error("not a Path of Exile item (missing `Rarity:` header)")]
    NotAnItem,
}
