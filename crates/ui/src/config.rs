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
    /// Chat command typed into POE2 when F5 is pressed (via a uinput virtual
    /// keyboard: opens chat, types this, sends). `null` disables it. Injection
    /// steps past the clipboard-only design (PRD App. B) — opt-in.
    pub f5_command: Option<String>,
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
    /// On first run (no file) the defaults are written out so the file exists
    /// and can be hand-edited.
    pub fn load() -> Self {
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!(path = %path.display(), error = %e, "invalid config; using defaults");
                Config::default()
            }),
            Err(_) => {
                let config = Config::default();
                if let Err(e) = config.save() {
                    tracing::warn!(path = %path.display(), error = %e, "could not seed config");
                }
                config
            }
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
