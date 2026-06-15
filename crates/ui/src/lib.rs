//! The price-check UI (PRD §4.6, §4.7): the egui view + app logic, windowing
//! agnostic so the `overlay` crate can drive it on a layer surface.
//!
//! Flow: Ctrl+C on an item → parse it → search + fetch via `trade-api` on a
//! background tokio task → show the median asking price and the cheapest
//! listings, each with Whisper / Invite / Hideout / Trade-with buttons that copy
//! the chat command to the clipboard (we can't type into POE2 on Wayland, so
//! the user pastes — PRD §4.6, §9.1). The popup is pinned open with a filter
//! panel (per-stat toggles, price range, similar-item) that re-queries live.

pub mod config;

use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use std::collections::HashSet;

use config::Config;
use egui::{Color32, RichText};
use parser::{Item, ModKind, Rarity};
use trade_api::{
    build_detailed_query, fetch_definitions, fetch_leagues, ClientConfig, DetailedFilters,
    EquipmentSelection, League, ListingStatus, MiscSelection, PriceCheck, PriceEstimate,
    PriceFilter, ReqwestTransport, ResultEntry, StatSelection, TradeClient,
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
/// §4.7 "debounced"). Deliberately long so toggling several filters fires one
/// request, not a burst — easier on the rate limiter. "Apply now" bypasses it.
const FILTER_DEBOUNCE: Duration = Duration::from_secs(4);

/// In-game-ish colour for rolled mod text.
const AFFIX_BLUE: Color32 = Color32::from_rgb(0x8a, 0x8a, 0xf0);
const HEADER_BG: Color32 = Color32::from_rgb(0x17, 0x17, 0x1c);

/// Popup width — wide enough for the filter panel.
pub const POPUP_WIDTH: u32 = 600;

pub type Client = TradeClient<ReqwestTransport>;

/// Result of a background price check, sent back to the UI thread.
enum Msg {
    Result(Box<Result<PriceCheck, String>>),
    /// poeprices.info ML estimate (rares). `None` = poeprices declined to price
    /// it; `Err` = it failed.
    Estimate(Box<Result<Option<PriceEstimate>, String>>),
}

/// What the global-hotkey watcher observed.
pub enum Hotkey {
    /// A new item landed on the clipboard (Ctrl+C / Ctrl+Alt+C — both open the
    /// pinned filter popup).
    Item { text: String },
    /// The clipboard never produced an item before the timeout — usually POE2
    /// skipping the copy on a static cursor (PRD §9.3).
    Missed,
    /// Escape was pressed — dismiss the popup.
    Close,
    /// Ctrl / Alt held-state changed (from evdev, since the overlay has no
    /// keyboard focus).
    Mods { ctrl: bool, alt: bool },
    /// F5 — run the configured chat macro (e.g. `/hideout`).
    Macro,
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
    /// This filter is an implicit mod — flagged with a pill and off by default.
    is_implicit: bool,
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

/// A defence/offence equipment-property filter (PRD §4.7), built from the item's
/// parsed properties (e.g. `Evasion Rating: 1099`) rather than its affix mods.
struct EquipmentRow {
    /// Trade filter id (`ev`, `ar`, `es`, …).
    key: String,
    /// Display label (the property name, e.g. `Evasion Rating`).
    label: String,
    enabled: bool,
    min: String,
    max: String,
}

impl EquipmentRow {
    fn selection(&self) -> EquipmentSelection {
        EquipmentSelection {
            key: self.key.clone(),
            enabled: self.enabled,
            min: parse_num(&self.min),
            max: parse_num(&self.max),
        }
    }
}

/// Map a parsed item-property name to its trade equipment-filter id, for the
/// properties worth filtering on (defences + spirit).
fn equipment_key(property_name: &str) -> Option<&'static str> {
    match property_name {
        "Armour" => Some("ar"),
        "Evasion Rating" => Some("ev"),
        "Energy Shield" => Some("es"),
        "Spirit" => Some("spirit"),
        "Ward" => Some("ward"),
        "Block" | "Block chance" => Some("block"),
        _ => None,
    }
}

/// First numeric run in a property value (`"1099 (augmented)"` → `1099`).
fn first_number(s: &str) -> Option<f64> {
    let start = s.find(|c: char| c.is_ascii_digit())?;
    let rest = &s[start..];
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// Build equipment-property filter rows from the item's defences (PRD §4.7),
/// prefilled with the item's value and ticked — the key thing you search armour
/// by, but absent before because they're properties, not affix mods.
fn build_equipment_rows(item: &Item, percent: u32, exceptional: bool) -> Vec<EquipmentRow> {
    let mut rows: Vec<EquipmentRow> = item
        .properties
        .iter()
        .filter_map(|prop| {
            let key = equipment_key(&prop.name)?;
            let value = first_number(&prop.value)?;
            Some(EquipmentRow {
                key: key.to_string(),
                label: prop.name.clone(),
                enabled: true,
                min: fmt_amount(scaled_min(value, percent)),
                max: String::new(),
            })
        })
        .collect();

    // Rune sockets (the "S S S" line). Usually not worth filtering — but on an
    // Exceptional base the extra socket is the whole value, so default it ON
    // (min = the item's own count) there; otherwise leave it available but off.
    let sockets = socket_count(item);
    if sockets > 0 {
        rows.push(EquipmentRow {
            key: "rune_sockets".to_string(),
            label: "Rune sockets".to_string(),
            enabled: exceptional,
            min: sockets.to_string(),
            max: String::new(),
        });
    }
    rows
}

/// Number of rune sockets (count of `S` on the parsed `Sockets:` line).
fn socket_count(item: &Item) -> usize {
    item.sockets
        .as_deref()
        .map(|s| s.chars().filter(|c| *c == 'S').count())
        .unwrap_or(0)
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

/// A single-value "≥ min" filter with an enable toggle (item quality, item
/// level — both routed to `type_filters`).
#[derive(Default)]
struct MinFilter {
    enabled: bool,
    min: String,
}

impl MinFilter {
    fn new(enabled: bool, min: Option<u32>) -> Self {
        MinFilter {
            enabled,
            min: min.filter(|v| *v > 0).map(|v| v.to_string()).unwrap_or_default(),
        }
    }

    fn value(&self) -> Option<f64> {
        if self.enabled {
            parse_num(&self.min)
        } else {
            None
        }
    }
}

/// Boolean item attributes for the Miscellaneous section (trade filter id,
/// label), sorted alphabetically by label. All off by default.
const MISC_OPTIONS: &[(&str, &str)] = &[
    ("corrupted", "Corrupted"),
    ("crafted", "Crafted"),
    ("desecrated", "Desecrated"),
    ("fractured_item", "Fractured"),
    ("identified", "Identified"),
    ("mirrored", "Mirrored"),
    ("sanctified", "Sanctified"),
    ("twice_corrupted", "Twice Corrupted"),
];

/// A boolean Miscellaneous toggle (e.g. Corrupted). Checked → require `true`.
struct MiscToggle {
    key: &'static str,
    label: &'static str,
    on: bool,
}

/// Currencies offered in the price-range dropdown (id, label). Empty id = any.
const PRICE_CURRENCIES: &[(&str, &str)] = &[
    ("", "any"),
    ("exalted", "exalted"),
    ("divine", "divine"),
    ("chaos", "chaos"),
];

/// Width of each min/max filter field.
const FILTER_FIELD_W: f32 = 60.0;

/// Right-aligned min + max fields (they hug the right edge of the row so the
/// columns line up and the row fills the width). Returns whether either changed.
fn min_max_fields(ui: &mut egui::Ui, min: &mut String, max: &mut String) -> bool {
    let mut changed = false;
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        // In a right-to-left layout the first item is rightmost, so max first.
        changed |= ui
            .add(
                egui::TextEdit::singleline(max)
                    .hint_text("max")
                    .desired_width(FILTER_FIELD_W),
            )
            .changed();
        changed |= ui
            .add(
                egui::TextEdit::singleline(min)
                    .hint_text("min")
                    .desired_width(FILTER_FIELD_W),
            )
            .changed();
    });
    changed
}

/// A checkbox + label for a single-value (min-only) filter, with the min field
/// right-aligned to fill the row width. Returns whether it changed.
fn min_filter_row(ui: &mut egui::Ui, label: &str, filter: &mut MinFilter) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        changed |= ui.checkbox(&mut filter.enabled, "").changed();
        ui.label(label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            changed |= ui
                .add(
                    egui::TextEdit::singleline(&mut filter.min)
                        .hint_text("min")
                        .desired_width(FILTER_FIELD_W),
                )
                .changed();
        });
    });
    changed
}

