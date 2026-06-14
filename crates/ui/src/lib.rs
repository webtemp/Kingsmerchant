//! Phase 3 UI: a plain egui window (not an overlay yet — that's Phase 4) that
//! shows quick-mode price-check results (PRD §4.6).
//!
//! Flow: paste/copy a POE2 item → parse it → search + fetch via `trade-api` on
//! a background tokio task → show the median asking price and the cheapest
//! listings, each with Whisper / Invite / Hideout / Trade-with buttons that copy
//! the chat command to the clipboard (we can't type into POE2 on Wayland, so
//! the user pastes — PRD §4.6, §9.1).

use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use egui::{Color32, RichText};
use parser::{Item, ModKind, Rarity};
use trade_api::{
    fetch_definitions, ClientConfig, PriceCheck, QueryOptions, ReqwestTransport, ResultEntry,
    TradeClient,
};

const BASE_URL: &str = "https://www.pathofexile.com";
const USER_AGENT: &str = "poe2-pricer/0.1 (+phase3 ui)";
/// Fetch a sample of this many so the median is meaningful; show the cheapest N.
const SAMPLE: usize = 10;
const SHOWN: usize = 7;
/// How long to wait for POE2 to write the clipboard after Ctrl+C (PRD §4.2).
const CLIPBOARD_TIMEOUT: Duration = Duration::from_millis(500);
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// In-game-ish colour for rolled mod text.
const AFFIX_BLUE: Color32 = Color32::from_rgb(0x8a, 0x8a, 0xf0);
const HEADER_BG: Color32 = Color32::from_rgb(0x17, 0x17, 0x1c);

/// Prefilled so the first "Price check" works out of the box (a Topaz Ring base,
/// which exists in any league). Replace it by pasting or reading the clipboard.
const SAMPLE_ITEM: &str = "Item Class: Rings
Rarity: Rare
Honour Spiral
Topaz Ring
--------
Item Level: 79
--------
{ Implicit Modifier - Elemental, Lightning, Resistance }
+30(20-30)% to Lightning Resistance
--------
{ Prefix Modifier \"Adroit\" (Tier: 1) - Evasion }
+221(203-233) to Evasion Rating
{ Suffix Modifier \"of the Thunderhead\" (Tier: 5) - Elemental, Lightning, Resistance }
+23(21-25)% to Lightning Resistance";

type Client = TradeClient<ReqwestTransport>;

/// Result of a background price check, sent back to the UI thread.
enum Msg {
    Result(Box<Result<PriceCheck, String>>),
}

#[derive(Default)]
enum Phase {
    #[default]
    Idle,
    Loading,
    Done(PriceCheck),
    Failed(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Item,
    Text,
}

pub struct QuickModeApp {
    // Held to keep the runtime alive for the app's lifetime.
    rt: tokio::runtime::Runtime,
    client: Arc<Client>,
    league: String,
    item_text: String,
    view: View,
    /// The item the current/last search was built from (for the header).
    item: Option<Item>,
    /// Icon URL of the priced item, learned from the search results.
    icon_url: Option<String>,
    phase: Phase,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    /// Item text pushed in by the global-hotkey watcher (Ctrl+C in game).
    hotkey_rx: Receiver<String>,
}

impl QuickModeApp {
    fn new(
        rt: tokio::runtime::Runtime,
        client: Arc<Client>,
        league: String,
        hotkey_rx: Receiver<String>,
    ) -> Self {
        let (tx, rx) = channel();
        QuickModeApp {
            rt,
            client,
            league,
            item_text: SAMPLE_ITEM.to_string(),
            view: View::Item,
            item: None,
            icon_url: None,
            phase: Phase::Idle,
            tx,
            rx,
            hotkey_rx,
        }
    }

    fn start_price_check(&mut self, ctx: &egui::Context) {
        let item = match parser::parse_item(&self.item_text) {
            Ok(item) => item,
            Err(e) => {
                self.phase = Phase::Failed(format!("Not a POE2 item: {e}"));
                self.item = None;
                return;
            }
        };
        self.item = Some(item.clone());
        self.icon_url = None;
        self.phase = Phase::Loading;

        let client = Arc::clone(&self.client);
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        self.rt.spawn(async move {
            let result = client
                .price_check(&item, QueryOptions::default(), SAMPLE)
                .await
                .map_err(|e| e.to_string());
            let _ = tx.send(Msg::Result(Box::new(result)));
            ctx.request_repaint();
        });
    }

    fn read_clipboard(&mut self) {
        match platform_linux::read_clipboard_text() {
            Ok(Some(text)) => {
                self.item_text = text;
                self.view = View::Item;
            }
            Ok(None) => self.phase = Phase::Failed("Clipboard is empty.".to_string()),
            Err(e) => self.phase = Phase::Failed(format!("Clipboard read failed: {e}")),
        }
    }
}

impl eframe::App for QuickModeApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Ctrl+C in game → the watcher pushes the copied item text here; raise
        // the window and price-check it automatically.
        while let Ok(text) = self.hotkey_rx.try_recv() {
            self.item_text = text;
            self.view = View::Item;
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            self.start_price_check(ctx);
        }

        while let Ok(Msg::Result(result)) = self.rx.try_recv() {
            self.phase = match *result {
                Ok(pc) => {
                    self.icon_url = pc
                        .listings
                        .first()
                        .and_then(|e| e.item.get("icon"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    Phase::Done(pc)
                }
                Err(e) => Phase::Failed(e),
            };
        }

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.heading("poe2-pricer");
                ui.label(RichText::new("· quick mode").weak());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(format!("league: {}", self.league)).weak());
                });
            });
            ui.add_space(4.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(6.0);

            // View toggle + actions.
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.view, View::Item, "🛡 Item");
                ui.selectable_value(&mut self.view, View::Text, "📝 Text");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let busy = matches!(self.phase, Phase::Loading);
                    let can_search = !self.item_text.trim().is_empty() && !busy;
                    if ui
                        .add_enabled(can_search, egui::Button::new("💰 Price check"))
                        .clicked()
                    {
                        self.start_price_check(ctx);
                    }
                    if ui.button("📋 Read clipboard").clicked() {
                        self.read_clipboard();
                    }
                });
            });

            ui.add_space(4.0);

            match self.view {
                View::Text => {
                    ui.add(
                        egui::TextEdit::multiline(&mut self.item_text)
                            .desired_rows(8)
                            .desired_width(f32::INFINITY)
                            .font(egui::TextStyle::Monospace),
                    );
                }
                View::Item => match parser::parse_item(&self.item_text) {
                    Ok(item) => item_card(ui, &item, self.icon_url.as_deref()),
                    Err(e) => {
                        ui.colored_label(
                            Color32::from_rgb(0xff, 0x6b, 0x6b),
                            format!("Can't render — not a POE2 item: {e}"),
                        );
                        ui.label(RichText::new("Switch to 📝 Text to edit.").weak());
                    }
                },
            }

            ui.add_space(6.0);

            if matches!(self.phase, Phase::Loading) {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("searching…");
                });
            }
            ui.separator();

            match &self.phase {
                Phase::Idle => {
                    ui.label(RichText::new("Press 💰 Price check.").weak().italics());
                }
                Phase::Loading => {}
                Phase::Failed(e) => {
                    ui.colored_label(Color32::from_rgb(0xff, 0x6b, 0x6b), e);
                }
                Phase::Done(pc) => show_results(ui, pc),
            }
        });
    }
}

