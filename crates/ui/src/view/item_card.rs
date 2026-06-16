//! The in-game-style item tooltip card, plus the smaller hover-preview card
//! shown for each listing.
//!
//! Two render paths share the badge/colour helpers below: the central card
//! renders a fully parsed [`parser::Item`]; the hover preview renders the
//! trade API's raw listing JSON (fewer fields, mods already split by category).

use std::fmt::Write as _;

use egui::{Color32, RichText};
use parser::{Item, ModKind, ModSource, Modifier};

use super::theme::{frame_color, rarity_color, AFFIX_BLUE};

// ---- palette ---------------------------------------------------------------

/// Solid card background. Deliberately *not* near-black: these cards float over
/// POE2's dark scene, so a too-dark fill reads as transparent/invisible.
const CARD_FILL: Color32 = Color32::from_rgb(0x2a, 0x2c, 0x34);
/// Defence / property values (armour, evasion, requirements).
const PROP_COLOR: Color32 = Color32::from_rgb(0x8f, 0xb8, 0xd6);
// Mod-text colours, matching the trade site's affix colouring.
const FRACTURED_TEXT: Color32 = Color32::from_rgb(0xa2, 0x91, 0x62);
const CRAFTED_TEXT: Color32 = Color32::from_rgb(0xb4, 0xb4, 0xff);
const DESECRATED_TEXT: Color32 = Color32::from_rgb(0xd4, 0x84, 0xe0);
const RUNE_TEXT: Color32 = Color32::from_rgb(0xe6, 0xc2, 0x5a);