/// A small green "implicit" pill, drawn before an implicit filter's label.
fn implicit_pill(ui: &mut egui::Ui) {
    egui::Frame::none()
        .fill(Color32::from_rgb(0x2e, 0x7d, 0x32))
        .rounding(7.0)
        .inner_margin(egui::Margin::symmetric(5.0, 1.0))
        .show(ui, |ui| {
            ui.label(
                RichText::new("implicit")
                    .color(Color32::from_rgb(0xe6, 0xff, 0xe6))
                    .small(),
            );
        });
}

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
    /// The item the current/last search was built from (for the header).
    item: Option<Item>,
    /// Icon URL of the priced item, learned from the search results.
    icon_url: Option<String>,
    /// Per-stat affix filter rows (rebuilt on a fresh check).
    filters: Vec<StatFilterRow>,
    /// Equipment-property filter rows (armour/evasion/ES/… defences).
    equipment: Vec<EquipmentRow>,
    /// Price-range filter.
    price_filter: PriceFilterState,
    /// Item-quality filter (default-on for bonus-quality bases).
    quality_filter: MinFilter,
    /// Item-level filter (default-on for any item with an item level — a major
    /// price driver).
    ilvl_filter: MinFilter,
    /// Boolean Miscellaneous attribute toggles (corrupted, mirrored, …), all
    /// off by default; persist across items.
    misc: Vec<MiscToggle>,
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
    /// Hash of the last item we actually searched. A Ctrl+C whose clipboard
    /// hashes the same is a duplicate of the loaded item → no new request.
    last_query_hash: Option<u64>,
}

