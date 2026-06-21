//! Persistent settings at `$XDG_CONFIG_HOME/kingsmerchant/config.json`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub(crate) const DEFAULT_CACHE_TTL_SECS: u32 = 30;
pub(crate) const MAX_CACHE_TTL_SECS: u32 = 120;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Trade league id; empty = auto-resolve current non-HC league at startup.
    pub league: String,
    /// True once the user picks a league; while false it's re-derived each startup.
    pub league_pinned: bool,
    /// Realm (`pc` / `sony` / `xbox`); `None` = pc.
    pub realm: Option<String>,
    pub implicits_off_by_default: bool,
    /// Stat filters whose label contains any of these substrings start unticked.
    pub filters_off_by_default: Vec<String>,
    /// Filter mins seeded at this percentage of the rolled value. 1..=100.
    pub filter_min_percent: u32,
    /// Cache lifetime in seconds; `0` disables, capped at 120s.
    pub cache_ttl_secs: u32,
    /// Chat command for the macro hotkey; `null` disables it.
    pub f5_command: Option<String>,
    /// Second chat macro (default `/exit`, on F2); `null` disables it.
    pub macro2_command: Option<String>,
    /// Rebindable hotkeys, e.g. `"Ctrl+C"`, `"F5"`, `"Escape"`.
    pub hotkey_quick: String,
    pub hotkey_macro: String,
    pub hotkey_macro2: String,
    pub hotkey_close: String,
    /// Opens settings; fires regardless of focused window.
    pub hotkey_settings: String,
    /// `tracing` level: `auto`/`off`/`error`/`warn`/`info`/`debug`/`trace`. `RUST_LOG` overrides.
    pub log_level: String,
    /// Only fire price-check / macro hotkeys while POE2 is focused.
    pub require_poe2_focus: bool,
    /// `securable` (default) / `online` / `available` / `any`.
    pub trade_status: String,
    /// `center` (default) / `fixed` / `at-cursor` (unimplemented, falls back to center).
    pub position_mode: String,
    /// Fixed-mode top-left position, in output-logical pixels from the top-left.
    pub fixed_x: i32,
    pub fixed_y: i32,
    /// Emit the per-second overlay performance log.
    pub perf_metrics: bool,
    /// `POESESSID` cookie (32-hex); sent only to pathofexile.com. Treat like a password.
    pub poesessid: Option<String>,
    /// Route requests through a Chrome-emulating client to get past Cloudflare's
    /// bot-check. Off by default; needs [`cf_clearance`](Self::cf_clearance).
    #[serde(default)]
    pub impersonate: bool,
    /// `cf_clearance` cookie from the browser, used only when `impersonate` is on.
    #[serde(default)]
    pub cf_clearance: Option<String>,
    pub theme: ThemeConfig,
}

/// Hex `#rrggbb` colours (malformed → default); rarity/frame colours stay in-game.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeConfig {
    pub accent_gold: String,
    pub affix_blue: String,
    pub online_dot: String,
    pub header_bg: String,
    pub overlay_fill: String,
    pub overlay_stroke: String,
    /// Popup background opacity, `0.0`..=`1.0`.
    pub opacity: f32,
}

impl Default for ThemeConfig {
    fn default() -> Self {
        ThemeConfig {
            accent_gold: "#e6c25a".to_string(),
            affix_blue: "#8a8af0".to_string(),
            online_dot: "#4cd137".to_string(),
            header_bg: "#17171c".to_string(),
            overlay_fill: "#2c2e36".to_string(),
            overlay_stroke: "#50525e".to_string(),
            opacity: 1.0,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            league: String::new(),
            league_pinned: false,
            realm: None,
            implicits_off_by_default: true,
            filters_off_by_default: vec![
                "Life Regeneration per second".to_string(),
                "Light Radius".to_string(),
            ],
            filter_min_percent: 100,
            cache_ttl_secs: DEFAULT_CACHE_TTL_SECS,
            f5_command: Some("/hideout".to_string()),
            macro2_command: Some("/exit".to_string()),
            hotkey_quick: "Ctrl+C".to_string(),
            hotkey_macro: "F5".to_string(),
            hotkey_macro2: "F2".to_string(),
            hotkey_close: "Escape".to_string(),
            hotkey_settings: "Ctrl+Alt+S".to_string(),
            log_level: "auto".to_string(),
            require_poe2_focus: true,
            trade_status: "securable".to_string(),
            position_mode: "center".to_string(),
            fixed_x: 100,
            fixed_y: 100,
            perf_metrics: false,
            poesessid: None,
            impersonate: false,
            cf_clearance: None,
            theme: ThemeConfig::default(),
        }
    }
}