/// Render a parsed item as an in-game-style tooltip card.
fn item_card(ui: &mut egui::Ui, item: &Item, icon_url: Option<&str>) {
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
        head.push_str(&format!(" · {name}"));
    }
    if let Some(tier) = m.tier {
        head.push_str(&format!(" (T{tier})"));
    }
    ui.label(RichText::new(head).weak().small());
    for stat in &m.stats {
        ui.label(RichText::new(stat).color(AFFIX_BLUE));
    }
}

fn thin_separator(ui: &mut egui::Ui) {
    ui.add_space(4.0);
    ui.separator();
    ui.add_space(2.0);
}

fn show_results(ui: &mut egui::Ui, pc: &PriceCheck) {
    match pc.median_price() {
        Some(p) => {
            ui.label(
                RichText::new(format!("Median: {} {}", fmt_amount(p.amount), p.currency))
                    .size(18.0)
                    .strong(),
            );
        }
        None => {
            ui.label(RichText::new("No priced listings.").italics());
        }
    }
    ui.label(
        RichText::new(format!(
            "{} online listing(s) · showing cheapest {}",
            pc.total, SHOWN
        ))
        .weak(),
    );
    ui.add_space(6.0);

    let cheapest = pc.cheapest(SHOWN);
    if cheapest.is_empty() {
        return;
    }

    egui::Grid::new("listings")
        .striped(true)
        .num_columns(3)
        .spacing([10.0, 10.0])
        .show(ui, |ui| {
            for entry in cheapest {
                listing_row(ui, entry);
                ui.end_row();
            }
        });
}

