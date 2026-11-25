//! User-editable application settings (`config.toml` in the VALSYNC data dir).
//! Everything works with the defaults; nothing here needs to be set up.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// Region override (na, latam, br, eu, ap, kr). Detected automatically.
    pub region: Option<String>,
    /// Live Match refresh interval in seconds while you are in a match.
    pub live_poll_secs: u64,
    /// Hide other players' names in match screens (streamer mode).
    pub hide_other_names: bool,
    /// Keep at most this many automatic backups per account (0 = keep all).
    pub auto_backup_retention: u32,
    /// Fallback account when the Riot Client is closed.
    pub manual_active_account: Option<String>,
    pub color: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        AppConfig { region: None, live_poll_secs: 10, hide_other_names: false, auto_backup_retention: 0, manual_active_account: None, color: true }
    }
}

impl AppConfig {
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(toml::from_str(&text).unwrap_or_default()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(AppConfig::default()),
            Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        crate::txn::atomic_write(path, toml::to_string_pretty(self)?.as_bytes())
    }

    pub fn poll_secs(&self) -> u64 {
        self.live_poll_secs.max(5)
    }

    pub fn set_field(&mut self, key: &str, value: &str) -> Result<()> {
        let v = value.trim();
        let parse_bool = |v: &str| -> Result<bool> {
            match v.to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => Ok(true),
                "0" | "false" | "no" | "off" => Ok(false),
                _ => anyhow::bail!("expected on/off, got '{v}'"),
            }
        };
        match key {
            "region" => {
                self.region = if v.is_empty() || v.eq_ignore_ascii_case("auto") {
                    None
                } else {
                    Some(crate::riot::normalize_region(v).with_context(|| format!("unknown region '{v}' (use na, latam, br, eu, ap, kr or auto)"))?)
                }
            }
            "live_poll_secs" => {
                let n: u64 = v.parse().context("expected a number of seconds")?;
                anyhow::ensure!(n >= 5, "minimum refresh interval is 5 seconds");
                self.live_poll_secs = n
            }
            "hide_other_names" => self.hide_other_names = parse_bool(v)?,
            "auto_backup_retention" => self.auto_backup_retention = v.parse().context("expected a number")?,
            "manual_active_account" => self.manual_active_account = if v.is_empty() { None } else { Some(v.to_string()) },
            "color" => self.color = parse_bool(v)?,
            _ => anyhow::bail!("unknown setting '{key}'. Known: {}", Self::KEYS.join(", ")),
        }
        Ok(())
    }

    pub const KEYS: [&'static str; 5] = ["region", "live_poll_secs", "hide_other_names", "auto_backup_retention", "color"];

    pub fn get_display(&self, key: &str) -> String {
        match key {
            "region" => self.region.clone().map(|r| r.to_uppercase()).unwrap_or_else(|| "auto".into()),
            "live_poll_secs" => format!("{} s", self.live_poll_secs),
            "hide_other_names" => if self.hide_other_names { "on" } else { "off" }.into(),
            "auto_backup_retention" => if self.auto_backup_retention == 0 { "keep all".into() } else { self.auto_backup_retention.to_string() },
            "manual_active_account" => self.manual_active_account.clone().unwrap_or_else(|| "—".into()),
            "color" => if self.color { "on" } else { "off" }.into(),
            _ => String::new(),
        }
    }

    pub fn describe(key: &str) -> &'static str {
        match key {
            "region" => "Server region (auto-detected)",
            "live_poll_secs" => "Live Match refresh while in a match",
            "hide_other_names" => "Hide other players' names (streamer mode)",
            "auto_backup_retention" => "Automatic backups to keep per account",
            "color" => "Colors",
            _ => "",
        }
    }

    pub fn is_bool(key: &str) -> bool {
        matches!(key, "hide_other_names" | "color")
    }
}
