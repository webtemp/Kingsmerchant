//! Quick-mode price-check UI (PRD §4.6): the egui view + app logic, windowing
//! agnostic so the `overlay` crate can drive it on a layer surface.
//!
//! Flow: Ctrl+C on an item → parse it → search + fetch via `trade-api` on a
//! background tokio task → show the median asking price and the cheapest
//! listings, each with Whisper / Invite / Hideout / Trade-with buttons that copy
//! the chat command to the clipboard (we can't type into POE2 on Wayland, so
//! the user pastes — PRD §4.6, §9.1).

pub mod config;

use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use std::collections::HashSet;

use config::Config;
use egui::{Color32, RichText};
use parser::{Item, ModKind, Rarity};
use trade_api::{
    fetch_definitions, fetch_leagues, ClientConfig, League, ListingStatus, PriceCheck, PriceEstimate,
    PriceFilter, QueryOptions, ReqwestTransport, ResultEntry, StatSelection, TradeClient,
};

const BASE_URL: &str = "https://www.pathofexile.com";
const USER_AGENT: &str = "poe2ddd/0.1 (+phase3 ui)";
/// Fetch a sample of this many so the median is meaningful; show the cheapest N.
const SAMPLE: usize = 10;
const SHOWN: usize = 7;
/// How long to wait for POE2 to write the clipboard after Ctrl+C. The PRD §4.2
/// budget was 500ms; we're more patient (1s) because POE2's write latency is
/// variable, and a too-short window is what made some presses "do nothing".
const CLIPBOARD_TIMEOUT: Duration = Duration::from_millis(1000);
const POLL_INTERVAL: Duration = Duration::from_millis(8);
/// Quiet period after the last filter edit before a live re-query fires (PRD
/// §4.7 "debounced"). Long enough to coalesce a flurry of slider/checkbox edits,
/// short enough to feel live.
const FILTER_DEBOUNCE: Duration = Duration::from_millis(350);

/// In-game-ish colour for rolled mod text.
const AFFIX_BLUE: Color32 = Color32::from_rgb(0x8a, 0x8a, 0xf0);
const HEADER_BG: Color32 = Color32::from_rgb(0x17, 0x17, 0x1c);

/// Popup width in quick mode; detailed mode is wider for the filter panel.
pub const QUICK_WIDTH: u32 = 470;
pub const DETAILED_WIDTH: u32 = 600;

pub type Client = TradeClient<ReqwestTransport>;

/// Result of a background price check, sent back to the UI thread.
enum Msg {
    Result(Box<Result<PriceCheck, String>>),
    /// poeprices.info ML estimate (detailed mode, rares). `None` = poeprices
    /// declined to price it; `Err` = it failed.
    Estimate(Box<Result<Option<PriceEstimate>, String>>),
}

/// Quick (Ctrl+C) vs detailed (Ctrl+Alt+C) price check. Detailed mode pins the
/// popup open (it doesn't hide on Ctrl release) so filters can be toggled.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Quick,
    Detailed,
}