/// Whitespace-collapsed form of clipboard text. Two copies of the SAME item can
/// differ by line endings / spacing (the XWayland clipboard bridge isn't
/// byte-stable), which `trim()` alone wouldn't normalise — so we collapse every
/// whitespace run to one space before comparing/hashing.
fn normalize_item_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Stable hash identifying an item, for de-duplicating repeated Ctrl+C on the
/// same item. Hashes the *parsed* structure (name / base / class / mod lines) so
/// it's invariant to any clipboard text-formatting differences between copies;
/// falls back to whitespace-normalised text if the item doesn't parse.
fn item_hash(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    match parser::parse_item(text) {
        Ok(item) => {
            item.name.hash(&mut h);
            item.base_type.hash(&mut h);
            item.item_class.hash(&mut h);
            for m in &item.modifiers {
                m.stats.hash(&mut h);
            }
        }
        Err(_) => normalize_item_text(text).hash(&mut h),
    }
    h.finish()
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
            item: None,
            icon_url: None,
            filters: Vec::new(),
            equipment: Vec::new(),
            price_filter: PriceFilterState::default(),
            quality_filter: MinFilter::default(),
            ilvl_filter: MinFilter::default(),
            misc: MISC_OPTIONS
                .iter()
                .map(|(key, label)| MiscToggle { key, label, on: false })
                .collect(),
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
            last_query_hash: None,
        }
    }

    /// Whether Ctrl is currently held (from the evdev watcher).
    pub fn ctrl_held(&self) -> bool {
        self.ctrl_held
    }

    /// Logical surface width; the overlay reads this each frame to size the
    /// layer.
    pub fn surface_width(&self) -> u32 {
        POPUP_WIDTH
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
        // "Exceptional" bases carry a tier prefix that resolve_base strips
        // (e.g. "Exceptional Obliterator Bow" → "Obliterator Bow"); on those the
        // extra sockets / quality are the value, so default those filters on.
        let exceptional = self.is_exceptional_base(&item);
        self.filters = self.build_filter_rows(&item);
        self.equipment = build_equipment_rows(&item, self.config.filter_min_percent, exceptional);
        // Quality: on when above the normal 20% cap (bonus quality).
        let quality = item.quality.unwrap_or(0);
        self.quality_filter = MinFilter::new(quality > 20, (quality > 0).then_some(quality as u32));
        // Item level: on for any item that has one (a major price driver).
        self.ilvl_filter = MinFilter::new(item.item_level.is_some(), item.item_level);
        self.price_filter = PriceFilterState::default();
        self.filter_dirty = false; // no stale debounce from the previous item
        // poeprices ML estimate is rares-only and doesn't depend on the
        // filters, so fetch it once per fresh check (PRD §4.7).
        if item.rarity == Rarity::Rare {
            self.spawn_estimate(ctx);
        }
        self.item = Some(item.clone());
        self.spawn_query(ctx, item);
    }

    /// Fetch the poeprices.info ML estimate for the current `item_text` on a
    /// background task (rares).
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

    /// Whether the item's base carries a tier prefix (Exceptional/Advanced/…)
    /// that the trade `type` omits — i.e. a high-tier base where the extra
    /// sockets / quality drive the price.
    fn is_exceptional_base(&self, item: &Item) -> bool {
        item.base_type.as_deref().is_some_and(|raw| {
            self.client
                .items()
                .resolve_base(raw)
                .is_some_and(|resolved| resolved != raw)
        })
    }

    /// Snapshot the current panel state into the trade-api filter struct.
    fn detailed_filters(&self) -> DetailedFilters {
        DetailedFilters {
            status: parse_status(&self.config.trade_status),
            stats: self.filters.iter().map(StatFilterRow::selection).collect(),
            equipment: self.equipment.iter().map(EquipmentRow::selection).collect(),
            misc: self
                .misc
                .iter()
                .map(|m| MiscSelection {
                    key: m.key.to_string(),
                    on: m.on,
                })
                .collect(),
            quality: self.quality_filter.value(),
            item_level: self.ilvl_filter.value(),
            price: self.price_filter.to_filter(),
        }
    }

    /// Spawn the background search/fetch for `item` using the current filters.
    fn spawn_query(&mut self, ctx: &egui::Context, item: Item) {
        tracing::info!(
            item = item.name.as_deref().or(item.base_type.as_deref()).unwrap_or("?"),
            "search"
        );
        self.phase = Phase::Loading;
        let client = Arc::clone(&self.client);
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        let filters = self.detailed_filters();
        self.rt.spawn(async move {
            let result = client
                .price_check_detailed(&item, &filters, SAMPLE)
                .await
                .map_err(|e| e.to_string());
            if let Err(ref e) = result {
                // Log so it's easy to copy out of the terminal, not just the popup.
                tracing::error!(error = %e, "price check failed");
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
        // body armour). Most mods start ticked so the first search matches the
        // item; implicits and the configured noise mods (life regen, light
        // radius, …) start unticked (PRD §4.7, config-driven).
        for mapped in self.client.stats().map_item(item) {
            if !seen.insert(mapped.id.clone()) {
                continue;
            }
            let rolled = mapped.filter_value();
            let is_implicit = mapped.stat_type == "implicit";
            let off = self.config.filter_off_by_default(&mapped.template, is_implicit);
            let pct = self.config.filter_min_percent;
            rows.push(StatFilterRow {
                id: mapped.id,
                label: mapped.template,
                enabled: !off,
                min: rolled
                    .map(|v| fmt_amount(scaled_min(v, pct)))
                    .unwrap_or_default(),
                max: String::new(),
                rolled,
                is_implicit,
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
                Hotkey::Item { text } => {
                    self.hint = None;
                    self.pop_requested = true;
                    // De-dup by hash: a Ctrl+C on the SAME item just re-shows the
                    // popup, never fires another request (saves rate limit).
                    let hash = item_hash(&text);
                    if self.last_query_hash == Some(hash) {
                        tracing::debug!("same item re-copied — not re-searching");
                    } else {
                        self.last_query_hash = Some(hash);
                        self.item_text = text;
                        self.view = View::Item;
                        self.start_price_check(ctx);
                    }
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
                Hotkey::Macro => {
                    // F5 chat macro (e.g. /hideout), injected via uinput into the
                    // focused window (POE2). Runs off-thread — it blocks ~½s.
                    if let Some(cmd) = self.config.f5_command.clone() {
                        tracing::info!(command = %cmd, "running chat macro");
                        std::thread::spawn(move || {
                            if let Err(e) = platform_linux::send_chat_command(&cmd) {
                                tracing::warn!(error = %format!("{e:#}"), "chat macro failed");
                            }
                        });
                    }
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

        // Header: title (left) + league selector & close button (right). All
        // header items use the SAME (default) text size so they share a baseline
        // and line up — a bigger title sat off from the smaller controls.
        // Dismissed by the X (or Esc, or clicking outside).
        ui.horizontal(|ui| {
            ui.label(RichText::new("poe2ddd").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("X")
                    .on_hover_text("Close (Esc / click outside)")
                    .clicked()
                {
                    self.close_requested = true;
                }
                self.league_selector(ui, &ctx);
            });
        });
        ui.add_space(4.0);

        // View toggle (Item ⇄ Text). Pricing is driven by Ctrl+C / the filters,
        // so there's no manual "price check" button any more.
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.view, View::Item, "🛡 Item");
            ui.selectable_value(&mut self.view, View::Text, "📝 Text");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("📋 Read clipboard").clicked() {
                    self.read_clipboard();
                    self.start_price_check(&ctx);
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
                // Render from the already-parsed item — NOT a re-parse of the
                // text every frame (that was a per-frame cost that made the
                // continuously-redrawn overlay lag).
                if let Some(item) = &self.item {
                    item_card(ui, item, self.icon_url.as_deref());
                } else if self.item_text.trim().is_empty() {
                    ui.label(
                        RichText::new("Hover an item in POE2 and press Ctrl+C to price it.")
                            .weak()
                            .italics(),
                    );
                } else {
                    ui.label(
                        RichText::new("Not a POE2 item — switch to 📝 Text to edit.")
                            .weak()
                            .italics(),
                    );
                }
            }
        }

        // The filter panel (PRD §4.7), between the item and the listings. Edits
        // re-run the search after a short debounce; "Apply now" fires immediately.
        {
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

        // poeprices.info ML estimate badge (rares — PRD §4.7).
        self.estimate_badge(ui);

        ui.separator();

        let mut copied: Option<String> = None;
        let mut open_trade: Option<String> = None;
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
                if ui
                    .button("🌐 Open on trade site")
                    .on_hover_text("Opens your browser with this exact search")
                    .clicked()
                {
                    // Built only on click — the URL encodes the full query
                    // (disabled filters included) and is too costly to rebuild
                    // every frame.
                    open_trade = Some(self.trade_url());
                }
            }
        }
        if let Some(label) = copied {
            self.copy_status = Some(label);
        }
        if let Some(url) = open_trade {
            match platform_linux::open_url(&url) {
                // Hide the popup so the browser comes forward — we're an
                // always-on-top overlay that would otherwise cover it.
                Ok(()) => self.close_requested = true,
                Err(e) => tracing::warn!(error = %e, "xdg-open failed"),
            }
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
                // Price range (PRD §4.7 price-range filter), right-aligned.
                ui.horizontal(|ui| {
                    ui.label("Price");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
                        changed |= ui
                            .add(
                                egui::TextEdit::singleline(&mut self.price_filter.max)
                                    .hint_text("max")
                                    .desired_width(FILTER_FIELD_W),
                            )
                            .changed();
                        ui.label("–");
                        changed |= ui
                            .add(
                                egui::TextEdit::singleline(&mut self.price_filter.min)
                                    .hint_text("min")
                                    .desired_width(FILTER_FIELD_W),
                            )
                            .changed();
                    });
                });

                // Item level (type_filters.ilvl) — default-on; a major price
                // driver. And item quality (type_filters.quality).
                changed |= min_filter_row(ui, "Item level ≥", &mut self.ilvl_filter);
                changed |= min_filter_row(ui, "Quality ≥", &mut self.quality_filter);

                // Defences / equipment properties (armour / evasion / ES / …),
                // built from the item's stats block, not its affix mods.
                if !self.equipment.is_empty() {
                    ui.add_space(6.0);
                    ui.label(RichText::new("Defences").strong());
                    for row in &mut self.equipment {
                        ui.horizontal(|ui| {
                            changed |= ui.checkbox(&mut row.enabled, "").changed();
                            ui.label(RichText::new(&row.label).strong());
                            changed |= min_max_fields(ui, &mut row.min, &mut row.max);
                        });
                    }
                }

                ui.add_space(6.0);
                if !self.equipment.is_empty() {
                    ui.label(RichText::new("Modifiers").strong());
                }
                if self.filters.is_empty() {
                    ui.label(
                        RichText::new("No mapped stats to filter on this item.")
                            .weak()
                            .italics(),
                    );
                } else {
                    egui::ScrollArea::vertical()
                        .max_height(240.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for row in &mut self.filters {
                                ui.horizontal(|ui| {
                                    changed |= ui.checkbox(&mut row.enabled, "").changed();
                                    if row.is_implicit {
                                        implicit_pill(ui);
                                    }
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(&row.label).color(AFFIX_BLUE),
                                        )
                                        .truncate(),
                                    );
                                    changed |= min_max_fields(ui, &mut row.min, &mut row.max);
                                });
                            }
                        });
                }

                // Miscellaneous: boolean attribute filters, collapsed by default.
                ui.add_space(6.0);
                egui::CollapsingHeader::new(RichText::new("Miscellaneous").strong())
                    .default_open(false)
                    .show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            for m in &mut self.misc {
                                changed |= ui.checkbox(&mut m.on, m.label).changed();
                            }
                        });
                    });

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
                                .map(|v| fmt_amount(scaled_min(v, 80)))
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

    /// Deep link to the official trade site for the current item + filters
    /// (PRD §4.6). Encodes the whole query in `?q=` (not just a saved-search id)
    /// so every filter — including the disabled ones — shows on the site exactly
    /// as in the popup (unticked ones greyed, not missing).
    fn trade_url(&self) -> String {
        let base = format!(
            "https://www.pathofexile.com/trade2/search/poe2/{}",
            percent_encode(&self.config.league)
        );
        let Some(item) = &self.item else {
            return base;
        };
        let request = build_detailed_query(item, self.client.items(), &self.detailed_filters());
        match serde_json::to_string(&request) {
            Ok(json) => format!("{base}?q={}", percent_encode(&json)),
            Err(e) => {
                tracing::warn!(error = %e, "could not encode trade query");
                base
            }
        }
    }
}

