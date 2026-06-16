//! In-game-style colours shared across the views.

use egui::Color32;
use parser::Rarity;

/// In-game-ish colour for rolled mod text.
pub(super) const AFFIX_BLUE: Color32 = Color32::from_rgb(0x8a, 0x8a, 0xf0);
/// Gold accent (matches the app icon) for the headline median price.
pub(super) const ACCENT_GOLD: Color32 = Color32::from_rgb(0xe6, 0xc2, 0x5a);
/// Green "online" indicator.
pub(super) const ONLINE_DOT: Color32 = Color32::from_rgb(0x4c, 0xd1, 0x37);

/// A parsed item's rarity → its in-game text colour.
pub(super) fn rarity_color(rarity: &Rarity) -> Color32 {
    match rarity {
        Rarity::Normal => Color32::from_rgb(0xc8, 0xc8, 0xc8),
        Rarity::Magic => Color32::from_rgb(0x88, 0x88, 0xff),
        Rarity::Rare => Color32::from_rgb(0xff, 0xff, 0x77),
        Rarity::Unique => Color32::from_rgb(0xaf, 0x60, 0x25),
        Rarity::Gem => Color32::from_rgb(0x1b, 0xa2, 0x9b),
        Rarity::Currency => Color32::from_rgb(0xaa, 0x99, 0x77),
        Rarity::Other(_) => Color32::WHITE,
    }
}

/// Trade `frameType` → its in-game rarity colour (mirrors [`rarity_color`]).
pub(super) fn frame_color(frame_type: u64) -> Color32 {
    match frame_type {
        1 => Color32::from_rgb(0x88, 0x88, 0xff), // magic
        2 => Color32::from_rgb(0xff, 0xff, 0x77), // rare
        3 => Color32::from_rgb(0xaf, 0x60, 0x25), // unique
        4 => Color32::from_rgb(0x1b, 0xa2, 0x9b), // gem
        5 => Color32::from_rgb(0xaa, 0x99, 0x77), // currency
        _ => Color32::from_rgb(0xc8, 0xc8, 0xc8), // normal / other
    }
}
