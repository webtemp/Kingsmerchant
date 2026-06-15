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
    EquipmentSelection, ExchangeCheck, ExchangeOffer, League, ListingStatus, MiscSelection,
    PriceCheck, PriceEstimate, PriceFilter, ReqwestTransport, ResultEntry, StatSelection,
    TradeClient,
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
/// How long a price-check result stays "fresh": re-viewing the same item within
/// this window re-shows the cached results without hitting the API again (PRD
/// §4.4 rate-limit care); after it, a re-view refreshes.
const CACHE_TTL: Duration = Duration::from_secs(120);
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
    /// Bulk-exchange result for a stackable (currency/rune/fragment/…).
    Exchange(Box<Result<ExchangeCheck, String>>),
}

/// Which pricing path the loaded item uses (PRD §4.4): a normal per-item search,
/// or the bulk currency exchange for stackables.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PriceMode {
    Item,
    Exchange,
}

/// Background state of a bulk-exchange price check (parallel to [`Phase`], which
/// covers the per-item search).
#[derive(Default)]
enum ExchangePhase {
    #[default]
    Idle,
    Loading,
    Done(ExchangeCheck),
    Failed(String),
}

/// What the global-hotkey watcher (or the tray / config watcher) observed.
/// Everything that needs to reach the UI thread funnels through this one
/// channel, which [`pump`](QuickModeApp::pump) drains every frame.
pub enum Hotkey {
    /// A price-check combo was pressed and POE2 is focused — pop the popup
    /// IMMEDIATELY into a "reading…" state, before the (up-to-1s) clipboard poll
    /// runs, so the user gets instant feedback that something is happening.
    CopyStarted,
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
    /// F2 — run the second configured chat macro (e.g. `/exit`).
    Macro2,
    /// Open the settings surface (from the tray menu or the gear button).
    OpenSettings,
    /// Quit the app (from the tray menu).
    Quit,
    /// `config.json` changed on disk (PRD §4.8 hot-reload) — apply the
    /// live-reloadable fields. Boxed: it's the largest variant and rare.
    ConfigReloaded(Box<Config>),
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

/// Trade listing-status options for the settings dropdown (config id, label).
const TRADE_STATUSES: &[(&str, &str)] = &[
    ("securable", "Instant Buyout"),
    ("online", "Online (In Person)"),
    ("available", "Online + Buyout"),
    ("any", "Any"),
];

fn trade_status_label(id: &str) -> &str {
    TRADE_STATUSES
        .iter()
        .find(|(i, _)| *i == id)
        .map(|(_, l)| *l)
        .unwrap_or("Instant Buyout")
}

/// Popup position modes for the settings dropdown (config id, label).
const POSITION_MODES: &[(&str, &str)] = &[
    ("center", "Center"),
    ("fixed", "Fixed"),
    ("at-cursor", "At cursor (Phase 7)"),
];

fn position_label(id: &str) -> &str {
    POSITION_MODES
        .iter()
        .find(|(i, _)| *i == id)
        .map(|(_, l)| *l)
        .unwrap_or("Center")
}

/// A labelled, right-aligned hotkey-string text field. Sets `*changed` and
/// returns whether it changed (the caller folds that into the restart flag,
/// since bindings are only read at startup).
fn hotkey_row(ui: &mut egui::Ui, label: &str, value: &mut String, changed: &mut bool) -> bool {
    let mut row_changed = false;
    ui.horizontal(|ui| {
        ui.label(format!("  {label}"));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            row_changed = ui
                .add(egui::TextEdit::singleline(value).desired_width(140.0))
                .changed();
        });
    });
    if row_changed {
        *changed = true;
    }
    row_changed
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
    /// hashes the same is a duplicate of the loaded item → re-show, no request.
    last_query_hash: Option<u64>,
    /// When the loaded item was last actually queried (cache freshness). A
    /// re-view within [`CACHE_TTL`] shows the cached results without re-querying.
    last_query_at: Option<Instant>,
    /// A Ctrl+C was just detected and we're polling the clipboard for the item —
    /// drives the instant "reading…" spinner so the popup never sits silent.
    awaiting_copy: bool,
    /// Tray handle for pushing the current state to the tooltip (PRD §4.9).
    /// `None` if the tray failed to start (no SNI host).
    tray: Option<platform_linux::TrayHandle>,
    /// Set when the gear button or the tray's "Open Settings" fires — the
    /// overlay reads this to show the settings surface.
    settings_requested: bool,
    /// Set when the settings surface's close button fires.
    settings_close_requested: bool,
    /// Set when the tray's "Quit" fires — the overlay reads this to exit.
    quit_requested: bool,
    /// A note shown in the settings panel after an action (saved / restart
    /// needed for hotkeys, …).
    settings_note: Option<String>,
    /// Whether the loaded item prices via the per-item search or the bulk
    /// currency exchange (PRD §4.4).
    mode: PriceMode,
    /// Background state of the bulk-exchange check (used in [`PriceMode::Exchange`]).
    exchange_phase: ExchangePhase,
    /// The `data/static` exchange id of the loaded stackable (the `want`).
    exchange_want_id: String,
    /// Currency the exchange prices are shown in (the `have`); default Exalted.
    pay_currency: String,
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
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        rt: tokio::runtime::Runtime,
        client: Arc<Client>,
        config: Config,
        leagues: Vec<League>,
        hotkey_rx: Receiver<Hotkey>,
        tray: Option<platform_linux::TrayHandle>,
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
            last_query_at: None,
            awaiting_copy: false,
            tray,
            settings_requested: false,
            settings_close_requested: false,
            quit_requested: false,
            settings_note: None,
            mode: PriceMode::Item,
            exchange_phase: ExchangePhase::Idle,
            exchange_want_id: String::new(),
            pay_currency: "exalted".to_string(),
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

    /// Consume a pending "open settings" request (gear button / tray).
    pub fn take_settings_request(&mut self) -> bool {
        std::mem::take(&mut self.settings_requested)
    }

    /// Consume a pending "close settings" request (settings X button).
    pub fn take_settings_close_request(&mut self) -> bool {
        std::mem::take(&mut self.settings_close_requested)
    }

    /// Consume a pending "quit the app" request (tray Quit).
    pub fn take_quit_request(&mut self) -> bool {
        std::mem::take(&mut self.quit_requested)
    }

    /// Configured popup position mode (`center` / `fixed` / `at-cursor`). The
    /// overlay reads this each frame to place the popup surface.
    pub fn position_mode(&self) -> &str {
        &self.config.position_mode
    }

    /// Configured fixed-mode top-left position (output-logical pixels).
    pub fn fixed_pos(&self) -> (i32, i32) {
        (self.config.fixed_x, self.config.fixed_y)
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
        self.exchange_phase = ExchangePhase::Idle;

        // Stackables (currency, runes, fragments, essences, …) aren't sold as
        // individual listings — they trade via the bulk exchange, which the
        // per-item search can't price (PRD §4.4). Route them there instead.
        if let Some(want_id) = self.exchange_id_for(&item) {
            self.mode = PriceMode::Exchange;
            self.exchange_want_id = want_id;
            self.item = Some(item);
            self.spawn_exchange_query(ctx);
            return;
        }
        self.mode = PriceMode::Item;

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

    /// The bulk-exchange currency id for an item, if it's a stackable that
    /// prices via the exchange rather than the per-item search. Tries the item
    /// name then the base-type line against the `data/static` catalogue.
    fn exchange_id_for(&self, item: &Item) -> Option<String> {
        [item.name.as_deref(), item.base_type.as_deref()]
            .into_iter()
            .flatten()
            .find_map(|name| self.client.currencies().lookup(name).map(|e| e.id.clone()))
    }

    /// Price the loaded stackable via the bulk exchange on a background task,
    /// paying in the currently-selected `pay_currency`.
    fn spawn_exchange_query(&mut self, ctx: &egui::Context) {
        self.exchange_phase = ExchangePhase::Loading;
        self.last_query_at = Some(Instant::now());
        let client = Arc::clone(&self.client);
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        let want = self.exchange_want_id.clone();
        let pay = self.pay_currency.clone();
        tracing::info!(want = %want, pay = %pay, "exchange check");
        self.rt.spawn(async move {
            let result = client
                .price_check_exchange(&want, &pay)
                .await
                .map_err(|e| e.to_string());
            if let Err(ref e) = result {
                tracing::error!(error = %e, "exchange price check failed");
            }
            let _ = tx.send(Msg::Exchange(Box::new(result)));
            ctx.request_repaint();
        });
    }

    /// Re-run the price check for the already-loaded item, keeping current
    /// state (used by the detailed panel, the pay-currency selector, and a
    /// league switch). Routes to the search or the exchange per the mode.
    fn rerun_query(&mut self, ctx: &egui::Context) {
        match self.mode {
            PriceMode::Item => {
                if let Some(item) = self.item.clone() {
                    self.spawn_query(ctx, item);
                }
            }
            PriceMode::Exchange => self.spawn_exchange_query(ctx),
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
        self.last_query_at = Some(Instant::now());
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
                Hotkey::CopyStarted => {
                    // Instant feedback: show the popup with a "reading…" spinner
                    // the moment Ctrl+C is detected (the item/results follow).
                    self.hint = None;
                    self.awaiting_copy = true;
                    self.pop_requested = true;
                }
                Hotkey::Item { text } => {
                    self.hint = None;
                    self.awaiting_copy = false;
                    // ALWAYS re-show the popup — even for the same item (the user
                    // closed it and looked again). This is the fix for "re-view
                    // does nothing": the API call is what we de-dup, not the pop.
                    self.pop_requested = true;
                    let hash = item_hash(&text);
                    if self.last_query_hash == Some(hash) {
                        // Same item as loaded → keep the cached results (and any
                        // filter state). Only refresh from the API if the cache
                        // has gone stale (older than CACHE_TTL).
                        let stale = self
                            .last_query_at
                            .map(|t| t.elapsed() >= CACHE_TTL)
                            .unwrap_or(true);
                        if stale {
                            tracing::info!("same item, cache stale → refreshing");
                            self.rerun_query(ctx);
                        } else {
                            tracing::info!("same item, cache fresh → re-showing cached results");
                        }
                    } else {
                        self.last_query_hash = Some(hash);
                        self.item_text = text;
                        self.view = View::Item;
                        self.start_price_check(ctx);
                    }
                }
                Hotkey::Missed => {
                    self.awaiting_copy = false;
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
                    // focused window (POE2).
                    run_chat_macro(self.config.f5_command.clone());
                }
                Hotkey::Macro2 => {
                    // F2 chat macro (e.g. /exit).
                    run_chat_macro(self.config.macro2_command.clone());
                }
                Hotkey::OpenSettings => {
                    self.settings_requested = true;
                }
                Hotkey::Quit => {
                    self.quit_requested = true;
                }
                Hotkey::ConfigReloaded(config) => {
                    self.apply_reloaded_config(*config, ctx);
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
                Msg::Exchange(result) => {
                    self.exchange_phase = match *result {
                        Ok(ex) => ExchangePhase::Done(ex),
                        Err(e) => ExchangePhase::Failed(e),
                    };
                }
            }
        }

        self.update_tray();
    }

    /// Push the current app state to the tray tooltip (PRD §4.9). Idempotent —
    /// the handle skips the D-Bus update when the state is unchanged.
    fn update_tray(&mut self) {
        let Some(tray) = self.tray.as_mut() else {
            return;
        };
        let state = if let Some(wait) = self.client.retry_in() {
            let secs = (wait.as_millis() as u64).div_ceil(1000);
            platform_linux::TrayState::RateLimited(secs)
        } else if let Phase::Failed(e) = &self.phase {
            // Keep the tooltip short — first line only.
            let short = e.lines().next().unwrap_or(e).to_string();
            platform_linux::TrayState::Error(short)
        } else {
            platform_linux::TrayState::Listening
        };
        tray.set_state(state);
    }

    /// Apply the live-reloadable fields of a config reloaded from disk (PRD
    /// §4.8 hot-reload). League switches the client + re-prices; filter defaults
    /// and placement take effect on the next item. Hotkey bindings, realm, and
    /// the POE2-focus gate are read once at startup by the evdev watcher / API
    /// client, so those need a restart — flagged, not silently dropped.
    fn apply_reloaded_config(&mut self, new: Config, ctx: &egui::Context) {
        let league_changed = new.league != self.config.league;
        let restart_needed = new.hotkey_quick != self.config.hotkey_quick
            || new.hotkey_detailed != self.config.hotkey_detailed
            || new.hotkey_macro != self.config.hotkey_macro
            || new.hotkey_macro2 != self.config.hotkey_macro2
            || new.hotkey_close != self.config.hotkey_close
            || new.require_poe2_focus != self.config.require_poe2_focus
            || new.realm != self.config.realm;
        if league_changed {
            self.client.set_league(new.league.clone());
        }
        self.config = new;
        if restart_needed {
            self.settings_note =
                Some("Saved. Hotkeys / realm / focus-gate apply after a restart.".to_string());
        }
        tracing::info!(league_changed, restart_needed, "applied reloaded config");
        // A league change re-prices the loaded item immediately; other reloaded
        // fields (filter defaults, placement) take effect on the next item.
        if league_changed {
            self.rerun_query(ctx);
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
                if ui.button("⚙").on_hover_text("Settings").clicked() {
                    self.settings_requested = true;
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

        // Instant feedback while the clipboard is being read after a Ctrl+C, so
        // the popup never sits silent (kept non-destructive: any already-shown
        // item/results stay visible underneath).
        if self.awaiting_copy {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Reading item from POE2…").strong());
            });
        }

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

        ui.add_space(6.0);

        // Rate-limit feedback (PRD §4.4, shared by both modes): don't fire
        // blindly — tell the user we're waiting on the trade API's bucket.
        if let Some(wait) = self.client.retry_in() {
            let secs = (wait.as_millis() as u64).div_ceil(1000);
            ui.colored_label(
                Color32::from_rgb(0xff, 0xc8, 0x4b),
                format!("⏳ rate limited — retrying in {secs}s"),
            );
        }

        let mut copied: Option<String> = None;
        let mut open_trade: Option<String> = None;
        match self.mode {
            // Stackables (currency/runes/…) price via the bulk exchange.
            PriceMode::Exchange => self.exchange_content(ui, &ctx, &mut copied, &mut open_trade),
            // Normal items: the stat-filter panel + per-item listings.
            PriceMode::Item => {
                // The filter panel (PRD §4.7), between the item and the listings.
                // Edits re-run the search after a debounce; "Apply now" is
                // immediate.
                {
                    ui.add_space(6.0);
                    let apply_now = self.filter_panel(ui);
                    let debounced = self.filter_dirty
                        && self.filter_changed_at.elapsed() >= FILTER_DEBOUNCE
                        && !matches!(self.phase, Phase::Loading);
                    if apply_now || debounced {
                        self.filter_dirty = false;
                        self.rerun_query(&ctx);
                    }
                }
                ui.add_space(6.0);
                if matches!(self.phase, Phase::Loading) {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("searching…");
                    });
                }
                // poeprices.info ML estimate badge (rares — PRD §4.7).
                self.estimate_badge(ui);
                ui.separator();
                match &self.phase {
                    Phase::Idle => {
                        ui.label(RichText::new("Waiting for an item…").weak().italics());
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
                            // Built only on click — the URL encodes the full
                            // query (disabled filters included) and is too costly
                            // to rebuild every frame.
                            open_trade = Some(self.trade_url());
                        }
                    }
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
                format!("✓ Sent {status} to POE2"),
            );
        }
    }

    /// Render the bulk-exchange results for a stackable (PRD §4.4): a pay-with
    /// currency selector, the median + cheapest offers with whisper buttons, and
    /// a link to the exchange page. No stat filters (they don't apply).
    fn exchange_content(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        copied: &mut Option<String>,
        open_trade: &mut Option<String>,
    ) {
        // Pick what to pay with. Offers are listed in the seller's currency, so
        // we query one currency at a time (default Exalted) and the list stays
        // sortable — sidestepping exalted-vs-divine normalisation.
        ui.horizontal(|ui| {
            ui.label("Pay with");
            let before = self.pay_currency.clone();
            egui::ComboBox::from_id_salt("pay-currency")
                .selected_text(pay_label(&self.pay_currency))
                .show_ui(ui, |ui| {
                    for (id, label) in PAY_CURRENCIES {
                        ui.selectable_value(&mut self.pay_currency, id.to_string(), *label);
                    }
                });
            if self.pay_currency != before {
                self.spawn_exchange_query(ctx);
            }
        });
        if matches!(self.exchange_phase, ExchangePhase::Loading) {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("checking exchange…");
            });
        }
        ui.separator();

        match &self.exchange_phase {
            ExchangePhase::Idle => {
                ui.label(RichText::new("Waiting for the exchange…").weak().italics());
            }
            ExchangePhase::Loading => {}
            ExchangePhase::Failed(e) => {
                ui.colored_label(Color32::from_rgb(0xff, 0x6b, 0x6b), e);
            }
            ExchangePhase::Done(ex) => {
                let pay = pay_label(&ex.pay_currency);
                match ex.median_unit_price() {
                    Some(m) => {
                        ui.label(
                            RichText::new(format!("Median: {} {} each", fmt_amount(m), pay))
                                .size(18.0)
                                .strong(),
                        );
                    }
                    None => {
                        ui.label(
                            RichText::new(format!(
                                "No {pay} offers — try a different pay currency."
                            ))
                            .italics(),
                        );
                    }
                }
                ui.label(
                    RichText::new(format!(
                        "{} offer(s) · showing cheapest {}",
                        ex.offers.len(),
                        SHOWN
                    ))
                    .weak(),
                );
                ui.add_space(6.0);
                egui::Grid::new("exchange-offers")
                    .striped(true)
                    .num_columns(3)
                    .spacing([10.0, 10.0])
                    .show(ui, |ui| {
                        for offer in ex.cheapest(SHOWN) {
                            exchange_row(ui, offer, pay, copied);
                            ui.end_row();
                        }
                    });
                ui.add_space(6.0);
                if ui
                    .button("🌐 Open exchange page")
                    .on_hover_text("Opens the in-game-style currency exchange in your browser")
                    .clicked()
                {
                    *open_trade = Some(exchange_url(&self.config.league, &ex.id));
                }
            }
        }
    }

    /// Render the settings surface body (PRD §4.8). Edits write straight to
    /// `config` and persist on change. Fields the evdev watcher / API client
    /// read only at startup (hotkeys, realm, focus gate) are flagged "restart
    /// to apply" rather than pretending to take effect live. Call
    /// [`pump`](Self::pump) first (shared with the popup surface).
    pub fn settings_content(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        // Esc closes the settings panel when it has focus (it gets the key event
        // via Wayland; the popup's Esc is handled globally by the evdev watcher).
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.settings_close_requested = true;
        }
        // What kind of follow-up an edit needs.
        let mut changed = false; // any field → persist to disk
        let mut requery = false; // league / status → re-price now
        let mut restart = false; // a startup-only field → show the restart note

        ui.horizontal(|ui| {
            ui.label(RichText::new("⚙ Settings").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("X").on_hover_text("Close (Esc)").clicked() {
                    self.settings_close_requested = true;
                }
            });
        });
        ui.label(
            RichText::new("Changes save automatically — no save button.")
                .weak()
                .small(),
        );
        ui.separator();

        egui::ScrollArea::vertical()
            .max_height(560.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                // League (live — the client switches without a rebuild).
                ui.horizontal(|ui| {
                    ui.label("League");
                    if self.leagues.is_empty() {
                        ui.label(RichText::new(&self.config.league).weak());
                    } else {
                        let before = self.config.league.clone();
                        egui::ComboBox::from_id_salt("settings-league")
                            .selected_text(&self.config.league)
                            .show_ui(ui, |ui| {
                                for lg in &self.leagues {
                                    ui.selectable_value(
                                        &mut self.config.league,
                                        lg.id.clone(),
                                        &lg.text,
                                    );
                                }
                            });
                        if self.config.league != before {
                            self.client.set_league(self.config.league.clone());
                            changed = true;
                            requery = true;
                        }
                    }
                });

                // Realm (read into the request URL at startup — restart-only).
                ui.horizontal(|ui| {
                    ui.label("Realm");
                    let current = self.config.realm.clone().unwrap_or_else(|| "pc".into());
                    let mut chosen = current.clone();
                    egui::ComboBox::from_id_salt("settings-realm")
                        .selected_text(&current)
                        .show_ui(ui, |ui| {
                            for r in ["pc", "sony", "xbox"] {
                                ui.selectable_value(&mut chosen, r.to_string(), r);
                            }
                        });
                    if chosen != current {
                        self.config.realm = if chosen == "pc" { None } else { Some(chosen) };
                        changed = true;
                        restart = true;
                    }
                    ui.label(RichText::new("(restart)").weak().small());
                });

                // Listing type / trade status (live — read per query).
                ui.horizontal(|ui| {
                    ui.label("Listings");
                    let before = self.config.trade_status.clone();
                    egui::ComboBox::from_id_salt("settings-status")
                        .selected_text(trade_status_label(&self.config.trade_status))
                        .show_ui(ui, |ui| {
                            for (id, label) in TRADE_STATUSES {
                                ui.selectable_value(
                                    &mut self.config.trade_status,
                                    id.to_string(),
                                    *label,
                                );
                            }
                        });
                    if self.config.trade_status != before {
                        changed = true;
                        requery = true;
                    }
                });

                ui.separator();

                // Position mode + fixed coordinates (live — the overlay reads
                // these every frame to place the popup).
                ui.horizontal(|ui| {
                    ui.label("Popup position");
                    let before = self.config.position_mode.clone();
                    egui::ComboBox::from_id_salt("settings-position")
                        .selected_text(position_label(&self.config.position_mode))
                        .show_ui(ui, |ui| {
                            for (id, label) in POSITION_MODES {
                                ui.selectable_value(
                                    &mut self.config.position_mode,
                                    id.to_string(),
                                    *label,
                                );
                            }
                        });
                    if self.config.position_mode != before {
                        changed = true;
                    }
                });
                if self.config.position_mode == "fixed" {
                    ui.horizontal(|ui| {
                        ui.label("    x / y");
                        changed |= ui
                            .add(egui::DragValue::new(&mut self.config.fixed_x).speed(2))
                            .changed();
                        changed |= ui
                            .add(egui::DragValue::new(&mut self.config.fixed_y).speed(2))
                            .changed();
                        ui.label(RichText::new("px from top-left").weak().small());
                    });
                }
                if self.config.position_mode == "at-cursor" {
                    ui.label(
                        RichText::new("    at-cursor placement is Phase 7 — centers for now.")
                            .weak()
                            .small(),
                    );
                }

                ui.separator();

                // Filter defaults (live — applied when the next item is priced).
                ui.horizontal(|ui| {
                    ui.label("Filter min %");
                    changed |= ui
                        .add(egui::Slider::new(&mut self.config.filter_min_percent, 50..=100))
                        .changed();
                });
                changed |= ui
                    .checkbox(
                        &mut self.config.implicits_off_by_default,
                        "Implicit mods off by default",
                    )
                    .changed();

                // Chat macros (live — pump reads the command on press). Two
                // slots: F5 (default /hideout) and F2 (default /exit).
                ui.horizontal(|ui| {
                    let mut enabled = self.config.f5_command.is_some();
                    if ui.checkbox(&mut enabled, "Macro · F5").changed() {
                        self.config.f5_command =
                            if enabled { Some("/hideout".into()) } else { None };
                        changed = true;
                    }
                    if let Some(cmd) = &mut self.config.f5_command {
                        changed |= ui.text_edit_singleline(cmd).changed();
                    }
                });
                ui.horizontal(|ui| {
                    let mut enabled = self.config.macro2_command.is_some();
                    if ui.checkbox(&mut enabled, "Macro · F2").changed() {
                        self.config.macro2_command =
                            if enabled { Some("/exit".into()) } else { None };
                        changed = true;
                    }
                    if let Some(cmd) = &mut self.config.macro2_command {
                        changed |= ui.text_edit_singleline(cmd).changed();
                    }
                });

                // POE2-focus gate (read once by the evdev watcher — restart).
                ui.horizontal(|ui| {
                    if ui
                        .checkbox(
                            &mut self.config.require_poe2_focus,
                            "Only fire hotkeys while POE2 is focused",
                        )
                        .changed()
                    {
                        changed = true;
                        restart = true;
                    }
                    ui.label(RichText::new("(restart)").weak().small());
                });

                ui.separator();

                // Hotkey bindings (parsed by the evdev watcher at startup —
                // restart-only). Free-text like "Ctrl+Alt+C", "F5", "Escape".
                ui.label(RichText::new("Hotkeys (restart to apply)").strong());
                restart |= hotkey_row(ui, "Quick", &mut self.config.hotkey_quick, &mut changed);
                restart |=
                    hotkey_row(ui, "Detailed", &mut self.config.hotkey_detailed, &mut changed);
                restart |= hotkey_row(ui, "Macro · F5", &mut self.config.hotkey_macro, &mut changed);
                restart |=
                    hotkey_row(ui, "Macro · F2", &mut self.config.hotkey_macro2, &mut changed);
                restart |= hotkey_row(ui, "Close", &mut self.config.hotkey_close, &mut changed);
            });

        ui.separator();
        ui.label(
            RichText::new(format!("config.json: {}", Config::path().display()))
                .weak()
                .small(),
        );
        // A note only when there's something to say: a restart-required field
        // changed, or a save failed. Plain saves are silent (auto-save caption
        // up top already explains persistence).
        if let Some(note) = &self.settings_note {
            ui.colored_label(Color32::from_rgb(0xff, 0xc8, 0x4b), note);
        }

        if changed {
            if let Err(e) = self.config.save() {
                tracing::warn!(error = %e, "could not save config");
                self.settings_note = Some(format!("Could not save: {e}"));
            } else if restart {
                self.settings_note =
                    Some("Hotkeys / realm / focus-gate apply after a restart.".to_string());
            } else {
                self.settings_note = None;
            }
        }
        if requery {
            self.rerun_query(&ctx);
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
                        // Two even columns of four (4 + 4), evenly spaced.
                        ui.columns(2, |cols| {
                            for (i, m) in self.misc.iter_mut().enumerate() {
                                let col = if i < 4 { 0 } else { 1 };
                                changed |= cols[col].checkbox(&mut m.on, m.label).changed();
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
            if ui.button("💬").on_hover_text("Whisper (sends in POE2)").clicked() {
                send_chat_to_poe2(whisper.clone());
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

/// Currencies offered in the exchange "pay with" selector (id, label).
const PAY_CURRENCIES: &[(&str, &str)] = &[
    ("exalted", "Exalted"),
    ("divine", "Divine"),
    ("chaos", "Chaos"),
];

fn pay_label(id: &str) -> &str {
    PAY_CURRENCIES
        .iter()
        .find(|(i, _)| *i == id)
        .map(|(_, l)| *l)
        .unwrap_or("Exalted")
}

/// Deep link to the bulk-exchange page for a result (PRD §4.4).
fn exchange_url(league: &str, id: &str) -> String {
    format!(
        "https://www.pathofexile.com/trade2/exchange/poe2/{}/{}",
        percent_encode(league),
        id
    )
}

/// One bulk-exchange offer row: unit price, seller (+ online dot / stock), and
/// Whisper / Invite / Hideout / Trade buttons (the whisper is pre-filled).
fn exchange_row(ui: &mut egui::Ui, offer: &ExchangeOffer, pay: &str, copied: &mut Option<String>) {
    ui.label(RichText::new(format!("{} {}", fmt_amount(offer.unit_price), pay)).strong());

    let dot = if offer.online {
        Color32::from_rgb(0x4c, 0xd1, 0x37)
    } else {
        Color32::DARK_GRAY
    };
    ui.horizontal(|ui| {
        ui.colored_label(dot, "●");
        let label = ui.label(&offer.account);
        if let Some(stock) = offer.stock {
            label.on_hover_text(format!("stock: {}", fmt_amount(stock)));
        }
    });

    let character = offer.character.clone();
    let seller = offer.account.clone();
    ui.horizontal(|ui| {
        if let Some(whisper) = &offer.whisper {
            if ui.button("💬").on_hover_text("Whisper (sends in POE2)").clicked() {
                send_chat_to_poe2(whisper.clone());
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

/// An icon button that sends a chat `command` into POE2. `name` is the hover
/// label. Disabled (greyed) when we couldn't build a command (e.g. the listing
/// has no character name).
fn chat_button(
    ui: &mut egui::Ui,
    icon: &str,
    name: &str,
    command: Option<String>,
    copied: &mut Option<String>,
) {
    match command {
        Some(cmd) => {
            if ui.button(icon).on_hover_text(format!("{name} (sends in POE2)")).clicked() {
                send_chat_to_poe2(cmd.clone());
                *copied = Some(cmd);
            }
        }
        None => {
            ui.add_enabled(false, egui::Button::new(icon))
                .on_hover_text(format!("{name} (no character name)"));
        }
    }
}

/// Send a chat command straight into POE2 (PRD §4.6 — the buttons *act*, you
/// don't paste). Refocuses the game (our overlay had click focus), confirms it's
/// active, then injects via the same uinput paste path as the macros. Falls back
/// to leaving the command on the clipboard if POE2 can't be focused. Off-thread:
/// the focus settle + inject block ~½s.
fn send_chat_to_poe2(command: String) {
    if command.trim().is_empty() {
        return;
    }
    std::thread::spawn(move || {
        platform_linux::focus_poe2();
        // Give the compositor a moment to move keyboard focus to POE2 before we
        // inject, else the keystrokes land in our overlay (which had focus).
        std::thread::sleep(Duration::from_millis(120));
        if platform_linux::is_poe2_active() {
            if let Err(e) = platform_linux::send_chat_command(&command) {
                tracing::warn!(error = %format!("{e:#}"), "chat send failed; left on clipboard");
                let _ = platform_linux::write_clipboard_text(&command);
            }
        } else {
            tracing::info!("POE2 not focusable — left command on clipboard to paste");
            let _ = platform_linux::write_clipboard_text(&command);
        }
    });
}

/// Run a chat macro (e.g. `/hideout`, `/exit`) off-thread — it injects via
/// uinput into the focused window (POE2) and blocks ~½s. `None` / empty is a
/// no-op (the macro is disabled).
fn run_chat_macro(command: Option<String>) {
    let Some(cmd) = command else { return };
    if cmd.trim().is_empty() {
        return;
    }
    tracing::info!(command = %cmd, "running chat macro");
    std::thread::spawn(move || {
        if let Err(e) = platform_linux::send_chat_command(&cmd) {
            tracing::warn!(error = %format!("{e:#}"), "chat macro failed");
        }
    });
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
        &config.hotkey_macro2,
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
        if config.f5_command.is_some() || config.macro2_command.is_some() {
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
                // Chat macros — only into POE2. Off-thread so the focus check
                // (xdotool) doesn't stall the loop.
                HotkeyEvent::Macro | HotkeyEvent::Macro2 => {
                    let (tx, ctx) = (tx.clone(), ctx.clone());
                    let msg = if event == HotkeyEvent::Macro2 {
                        Hotkey::Macro2
                    } else {
                        Hotkey::Macro
                    };
                    std::thread::spawn(move || {
                        if require_focus && !platform_linux::is_poe2_active() {
                            tracing::info!("macro ignored — POE2 not focused");
                            return;
                        }
                        let _ = tx.send(msg);
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
                        // Pop the popup NOW (focus is confirmed), before the
                        // clipboard poll, so the UI reacts instantly.
                        let _ = tx.send(Hotkey::CopyStarted);
                        ctx.request_repaint();

                        let prev = last.lock().expect("last_seen lock").clone();
                        let start = Instant::now();
                        let outcome = match wait_for_item(&prev) {
                            Some(text) => {
                                tracing::info!(
                                    elapsed_ms = start.elapsed().as_millis(),
                                    hash = item_hash(&text),
                                    "clipboard: item → showing (UI de-dups the query)"
                                );
                                *last.lock().expect("last_seen lock") = Some(text.clone());
                                Hotkey::Item { text }
                            }
                            None => {
                                tracing::info!("clipboard: no item → ignored");
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

/// Watch `config.json` for external edits and push the reloaded config to the
/// UI thread (PRD §4.8 hot-reload). Best-effort: if the watcher can't start we
/// log and carry on (settings still apply on the next launch).
///
/// We watch the containing directory (not the file) because editors often save
/// by replacing the file via rename, which drops a watch on the inode itself.
/// Reads are write-free ([`Config::load_no_write`]) so our own reload can't
/// re-trigger the watcher.
pub fn spawn_config_watcher(ctx: egui::Context, tx: Sender<Hotkey>) {
    use notify::{RecursiveMode, Watcher};
    let path = Config::path();
    let Some(dir) = path.parent().map(|d| d.to_path_buf()) else {
        tracing::warn!("config has no parent dir; hot-reload disabled");
        return;
    };
    let file_name = path.file_name().map(|s| s.to_os_string());

    std::thread::spawn(move || {
        // Editors fire several events per save; coalesce them.
        let last = Mutex::new(Instant::now() - Duration::from_secs(1));
        let handler = move |res: notify::Result<notify::Event>| {
            let Ok(event) = res else { return };
            // Only our file, and only content-changing events.
            let touches_config = event
                .paths
                .iter()
                .any(|p| p.file_name().map(|n| n.to_os_string()) == file_name);
            if !touches_config || !matches!(event.kind, notify::EventKind::Modify(_) | notify::EventKind::Create(_)) {
                return;
            }
            {
                let mut l = last.lock().expect("config-watch debounce lock");
                if l.elapsed() < Duration::from_millis(200) {
                    return;
                }
                *l = Instant::now();
            }
            // Let the writer finish flushing before we read.
            std::thread::sleep(Duration::from_millis(60));
            let config = Config::load_no_write();
            tracing::info!("config.json changed → reloading");
            if tx.send(Hotkey::ConfigReloaded(Box::new(config))).is_ok() {
                ctx.request_repaint();
            }
        };
        let mut watcher = match notify::recommended_watcher(handler) {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!(error = %e, "config watcher disabled");
                return;
            }
        };
        if let Err(e) = watcher.watch(&dir, RecursiveMode::NonRecursive) {
            tracing::warn!(error = %e, dir = %dir.display(), "config watch failed");
            return;
        }
        tracing::info!(dir = %dir.display(), "watching config.json for changes");
        // Keep the watcher alive for the process lifetime.
        loop {
            std::thread::park();
        }
    });
}

/// Poll the clipboard until it both *changed* and *parses as a POE2 item*, or
/// the timeout hits (PRD §4.2). Gating on "is an item" — not merely "changed" —
/// avoids grabbing the transient/stale clipboard value the X11↔Wayland bridge
/// can briefly expose before POE2 finishes writing (which made the first Ctrl+C
/// fail while the second worked).
fn wait_for_item(last_seen: &Option<String>) -> Option<String> {
    let deadline = Instant::now() + CLIPBOARD_TIMEOUT;
    let last = last_seen.as_deref().map(normalize_item_text);
    // If the clipboard only ever holds the SAME item as before, that's a
    // *re-view* of the loaded item — return it so the popup re-shows (the UI
    // de-dups the API call via a short cache). We still poll the full window
    // first: a genuine switch to a different item must win, and POE2's write of
    // the new item can lag the keypress. Returning `None` only when the
    // clipboard never holds a parseable item at all (a truly missed copy).
    let mut same: Option<String> = None;
    loop {
        if let Ok(Some(text)) = platform_linux::read_clipboard_text() {
            match clip_step(&text, last.as_deref()) {
                ClipStep::Different => return Some(text), // a new item appeared → use it now
                ClipStep::Same => same = Some(text), // same as loaded; keep watching for a switch
                ClipStep::NotItem => {}
            }
        }
        if Instant::now() >= deadline {
            return same;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// What a single clipboard read means relative to the last-seen item.
#[derive(Debug, PartialEq, Eq)]
enum ClipStep {
    /// A different parseable item than last seen — show it now.
    Different,
    /// The same item as last seen — a re-view (show it; the UI caches the query).
    Same,
    /// Not a POE2 item (ignore this read).
    NotItem,
}

/// Classify a clipboard read against the whitespace-normalised last-seen item.
fn clip_step(text: &str, last_normalized: Option<&str>) -> ClipStep {
    if parser::parse_item(text).is_err() {
        return ClipStep::NotItem;
    }
    if last_normalized == Some(normalize_item_text(text).as_str()) {
        ClipStep::Same
    } else {
        ClipStep::Different
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
pub fn build_app(
    hotkey_rx: Receiver<Hotkey>,
    tray: Option<platform_linux::TrayHandle>,
) -> anyhow::Result<QuickModeApp> {
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
    let (stats, items, currencies) = rt
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
    let client = Arc::new(TradeClient::new(
        transport,
        client_config,
        stats,
        items,
        currencies,
    ));

    Ok(QuickModeApp::new(
        rt, client, config, leagues, hotkey_rx, tray,
    ))
}

#[cfg(test)]
mod tests {
    use super::{clip_step, ClipStep};

    const RING: &str = "Item Class: Rings\nRarity: Rare\nHonour Spiral\nTopaz Ring\n--------\n+30% to Lightning Resistance";
    const RUNE: &str = "Item Class: Augment\nRarity: Currency\nFarrul's Rune of the Chase\n--------\nStack Size: 1/10\nRune";

    #[test]
    fn reviewing_the_same_item_is_not_ignored() {
        let last = super::normalize_item_text(RING);
        // Re-copying the SAME item must classify as Same (so the popup re-shows),
        // NOT be dropped — this was the "re-view does nothing" bug.
        assert_eq!(clip_step(RING, Some(&last)), ClipStep::Same);
    }

    #[test]
    fn a_different_item_is_new() {
        let last = super::normalize_item_text(RING);
        assert_eq!(clip_step(RUNE, Some(&last)), ClipStep::Different);
        // With nothing seen yet, any item is new.
        assert_eq!(clip_step(RING, None), ClipStep::Different);
    }

    #[test]
    fn non_item_clipboard_is_ignored() {
        assert_eq!(clip_step("https://example.com/not-an-item", None), ClipStep::NotItem);
    }
}