/// A small coloured pill: `(label, background, foreground)`.
type Pill = (&'static str, Color32, Color32);

const IMPLICIT_PILL: Pill = ("implicit", rgb(0x2e, 0x7d, 0x32), rgb(0xe6, 0xff, 0xe6));
const DESECRATED_PILL: Pill = ("desecrated", rgb(0x6e, 0x24, 0x52), rgb(0xff, 0xcf, 0xf0));
const FRACTURED_PILL: Pill = ("fractured", rgb(0x5a, 0x4a, 0x22), rgb(0xe8, 0xd8, 0xa0));
const CRAFTED_PILL: Pill = ("crafted", rgb(0x24, 0x3a, 0x6e), rgb(0xcf, 0xe0, 0xff));
const ENCHANT_PILL: Pill = ("enchant", rgb(0x3a, 0x2e, 0x6e), rgb(0xd8, 0xcf, 0xff));
// POE2 suffixes *every* socket-granted line with `(rune)` regardless of what's
// actually socketed (runes, soul cores, idols, …), so label it generically.
const SOCKETED_PILL: Pill = ("socketed", rgb(0x6e, 0x4a, 0x22), rgb(0xff, 0xd9, 0xa0));

const CORRUPTED_PILL: Pill = ("corrupted", rgb(0x6e, 0x1f, 0x1f), rgb(0xff, 0xb3, 0xb3));
const MIRRORED_PILL: Pill = ("mirrored", rgb(0x24, 0x3a, 0x6e), rgb(0xc9, 0xd6, 0xff));
const FRACTURED_STATE_PILL: Pill = ("fractured", rgb(0x5a, 0x4a, 0x22), rgb(0xe8, 0xd8, 0xa0));
const UNID_PILL: Pill = ("unidentified", rgb(0x3a, 0x3a, 0x44), rgb(0xd6, 0xd6, 0xde));
const FOIL_PILL: Pill = ("foil", rgb(0x5a, 0x44, 0x10), rgb(0xff, 0xe0, 0x90));
const FLAG_BG: Color32 = rgb(0x3a, 0x3a, 0x44);
const FLAG_FG: Color32 = rgb(0xd6, 0xd6, 0xde);

// Brighter accents for the outer "special state" frame glow.
const CORRUPT_ACCENT: Color32 = rgb(0xa8, 0x32, 0x32);
const MIRROR_ACCENT: Color32 = rgb(0x6a, 0x7a, 0xdf);
const FRACTURED_ACCENT: Color32 = rgb(0xa2, 0x91, 0x62);
const DESECRATED_ACCENT: Color32 = rgb(0xc9, 0x7b, 0xdd);
const FOIL_ACCENT: Color32 = rgb(0xe6, 0xc2, 0x5a);

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

// ---- central card (parsed item) --------------------------------------------

/// Render a parsed item as an in-game-style tooltip card.
pub(super) fn item_card(ui: &mut egui::Ui, item: &Item, icon_url: Option<&str>) {
    let color = rarity_color(&item.rarity);
    framed(
        ui,
        color,
        item_frame_accent(item),
        egui::Margin::symmetric(12.0, 10.0),
        |ui| {
            ui.set_width(ui.available_width());

            ui.horizontal(|ui| {
                if let Some(url) = icon_url {
                    ui.add(
                        egui::Image::new(url)
                            .fit_to_exact_size(egui::vec2(44.0, 44.0))
                            .rounding(4.0),
                    );
                    ui.add_space(6.0);
                }
                ui.vertical(|ui| {
                    let title = item
                        .name
                        .as_deref()
                        .or(item.base_type.as_deref())
                        .unwrap_or("Unknown item");
                    ui.label(RichText::new(title).color(color).size(18.0).strong());
                    if item.name.is_some() {
                        if let Some(base) = &item.base_type {
                            ui.label(RichText::new(base).color(color).weak());
                        }
                    }
                    ui.label(RichText::new(&item.item_class).weak().small());
                });
            });

            state_badges(ui, item);
            meta_line(ui, item);
            properties_block(ui, item);
            sockets_block(ui, item);

            let implicits = item
                .modifiers
                .iter()
                .filter(|m| m.kind == ModKind::Implicit);
            let explicits = item
                .modifiers
                .iter()
                .filter(|m| m.kind != ModKind::Implicit);
            if item.modifiers.iter().any(|m| m.kind == ModKind::Implicit) {
                thin_separator(ui);
                for m in implicits {
                    render_mod(ui, m);
                }
            }
            if item.modifiers.iter().any(|m| m.kind != ModKind::Implicit) {
                thin_separator(ui);
                for m in explicits {
                    render_mod(ui, m);
                }
            }
        },
    );
}

/// Corrupted / mirrored / fractured / unidentified flags as coloured pills.
fn state_badges(ui: &mut egui::Ui, item: &Item) {
    let has_state = item.corrupted
        || item.mirrored
        || item.fractured
        || item.unidentified
        || !item.flags.is_empty();
    if !has_state {
        return;
    }
    ui.add_space(3.0);
    ui.horizontal_wrapped(|ui| {
        if item.corrupted {
            badge(ui, CORRUPTED_PILL);
        }
        if item.mirrored {
            badge(ui, MIRRORED_PILL);
        }
        if item.fractured {
            badge(ui, FRACTURED_STATE_PILL);
        }
        if item.unidentified {
            badge(ui, UNID_PILL);
        }
        for flag in &item.flags {
            pill(ui, flag, FLAG_BG, FLAG_FG);
        }
    });
}

/// iLvl / quality / requirements.
fn meta_line(ui: &mut egui::Ui, item: &Item) {
    let mut meta: Vec<String> = Vec::new();
    if let Some(ilvl) = item.item_level {
        meta.push(format!("iLvl {ilvl}"));
    }
    if let Some(q) = item.quality {
        meta.push(format!("Q +{q}%"));
    }
    let req = &item.requirements;
    let mut parts = Vec::new();
    if let Some(l) = req.level {
        parts.push(format!("Lvl {l}"));
    }
    if let Some(s) = req.strength {
        parts.push(format!("{s} Str"));
    }
    if let Some(d) = req.dexterity {
        parts.push(format!("{d} Dex"));
    }
    if let Some(i) = req.intelligence {
        parts.push(format!("{i} Int"));
    }
    if !parts.is_empty() {
        meta.push(format!("Req {}", parts.join(" / ")));
    }
    if !meta.is_empty() {
        ui.add_space(2.0);
        ui.label(RichText::new(meta.join("   ")).weak().small());
    }
}

/// Defence / offence properties (armour, evasion, ES, spirit, weapon stats).
fn properties_block(ui: &mut egui::Ui, item: &Item) {
    if item.properties.is_empty() {
        return;
    }
    thin_separator(ui);
    for p in &item.properties {
        ui.label(RichText::new(format!("{}: {}", p.name, p.value)).color(PROP_COLOR));
    }
}

/// Sockets and any stats granted by what's socketed into them.
fn sockets_block(ui: &mut egui::Ui, item: &Item) {
    let count = item
        .sockets
        .as_deref()
        .map_or(0, |s| s.chars().filter(|c| *c == 'S').count());
    if count == 0 && item.rune_mods.is_empty() {
        return;
    }
    thin_separator(ui);
    if count > 0 {
        let filled = item.rune_mods.len().min(count);
        ui.label(RichText::new(format!("Sockets: {filled}/{count} filled")).color(PROP_COLOR));
    }
    for r in &item.rune_mods {
        let text = r.trim_end_matches("(rune)").trim();
        ui.horizontal(|ui| {
            badge(ui, SOCKETED_PILL);
            ui.label(RichText::new(text).color(RUNE_TEXT));
        });
    }
}

fn render_mod(ui: &mut egui::Ui, m: &Modifier) {
    let pill_for = m
        .source
        .map(source_pill)
        .or_else(|| (m.kind == ModKind::Implicit).then_some(IMPLICIT_PILL));
    ui.horizontal(|ui| {
        if let Some(p) = pill_for {
            badge(ui, p);
        }
        ui.label(RichText::new(mod_header(m)).weak().small());
    });
    let color = source_text_color(m.source);
    for stat in &m.stats {
        ui.label(RichText::new(stat).color(color));
    }
}

/// The slot / name / tier line above a mod's stats (the pill shows its source).
fn mod_header(m: &Modifier) -> String {
    let kind = match &m.kind {
        ModKind::Implicit => "Implicit",
        ModKind::Prefix => "Prefix",
        ModKind::Suffix => "Suffix",
        ModKind::Unique => "Unique",
        ModKind::Other(s) => s.as_str(),
    };
    let mut head = kind.to_string();
    if let Some(name) = &m.name {
        head.push_str(" · ");
        head.push_str(name);
    }
    if let Some(tier) = m.tier {
        let _ = write!(head, " (T{tier})");
    }
    head
}

fn source_pill(source: ModSource) -> Pill {
    match source {
        ModSource::Desecrated => DESECRATED_PILL,
        ModSource::Fractured => FRACTURED_PILL,
        ModSource::Crafted => CRAFTED_PILL,
    }
}

fn source_text_color(source: Option<ModSource>) -> Color32 {
    match source {
        Some(ModSource::Fractured) => FRACTURED_TEXT,
        Some(ModSource::Crafted) => CRAFTED_TEXT,
        Some(ModSource::Desecrated) => DESECRATED_TEXT,
        None => AFFIX_BLUE,
    }
}

/// The strongest "special state" → an accent colour for the outer frame glow.
fn item_frame_accent(item: &Item) -> Option<Color32> {
    let has = |s: ModSource| item.modifiers.iter().any(|m| m.source == Some(s));
    if item.corrupted {
        Some(CORRUPT_ACCENT)
    } else if item.mirrored {
        Some(MIRROR_ACCENT)
    } else if item.fractured || has(ModSource::Fractured) {
        Some(FRACTURED_ACCENT)
    } else if has(ModSource::Desecrated) {
        Some(DESECRATED_ACCENT)
    } else {
        None
    }
}

// ---- shared widgets --------------------------------------------------------

/// A rounded card: the rarity-coloured border, plus an optional outer accent
/// stroke (the trade-site-style glow for corrupted/mirrored/fractured/… items).
fn framed(
    ui: &mut egui::Ui,
    border: Color32,
    accent: Option<Color32>,
    margin: egui::Margin,
    body: impl FnOnce(&mut egui::Ui),
) {
    let inner = egui::Frame::none()
        .fill(CARD_FILL)
        .stroke(egui::Stroke::new(1.5, border))
        .rounding(6.0)
        .inner_margin(margin);
    if let Some(a) = accent {
        // Solid fill on the outer ring too, so the accent never leaves a
        // see-through gap around the card.
        egui::Frame::none()
            .fill(CARD_FILL)
            .stroke(egui::Stroke::new(1.0, a))
            .rounding(9.0)
            .inner_margin(egui::Margin::same(3.0))
            .show(ui, |ui| {
                inner.show(ui, body);
            });
    } else {
        inner.show(ui, body);
    }
}

fn badge(ui: &mut egui::Ui, (label, bg, fg): Pill) {
    pill(ui, label, bg, fg);
}

fn pill(ui: &mut egui::Ui, label: &str, bg: Color32, fg: Color32) {
    egui::Frame::none()
        .fill(bg)
        .rounding(7.0)
        .inner_margin(egui::Margin::symmetric(5.0, 1.0))
        .show(ui, |ui| {
            ui.label(RichText::new(label).color(fg).small());
        });
}

fn thin_separator(ui: &mut egui::Ui) {
    ui.add_space(4.0);
    ui.separator();
    ui.add_space(2.0);
}

// ---- hover preview (trade listing JSON) ------------------------------------

/// Where a previewed mod line came from — drives its pill and text colour.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ModCat {
    Implicit,
    Enchant,
    Rune,
    Fractured,
    Explicit,
    Crafted,
    Desecrated,
}

