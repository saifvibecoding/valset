//! User-editable application settings (`config.toml` in the VALSYNC data dir).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// Riot Developer Portal API key (production key with VALORANT access).
    /// `RIOT_API_KEY` environment variable takes precedence.
    pub riot_api_key: Option<String>,
    /// Fallback VALORANT shard (na, latam, br, eu, ap, kr) when unknown.
    pub default_shard: Option<String>,
    /// RSO (Riot Sign On) client id issued by Riot for an approved product.
    pub rso_client_id: Option<String>,
    /// RSO client secret. Prefer the `VALSYNC_RSO_CLIENT_SECRET` env variable.
    pub rso_client_secret: Option<String>,
    /// Local port for the RSO redirect URI (`http://127.0.0.1:<port>/oauth-callback`).
    pub rso_redirect_port: u16,
    /// Also write account-synced (cloud) values into the local cache files.
    /// VALORANT normally overwrites them from Riot's servers, so this is off.
    pub write_cloud_cache: bool,
    /// Keep at most this many automatic safety backups per account (0 = unlimited).
    /// Manual, original and undo/redo-referenced backups are never pruned.
    pub auto_backup_retention: u32,
    /// Seconds between match-list checks on the Live Match screen (minimum 60).
    pub live_poll_secs: u64,
    /// Hide other players' Riot IDs in match dashboards (streamer friendly).
    pub hide_other_names: bool,
    /// Manually selected active account PUUID (fallback when detection fails).
    pub manual_active_account: Option<String>,
    /// Colored output.
    pub color: bool,
    /// Locale for VAL-CONTENT-V1 names.
    pub locale: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        AppConfig {
            riot_api_key: None,
            default_shard: None,
            rso_client_id: None,
            rso_client_secret: None,
            rso_redirect_port: 51734,
            write_cloud_cache: false,
            auto_backup_retention: 0,
            live_poll_secs: 120,
            hide_other_names: false,
            manual_active_account: None,
            color: true,
            locale: "en-US".to_string(),
        }
    }
}