/// What the global-hotkey watcher observed.
pub enum Hotkey {
    /// A new item landed on the clipboard. `detailed` is true for Ctrl+Alt+C.
    Item { text: String, detailed: bool },
    /// The clipboard never produced an item before the timeout — usually POE2
    /// skipping the copy on a static cursor (PRD §9.3).
    Missed,
    /// Escape was pressed — dismiss the popup.
    Close,
    /// Ctrl / Alt held-state changed (from evdev, since the overlay has no
    /// keyboard focus).
    Mods { ctrl: bool, alt: bool },
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

/// One toggleable stat filter in the detailed panel (PRD §4.7), built from the
/// item's mapped stats. `min`/`max` are text buffers (blank = unbounded) so the
/// numeric fields can be cleared.
struct StatFilterRow {
    id: String,
    /// Human-ish label (the canonical stat template, e.g. `#% to Fire Resistance`).
    label: String,
    enabled: bool,
    min: String,
    max: String,
    /// The item's own rolled value, used to seed the min and to relax it for
    /// the "Similar item" preset.
    rolled: Option<f64>,
}

impl StatFilterRow {
    fn selection(&self) -> StatSelection {
        StatSelection {
            id: self.id.clone(),
            enabled: self.enabled,
            min: parse_num(&self.min),
            max: parse_num(&self.max),
        }
    }
}

/// The detailed-mode price-range filter inputs (PRD §4.7).
#[derive(Default)]
struct PriceFilterState {
    min: String,
    max: String,
    /// Currency id (`exalted`, …) or empty for "any".
    currency: String,
}

impl PriceFilterState {
    fn to_filter(&self) -> PriceFilter {
        PriceFilter {
            min: parse_num(&self.min),
            max: parse_num(&self.max),
            currency: if self.currency.is_empty() {
                None
            } else {
                Some(self.currency.clone())
            },
        }
    }
}

/// Currencies offered in the price-range dropdown (id, label). Empty id = any.
const PRICE_CURRENCIES: &[(&str, &str)] = &[
    ("", "any"),
    ("exalted", "exalted"),
    ("divine", "divine"),
    ("chaos", "chaos"),
];

/// Label for the price-currency dropdown's current id.
fn currency_label(id: &str) -> &str {
    PRICE_CURRENCIES
        .iter()
        .find(|(cid, _)| *cid == id)
        .map(|(_, label)| *label)
        .unwrap_or("any")
}

/// Parse a numeric filter buffer; blank or unparseable → no bound.
fn parse_num(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        t.parse().ok()
    }
}

pub struct QuickModeApp {
    // Held to keep the runtime alive for the app's lifetime.
    rt: tokio::runtime::Runtime,
    client: Arc<Client>,
    /// Persisted settings; rewritten when the league selector changes.
    config: Config,
    /// Leagues offered in the top-right selector (PRD §4.8).
    leagues: Vec<League>,
    item_text: String,
    view: View,
    /// Quick vs detailed — set by the last hotkey. Detailed pins the popup.
    mode: Mode,
    /// The item the current/last search was built from (for the header).
    item: Option<Item>,
    /// Icon URL of the priced item, learned from the search results.
    icon_url: Option<String>,
    /// Detailed-mode stat filter rows (rebuilt on a fresh detailed check).
    filters: Vec<StatFilterRow>,
    /// Detailed-mode price-range filter.
    price_filter: PriceFilterState,
    /// A filter edit is pending a debounced live re-query.
    filter_dirty: bool,
    /// When the last filter edit happened (debounce timer base).
    filter_changed_at: Instant,
    /// poeprices.info ML estimate for the loaded rare (detailed mode).
    estimate: Option<PriceEstimate>,
    /// A poeprices request is in flight.
    estimate_loading: bool,
    phase: Phase,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    /// Events pushed in by the global-hotkey watcher (Ctrl+C in game).
    hotkey_rx: Receiver<Hotkey>,
    /// Last "copied to clipboard" note, shown as feedback under the listings.
    copy_status: Option<String>,
    /// Transient hint (e.g. a missed copy), shown near the top.
    hint: Option<String>,
    /// Set when a Ctrl+C produced a fresh item — the overlay loop reads this
    /// (via [`take_pop_request`](Self::take_pop_request)) to show the popup.
    pop_requested: bool,
    /// Set when Escape was pressed — the overlay reads this to hide the popup.
    close_requested: bool,
    /// Live Ctrl / Alt held-state (from evdev). The overlay reads these to keep
    /// the popup visible only while Ctrl is held, and to gate Ctrl+Alt drag.
    ctrl_held: bool,
    alt_held: bool,
}

impl QuickModeApp {
    pub fn new(
        rt: tokio::runtime::Runtime,
        client: Arc<Client>,
        config: Config,
        leagues: Vec<League>,
        hotkey_rx: Receiver<Hotkey>,
    ) -> Self {
        let (tx, rx) = channel();
        QuickModeApp {
            rt,
            client,
            config,
            leagues,
            item_text: String::new(),
            view: View::Item,
            mode: Mode::Quick,
            item: None,
            icon_url: None,
            filters: Vec::new(),
            price_filter: PriceFilterState::default(),
            filter_dirty: false,
            filter_changed_at: Instant::now(),
            estimate: None,
            estimate_loading: false,
            phase: Phase::Idle,
            tx,
            rx,
            hotkey_rx,
            copy_status: None,
            hint: None,
            pop_requested: false,
            close_requested: false,
            ctrl_held: false,
            alt_held: false,
        }
    }