impl ModCat {
    fn pill(self) -> Option<Pill> {
        match self {
            ModCat::Implicit => Some(IMPLICIT_PILL),
            ModCat::Enchant => Some(ENCHANT_PILL),
            ModCat::Rune => Some(SOCKETED_PILL),
            ModCat::Fractured => Some(FRACTURED_PILL),
            ModCat::Crafted => Some(CRAFTED_PILL),
            ModCat::Desecrated => Some(DESECRATED_PILL),
            ModCat::Explicit => None,
        }
    }

    fn text_color(self) -> Color32 {
        match self {
            ModCat::Fractured => FRACTURED_TEXT,
            ModCat::Crafted | ModCat::Enchant => CRAFTED_TEXT,
            ModCat::Desecrated => DESECRATED_TEXT,
            ModCat::Rune => RUNE_TEXT,
            ModCat::Implicit | ModCat::Explicit => AFFIX_BLUE,
        }
    }
}

/// The bits of a listing's item shown in the hover preview.
pub(super) struct ItemPreview {
    icon: Option<String>,
    name: Option<String>,
    base: Option<String>,
    /// Trade `frameType` (0 normal, 1 magic, 2 rare, 3 unique, …) → rarity colour.
    frame_type: u64,
    corrupted: bool,
    foil: bool,
    /// Defence / offence properties as `(name, value)`.
    properties: Vec<(String, String)>,
    /// Requirements joined for one line, e.g. `Level 80 · 121 Dex`.
    requirements: String,
    /// Rune-socket count.
    sockets: usize,
    /// Mod lines in display order, each tagged with its origin.
    mods: Vec<(ModCat, String)>,
}

