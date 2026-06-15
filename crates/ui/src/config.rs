//! Persistent settings (PRD §4.8). Read on startup, rewritten when the user
//! changes a setting (currently just the league, via the popup selector) so the
//! choice survives restarts — no env vars needed.
//!
//! Lives at `$XDG_CONFIG_HOME/poe2ddd/config.json` (i.e.
//! `~/.config/poe2ddd/config.json`). Hand-editable JSON; a missing or malformed
//! file falls back to defaults rather than erroring. Hot-reload (file watcher)
//! is a later phase — for now it's read once at launch and written on change.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// User settings persisted across runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Trade league id (e.g. `Runes of Aldur`).
    pub league: String,
    /// Realm (`pc` / `sony` / `xbox`); `None` = pc.
    pub realm: Option<String>,
    /// Start implicit-mod filters unticked in the detailed panel (they're rarely
    /// the point and would over-constrain a search). Hand-editable.
    pub implicits_off_by_default: bool,
    /// Stat filters whose label contains any of these (case-insensitive)
    /// substrings start unticked — low-value "noise" mods. Hand-editable list.
    pub filters_off_by_default: Vec<String>,
    /// Filter mins are seeded at this percentage of the item's rolled value
    /// (100 = exact roll; 90 = 10% below, a looser default search). 1..=100.
    pub filter_min_percent: u32,
    /// Chat command typed into POE2 when the macro hotkey is pressed (via a
    /// uinput virtual keyboard: opens chat, types this, sends). `null` disables
    /// it. Injection steps past the clipboard-only design (PRD App. B) — opt-in.
    pub f5_command: Option<String>,
    /// Second chat macro (default `/exit`, on F2) — same mechanism as
    /// [`f5_command`](Self::f5_command). `null` disables it.
    pub macro2_command: Option<String>,
    /// Rebindable hotkeys (PRD §4.8). Strings like `"Ctrl+C"`, `"Ctrl+Alt+C"`,
    /// `"F5"`, `"Escape"` — modifiers `Ctrl`/`Alt`/`Shift` + one key.
    pub hotkey_quick: String,
    pub hotkey_detailed: String,
    pub hotkey_macro: String,
    pub hotkey_macro2: String,
    pub hotkey_close: String,
    /// Only fire the price-check / macro hotkeys while Path of Exile is the
    /// focused window (so Ctrl+C in other apps isn't hijacked, and the macro
    /// never types into the wrong window). Set false if focus detection
    /// misbehaves on your setup and blocks the hotkeys.
    pub require_poe2_focus: bool,
    /// Which listings to search: `securable` (Instant Buyout — default),
    /// `online` (In Person), `available` (both), or `any`.
    pub trade_status: String,
    /// Where the popup appears (PRD §4.5/§4.8):
    /// - `center` — centered on the output (default).
    /// - `fixed` — at [`fixed_x`](Self::fixed_x) / [`fixed_y`](Self::fixed_y).
    /// - `at-cursor` — next to the cursor at Ctrl+C (Phase 7; currently falls
    ///   back to `center`).
    pub position_mode: String,
    /// Fixed-mode top-left position, in output-logical pixels from the top-left.
    pub fixed_x: i32,
    pub fixed_y: i32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            league: "Runes of Aldur".to_string(),
            realm: None,
            implicits_off_by_default: true,
            filters_off_by_default: vec![
                "Life Regeneration per second".to_string(),
                "Light Radius".to_string(),
            ],
            filter_min_percent: 100,
            f5_command: Some("/hideout".to_string()),
            macro2_command: Some("/exit".to_string()),
            hotkey_quick: "Ctrl+C".to_string(),
            hotkey_detailed: "Ctrl+Alt+C".to_string(),
            hotkey_macro: "F5".to_string(),
            hotkey_macro2: "F2".to_string(),
            hotkey_close: "Escape".to_string(),
            require_poe2_focus: true,
            trade_status: "securable".to_string(),
            position_mode: "center".to_string(),
            fixed_x: 100,
            fixed_y: 100,
        }
    }
}

impl Config {
    /// Whether a stat filter with this `label` should start unticked, per config
    /// (implicits and the noise-mod list).
    pub fn filter_off_by_default(&self, label: &str, is_implicit: bool) -> bool {
        if is_implicit && self.implicits_off_by_default {
            return true;
        }
        let lower = label.to_lowercase();
        self.filters_off_by_default
            .iter()
            .any(|p| lower.contains(&p.to_lowercase()))
    }

    /// `~/.config/poe2ddd/config.json` (honouring `XDG_CONFIG_HOME`).
    pub fn path() -> PathBuf {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| {
                let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
                home.join(".config")
            });
        base.join("poe2ddd").join("config.json")
    }

    /// Load from disk, falling back to defaults on a missing or invalid file.
    ///
    /// The loaded config is always written back out, so the file on disk is
    /// seeded on first run AND backfilled with any newly-added fields (with
    /// their defaults) — otherwise new settings would be applied at runtime but
    /// never visible/editable in the file.
    pub fn load() -> Self {
        let path = Self::path();
        let config = Self::load_no_write();
        if let Err(e) = config.save() {
            tracing::warn!(path = %path.display(), error = %e, "could not write config");
        }
        config
    }

    /// Load from disk WITHOUT the backfill write-back. Used by the hot-reload
    /// watcher (PRD §4.8), which must not re-trigger itself by writing the file.
    pub fn load_no_write() -> Self {
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!(path = %path.display(), error = %e, "invalid config on reload; using defaults");
                Config::default()
            }),
            Err(_) => Config::default(),
        }
    }

    /// Write back to disk (creating the directory if needed).
    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json)?;
        Ok(())
    }
}