    /// Whether Ctrl is currently held (from the evdev watcher).
    pub fn ctrl_held(&self) -> bool {
        self.ctrl_held
    }

    /// Whether the popup is pinned open (detailed mode). The overlay keeps a
    /// pinned popup visible even after Ctrl is released — it's dismissed only by
    /// Escape or the ✕ button.
    pub fn pinned(&self) -> bool {
        self.mode == Mode::Detailed
    }

    /// Logical surface width for the current mode. Detailed mode is wider to fit
    /// the filter panel; the overlay reads this each frame to size the layer.
    pub fn surface_width(&self) -> u32 {
        match self.mode {
            Mode::Quick => QUICK_WIDTH,
            Mode::Detailed => DETAILED_WIDTH,
        }
    }

    /// Whether Alt is currently held (from the evdev watcher).
    pub fn alt_held(&self) -> bool {
        self.alt_held
    }

    /// Consume a pending "pop the overlay" request raised by the last Ctrl+C.
    pub fn take_pop_request(&mut self) -> bool {
        std::mem::take(&mut self.pop_requested)
    }

    /// Consume a pending "close the overlay" request raised by Escape.
    pub fn take_close_request(&mut self) -> bool {
        std::mem::take(&mut self.close_requested)
    }

    /// Start a *fresh* price check from `item_text` (a new Ctrl+C, the manual
    /// button, or a paste). In detailed mode this rebuilds the filter panel from
    /// the item and resets the price filter; a filter-driven re-query goes
    /// through [`rerun_query`](Self::rerun_query) instead so toggles survive.
    fn start_price_check(&mut self, ctx: &egui::Context) {
        let item = match parser::parse_item(&self.item_text) {
            Ok(item) => item,
            Err(e) => {
                self.phase = Phase::Failed(format!("Not a POE2 item: {e}"));
                self.item = None;
                return;
            }
        };
        self.icon_url = None;
        self.estimate = None;
        self.estimate_loading = false;
        if self.mode == Mode::Detailed {
            self.filters = self.build_filter_rows(&item);
            self.price_filter = PriceFilterState::default();
            // poeprices ML estimate is rares-only and doesn't depend on the
            // filters, so fetch it once per fresh detailed check (PRD §4.7).
            if item.rarity == Rarity::Rare {
                self.spawn_estimate(ctx);
            }
        }
        self.item = Some(item.clone());
        self.spawn_query(ctx, item);
    }

    /// Fetch the poeprices.info ML estimate for the current `item_text` on a
    /// background task (detailed mode, rares).
    fn spawn_estimate(&mut self, ctx: &egui::Context) {
        self.estimate_loading = true;
        let client = Arc::clone(&self.client);
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        let text = self.item_text.clone();
        self.rt.spawn(async move {
            let result = client.price_estimate(&text).await.map_err(|e| e.to_string());
            let _ = tx.send(Msg::Estimate(Box::new(result)));
            ctx.request_repaint();
        });
    }

    /// Re-run the search for the already-loaded item, keeping the current filter
    /// state (used by the detailed panel and by a league switch).
    fn rerun_query(&mut self, ctx: &egui::Context) {
        if let Some(item) = self.item.clone() {
            self.spawn_query(ctx, item);
        }
    }