impl AppConfig {
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).with_context(|| format!("invalid config file {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(AppConfig::default()),
            Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self)?;
        crate::txn::atomic_write(path, text.as_bytes())
    }

    pub fn api_key(&self) -> Option<String> {
        std::env::var("RIOT_API_KEY").ok().filter(|k| !k.trim().is_empty()).or_else(|| self.riot_api_key.clone()).filter(|k| !k.trim().is_empty())
    }

    pub fn rso_secret(&self) -> Option<String> {
        std::env::var("VALSYNC_RSO_CLIENT_SECRET").ok().filter(|k| !k.is_empty()).or_else(|| self.rso_client_secret.clone())
    }

    pub fn poll_secs(&self) -> u64 {
        self.live_poll_secs.max(60)
    }

    /// Set a value by name (used by `valsync config set` and the TUI).
    pub fn set_field(&mut self, key: &str, value: &str) -> Result<()> {
        let none_if_empty = |v: &str| if v.trim().is_empty() { None } else { Some(v.trim().to_string()) };
        let parse_bool = |v: &str| -> Result<bool> {
            match v.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => Ok(true),
                "0" | "false" | "no" | "off" => Ok(false),
                _ => anyhow::bail!("expected true/false, got '{v}'"),
            }
        };
        match key {
            "riot_api_key" => self.riot_api_key = none_if_empty(value),
            "default_shard" => {
                let v = none_if_empty(value).map(|s| s.to_ascii_lowercase());
                if let Some(s) = &v {
                    anyhow::ensure!(crate::riot::SHARDS.contains(&s.as_str()), "unknown shard '{s}' (expected one of {})", crate::riot::SHARDS.join(", "));
                }
                self.default_shard = v
            }
            "rso_client_id" => self.rso_client_id = none_if_empty(value),
            "rso_client_secret" => self.rso_client_secret = none_if_empty(value),
            "rso_redirect_port" => self.rso_redirect_port = value.trim().parse().context("expected a port number")?,
            "write_cloud_cache" => self.write_cloud_cache = parse_bool(value)?,
            "auto_backup_retention" => self.auto_backup_retention = value.trim().parse().context("expected a number")?,
            "live_poll_secs" => {
                let v: u64 = value.trim().parse().context("expected seconds")?;
                anyhow::ensure!(v >= 60, "minimum polling interval is 60 seconds");
                self.live_poll_secs = v
            }
            "hide_other_names" => self.hide_other_names = parse_bool(value)?,
            "manual_active_account" => self.manual_active_account = none_if_empty(value),
            "color" => self.color = parse_bool(value)?,
            "locale" => self.locale = value.trim().to_string(),
            _ => anyhow::bail!("unknown setting '{key}'. Known: {}", Self::KEYS.join(", ")),
        }
        Ok(())
    }

    pub const KEYS: [&'static str; 12] = [
        "riot_api_key",
        "default_shard",
        "rso_client_id",
        "rso_client_secret",
        "rso_redirect_port",
        "write_cloud_cache",
        "auto_backup_retention",
        "live_poll_secs",
        "hide_other_names",
        "manual_active_account",
        "color",
        "locale",
    ];

    /// Display value (secrets masked).
    pub fn get_display(&self, key: &str) -> String {
        let mask = |v: &Option<String>| match v {
            Some(s) if s.chars().count() > 8 => {
                let c: Vec<char> = s.chars().collect();
                format!("{}…{} (set)", c[..4].iter().collect::<String>(), c[c.len() - 4..].iter().collect::<String>())
            }
            Some(_) => "(set)".to_string(),
            None => "(not set)".to_string(),
        };
        let opt = |v: &Option<String>| v.clone().unwrap_or_else(|| "(not set)".into());
        match key {
            "riot_api_key" => {
                if std::env::var("RIOT_API_KEY").map(|v| !v.is_empty()).unwrap_or(false) {
                    "(from RIOT_API_KEY env)".into()
                } else {
                    mask(&self.riot_api_key)
                }
            }
            "default_shard" => opt(&self.default_shard),
            "rso_client_id" => opt(&self.rso_client_id),
            "rso_client_secret" => mask(&self.rso_client_secret),
            "rso_redirect_port" => self.rso_redirect_port.to_string(),
            "write_cloud_cache" => self.write_cloud_cache.to_string(),
            "auto_backup_retention" => {
                if self.auto_backup_retention == 0 { "0 (unlimited)".into() } else { self.auto_backup_retention.to_string() }
            }
            "live_poll_secs" => self.live_poll_secs.to_string(),
            "hide_other_names" => self.hide_other_names.to_string(),
            "manual_active_account" => opt(&self.manual_active_account),
            "color" => self.color.to_string(),
            "locale" => self.locale.clone(),
            _ => String::new(),
        }
    }

    pub fn describe(key: &str) -> &'static str {
        match key {
            "riot_api_key" => "Riot API key (production key required for VALORANT data)",
            "default_shard" => "Fallback shard: na, latam, br, eu, ap, kr",
            "rso_client_id" => "Riot Sign On client id (approved products only)",
            "rso_client_secret" => "Riot Sign On client secret (prefer env VALSYNC_RSO_CLIENT_SECRET)",
            "rso_redirect_port" => "Local port for the RSO redirect",
            "write_cloud_cache" => "Also write cloud-synced values to local cache (unreliable)",
            "auto_backup_retention" => "Max automatic safety backups per account (0 = unlimited)",
            "live_poll_secs" => "Live match refresh interval in seconds (min 60)",
            "hide_other_names" => "Hide other players' Riot IDs in dashboards",
            "manual_active_account" => "Manually selected active account PUUID",
            "color" => "Colored terminal output",
            "locale" => "Locale for agent/map names",
            _ => "",
        }
    }

    pub fn is_bool(key: &str) -> bool {
        matches!(key, "write_cloud_cache" | "hide_other_names" | "color")
    }
}