/// Pull the previewable fields out of a fetch result's raw `item` JSON.
pub(super) fn item_preview(item: &serde_json::Value) -> ItemPreview {
    let s = |k: &str| {
        item.get(k)
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|t| !t.is_empty())
    };

    let properties = json_pairs(item, "properties");
    let requirements = json_pairs(item, "requirements")
        .into_iter()
        .map(|(name, val)| format!("{name} {val}"))
        .collect::<Vec<_>>()
        .join(" · ");
    let sockets = item
        .get("sockets")
        .and_then(serde_json::Value::as_array)
        .map_or(0, Vec::len);

    // Pull from every mod field the trade API uses, keeping each line's origin
    // so it can be badged (a rare's lines can live in any of these).
    let mut mods = Vec::new();
    for (key, cat) in [
        ("implicitMods", ModCat::Implicit),
        ("enchantMods", ModCat::Enchant),
        ("runeMods", ModCat::Rune),
        ("fracturedMods", ModCat::Fractured),
        ("explicitMods", ModCat::Explicit),
        ("craftedMods", ModCat::Crafted),
        ("desecratedMods", ModCat::Desecrated),
        ("scourgeMods", ModCat::Explicit),
    ] {
        if let Some(arr) = item.get(key).and_then(serde_json::Value::as_array) {
            for line in arr.iter().filter_map(serde_json::Value::as_str) {
                mods.push((cat, line.to_string()));
            }
        }
    }
    if mods.is_empty() {
        tracing::debug!(
            name = ?item.get("name").and_then(serde_json::Value::as_str),
            keys = ?item.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()),
            "item preview has no mods"
        );
    }

    ItemPreview {
        icon: s("icon"),
        name: s("name"),
        base: s("typeLine"),
        frame_type: item
            .get("frameType")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        corrupted: item
            .get("corrupted")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        foil: item.get("foilVariation").is_some(),
        properties,
        requirements,
        sockets,
        mods,
    }
}