    /// Spawn the background search/fetch for `item`, picking the quick or
    /// detailed query per the current mode.
    fn spawn_query(&mut self, ctx: &egui::Context, item: Item) {
        self.phase = Phase::Loading;
        let client = Arc::clone(&self.client);
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        let detailed = self.mode == Mode::Detailed;
        let selections: Vec<StatSelection> = self.filters.iter().map(StatFilterRow::selection).collect();
        let price = self.price_filter.to_filter();
        self.rt.spawn(async move {
            let result = if detailed {
                client
                    .price_check_detailed(&item, ListingStatus::Online, &selections, &price, SAMPLE)
                    .await
            } else {
                client.price_check(&item, QueryOptions::default(), SAMPLE).await
            }
            .map_err(|e| e.to_string());
            if let Err(ref e) = result {
                // Log so it's easy to copy out of the terminal, not just the popup.
                tracing::error!(error = %e, detailed, "price check failed");
            }
            let _ = tx.send(Msg::Result(Box::new(result)));
            ctx.request_repaint();
        });
    }

    /// Enumerate the item's mapped stats into toggleable filter rows, deduped by
    /// stat id, with the rolled value pre-filled as the min (blank max).
    fn build_filter_rows(&self, item: &Item) -> Vec<StatFilterRow> {
        let mut rows = Vec::new();
        let mut seen = HashSet::new();
        // Mapped with the item's local/global context (e.g. local evasion on
        // body armour). Enabled by default so the first detailed search matches
        // the item's own mods; the user toggles off what they don't care about.
        for mapped in self.client.stats().map_item(item) {
            if !seen.insert(mapped.id.clone()) {
                continue;
            }
            let rolled = mapped.filter_value();
            rows.push(StatFilterRow {
                id: mapped.id,
                label: mapped.template,
                enabled: true,
                min: rolled.map(fmt_amount).unwrap_or_default(),
                max: String::new(),
                rolled,
            });
        }
        rows
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

impl QuickModeApp {
    /// Drain the hotkey + price-check channels. Side-effect only (no drawing),
    /// so the overlay can call it every frame — even while hidden — to notice a
    /// fresh Ctrl+C and decide to pop.
    pub fn pump(&mut self, ctx: &egui::Context) {
        // Ctrl+C in game → the watcher pushes the copied item here; price-check
        // it automatically and flag a pop. A missed copy gets a hint instead of
        // silently doing nothing.
        while let Ok(event) = self.hotkey_rx.try_recv() {
            match event {
                Hotkey::Item { text, detailed } => {
                    self.hint = None;
                    self.item_text = text;
                    self.view = View::Item;
                    self.mode = if detailed { Mode::Detailed } else { Mode::Quick };
                    self.pop_requested = true;
                    self.start_price_check(ctx);
                }
                Hotkey::Missed => {
                    self.hint = Some(
                        "No item copied — nudge the mouse over the item, then press \
                         Ctrl+C again. (POE2 skips the copy when the cursor is still.)"
                            .to_string(),
                    );
                }
                Hotkey::Close => {
                    self.close_requested = true;
                }
                Hotkey::Mods { ctrl, alt } => {
                    self.ctrl_held = ctrl;
                    self.alt_held = alt;
                }
            }
        }

        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Result(result) => {
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
                Msg::Estimate(result) => {
                    self.estimate_loading = false;
                    match *result {
                        Ok(est) => self.estimate = est,
                        Err(e) => tracing::debug!(error = %e, "poeprices estimate failed"),
                    }
                }
            }
        }
    }

    /// Render the popup body into the given `Ui`. No panels — the overlay
    /// frames it in an auto-sizing translucent `Area`. Call
    /// [`pump`](Self::pump) first.
    pub fn content(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();

        // Header: title + league selector (top-right, PRD §4.8). Detailed mode
        // is pinned, so it gets a ✕ to dismiss (quick mode hides on Ctrl release).
        ui.horizontal(|ui| {
            ui.heading("poe2ddd");
            let mode_label = match self.mode {
                Mode::Quick => "· quick mode",
                Mode::Detailed => "· detailed mode",
            };
            ui.label(RichText::new(mode_label).weak());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.mode == Mode::Detailed
                    && ui
                        .button(RichText::new("X").strong())
                        .on_hover_text("Close (Esc)")
                        .clicked()
                {
                    self.close_requested = true;
                }
                self.league_selector(ui, &ctx);
            });
        });
        ui.add_space(4.0);

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
                    self.start_price_check(&ctx);
                }
                if ui.button("📋 Read clipboard").clicked() {
                    self.read_clipboard();
                }
            });
        });

        if let Some(hint) = &self.hint {
            ui.add_space(4.0);
            ui.colored_label(Color32::from_rgb(0xff, 0xc8, 0x4b), format!("⚠ {hint}"));
        }

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
            View::Item => {
                if self.item_text.trim().is_empty() {
                    ui.label(
                        RichText::new("Hover an item in POE2 and press Ctrl+C to price it.")
                            .weak()
                            .italics(),
                    );
                } else {
                    match parser::parse_item(&self.item_text) {
                        Ok(item) => item_card(ui, &item, self.icon_url.as_deref()),
                        Err(e) => {
                            ui.colored_label(
                                Color32::from_rgb(0xff, 0x6b, 0x6b),
                                format!("Can't render — not a POE2 item: {e}"),
                            );
                            ui.label(RichText::new("Switch to 📝 Text to edit.").weak());
                        }
                    }
                }
            }
        }

        // Detailed mode: the filter panel (PRD §4.7), between the item and the
        // listings. Edits re-run the search after a short debounce; "Apply" (or
        // Enter in a field) fires immediately.
        if self.mode == Mode::Detailed {
            ui.add_space(6.0);
            let apply_now = self.filter_panel(ui);
            // Fire once edits go quiet and nothing is in flight. The overlay
            // redraws continuously, so this is re-checked every frame.
            let debounced = self.filter_dirty
                && self.filter_changed_at.elapsed() >= FILTER_DEBOUNCE
                && !matches!(self.phase, Phase::Loading);
            if apply_now || debounced {
                self.filter_dirty = false;
                self.rerun_query(&ctx);
            }
        }

        ui.add_space(6.0);

        // Rate-limit feedback (PRD §4.4): don't fire blindly — tell the user
        // we're waiting on the trade API's bucket.
        if let Some(wait) = self.client.retry_in() {
            let secs = (wait.as_millis() as u64).div_ceil(1000);
            ui.colored_label(
                Color32::from_rgb(0xff, 0xc8, 0x4b),
                format!("⏳ rate limited — retrying in {secs}s"),
            );
        }
        if matches!(self.phase, Phase::Loading) {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("searching…");
            });
        }

        // poeprices.info ML estimate badge (detailed mode, rares — PRD §4.7).
        if self.mode == Mode::Detailed {
            self.estimate_badge(ui);
        }

        ui.separator();

        let mut copied: Option<String> = None;
        match &self.phase {
            Phase::Idle => {
                ui.label(
                    RichText::new("Waiting for an item…")
                        .weak()
                        .italics(),
                );
            }
            Phase::Loading => {}
            Phase::Failed(e) => {
                ui.colored_label(Color32::from_rgb(0xff, 0x6b, 0x6b), e);
            }
            Phase::Done(pc) => {
                show_results(ui, pc, &mut copied);
                ui.add_space(6.0);
                let url = trade_url(&self.config.league, &pc.query_id);
                if ui
                    .button("🌐 Open on trade site")
                    .on_hover_text(&url)
                    .clicked()
                {
                    if let Err(e) = platform_linux::open_url(&url) {
                        tracing::warn!(error = %e, "xdg-open failed");
                    }
                }
            }
        }
        if let Some(label) = copied {
            self.copy_status = Some(label);
        }

        if let Some(status) = &self.copy_status {
            ui.add_space(4.0);
            ui.colored_label(
                Color32::from_rgb(0x4c, 0xd1, 0x37),
                format!("✓ Copied {status} — paste into POE2 chat (Enter)"),
            );
        }
    }

    /// The league dropdown. Switching re-prices the loaded item under the new
    /// league. Falls back to a plain label if the leagues list failed to load.
    fn league_selector(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if self.leagues.is_empty() {
            ui.label(RichText::new(&self.config.league).weak());
            return;
        }
        let current = self.config.league.clone();
        let mut chosen = current.clone();
        egui::ComboBox::from_id_salt("league-selector")
            .selected_text(&current)
            .show_ui(ui, |ui| {
                for lg in &self.leagues {
                    ui.selectable_value(&mut chosen, lg.id.clone(), &lg.text);
                }
            });
        if chosen != current {
            self.config.league = chosen.clone();
            // Persist the choice so it sticks across restarts (no env var).
            if let Err(e) = self.config.save() {
                tracing::warn!(error = %e, "could not save config");
            }
            self.client.set_league(chosen);
            // Re-price the currently loaded item under the new league, keeping
            // any detailed-mode filters in place.
            self.rerun_query(ctx);
        }
    }

    /// The detailed-mode filter panel: a price range plus a toggleable row per
    /// mapped stat (PRD §4.7). Returns `true` when the user asked to re-run the
    /// search (the Apply button).
    fn filter_panel(&mut self, ui: &mut egui::Ui) -> bool {
        let mut requery = false;
        let mut changed = false;
        egui::CollapsingHeader::new(RichText::new("🔍 Filters").strong())
            .default_open(true)
            .show(ui, |ui| {
                // Price range (PRD §4.7 price-range filter).
                ui.horizontal(|ui| {
                    ui.label("Price");
                    changed |= ui
                        .add(
                            egui::TextEdit::singleline(&mut self.price_filter.min)
                                .hint_text("min")
                                .desired_width(48.0),
                        )
                        .changed();
                    ui.label("–");
                    changed |= ui
                        .add(
                            egui::TextEdit::singleline(&mut self.price_filter.max)
                                .hint_text("max")
                                .desired_width(48.0),
                        )
                        .changed();
                    let before = self.price_filter.currency.clone();
                    egui::ComboBox::from_id_salt("price-currency")
                        .selected_text(currency_label(&self.price_filter.currency))
                        .show_ui(ui, |ui| {
                            for (id, label) in PRICE_CURRENCIES {
                                ui.selectable_value(
                                    &mut self.price_filter.currency,
                                    id.to_string(),
                                    *label,
                                );
                            }
                        });
                    changed |= self.price_filter.currency != before;
                });

                ui.add_space(4.0);
                if self.filters.is_empty() {
                    ui.label(
                        RichText::new("No mapped stats to filter on this item.")
                            .weak()
                            .italics(),
                    );
                } else {
                    egui::ScrollArea::vertical()
                        .max_height(220.0)
                        .show(ui, |ui| {
                            egui::Grid::new("stat-filters")
                                .num_columns(4)
                                .spacing([6.0, 6.0])
                                .striped(true)
                                .show(ui, |ui| {
                                    for row in &mut self.filters {
                                        changed |= ui.checkbox(&mut row.enabled, "").changed();
                                        ui.label(
                                            RichText::new(&row.label).color(AFFIX_BLUE).small(),
                                        );
                                        changed |= ui
                                            .add(
                                                egui::TextEdit::singleline(&mut row.min)
                                                    .hint_text("min")
                                                    .desired_width(46.0),
                                            )
                                            .changed();
                                        changed |= ui
                                            .add(
                                                egui::TextEdit::singleline(&mut row.max)
                                                    .hint_text("max")
                                                    .desired_width(46.0),
                                            )
                                            .changed();
                                        ui.end_row();
                                    }
                                });
                        });
                }

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button("🔄 Apply now").clicked() {
                        requery = true;
                    }
                    // "Similar item" (PRD §4.7): same base, every mapped mod
                    // enabled at ~80% of its roll — find comparable items.
                    if ui
                        .button("🔎 Similar item")
                        .on_hover_text("Same base, every mod present at ~80% of its roll")
                        .clicked()
                    {
                        for row in &mut self.filters {
                            row.enabled = true;
                            row.min = row
                                .rolled
                                .map(|v| fmt_amount((v * 0.8).floor()))
                                .unwrap_or_default();
                            row.max.clear();
                        }
                        requery = true;
                    }
                });
            });

        // Any edit (re)starts the debounce timer; the caller fires the re-query
        // once it elapses.
        if changed {
            self.filter_dirty = true;
            self.filter_changed_at = Instant::now();
        }
        requery
    }

    /// The poeprices.info ML estimate badge: a spinner while it loads, then the
    /// predicted range + confidence, or nothing if poeprices declined.
    fn estimate_badge(&self, ui: &mut egui::Ui) {
        if self.estimate_loading {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(
                    RichText::new("poeprices.info ML estimate…")
                        .weak()
                        .small(),
                );
            });
            return;
        }
        let Some(est) = &self.estimate else {
            return;
        };
        let conf = est
            .confidence
            .map(|c| format!("  ·  {c:.0}% confidence"))
            .unwrap_or_default();
        let text = format!(
            "🤖 poeprices ML: {}–{} {}{}",
            fmt_amount(est.min),
            fmt_amount(est.max),
            est.currency,
            conf
        );
        egui::Frame::none()
            .fill(Color32::from_rgb(0x23, 0x2a, 0x3a))
            .stroke(egui::Stroke::new(1.0, Color32::from_rgb(0x3c, 0x55, 0x7a)))
            .rounding(6.0)
            .inner_margin(egui::Margin::symmetric(8.0, 4.0))
            .show(ui, |ui| {
                ui.label(RichText::new(text).color(Color32::from_rgb(0x7e, 0xc8, 0xff)));
            });
    }
}

