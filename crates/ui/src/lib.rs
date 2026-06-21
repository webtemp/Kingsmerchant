//! The price-check UI: the egui view + app logic, windowing agnostic.

pub mod config;

mod model;
mod query;
mod view;
mod watchers;

pub use watchers::{spawn_config_watcher, spawn_hotkey_watcher, HotkeyHandle};

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
    item_hash, EquipmentRow, ExchangePhase, FilterTab, MinFilter, MiscToggle, Msg, Phase,
    PriceFilterState, PriceMode, ScoutPhase, SessionCheck, StatFilterRow, View, MISC_OPTIONS,
};

const BASE_URL: &str = "https://www.pathofexile.com";
// GGG's trade API wants a descriptive User-Agent with a contact/repo URL; a bare
// token reads as a bot and draws Cloudflare challenges sooner.
const USER_AGENT: &str = concat!(
    "kingsmerchant/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/webtemp/Kingsmerchant)"
);
pub(crate) const SAMPLE: usize = 10;
pub(crate) const SHOWN: usize = 10;
/// How long to wait for POE2 to write the clipboard after Ctrl+C.
pub(crate) const CLIPBOARD_TIMEOUT: Duration = Duration::from_secs(1);
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(8);
/// Quiet period after the last filter edit before a live re-query fires.
pub(crate) const FILTER_DEBOUNCE: Duration = Duration::from_secs(4);

/// Quiet period after the last POESESSID edit before the live validation fires.
pub(crate) const POESESSID_DEBOUNCE: Duration = Duration::from_millis(700);

pub const POPUP_WIDTH: u32 = 600;

pub type Client = TradeClient<ReqwestTransport>;

/// What the watchers observed; drained by [`pump`](QuickModeApp::pump) each frame.
pub enum Hotkey {
    /// Price-check combo pressed and POE2 focused — show a "reading…" state.
    CopyStarted,
    Item {
        text: String,
    },
    /// No item before the timeout — usually POE2 skipping the copy on a static cursor.
    Missed,
    Close,
    Mods {
        ctrl: bool,
        alt: bool,
    },
    Macro,
    Macro2,
    OpenSettings,
    Quit,
    /// Boxed: largest variant and rare.
    ConfigReloaded(Box<Config>),
}

/// Which rebindable hotkey the settings panel is currently recording.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum HotkeySlot {
    Quick,
    Macro,
    Macro2,
    Close,
    Settings,
}

pub struct QuickModeApp {
    rt: tokio::runtime::Runtime,
    client: Arc<Client>,
    config: Config,
    /// Render-time palette parsed from `config.theme`; rebuilt on config change.
    theme: view::theme::Theme,
    leagues: Vec<League>,
    item_text: String,
    view: View,
    item: Option<Item>,
    icon_url: Option<String>,
    filters: Vec<StatFilterRow>,
    /// Explicit mods with no GGG trade filter, shown read-only.
    unfilterable_mods: Vec<String>,
    equipment: Vec<EquipmentRow>,
    price_filter: PriceFilterState,
    quality_filter: MinFilter,
    ilvl_filter: MinFilter,
    waystone_filter: MinFilter,
    /// Selected `type_filters.rarity`; empty = item's own rarity.
    rarity_filter: String,
    misc: Vec<MiscToggle>,
    filter_tab: FilterTab,
    /// Tallest filter-tab body seen; shorter tab pads to it so height stays constant.
    filter_body_h: f32,
    resistance_mode: trade_api::ResistanceMode,
    filter_dirty: bool,
    filter_changed_at: Instant,
    estimate: Option<PriceEstimate>,
    estimate_loading: bool,
    phase: Phase,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    hotkey_rx: Receiver<Hotkey>,
    copy_status: Option<String>,
    hint: Option<String>,
    pop_requested: bool,
    close_requested: bool,
    ctrl_held: bool,
    alt_held: bool,
    /// Hash of the last searched item, to de-dup repeated Ctrl+C.
    last_query_hash: Option<u64>,
    /// When the loaded item was last queried (cache freshness).
    last_query_at: Option<Instant>,
    /// Polling the clipboard after a detected Ctrl+C; drives the "reading…" spinner.
    awaiting_copy: bool,
    /// `None` if the tray failed to start (no SNI host).
    tray: Option<platform::TrayHandle>,
    settings_requested: bool,
    settings_close_requested: bool,
    quit_requested: bool,
    settings_note: Option<String>,
    /// Which hotkey row is capturing a keypress; `None` = not recording.
    recording_hotkey: Option<HotkeySlot>,
    hotkeys: HotkeyHandle,
    session_status: SessionCheck,
    /// When the POESESSID field last changed, for debouncing; `None` when nothing pending.
    session_check_at: Option<Instant>,
    /// When the session was last validated against the server; drives the
    /// automatic startup + periodic re-check so an expired cookie is noticed.
    last_session_check: Option<Instant>,
    /// Whether the loaded item prices via per-item search or the bulk exchange.
    mode: PriceMode,
    scout_phase: ScoutPhase,
    exchange_phase: ExchangePhase,
    /// The `data/static` exchange id of the loaded stackable (the `want`).
    exchange_want_id: String,
    /// Currency exchange prices are shown in (the `have`); default Exalted.
    pay_currency: String,
}