/// Flatten a trade `properties`/`requirements` array into `(name, first value)`
/// pairs, dropping entries with no value (e.g. the bare item-class label).
fn json_pairs(item: &serde_json::Value, key: &str) -> Vec<(String, String)> {
    let Some(arr) = item.get(key).and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|p| {
            let name = p.get("name")?.as_str()?.to_string();
            let value = p
                .get("values")?
                .as_array()?
                .first()?
                .as_array()?
                .first()?
                .as_str()?
                .to_string();
            (!value.is_empty()).then_some((name, value))
        })
        .collect()
}

/// Show the item preview horizontally centred on the cursor and floating just
/// above it (the cursor sits ~3px inside the bottom edge) — a non-interactive,
/// top-most tooltip. `constrain(true)` keeps it inside the surface (the popup
/// can't draw outside its own bounds).
pub(super) fn show_item_preview_at_cursor(ctx: &egui::Context, item: &ItemPreview) {
    let Some(pos) = ctx.pointer_latest_pos() else {
        return;
    };
    egui::Area::new(egui::Id::new("item-preview"))
        .order(egui::Order::Tooltip)
        .interactable(false)
        // Disable the default fade-in: the area follows the cursor and is
        // re-shown each frame, so its fade age keeps resetting and it never
        // reaches full opacity — it renders near-invisible. Show it solid.
        .fade_in(false)
        .constrain(true)
        // Bottom-centre pivot: the area is centred on the cursor's x and grows
        // upward, nudged down 3px so the cursor sits just inside the bottom.
        .fixed_pos(pos + egui::vec2(0.0, 3.0))
        .pivot(egui::Align2::CENTER_BOTTOM)
        .show(ctx, |ui| {
            render_item_preview(ui, item);
        });
}

/// The in-game-style listing card (rarity border, defences, sockets, badged mods).
fn render_item_preview(ui: &mut egui::Ui, item: &ItemPreview) {
    let color = frame_color(item.frame_type);
    let accent = if item.corrupted {
        Some(CORRUPT_ACCENT)
    } else if item.foil {
        Some(FOIL_ACCENT)
    } else {
        None
    };
    framed(
        ui,
        color,
        accent,
        egui::Margin::symmetric(10.0, 8.0),
        |ui| {
            ui.set_max_width(320.0);
            ui.horizontal(|ui| {
                if let Some(icon) = &item.icon {
                    // Paint the icon into a fixed 48x48 box so a slow/failed load
                    // can't steal space from the text.
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(48.0, 48.0), egui::Sense::hover());
                    egui::Image::new(icon).rounding(4.0).paint_at(ui, rect);
                    ui.add_space(6.0);
                }
                ui.vertical(|ui| {
                    if let Some(name) = &item.name {
                        ui.label(RichText::new(name).color(color).strong().size(15.0));
                    }
                    if let Some(base) = &item.base {
                        ui.label(RichText::new(base).color(color).weak());
                    }
                });
            });

            if item.corrupted || item.foil {
                ui.add_space(3.0);
                ui.horizontal_wrapped(|ui| {
                    if item.corrupted {
                        badge(ui, CORRUPTED_PILL);
                    }
                    if item.foil {
                        badge(ui, FOIL_PILL);
                    }
                });
            }

            if !item.properties.is_empty() {
                thin_separator(ui);
                for (name, value) in &item.properties {
                    ui.label(RichText::new(format!("{name}: {value}")).color(PROP_COLOR));
                }
            }
            if !item.requirements.is_empty() {
                ui.label(
                    RichText::new(format!("Requires {}", item.requirements))
                        .weak()
                        .small(),
                );
            }
            if item.sockets > 0 {
                ui.label(
                    RichText::new(format!("Rune sockets: {}", item.sockets)).color(PROP_COLOR),
                );
            }

            if !item.mods.is_empty() {
                thin_separator(ui);
                for (cat, text) in &item.mods {
                    let clean = clean_mod_markup(text);
                    let color = cat.text_color();
                    if let Some(p) = cat.pill() {
                        ui.horizontal(|ui| {
                            badge(ui, p);
                            ui.label(RichText::new(clean).color(color));
                        });
                    } else {
                        ui.label(RichText::new(clean).color(color));
                    }
                }
            }
        },
    );
}