impl Config {
    /// Whether a stat filter with this `label` should start unticked.
    pub fn filter_off_by_default(&self, label: &str, is_implicit: bool) -> bool {
        if is_implicit && self.implicits_off_by_default {
            return true;
        }
        let lower = label.to_lowercase();
        self.filters_off_by_default
            .iter()
            .any(|p| lower.contains(&p.to_lowercase()))
    }

    /// `~/.config/kingsmerchant/config.json` (honouring `XDG_CONFIG_HOME`).
    pub fn path() -> PathBuf {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| {
                let home = std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_default();
                home.join(".config")
            });
        base.join("kingsmerchant").join("config.json")
    }

    /// Load from disk (defaults on missing/invalid), written back to seed/backfill the file.
    pub fn load() -> Self {
        let path = Self::path();
        let config = Self::load_no_write();
        if let Err(e) = config.save() {
            tracing::warn!(path = %path.display(), error = %e, "could not write config");
        }
        config
    }

    /// Clamp hand-edited values into their valid ranges.
    fn normalize(&mut self) {
        self.filter_min_percent = self.filter_min_percent.clamp(1, 100);
        // Out-of-range TTL falls back to the default rather than clamping to the cap.
        if !(0..=MAX_CACHE_TTL_SECS).contains(&self.cache_ttl_secs) {
            self.cache_ttl_secs = DEFAULT_CACHE_TTL_SECS;
        }
    }

    pub fn load_no_write() -> Self {
        let path = Self::path();
        let mut config = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!(path = %path.display(), error = %e, "invalid config on reload; using defaults");
                Config::default()
            }),
            Err(_) => Config::default(),
        };
        config.normalize();
        config
    }

    /// Write back to disk via a temp file + atomic rename (so readers never see a truncated file).
    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        std::fs::write(&tmp, json)?;
        if let Err(e) = std::fs::rename(&tmp, &path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_off_by_default_matches_implicits_and_noise_substrings() {
        let cfg = Config::default();
        assert!(cfg.filter_off_by_default("anything", true));
        assert!(cfg.filter_off_by_default("increased Light Radius", false));
        assert!(cfg.filter_off_by_default("LIGHT RADIUS bonus", false));
        assert!(!cfg.filter_off_by_default("+50 to maximum Life", false));
    }

    #[test]
    fn config_round_trips_through_json() {
        let json = serde_json::to_string(&Config::default()).expect("serialize");
        let back: Config = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(
            back.filter_min_percent,
            Config::default().filter_min_percent
        );
        assert_eq!(back.cache_ttl_secs, DEFAULT_CACHE_TTL_SECS);
        assert_eq!(back.hotkey_quick, "Ctrl+C");
        assert_eq!(back.trade_status, "securable");
    }

    #[test]
    fn normalize_keeps_valid_values_and_repairs_bad_ones() {
        let normalized = |ttl, pct| {
            let mut c = Config {
                cache_ttl_secs: ttl,
                filter_min_percent: pct,
                ..Config::default()
            };
            c.normalize();
            c
        };
        assert_eq!(normalized(0, 50).cache_ttl_secs, 0);
        assert_eq!(
            normalized(MAX_CACHE_TTL_SECS, 50).cache_ttl_secs,
            MAX_CACHE_TTL_SECS
        );
        assert_eq!(normalized(999, 50).cache_ttl_secs, DEFAULT_CACHE_TTL_SECS);
        assert_eq!(normalized(30, 0).filter_min_percent, 1);
        assert_eq!(normalized(30, 250).filter_min_percent, 100);
    }
}
