//! POE2 item-text parser (PRD §4.3).
//!
//! Pure logic: clipboard item text → [`Item`] struct. No IO, no network, no UI.
//! The stat/item-definition snapshots mentioned in §4.3 (which *do* need
//! network IO) are a separate concern wired in by the binary; this crate only
//! turns the text the game writes into structured data.
//!
//! Targets POE2's **advanced** item description format (the one with
//! `{ Prefix Modifier "..." (Tier: N) - Tags }` descriptors), which is what a
//! price-check tool needs to read mod tiers and value ranges.

mod model;
mod parse;

pub use model::{
    Item, ModKind, ModSource, Modifier, ParseError, Property, Rarity, Requirements, StackSize,
};
pub use parse::parse_item;
