//! The price-check UI: the egui view + app logic, windowing agnostic so the
//! `overlay` crate can drive it on a layer surface.
//!
//! Flow: Ctrl+C on an item → parse → search/fetch via `trade-api` on a tokio
//! task → show the median price and cheapest listings, each with buttons that
//! copy the chat command to the clipboard (we can't type into POE2 on Wayland).
//! The popup pins open with a filter panel that re-queries live.
//!
//! Split across modules: `model` (shared types + pure helpers), `query`
//! (background tasks), `watchers` (OS-thread hotkey/config watchers) and
//! `view` (egui rendering). [`QuickModeApp`] and its lifecycle live here.

pub mod config;

mod model;
mod query;
mod view;
mod watchers;

pub use watchers::{spawn_config_watcher, spawn_hotkey_watcher};

use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use config::Config;
use parser::Item;
use trade_api::{
    fetch_definitions, fetch_leagues, ClientConfig, League, PriceEstimate, ReqwestTransport,
    TradeClient,
};

use model::{
    item_hash, EquipmentRow, ExchangePhase, MinFilter, MiscToggle, Msg, Phase, PriceFilterState,
    PriceMode, SessionCheck, StatFilterRow, View, MISC_OPTIONS,
};

const BASE_URL: &str = "https://www.pathofexile.com";
const USER_AGENT: &str = "poe2ddd/0.1 (+phase3 ui)";
/// Fetch a sample of this many so the median is meaningful; show the cheapest N.
pub(crate) const SAMPLE: usize = 10;
pub(crate) const SHOWN: usize = 10;
/// How long to wait for POE2 to write the clipboard after Ctrl+C. Generous (1s)
/// because POE2's write latency is variable and a short window drops presses.
pub(crate) const CLIPBOARD_TIMEOUT: Duration = Duration::from_secs(1);
/// How long a price-check result stays fresh: re-viewing the same item within
/// this window re-shows cached results without re-hitting the API.
const CACHE_TTL: Duration = Duration::from_mins(2);
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(8);
/// Quiet period after the last filter edit before a live re-query fires. Long
/// so toggling several filters fires one request, not a burst. "Apply now"
/// bypasses it.
pub(crate) const FILTER_DEBOUNCE: Duration = Duration::from_secs(4);

/// Quiet period after the last POESESSID edit before the live validation fires,
/// so a paste (which arrives as one change) is checked once typing settles
/// without firing a request mid-keystroke.
pub(crate) const POESESSID_DEBOUNCE: Duration = Duration::from_millis(700);

/// Popup width — wide enough for the filter panel.
pub const POPUP_WIDTH: u32 = 600;

pub type Client = TradeClient<ReqwestTransport>;

/// What the global-hotkey watcher (or the tray / config watcher) observed.
/// Everything that needs to reach the UI thread funnels through this one
/// channel, which [`pump`](QuickModeApp::pump) drains every frame.
pub enum Hotkey {
    /// A price-check combo was pressed and POE2 is focused — pop the popup into
    /// a "reading…" state before the clipboard poll runs, for instant feedback.
    CopyStarted,
    /// A new item landed on the clipboard (Ctrl+C / Ctrl+Alt+C — both open the
    /// pinned filter popup).
    Item { text: String },
    /// The clipboard never produced an item before the timeout — usually POE2
    /// skipping the copy on a static cursor.
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
    /// `config.json` changed on disk — apply the live-reloadable fields.
    /// Boxed: it's the largest variant and rare.
    ConfigReloaded(Box<Config>),
}

pub struct QuickModeApp {
    // Held to keep the runtime alive for the app's lifetime.
    rt: tokio::runtime::Runtime,
    client: Arc<Client>,
    /// Persisted settings; rewritten when the league selector changes.
    config: Config,
    /// Leagues offered in the top-right selector.
    leagues: Vec<League>,
    item_text: String,
    view: View,
    /// The item the current/last search was built from (for the header).
    item: Option<Item>,
    /// Icon URL of the priced item, learned from the search results.
    icon_url: Option<String>,
    /// Per-stat affix filter rows (rebuilt on a fresh check).
    filters: Vec<StatFilterRow>,
    /// Explicit mods with no GGG trade filter (e.g. "Map contains N additional
    /// Rare Chests" — GGG has no searchable plural variant). Shown read-only so
    /// they don't silently vanish from the detailed panel.
    unfilterable_mods: Vec<String>,
    /// Equipment-property filter rows (armour/evasion/ES/… defences).
    equipment: Vec<EquipmentRow>,
    /// Price-range filter.
    price_filter: PriceFilterState,
    /// Item-quality filter (default-on for bonus-quality bases).
    quality_filter: MinFilter,
    /// Item-level filter (default-on only for Normal bases).
    ilvl_filter: MinFilter,
    /// Selected `type_filters.rarity` option (`normal`/`magic`/`rare`/`unique`);
    /// empty = item's own rarity. Reset to the item's rarity on each new check.
    rarity_filter: String,
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
    /// Tray handle for pushing state to the tooltip. `None` if the tray failed
    /// to start (no SNI host).
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
    /// Live POESESSID validation state shown beside the field in Settings.
    session_status: SessionCheck,
    /// When the POESESSID field last changed, for debouncing the live check.
    /// Cleared once the check fires; `None` when nothing is pending.
    session_check_at: Option<Instant>,
    /// Whether the loaded item prices via the per-item search or the bulk
    /// currency exchange.
    mode: PriceMode,
    /// Background state of the bulk-exchange check (used in [`PriceMode::Exchange`]).
    exchange_phase: ExchangePhase,
    /// The `data/static` exchange id of the loaded stackable (the `want`).
    exchange_want_id: String,
    /// Currency the exchange prices are shown in (the `have`); default Exalted.
    pay_currency: String,
}