/// Percent-encode a string for use in a URL (RFC 3986 unreserved pass through).
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
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
        // Up to 3 decimals, trailing zeros trimmed — so 0.17 stays "0.17", not
        // "0.2" (the old {:.1} rounded a 0.17 roll up and over-tightened the
        // search below the item itself), and 2.5 stays "2.5".
        format!("{amount:.3}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}

/// Map the configured trade-status string to a [`ListingStatus`] (defaults to
/// Instant Buyout / securable for anything unrecognised).
fn parse_status(s: &str) -> ListingStatus {
    match s.trim().to_ascii_lowercase().as_str() {
        "online" => ListingStatus::Online,
        "available" => ListingStatus::Available,
        "any" => ListingStatus::Any,
        _ => ListingStatus::Securable,
    }
}

/// Scale a rolled value to the configured filter-min percentage (PRD §4.7).
/// Integer rolls floor (so 90% of 132 → 118, a clean buffer); fractional rolls
/// keep their precision.
fn scaled_min(rolled: f64, percent: u32) -> f64 {
    let scaled = rolled * percent as f64 / 100.0;
    if rolled.fract() == 0.0 {
        scaled.floor()
    } else {
        scaled
    }
}

/// Watch the global price-check hotkeys on a background thread. On each press
/// we wait for POE2 to write the clipboard, then push the item text to the UI.
/// If the watcher can't start (e.g. not in the `input` group), we log and carry
/// on — the window still works manually (PRD §4.1).
pub fn spawn_hotkey_watcher(ctx: egui::Context, tx: Sender<Hotkey>) {
    use platform_linux::{HotkeyBindings, HotkeyEvent};
    // Hotkeys and the POE2-focus gate are config-driven (rebindable, PRD §4.8).
    let config = Config::load();
    let bindings = HotkeyBindings::from_strings(
        &config.hotkey_quick,
        &config.hotkey_detailed,
        &config.hotkey_macro,
        &config.hotkey_close,
    );
    let require_focus = config.require_poe2_focus;

    std::thread::spawn(move || {
        let hotkeys = match platform_linux::watch_hotkeys(bindings) {
            Ok(rx) => rx,
            Err(e) => {
                tracing::warn!(error = %e, "hotkey watcher disabled; use the buttons");
                return;
            }
        };
        tracing::info!(
            quick = %config.hotkey_quick,
            detailed = %config.hotkey_detailed,
            macro_ = %config.hotkey_macro,
            require_poe2_focus = require_focus,
            "listening for hotkeys"
        );
        // Pre-create the injection device (after the watcher scanned keyboards,
        // so it isn't picked up) so the first macro press is instant.
        if config.f5_command.is_some() {
            std::thread::spawn(platform_linux::warm_up_injection);
        }
        // Shared so the clipboard wait can run OFF this loop (below): the loop
        // must NOT block, or evdev modifier events (Ctrl/Alt for the overlay's
        // show + Alt-drag) queue behind a ≤1s clipboard poll and the drag lags.
        let last_seen = Arc::new(Mutex::new(
            platform_linux::read_clipboard_text().unwrap_or(None),
        ));
        for event in hotkeys {
            match event {
                // Escape dismisses — overlay control, not gated to POE2 focus.
                HotkeyEvent::Close => {
                    let _ = tx.send(Hotkey::Close);
                    ctx.request_repaint();
                }
                // Ctrl/Alt state — must be forwarded INSTANTLY (overlay drag/show).
                HotkeyEvent::Modifiers { ctrl, alt } => {
                    let _ = tx.send(Hotkey::Mods { ctrl, alt });
                    ctx.request_repaint();
                }
                // Chat macro — only into POE2. Off-thread so the focus check
                // (xdotool) doesn't stall the loop.
                HotkeyEvent::Macro => {
                    let (tx, ctx) = (tx.clone(), ctx.clone());
                    std::thread::spawn(move || {
                        if require_focus && !platform_linux::is_poe2_active() {
                            tracing::info!("macro ignored — POE2 not focused");
                            return;
                        }
                        let _ = tx.send(Hotkey::Macro);
                        ctx.request_repaint();
                    });
                }
                // A copy combo: the focus check + the ≤1s clipboard poll run on
                // their own thread so this loop keeps forwarding modifier events.
                HotkeyEvent::QuickCopy | HotkeyEvent::DetailedCopy => {
                    let (tx, ctx, last) = (tx.clone(), ctx.clone(), last_seen.clone());
                    std::thread::spawn(move || {
                        if require_focus && !platform_linux::is_poe2_active() {
                            tracing::info!("Ctrl+C ignored — POE2 not focused");
                            return;
                        }
                        let prev = last.lock().expect("last_seen lock").clone();
                        let start = Instant::now();
                        let outcome = match wait_for_new_item(&prev) {
                            Some(text) => {
                                tracing::info!(
                                    elapsed_ms = start.elapsed().as_millis(),
                                    hash = item_hash(&text),
                                    "clipboard: NEW item → pricing"
                                );
                                *last.lock().expect("last_seen lock") = Some(text.clone());
                                Hotkey::Item { text }
                            }
                            None => {
                                tracing::info!("clipboard: same/no item → ignored");
                                Hotkey::Missed
                            }
                        };
                        let _ = tx.send(outcome);
                        ctx.request_repaint();
                    });
                }
            }
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
    // Compare whitespace-normalised: a same item re-copied must NOT count as a
    // new item (else it re-searches and burns the rate limit).
    let last = last_seen.as_deref().map(normalize_item_text);
    loop {
        if let Ok(Some(text)) = platform_linux::read_clipboard_text() {
            let is_new = last.as_deref() != Some(normalize_item_text(&text).as_str());
            if is_new && parser::parse_item(&text).is_ok() {
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