impl QuickModeApp {
    /// Assemble the app from its already-built dependencies. Most callers want [`build_app`].
    pub fn new(
        rt: tokio::runtime::Runtime,
        client: Arc<Client>,
        config: Config,
        leagues: Vec<League>,
        hotkey_rx: Receiver<Hotkey>,
        tray: Option<platform::TrayHandle>,
        hotkeys: HotkeyHandle,
    ) -> Self {
        let (tx, rx) = channel();
        let theme = view::theme::Theme::from_config(&config.theme);
        QuickModeApp {
            rt,
            client,
            config,
            theme,
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
            waystone_filter: MinFilter::default(),
            rarity_filter: String::new(),
            misc: MISC_OPTIONS
                .iter()
                .map(|(key, label)| MiscToggle {
                    key,
                    label,
                    state: trade_api::MiscState::default(),
                })
                .collect(),
            filter_tab: FilterTab::default(),
            filter_body_h: 0.0,
            resistance_mode: trade_api::ResistanceMode::default(),
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
            recording_hotkey: None,
            hotkeys,
            session_status: SessionCheck::Idle,
            session_check_at: None,
            last_session_check: None,
            mode: PriceMode::Item,
            scout_phase: ScoutPhase::Idle,
            exchange_phase: ExchangePhase::Idle,
            exchange_want_id: String::new(),
            pay_currency: "exalted".to_string(),
        }
    }

    pub fn ctrl_held(&self) -> bool {
        self.ctrl_held
    }

    pub fn surface_width(&self) -> u32 {
        POPUP_WIDTH
    }

    pub fn alt_held(&self) -> bool {
        self.alt_held
    }

    pub fn take_pop_request(&mut self) -> bool {
        std::mem::take(&mut self.pop_requested)
    }

    pub fn take_close_request(&mut self) -> bool {
        std::mem::take(&mut self.close_requested)
    }

    pub fn take_settings_request(&mut self) -> bool {
        std::mem::take(&mut self.settings_requested)
    }

    pub fn take_settings_close_request(&mut self) -> bool {
        std::mem::take(&mut self.settings_close_requested)
    }

    pub fn take_quit_request(&mut self) -> bool {
        std::mem::take(&mut self.quit_requested)
    }

    /// Whether a hotkey row in Settings is currently capturing a keypress.
    pub fn is_recording_hotkey(&self) -> bool {
        self.recording_hotkey.is_some()
    }

    pub fn cancel_hotkey_recording(&mut self) {
        self.recording_hotkey = None;
    }

