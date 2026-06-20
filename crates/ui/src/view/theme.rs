//! Colours shared across the views: user-themeable accents ([`Theme`], read via
//! a per-frame thread-local) and fixed in-game colours ([`rarity_color`], [`frame_color`]).

use std::cell::Cell;

use egui::Color32;
use parser::Rarity;

use crate::config::ThemeConfig;

/// Resolved, render-ready accent palette with opacity baked into the fill/border alpha.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Theme {
    pub accent_gold: Color32,
    pub affix_blue: Color32,
    pub online_dot: Color32,
    pub header_bg: Color32,
    pub overlay_fill: Color32,
    pub overlay_stroke: Color32,
}

impl Theme {
    /// Resolve a config block into render-ready colours, falling back per-field on bad hex.
    pub(crate) fn from_config(cfg: &ThemeConfig) -> Self {
        let d = Theme::default();
        let opacity = cfg.opacity.clamp(0.0, 1.0);
        Theme {
            accent_gold: parse_hex(&cfg.accent_gold).unwrap_or(d.accent_gold),
            affix_blue: parse_hex(&cfg.affix_blue).unwrap_or(d.affix_blue),
            online_dot: parse_hex(&cfg.online_dot).unwrap_or(d.online_dot),
            header_bg: parse_hex(&cfg.header_bg).unwrap_or(d.header_bg),
            overlay_fill: with_opacity(
                parse_hex(&cfg.overlay_fill).unwrap_or(d.overlay_fill),
                opacity,
            ),
            overlay_stroke: with_opacity(
                parse_hex(&cfg.overlay_stroke).unwrap_or(d.overlay_stroke),
                opacity,
            ),
        }
    }
}

impl Default for Theme {
    /// The built-in palette (matches [`ThemeConfig::default`], fully opaque).
    fn default() -> Self {
        Theme {
            accent_gold: Color32::from_rgb(0xe6, 0xc2, 0x5a),
            affix_blue: Color32::from_rgb(0x8a, 0x8a, 0xf0),
            online_dot: Color32::from_rgb(0x4c, 0xd1, 0x37),
            header_bg: Color32::from_rgb(0x17, 0x17, 0x1c),
            overlay_fill: Color32::from_rgb(0x2c, 0x2e, 0x36),
            overlay_stroke: Color32::from_rgb(0x50, 0x52, 0x5e),
        }
    }
}

/// Apply an `0.0..=1.0` opacity to a colour's alpha.
fn with_opacity(c: Color32, opacity: f32) -> Color32 {
    let a = (opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
}

/// Parse `#rrggbb` / `rrggbb` into an opaque colour; `None` for anything else.
pub(crate) fn parse_hex(s: &str) -> Option<Color32> {
    let h = s.strip_prefix('#').unwrap_or(s);
    if h.len() != 6 {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some(Color32::from_rgb(byte(0)?, byte(2)?, byte(4)?))
}

/// Format a colour back to `#rrggbb` (alpha dropped) for persisting to config.
pub(crate) fn to_hex(c: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

thread_local! {
    /// The palette the accent helpers read until a surface calls [`set_active`].
    static ACTIVE: Cell<Theme> = Cell::new(Theme::default());
}

/// Install the active palette for the current frame, before any accent helper is read.
pub(crate) fn set_active(theme: Theme) {
    ACTIVE.with(|t| t.set(theme));
}

pub(super) fn accent_gold() -> Color32 {
    ACTIVE.with(|t| t.get().accent_gold)
}

pub(super) fn affix_blue() -> Color32 {
    ACTIVE.with(|t| t.get().affix_blue)
}

pub(super) fn online_dot() -> Color32 {
    ACTIVE.with(|t| t.get().online_dot)
}

pub(super) fn header_bg() -> Color32 {
    ACTIVE.with(|t| t.get().header_bg)
}

/// A built-in theme applied with one click in Settings; the first is the default.
pub(crate) struct Preset {
    pub name: &'static str,
    pub theme: ThemeConfig,
}

/// The shipped presets, in display order.
pub(crate) fn presets() -> Vec<Preset> {
    vec![
        Preset {
            name: "Default Gold",
            theme: ThemeConfig::default(),
        },
        Preset {
            name: "Minimal Slate",
            theme: ThemeConfig {
                accent_gold: "#d7dde5".to_string(),
                affix_blue: "#8fb0c9".to_string(),
                online_dot: "#7bc97b".to_string(),
                header_bg: "#1b1f27".to_string(),
                overlay_fill: "#232831".to_string(),
                overlay_stroke: "#3a414e".to_string(),
                opacity: 0.92,
            },
        },
        Preset {
            name: "Crimson Ember",
            theme: ThemeConfig {
                accent_gold: "#ff8c42".to_string(),
                affix_blue: "#e8a0a0".to_string(),
                online_dot: "#6bcb7b".to_string(),
                header_bg: "#1a1315".to_string(),
                overlay_fill: "#241a1c".to_string(),
                overlay_stroke: "#6e3b3f".to_string(),
                opacity: 0.96,
            },
        },
        Preset {
            name: "Arcane Violet",
            theme: ThemeConfig {
                accent_gold: "#c9a6ff".to_string(),
                affix_blue: "#9db4ff".to_string(),
                online_dot: "#74d6a8".to_string(),
                header_bg: "#15121f".to_string(),
                overlay_fill: "#1d192b".to_string(),
                overlay_stroke: "#4d4080".to_string(),
                opacity: 0.94,
            },
        },
    ]
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hex_accepts_with_and_without_hash() {
        let want = Color32::from_rgb(0xe6, 0xc2, 0x5a);
        assert_eq!(parse_hex("#e6c25a"), Some(want));
        assert_eq!(parse_hex("E6C25A"), Some(want));
    }

    #[test]
    fn parse_hex_rejects_malformed() {
        assert_eq!(parse_hex(""), None);
        assert_eq!(parse_hex("#fff"), None);
        assert_eq!(parse_hex("#gggggg"), None);
        assert_eq!(parse_hex("#e6c25a00"), None);
    }

    #[test]
    fn to_hex_round_trips() {
        let c = Color32::from_rgb(0x12, 0xab, 0xff);
        assert_eq!(parse_hex(&to_hex(c)), Some(c));
    }

    #[test]
    fn from_config_bakes_opacity_into_fill_alpha() {
        let cfg = ThemeConfig {
            opacity: 0.5,
            ..ThemeConfig::default()
        };
        let theme = Theme::from_config(&cfg);
        assert_eq!(theme.accent_gold.a(), 0xff);
        assert_eq!(theme.overlay_fill.a(), 128);
        assert_eq!(theme.overlay_stroke.a(), 128);
    }

    #[test]
    fn from_config_falls_back_on_bad_hex_and_clamps_opacity() {
        let cfg = ThemeConfig {
            accent_gold: "not-a-colour".to_string(),
            opacity: 5.0,
            ..ThemeConfig::default()
        };
        let theme = Theme::from_config(&cfg);
        assert_eq!(theme.accent_gold, Theme::default().accent_gold);
        assert_eq!(theme.overlay_fill.a(), 0xff);
    }
}
