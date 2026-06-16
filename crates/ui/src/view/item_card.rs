//! The in-game-style item tooltip card, plus the smaller hover-preview card
//! shown for each listing.

use egui::{Color32, RichText};
use parser::{Item, ModKind};

use super::theme::{frame_color, rarity_color, AFFIX_BLUE, HEADER_BG};

/// Render a parsed item as an in-game-style tooltip card.
pub(super) fn item_card(ui: &mut egui::Ui, item: &Item, icon_url: Option<&str>) {
    let color = rarity_color(&item.rarity);
    egui::Frame::none()
        .fill(HEADER_BG)
        .stroke(egui::Stroke::new(1.5, color))
        .rounding(6.0)
        .inner_margin(egui::Margin::symmetric(12.0, 10.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());

            // Header: icon + name/base, centred-ish.
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

            // Meta line: ilvl / quality / requirements.
            let mut meta: Vec<String> = Vec::new();
            if let Some(ilvl) = item.item_level {
                meta.push(format!("iLvl {ilvl}"));
            }
            if let Some(q) = item.quality {
                meta.push(format!("Q +{q}%"));
            }
            if let Some(lvl) = item.requirements.level {
                meta.push(format!("Req Lvl {lvl}"));
            }
            if !meta.is_empty() {
                ui.add_space(2.0);
                ui.label(RichText::new(meta.join("   ")).weak().small());
            }

            let implicits: Vec<_> = item
                .modifiers
                .iter()
                .filter(|m| m.kind == ModKind::Implicit)
                .collect();
            let explicits: Vec<_> = item
                .modifiers
                .iter()
                .filter(|m| m.kind != ModKind::Implicit)
                .collect();

            if !implicits.is_empty() {
                thin_separator(ui);
                for m in implicits {
                    render_mod(ui, m);
                }
            }
            if !explicits.is_empty() {
                thin_separator(ui);
                for m in explicits {
                    render_mod(ui, m);
                }
            }
            if item.corrupted {
                thin_separator(ui);
                ui.label(RichText::new("Corrupted").color(Color32::from_rgb(0xd2, 0x4b, 0x4b)));
            }
        });
}

fn render_mod(ui: &mut egui::Ui, m: &parser::Modifier) {
    let kind = match &m.kind {
        ModKind::Implicit => "Implicit".to_string(),
        ModKind::Prefix => "Prefix".to_string(),
        ModKind::Suffix => "Suffix".to_string(),
        ModKind::Unique => "Unique".to_string(),
        ModKind::Other(s) => s.clone(),
    };
    let mut head = kind;
    if let Some(src) = m.source {
        head = format!("{src:?} {head}");
    }
    if let Some(name) = &m.name {
        head.push_str(" · ");
        head.push_str(name);
    }
    if let Some(tier) = m.tier {
        use std::fmt::Write as _;
        let _ = write!(head, " (T{tier})");
    }
    ui.label(RichText::new(head).weak().small());
    for stat in &m.stats {
        ui.label(RichText::new(stat).color(AFFIX_BLUE));
    }
}

pub(super) fn thin_separator(ui: &mut egui::Ui) {
    ui.add_space(4.0);
    ui.separator();
    ui.add_space(2.0);
}

/// The bits of a listing's item shown in the hover preview.
pub(super) struct ItemPreview {
    icon: Option<String>,
    name: Option<String>,
    base: Option<String>,
    mods: Vec<String>,
    /// Trade `frameType` (0 normal, 1 magic, 2 rare, 3 unique, …) → rarity colour.
    frame_type: u64,
}

/// Pull the previewable fields out of a fetch result's raw `item` JSON.
pub(super) fn item_preview(item: &serde_json::Value) -> ItemPreview {
    let s = |k: &str| {
        item.get(k)
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|t| !t.is_empty())
    };
    let mut mods = Vec::new();
    // Pull from every mod field the trade API uses — a rare's lines can live in
    // explicit/fractured/crafted/enchant/rune/desecrated/implicit.
    for key in [
        "implicitMods",
        "enchantMods",
        "runeMods",
        "fracturedMods",
        "explicitMods",
        "craftedMods",
        "desecratedMods",
        "scourgeMods",
    ] {
        if let Some(arr) = item.get(key).and_then(|v| v.as_array()) {
            mods.extend(arr.iter().filter_map(|v| v.as_str()).map(str::to_string));
        }
    }
    if mods.is_empty() {
        // Diagnose the "no description" case: log what the item carried
        // (RUST_LOG=ui=debug).
        tracing::debug!(
            name = ?item.get("name").and_then(|v| v.as_str()),
            keys = ?item.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()),
            "item preview has no mods"
        );
    }
    ItemPreview {
        icon: s("icon"),
        name: s("name"),
        base: s("typeLine"),
        mods,
        frame_type: item
            .get("frameType")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
    }
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
        .constrain(true)
        // Bottom-centre pivot: the area is centred on the cursor's x and grows
        // upward, nudged down 3px so the cursor sits just inside the bottom.
        .fixed_pos(pos + egui::vec2(0.0, 3.0))
        .pivot(egui::Align2::CENTER_BOTTOM)
        .show(ctx, |ui| {
            render_item_preview(ui, item);
        });
}

/// The in-game-style item card (rarity-coloured border + name, icon, mods).
fn render_item_preview(ui: &mut egui::Ui, item: &ItemPreview) {
    let color = frame_color(item.frame_type);
    egui::Frame::none()
        .fill(HEADER_BG)
        .stroke(egui::Stroke::new(1.5, color))
        .rounding(6.0)
        .inner_margin(egui::Margin::symmetric(10.0, 8.0))
        .show(ui, |ui| {
            ui.set_max_width(320.0);
            ui.horizontal(|ui| {
                if let Some(icon) = &item.icon {
                    // Paint the icon into a fixed 48x48 box so a slow/failed
                    // load can't steal space from the text.
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
            if !item.mods.is_empty() {
                thin_separator(ui);
                for m in &item.mods {
                    ui.label(RichText::new(clean_mod_markup(m)).color(AFFIX_BLUE));
                }
            }
        });
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
    use super::clean_mod_markup;

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