    /// Store a recorded hotkey into the recording row, persist, and stop. Pushed live.
    pub fn commit_hotkey(&mut self, binding: String) {
        let Some(slot) = self.recording_hotkey.take() else {
            return;
        };
        match slot {
            HotkeySlot::Quick => self.config.hotkey_quick = binding,
            HotkeySlot::Macro => self.config.hotkey_macro = binding,
            HotkeySlot::Macro2 => self.config.hotkey_macro2 = binding,
            HotkeySlot::Close => self.config.hotkey_close = binding,
            HotkeySlot::Settings => self.config.hotkey_settings = binding,
        }
        // Apply to the running watcher first so the rebind is live even if the write fails.
        self.hotkeys.apply_config(&self.config);
        if let Err(e) = self.config.save() {
            tracing::warn!(error = %e, "could not save hotkey");
            self.settings_note = Some(format!("Could not save: {e}"));
        } else {
            self.settings_note = None;
        }
    }

    pub fn position_mode(&self) -> &str {
        &self.config.position_mode
    }

    pub fn fixed_pos(&self) -> (i32, i32) {
        (self.config.fixed_x, self.config.fixed_y)
    }

    pub fn perf_metrics_enabled(&self) -> bool {
        self.config.perf_metrics
    }

    pub fn overlay_fill(&self) -> egui::Color32 {
        self.theme.overlay_fill
    }

    pub fn overlay_stroke(&self) -> egui::Color32 {
        self.theme.overlay_stroke
    }

