//! POE2 item-text parser: clipboard item text → [`Item`] struct.
//! Pure logic, no IO/network/UI.
//!
//! Targets POE2's **advanced** item format (with
//! `{ Prefix Modifier "..." (Tier: N) - Tags }` descriptors) so we can read mod
//! tiers and value ranges.

mod model;
mod parse;

pub use model::{
    Item, ModKind, ModSource, Modifier, ParseError, Property, Rarity, Requirements, StackSize,
};
pub use parse::parse_item;