/// Deep link to the official trade site for a finished search (PRD §4.6).
fn trade_url(league: &str, query_id: &str) -> String {
    format!(
        "https://www.pathofexile.com/trade2/search/poe2/{}/{}",
        league.replace(' ', "%20"),
        query_id
    )
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

fn show_results(ui: &mut egui::Ui, pc: &PriceCheck, copied: &mut Option<String>) {
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
                listing_row(ui, entry, copied);
                ui.end_row();
            }
        });
}

fn listing_row(ui: &mut egui::Ui, entry: &ResultEntry, copied: &mut Option<String>) {
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
    let seller = listing.account.name.clone();
    // Icon buttons (the row is too narrow for text labels); the action name is
    // the hover tooltip.
    ui.horizontal(|ui| {
        if let Some(whisper) = &listing.whisper {
            if ui.button("💬").on_hover_text("Whisper").clicked() {
                copy_to_clipboard(whisper);
                *copied = Some(format!("whisper to {seller}"));
            }
        } else {
            ui.add_enabled(false, egui::Button::new("💬"))
                .on_hover_text("Whisper (unavailable)");
        }
        chat_button(ui, "➕", "Invite", character.as_deref().map(|c| format!("/invite {c}")), copied);
        chat_button(ui, "🏠", "Hideout", character.as_deref().map(|c| format!("/hideout {c}")), copied);
        chat_button(ui, "💱", "Trade", character.as_deref().map(|c| format!("/tradewith {c}")), copied);
    });
}