    /// Persist a dragged popup position: switch to fixed mode at `(x, y)` and save.
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
        match platform::read_clipboard_text() {
            Ok(Some(text)) => {
                self.item_text = text;
                self.view = View::Item;
            }
            Ok(None) => self.phase = Phase::Failed("Clipboard is empty.".to_string()),
            Err(e) => self.phase = Phase::Failed(format!("Clipboard read failed: {e}")),
        }
    }

    /// Validate the POESESSID automatically: once on first use, then periodically,
    /// so an expired cookie flips to `Invalid` on its own — the user never has to
    /// open Settings to re-validate. [`spawn_session_check`](Self::spawn_session_check)
    /// stamps `last_session_check`.
    fn maybe_check_session(&mut self, ctx: &egui::Context) {
        const REVALIDATE: Duration = Duration::from_mins(10);
        // No session → nothing to validate; the anonymous results banner covers it.
        if self.config.poesessid.is_none() {
            return;
        }
        // Don't add traffic while we're in a rate-limit / Cloudflare cooldown.
        if self.client.retry_in().is_some() {
            return;
        }
        // Don't stack checks while one is already in flight.
        if matches!(self.session_status, SessionCheck::Checking) {
            return;
        }
        if self
            .last_session_check
            .is_none_or(|t| t.elapsed() >= REVALIDATE)
        {
            self.spawn_session_check(ctx);
        }
    }

    /// Whether price-check results are anonymous (no usable session), so the
    /// teleport features are unavailable and a banner should warn the user.
    pub(crate) fn session_anonymous(&self) -> bool {
        self.config.poesessid.is_none() || matches!(self.session_status, SessionCheck::Invalid)
    }

    /// Drain the hotkey + price-check channels. Side-effect only; safe to call every frame.
    pub fn pump(&mut self, ctx: &egui::Context) {
        self.maybe_check_session(ctx);
        while let Ok(event) = self.hotkey_rx.try_recv() {
            match event {
                Hotkey::CopyStarted => {
                    self.hint = None;
                    self.awaiting_copy = true;
                    self.pop_requested = true;
                }
                Hotkey::Item { text } => {
                    self.hint = None;
                    self.awaiting_copy = false;
                    // Always re-show the popup; only the API call is de-duped.
                    self.pop_requested = true;
                    let hash = item_hash(&text);
                    if self.last_query_hash == Some(hash) {
                        // Same item → keep cached results; refresh only if stale (TTL 0 = always stale).
                        let ttl = Duration::from_secs(u64::from(self.config.cache_ttl_secs));
                        let stale = self.last_query_at.is_none_or(|t| t.elapsed() >= ttl);
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
                    view::run_chat_macro(self.config.f5_command.clone());
                }
                Hotkey::Macro2 => {
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
                Msg::Scout(result) => {
                    self.scout_phase = match *result {
                        Ok(Some(price)) => {
                            if price.icon_url.is_some() {
                                self.icon_url.clone_from(&price.icon_url);
                            }
                            ScoutPhase::Done(price)
                        }
                        // No poe2scout data → fall back to the official exchange.
                        Ok(None) => {
                            tracing::info!("poe2scout had no data; falling back to exchange");
                            self.spawn_exchange_query(ctx);
                            ScoutPhase::NotFound
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "poe2scout price failed; falling back to exchange");
                            self.spawn_exchange_query(ctx);
                            ScoutPhase::Failed(e)
                        }
                    };
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

    /// Push the current app state to the tray tooltip (idempotent).
    fn update_tray(&mut self) {
        let Some(tray) = self.tray.as_mut() else {
            return;
        };
        let state = if let Some(wait) = self.client.retry_in() {
            let secs = (wait.as_millis() as u64).div_ceil(1000);
            platform::TrayState::RateLimited(secs)
        } else if let Phase::Failed(e) = &self.phase {
            let short = e.lines().next().unwrap_or(e).to_string();
            platform::TrayState::Error(short)
        } else {
            platform::TrayState::Listening
        };
        tray.set_state(state);
    }

    /// Apply the live-reloadable fields of a config reloaded from disk (realm needs a restart).
    fn apply_reloaded_config(&mut self, new: Config, ctx: &egui::Context) {
        let league_changed = new.league != self.config.league;
        let restart_needed = new.realm != self.config.realm;
        if league_changed {
            self.client.set_league(new.league.clone());
        }
        if new.poesessid != self.config.poesessid {
            self.client.set_poesessid(new.poesessid.clone());
        }
        self.config = new;
        self.theme = view::theme::Theme::from_config(&self.config.theme);
        self.hotkeys.apply_config(&self.config);
        if restart_needed {
            self.settings_note =
                Some("Saved. The realm change applies after a restart.".to_string());
        }
        tracing::info!(league_changed, restart_needed, "applied reloaded config");
        if league_changed {
            self.rerun_query(ctx);
        }
    }
}

/// Workspace crates the `auto`/dev directive raises to `debug` (deps stay at `info`).
const APP_CRATES: &[&str] = &[
    "kingsmerchant",
    "overlay",
    "ui",
    "platform_linux",
    "trade_api",
    "parser",
];

/// Resolve a [`Config::log_level`](config::Config) into a `tracing` `EnvFilter` directive.
pub fn resolve_log_filter(level: &str) -> String {
    match level.trim().to_ascii_lowercase().as_str() {
        "" | "auto" => {
            if cfg!(debug_assertions) {
                let ours = APP_CRATES
                    .iter()
                    .map(|c| format!("{c}=debug"))
                    .collect::<Vec<_>>()
                    .join(",");
                format!("info,{ours}")
            } else {
                "error".to_string()
            }
        }
        explicit => explicit.to_string(),
    }
}

/// Install the HTTP/image loaders egui_extras needs for item icons.
pub fn install_loaders(ctx: &egui::Context) {
    egui_extras::install_image_loaders(ctx);
}

/// Apply the app's egui style: the Phosphor icon font plus shared spacing/rounding.
pub fn configure_style(ctx: &egui::Context) {
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

/// Is this league id a Hardcore variant (`Hardcore`, `HC <league>`, `Hardcore <league>`)?
fn is_hardcore_league(id: &str) -> bool {
    id == "Hardcore" || id.starts_with("HC ") || id.starts_with("Hardcore ")
}

/// The auto-default league: the first non-HC entry GGG returns; `None` if the list is empty.
fn resolve_auto_league(leagues: &[League]) -> Option<String> {
    leagues
        .iter()
        .find(|l| !is_hardcore_league(&l.id))
        .map(|l| l.id.clone())
}

/// Build a ready-to-render [`QuickModeApp`]: load settings, runtime, definitions, leagues, client.
///
/// League auto-resolves unless pinned. `POE_LEAGUE` / `POE_REALM` override for the run (not persisted).
pub fn build_app(
    hotkey_rx: Receiver<Hotkey>,
    tray: Option<platform::TrayHandle>,
    hotkeys: HotkeyHandle,
) -> anyhow::Result<QuickModeApp> {
    let mut config = Config::load();
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
    // A leagues failure is only fatal when we need the list to auto-resolve (below).
    let leagues = rt
        .block_on(fetch_leagues(&transport, BASE_URL))
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "could not fetch leagues; selector disabled");
            Vec::new()
        });

    // Respect an explicit pick; otherwise take the latest non-HC league.
    if !league_explicit {
        match resolve_auto_league(&leagues) {
            Some(def) => {
                config.league = def;
                // Persist so disk matches memory (else a hot-reload reads the stale value).
                if let Err(e) = config.save() {
                    tracing::warn!(error = %e, "could not persist resolved league");
                }
            }
            None if config.league.is_empty() => anyhow::bail!(
                "could not determine the current trade league: the trade site \
                 returned no leagues and none is saved. Retry when online, or \
                 set POE_LEAGUE."
            ),
            None => {}
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
    client.set_poesessid(config.poesessid.clone());

    Ok(QuickModeApp::new(
        rt, client, config, leagues, hotkey_rx, tray, hotkeys,
    ))
}

#[cfg(test)]
mod tests {
    use super::{is_hardcore_league, resolve_auto_league, resolve_log_filter};
    use trade_api::League;

    fn league(id: &str) -> League {
        League {
            id: id.to_string(),
            text: id.to_string(),
        }
    }

    #[test]
    fn hardcore_leagues_are_detected_by_id_shape() {
        assert!(is_hardcore_league("Hardcore"));
        assert!(is_hardcore_league("HC Runes of Aldur"));
        assert!(is_hardcore_league("Hardcore Runes of Aldur"));
        assert!(!is_hardcore_league("Standard"));
        assert!(!is_hardcore_league("Runes of Aldur"));
        assert!(!is_hardcore_league("HCSSF"));
    }

    #[test]
    fn auto_league_picks_first_non_hardcore_entry() {
        let challenge = [
            league("Runes of Aldur"),
            league("HC Runes of Aldur"),
            league("Standard"),
            league("Hardcore"),
        ];
        assert_eq!(
            resolve_auto_league(&challenge).as_deref(),
            Some("Runes of Aldur")
        );
        let between = [league("Standard"), league("Hardcore")];
        assert_eq!(resolve_auto_league(&between).as_deref(), Some("Standard"));
        assert_eq!(resolve_auto_league(&[]), None);
    }

    #[test]
    fn explicit_levels_pass_through_lowercased() {
        assert_eq!(resolve_log_filter("error"), "error");
        assert_eq!(resolve_log_filter("WARN"), "warn");
        assert_eq!(resolve_log_filter("  Trace "), "trace");
        assert_eq!(resolve_log_filter("off"), "off");
    }

    #[test]
    fn auto_is_build_profile_dependent() {
        let dev = resolve_log_filter("auto");
        assert_eq!(resolve_log_filter(""), dev);
        assert!(
            dev.starts_with("info"),
            "dev directive caps deps at info: {dev}"
        );
        assert!(
            dev.contains("ui=debug"),
            "dev directive raises our crates: {dev}"
        );
        assert!(!dev.contains("error"));
    }
}