impl QuickModeApp {
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
            unfilterable_mods: Vec::new(),
            equipment: Vec::new(),
            price_filter: PriceFilterState::default(),
            quality_filter: MinFilter::default(),
            ilvl_filter: MinFilter::default(),
            rarity_filter: String::new(),
            misc: MISC_OPTIONS
                .iter()
                .map(|(key, label)| MiscToggle {
                    key,
                    label,
                    on: false,
                })
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
            session_status: SessionCheck::Idle,
            session_check_at: None,
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

    /// Configured popup position mode (`center` / `fixed`). The overlay reads
    /// this each frame to place the popup surface.
    pub fn position_mode(&self) -> &str {
        &self.config.position_mode
    }

    /// Configured fixed-mode top-left position (output-logical pixels).
    pub fn fixed_pos(&self) -> (i32, i32) {
        (self.config.fixed_x, self.config.fixed_y)
    }

    /// Persist a dragged popup position: switch to **fixed** mode at `(x, y)`
    /// and save, so wherever the user drops the popup is where it stays.
    /// No-op if nothing changed (avoids needless writes).
    pub fn set_fixed_position(&mut self, x: i32, y: i32) {
        if self.config.position_mode == "fixed"
            && self.config.fixed_x == x
            && self.config.fixed_y == y
        {
            return;
        }
        self.config.position_mode = "fixed".to_string();
        self.config.fixed_x = x;
        self.config.fixed_y = y;
        if let Err(e) = self.config.save() {
            tracing::warn!(error = %e, "could not save dragged popup position");
        }
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

    /// Drain the hotkey + price-check channels. Side-effect only (no drawing),
    /// so the overlay can call it every frame — even while hidden — to notice a
    /// fresh Ctrl+C and decide to pop.
    pub fn pump(&mut self, ctx: &egui::Context) {
        // Ctrl+C in game → the watcher pushes the copied item here; price-check
        // it and flag a pop. A missed copy gets a hint.
        while let Ok(event) = self.hotkey_rx.try_recv() {
            match event {
                Hotkey::CopyStarted => {
                    // Instant feedback: show a "reading…" spinner the moment
                    // Ctrl+C is detected (item/results follow).
                    self.hint = None;
                    self.awaiting_copy = true;
                    self.pop_requested = true;
                }
                Hotkey::Item { text } => {
                    self.hint = None;
                    self.awaiting_copy = false;
                    // Always re-show the popup, even for the same item — only the
                    // API call is de-duped, not the pop.
                    self.pop_requested = true;
                    let hash = item_hash(&text);
                    if self.last_query_hash == Some(hash) {
                        // Same item as loaded → keep cached results and filter
                        // state; refresh from the API only if the cache is stale.
                        let stale = self.last_query_at.is_none_or(|t| t.elapsed() >= CACHE_TTL);
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
                    // F5 chat macro (e.g. /hideout), injected via uinput into POE2.
                    view::run_chat_macro(self.config.f5_command.clone());
                }
                Hotkey::Macro2 => {
                    // F2 chat macro (e.g. /exit).
                    view::run_chat_macro(self.config.macro2_command.clone());
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
                Msg::Teleport(result) => {
                    if let Err(e) = result {
                        tracing::warn!(error = %e, "hideout teleport failed");
                        self.copy_status = Some(format!("teleport failed: {e}"));
                    }
                }
                Msg::SessionChecked(status) => {
                    self.session_status = match status {
                        trade_api::SessionStatus::Valid { account } => SessionCheck::Valid(account),
                        trade_api::SessionStatus::Invalid => SessionCheck::Invalid,
                        trade_api::SessionStatus::Unknown(e) => {
                            tracing::debug!(error = %e, "could not verify POESESSID");
                            SessionCheck::Unknown
                        }
                    };
                }
            }
        }

        self.update_tray();
    }

    /// Push the current app state to the tray tooltip. Idempotent — the handle
    /// skips the D-Bus update when the state is unchanged.
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

    /// Apply the live-reloadable fields of a config reloaded from disk. League
    /// switches the client + re-prices; filter defaults and placement take
    /// effect on the next item. Hotkeys, realm, and the POE2-focus gate are read
    /// once at startup, so those need a restart — flagged, not silently dropped.
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
        if new.poesessid != self.config.poesessid {
            self.client.set_poesessid(new.poesessid.clone());
        }
        self.config = new;
        if restart_needed {
            self.settings_note =
                Some("Saved. Hotkeys / realm / focus-gate apply after a restart.".to_string());
        }
        tracing::info!(league_changed, restart_needed, "applied reloaded config");
        // A league change re-prices the loaded item immediately.
        if league_changed {
            self.rerun_query(ctx);
        }
    }
}

/// Install the HTTP/image loaders egui_extras needs for item icons. Call once
/// per `egui::Context`.
pub fn install_loaders(ctx: &egui::Context) {
    egui_extras::install_image_loaders(ctx);
}

pub fn configure_style(ctx: &egui::Context) {
    // Add the Phosphor icon font so the button glyphs render (the default egui
    // font has no emoji). Phosphor is an extra family the `ph::*` constants use.
    let mut fonts = egui::FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    ctx.set_fonts(fonts);

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

/// Is this league id a Hardcore variant? GGG lists them as `Hardcore` and
/// `HC <league>` (and, historically, `Hardcore <league>`).
fn is_hardcore_league(id: &str) -> bool {
    id == "Hardcore" || id.starts_with("HC ") || id.starts_with("Hardcore ")
}

/// The auto-default league: the first non-HC entry GGG returns. During a
/// challenge league that's the softcore challenge league; between leagues the
/// list is just `[Standard, Hardcore]`, so it resolves to `Standard`. Returns
/// `None` only when the list is empty (the fetch failed).
fn resolve_auto_league(leagues: &[League]) -> Option<String> {
    leagues
        .iter()
        .find(|l| !is_hardcore_league(&l.id))
        .map(|l| l.id.clone())
}

/// Build a ready-to-render [`QuickModeApp`]: load settings, spin up a tokio
/// runtime, fetch the live trade definitions + leagues, and construct the API
/// client for the layer-shell overlay.
///
/// The league is auto-resolved from the live GGG list (the current non-HC
/// league, or Standard between leagues) unless the user has pinned one via the
/// selector. `POE_LEAGUE` / `POE_REALM`, if set, override for that run (handy
/// for testing) but are not persisted.
pub fn build_app(
    hotkey_rx: Receiver<Hotkey>,
    tray: Option<platform_linux::TrayHandle>,
) -> anyhow::Result<QuickModeApp> {
    let mut config = Config::load();
    // A pinned league or a POE_LEAGUE override is taken as-is; otherwise it's
    // auto-resolved from GGG below.
    let mut league_explicit = config.league_pinned;
    if let Ok(league) = std::env::var("POE_LEAGUE") {
        if !league.is_empty() {
            config.league = league;
            league_explicit = true;
        }
    }
    if let Ok(realm) = std::env::var("POE_REALM") {
        config.realm = Some(realm);
    }

    let rt = tokio::runtime::Runtime::new()?;
    let transport = ReqwestTransport::new(USER_AGENT)?;
    tracing::info!("fetching trade definitions…");
    let (stats, items, currencies) = rt
        .block_on(fetch_definitions(&transport, BASE_URL))
        .map_err(|e| anyhow::anyhow!("loading definitions: {e}"))?;
    // A leagues failure is only fatal when we need the list to auto-resolve
    // (handled below); a pinned league still starts with a static selector label.
    let leagues = rt
        .block_on(fetch_leagues(&transport, BASE_URL))
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "could not fetch leagues; selector disabled");
            Vec::new()
        });