/// Strip POE2 fetch-text reference markup: `[link|display]` → `display`,
/// `[text]` → `text` (the API returns e.g. `[Resistances|Fire Resistance]`).
fn clean_mod_markup(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        if let Some(close) = after.find(']') {
            let inner = &after[..close];
            // Display text is the part after the last '|' (or the whole thing).
            out.push_str(inner.rsplit('|').next().unwrap_or(inner));
            rest = &after[close + 1..];
        } else {
            out.push_str(&rest[open..]);
            return out;
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::{clean_mod_markup, item_preview, ModCat};

    #[test]
    fn preview_extracts_properties_requirements_sockets_and_categorised_mods() {
        let json = serde_json::json!({
            "typeLine": "Corsair Coat",
            "frameType": 2,
            "corrupted": true,
            "properties": [
                {"name": "Evasion Rating", "values": [["1099", 1]]},
                {"name": "Ring", "values": []}  // no value → dropped
            ],
            "requirements": [
                {"name": "Level", "values": [["80", 0]]},
                {"name": "Dex", "values": [["121", 0]]}
            ],
            "sockets": [{"group": 0}, {"group": 1}],
            "implicitMods": ["5% increased Movement Speed"],
            "explicitMods": ["+132 to maximum Life"],
            "desecratedMods": ["+32 to [Dexterity|Dexterity]"]
        });
        let p = item_preview(&json);

        assert!(p.corrupted);
        assert_eq!(p.sockets, 2);
        assert_eq!(p.base.as_deref(), Some("Corsair Coat"));
        // Empty-value property is dropped; the defence is kept.
        assert_eq!(
            p.properties,
            vec![("Evasion Rating".to_string(), "1099".to_string())]
        );
        assert_eq!(p.requirements, "Level 80 · Dex 121");
        // Mods keep their origin, in display order (implicit, explicit, desecrated).
        assert_eq!(p.mods.len(), 3);
        assert_eq!(p.mods[0].0, ModCat::Implicit);
        assert_eq!(p.mods[2].0, ModCat::Desecrated);
        assert!(p.mods[2].1.contains("Dexterity"));
    }

    #[test]
    fn strips_fetch_text_markup() {
        assert_eq!(
            clean_mod_markup("+162 to [Evasion] Rating"),
            "+162 to Evasion Rating"
        );
        assert_eq!(
            clean_mod_markup("36% increased [Evasion|Evasion] Rating"),
            "36% increased Evasion Rating"
        );
        // The display text is the part after '|'.
        assert_eq!(
            clean_mod_markup("+33% to [Resistances|Fire Resistance]"),
            "+33% to Fire Resistance"
        );
        assert_eq!(clean_mod_markup("no markup here"), "no markup here");
        // Unbalanced bracket is left as-is (no panic).
        assert_eq!(clean_mod_markup("oops [unclosed"), "oops [unclosed");
    }
}