/// An icon button that copies a chat `command` to the clipboard. `name` is the
/// hover label. Disabled (greyed) when we couldn't build a command (e.g. the
/// listing has no character name).
fn chat_button(
    ui: &mut egui::Ui,
    icon: &str,
    name: &str,
    command: Option<String>,
    copied: &mut Option<String>,
) {
    match command {
        Some(cmd) => {
            if ui.button(icon).on_hover_text(name).clicked() {
                copy_to_clipboard(&cmd);
                *copied = Some(cmd);
            }
        }
        None => {
            ui.add_enabled(false, egui::Button::new(icon))
                .on_hover_text(format!("{name} (no character name)"));
        }
    }
}

/// Write to the X11 clipboard (where POE2 will paste from), logging on failure.
fn copy_to_clipboard(text: &str) {
    if let Err(e) = platform_linux::write_clipboard_text(text) {
        tracing::warn!(error = %e, "clipboard write failed");
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
pub fn spawn_hotkey_watcher(ctx: egui::Context, tx: Sender<Hotkey>) {
    use platform_linux::HotkeyEvent;
    std::thread::spawn(move || {
        let hotkeys = match platform_linux::watch_hotkeys() {
            Ok(rx) => rx,
            Err(e) => {
                tracing::warn!(error = %e, "hotkey watcher disabled; use the buttons");
                return;
            }
        };
        tracing::info!("listening for Ctrl+C / Ctrl+Alt+C / Esc in game");
        let mut last_seen = platform_linux::read_clipboard_text().unwrap_or(None);
        for event in hotkeys {
            let outcome = match event {
                // Escape dismisses — no clipboard involved.
                HotkeyEvent::Close => Hotkey::Close,
                // Ctrl/Alt state forwarded straight through.
                HotkeyEvent::Modifiers { ctrl, alt } => Hotkey::Mods { ctrl, alt },
                // A copy combo: wait for POE2 to write the item. Ctrl+Alt+C
                // opens the pinned detailed view (PRD §4.7).
                HotkeyEvent::QuickCopy | HotkeyEvent::DetailedCopy => {
                    let detailed = event == HotkeyEvent::DetailedCopy;
                    let start = Instant::now();
                    match wait_for_new_item(&last_seen) {
                        Some(text) => {
                            tracing::debug!(elapsed_ms = start.elapsed().as_millis(), "item copied");
                            last_seen = Some(text.clone());
                            Hotkey::Item { text, detailed }
                        }
                        None => {
                            tracing::debug!("Ctrl+C produced no new item (cursor static?)");
                            Hotkey::Missed
                        }
                    }
                }
            };
            if tx.send(outcome).is_err() {
                return; // UI gone
            }
            ctx.request_repaint();
        }
    });
}

/// Poll the clipboard until it both *changed* and *parses as a POE2 item*, or
/// the timeout hits (PRD §4.2). Gating on "is an item" — not merely "changed" —
/// avoids grabbing the transient/stale clipboard value the X11↔Wayland bridge
/// can briefly expose before POE2 finishes writing (which made the first Ctrl+C
/// fail while the second worked).
fn wait_for_new_item(last_seen: &Option<String>) -> Option<String> {
    let deadline = Instant::now() + CLIPBOARD_TIMEOUT;
    loop {
        if let Ok(Some(text)) = platform_linux::read_clipboard_text() {
            if Some(&text) != last_seen.as_ref() && parser::parse_item(&text).is_ok() {
                return Some(text);
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Install the HTTP/image loaders egui_extras needs for item icons. Call once
/// per `egui::Context`.
pub fn install_loaders(ctx: &egui::Context) {
    egui_extras::install_image_loaders(ctx);
}

pub fn configure_style(ctx: &egui::Context) {
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

/// Build a ready-to-render [`QuickModeApp`]: load settings, spin up a tokio
/// runtime, fetch the live trade definitions + leagues, and construct the API
/// client for the layer-shell overlay.
///
/// The league comes from `config.json` (PRD §4.8), so no env var is needed.
/// `POE_LEAGUE` / `POE_REALM`, if set, still override for that run (handy for
/// testing) but are not persisted.
pub fn build_app(hotkey_rx: Receiver<Hotkey>) -> anyhow::Result<QuickModeApp> {
    let mut config = Config::load();
    if let Ok(league) = std::env::var("POE_LEAGUE") {
        if !league.is_empty() {
            config.league = league;
        }
    }
    if let Ok(realm) = std::env::var("POE_REALM") {
        config.realm = Some(realm);
    }
    tracing::info!(path = %config::Config::path().display(), league = %config.league, "loaded config");

    let rt = tokio::runtime::Runtime::new()?;
    let transport = ReqwestTransport::new(USER_AGENT)?;
    tracing::info!("fetching trade definitions…");
    let (stats, items) = rt
        .block_on(fetch_definitions(&transport, BASE_URL))
        .map_err(|e| anyhow::anyhow!("loading definitions: {e}"))?;
    // A leagues failure shouldn't block startup — the selector just falls back
    // to a static label.
    let leagues = rt
        .block_on(fetch_leagues(&transport, BASE_URL))
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "could not fetch leagues; selector disabled");
            Vec::new()
        });

    let mut client_config = ClientConfig::new(&config.league);
    client_config.realm = config.realm.clone();
    let client = Arc::new(TradeClient::new(transport, client_config, stats, items));

    Ok(QuickModeApp::new(rt, client, config, leagues, hotkey_rx))
}