fn listing_row(ui: &mut egui::Ui, entry: &ResultEntry) {
    let listing = &entry.listing;

    let price = listing
        .price
        .as_ref()
        .map(|p| format!("{} {}", fmt_amount(p.amount), p.currency))
        .unwrap_or_else(|| "—".to_string());
    ui.label(RichText::new(price).strong());

    let dot = if listing.is_online() {
        Color32::from_rgb(0x4c, 0xd1, 0x37)
    } else {
        Color32::DARK_GRAY
    };
    ui.horizontal(|ui| {
        ui.colored_label(dot, "●");
        let label = ui.label(&listing.account.name);
        if let Some(indexed) = &listing.indexed {
            label.on_hover_text(format!("listed {indexed}"));
        }
    });

    let character = listing.account.last_character_name.clone();
    ui.horizontal(|ui| {
        if let Some(whisper) = &listing.whisper {
            if ui.button("Whisper").on_hover_text(whisper).clicked() {
                ui.ctx().copy_text(whisper.clone());
            }
        } else {
            ui.add_enabled(false, egui::Button::new("Whisper"));
        }
        chat_button(ui, "Invite", character.as_deref().map(|c| format!("/invite {c}")));
        chat_button(ui, "Hideout", character.as_deref().map(|c| format!("/hideout {c}")));
        chat_button(ui, "Trade", character.as_deref().map(|c| format!("/tradewith {c}")));
    });
}

/// A button that copies `command` to the clipboard, disabled when we couldn't
/// build one (e.g. no character name).
fn chat_button(ui: &mut egui::Ui, label: &str, command: Option<String>) {
    match command {
        Some(cmd) => {
            if ui.button(label).on_hover_text(&cmd).clicked() {
                ui.ctx().copy_text(cmd);
            }
        }
        None => {
            ui.add_enabled(false, egui::Button::new(label))
                .on_hover_text("no character name in this listing");
        }
    }
}

fn rarity_color(rarity: &Rarity) -> Color32 {
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

fn fmt_amount(amount: f64) -> String {
    if amount.fract() == 0.0 {
        format!("{}", amount as i64)
    } else {
        format!("{amount:.1}")
    }
}

/// Watch the global price-check hotkeys on a background thread. On each press
/// we wait for POE2 to write the clipboard, then push the item text to the UI.
/// If the watcher can't start (e.g. not in the `input` group), we log and carry
/// on — the window still works manually (PRD §4.1).
fn spawn_hotkey_watcher(ctx: egui::Context, tx: Sender<String>) {
    std::thread::spawn(move || {
        let hotkeys = match platform_linux::watch_hotkeys() {
            Ok(rx) => rx,
            Err(e) => {
                tracing::warn!(error = %e, "hotkey watcher disabled; use the buttons");
                return;
            }
        };
        tracing::info!("listening for Ctrl+C / Ctrl+Alt+C in game");
        let mut last_seen = platform_linux::read_clipboard_text().unwrap_or(None);
        for _event in hotkeys {
            if let Some(text) = wait_for_new_clipboard(&last_seen) {
                last_seen = Some(text.clone());
                if tx.send(text).is_err() {
                    return; // UI gone
                }
                ctx.request_repaint();
            }
        }
    });
}

/// Poll the clipboard until it differs from `last_seen` or the timeout hits.
fn wait_for_new_clipboard(last_seen: &Option<String>) -> Option<String> {
    let deadline = Instant::now() + CLIPBOARD_TIMEOUT;
    loop {
        match platform_linux::read_clipboard_text() {
            Ok(Some(text)) if Some(&text) != last_seen.as_ref() => return Some(text),
            _ => {}
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn configure_style(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    style.visuals.window_rounding = 8.0.into();
    style.visuals.widgets.noninteractive.rounding = 6.0.into();
    style.visuals.widgets.inactive.rounding = 6.0.into();
    style.visuals.widgets.hovered.rounding = 6.0.into();
    style.visuals.widgets.active.rounding = 6.0.into();
    ctx.set_style(style);
}

/// Build the client (fetching live definitions) and run the window.
pub fn run() -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    let league = std::env::var("POE_LEAGUE").unwrap_or_else(|_| "Runes of Aldur".to_string());

    let transport = ReqwestTransport::new(USER_AGENT)?;
    tracing::info!("fetching trade definitions…");
    let (stats, items) = rt
        .block_on(fetch_definitions(&transport, BASE_URL))
        .map_err(|e| anyhow::anyhow!("loading definitions: {e}"))?;

    let mut config = ClientConfig::new(&league);
    config.realm = std::env::var("POE_REALM").ok();
    let client = Arc::new(TradeClient::new(transport, config, stats, items));

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([560.0, 720.0])
            .with_min_inner_size([440.0, 380.0])
            .with_title("poe2-pricer"),
        ..Default::default()
    };

    eframe::run_native(
        "poe2-pricer",
        options,
        Box::new(move |cc| {
            egui_extras::install_image_loaders(&cc.egui_ctx);
            configure_style(&cc.egui_ctx);
            let (hk_tx, hk_rx) = channel::<String>();
            spawn_hotkey_watcher(cc.egui_ctx.clone(), hk_tx);
            Ok(Box::new(QuickModeApp::new(rt, client, league, hk_rx)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe failed: {e}"))
}