    // Respect an explicit pick; otherwise take the latest non-HC league GGG
    // returns (the current challenge league, or Standard between leagues).
    if !league_explicit {
        match resolve_auto_league(&leagues) {
            Some(def) => {
                config.league = def;
                // Persist so disk matches memory: else a hot-reload would read
                // the stale on-disk value and wipe the resolved one mid-session.
                // Re-derived on every unpinned startup; doubles as offline fallback.
                if let Err(e) = config.save() {
                    tracing::warn!(error = %e, "could not persist resolved league");
                }
            }
            None if config.league.is_empty() => anyhow::bail!(
                "could not determine the current trade league: the trade site \
                 returned no leagues and none is saved. Retry when online, or \
                 set POE_LEAGUE."
            ),
            None => {} // keep the last saved league as a soft fallback
        }
    }
    tracing::info!(
        path = %config::Config::path().display(),
        league = %config.league,
        pinned = league_explicit,
        "resolved league"
    );

    let mut client_config = ClientConfig::new(&config.league);
    client_config.realm.clone_from(&config.realm);
    let client = Arc::new(TradeClient::new(
        transport,
        client_config,
        stats,
        items,
        currencies,
    ));
    // Authenticate (for Instant Buyout teleport tokens) if a session is saved.
    client.set_poesessid(config.poesessid.clone());

    Ok(QuickModeApp::new(
        rt, client, config, leagues, hotkey_rx, tray,
    ))
}
