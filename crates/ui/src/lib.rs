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

use eframe::egui;
use egui::Color32;
use parser::{Item, Rarity};
use trade_api::{
    fetch_definitions, ClientConfig, PriceCheck, QueryOptions, ReqwestTransport, ResultEntry,
    TradeClient,
};

const BASE_URL: &str = "https://www.pathofexile.com";

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
const USER_AGENT: &str = "poe2-pricer/0.1 (+phase3 ui)";
/// Fetch a sample of this many so the median is meaningful; show the cheapest 5.
const SAMPLE: usize = 10;
const SHOWN: usize = 5;

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

pub struct QuickModeApp {
    // Held to keep the runtime alive for the app's lifetime.
    rt: tokio::runtime::Runtime,
    client: Arc<Client>,
    league: String,
    item_text: String,
    /// The item the current/last search was built from (for the header).
    item: Option<Item>,
    phase: Phase,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
}

impl QuickModeApp {
    fn new(rt: tokio::runtime::Runtime, client: Arc<Client>, league: String) -> Self {
        let (tx, rx) = channel();
        QuickModeApp {
            rt,
            client,
            league,
            item_text: SAMPLE_ITEM.to_string(),
            item: None,
            phase: Phase::Idle,
            tx,
            rx,
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
            Ok(Some(text)) => self.item_text = text,
            Ok(None) => self.phase = Phase::Failed("Clipboard is empty.".to_string()),
            Err(e) => self.phase = Phase::Failed(format!("Clipboard read failed: {e}")),
        }
    }
}

impl eframe::App for QuickModeApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(Msg::Result(result)) = self.rx.try_recv() {
            self.phase = match *result {
                Ok(pc) => Phase::Done(pc),
                Err(e) => Phase::Failed(e),
            };
        }

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("poe2-pricer");
                ui.label(egui::RichText::new("· quick mode").weak());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new(format!("league: {}", self.league)).weak());
                });
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(4.0);
            ui.label("Paste a POE2 item (advanced text), or read it from the clipboard:");
            ui.add(
                egui::TextEdit::multiline(&mut self.item_text)
                    .desired_rows(6)
                    .desired_width(f32::INFINITY)
                    .font(egui::TextStyle::Monospace),
            );

            ui.horizontal(|ui| {
                if ui.button("📋 Read clipboard").clicked() {
                    self.read_clipboard();
                }
                let busy = matches!(self.phase, Phase::Loading);
                let can_search = !self.item_text.trim().is_empty() && !busy;
                if ui
                    .add_enabled(can_search, egui::Button::new("💰 Price check"))
                    .clicked()
                {
                    self.start_price_check(ctx);
                }
                if busy {
                    ui.spinner();
                    ui.label("searching…");
                }
            });

            ui.separator();

            match &self.phase {
                Phase::Idle => {
                    ui.label(
                        egui::RichText::new("Results will appear here.")
                            .weak()
                            .italics(),
                    );
                }
                Phase::Loading => {}
                Phase::Failed(e) => {
                    ui.colored_label(Color32::from_rgb(0xff, 0x6b, 0x6b), e);
                }
                Phase::Done(pc) => {
                    let item = self.item.as_ref();
                    show_results(ui, item, pc);
                }
            }
        });
    }
}

fn show_results(ui: &mut egui::Ui, item: Option<&Item>, pc: &PriceCheck) {
    if let Some(item) = item {
        ui.horizontal(|ui| {
            let title = item
                .name
                .as_deref()
                .or(item.base_type.as_deref())
                .unwrap_or("Unknown item");
            ui.heading(egui::RichText::new(title).color(rarity_color(&item.rarity)));
        });
        if let Some(base) = &item.base_type {
            if item.name.is_some() {
                ui.label(egui::RichText::new(base).weak());
            }
        }
        ui.add_space(4.0);
    }

    match pc.median_price() {
        Some(p) => {
            ui.label(
                egui::RichText::new(format!("Median: {} {}", fmt_amount(p.amount), p.currency))
                    .size(18.0)
                    .strong(),
            );
        }
        None => {
            ui.label(egui::RichText::new("No priced listings.").italics());
        }
    }
    ui.label(
        egui::RichText::new(format!(
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
        .num_columns(4)
        .spacing([10.0, 8.0])
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
    ui.label(egui::RichText::new(price).strong());

    let seller = listing.account.name.as_str();
    let dot = if listing.is_online() {
        Color32::from_rgb(0x4c, 0xd1, 0x37)
    } else {
        Color32::DARK_GRAY
    };
    ui.horizontal(|ui| {
        ui.colored_label(dot, "●");
        let label = ui.label(seller);
        if let Some(indexed) = &listing.indexed {
            label.on_hover_text(format!("listed {indexed}"));
        }
    });

    // Whisper is always available (the API hands us the exact line). The
    // /invite, /hideout, /tradewith commands need the seller's character name.
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
        chat_button(
            ui,
            "Trade",
            character.as_deref().map(|c| format!("/tradewith {c}")),
        );
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
            .with_inner_size([540.0, 660.0])
            .with_min_inner_size([420.0, 360.0])
            .with_title("poe2-pricer"),
        ..Default::default()
    };

    eframe::run_native(
        "poe2-pricer",
        options,
        Box::new(move |_cc| Ok(Box::new(QuickModeApp::new(rt, client, league)))),
    )
    .map_err(|e| anyhow::anyhow!("eframe failed: {e}"))
}
