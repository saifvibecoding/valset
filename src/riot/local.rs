//! The Riot Client's local API (`lockfile`).
//!
//! `%LOCALAPPDATA%\Riot Games\Riot Client\Config\lockfile` contains
//! `name:pid:port:password:protocol` while the Riot Client runs. Requests go to
//! `https://127.0.0.1:<port>` with HTTP basic auth `riot:<password>`; the client
//! uses a self-signed certificate, so certificate checks are disabled **for
//! 127.0.0.1 only**.

use super::models::{ChatSession, Entitlements, ExternalSession};
use anyhow::{Context, Result, bail};
use base64::Engine as _;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lockfile {
    pub pid: u32,
    pub port: u16,
    pub password: String,
    pub protocol: String,
}

pub fn lockfile_path() -> PathBuf {
    if let Some(p) = std::env::var_os("VALSYNC_LOCKFILE").filter(|v| !v.is_empty()) {
        return PathBuf::from(p);
    }
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| {
        std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from).unwrap_or_default().join("AppData").join("Local")
    });
    local.join("Riot Games").join("Riot Client").join("Config").join("lockfile")
}

impl Lockfile {
    pub fn parse(text: &str) -> Option<Lockfile> {
        let parts: Vec<&str> = text.trim().split(':').collect();
        if parts.len() < 5 {
            return None;
        }
        let n = parts.len();
        // The client name may itself contain ':' in theory; read from the end.
        Some(Lockfile {
            pid: parts[n - 4].parse().ok()?,
            port: parts[n - 3].parse().ok()?,
            password: parts[n - 2].to_string(),
            protocol: parts[n - 1].to_string(),
        })
    }

    /// `None` when the Riot Client is not running.
    pub fn read() -> Option<Lockfile> {
        // The Riot Client keeps the file open; read it shared.
        let text = std::fs::read_to_string(lockfile_path()).ok()?;
        Lockfile::parse(&text)
    }
}

pub struct LocalClient {
    lock: Lockfile,
    http: reqwest::Client,
}

impl LocalClient {
    pub fn new(lock: Lockfile) -> Result<LocalClient> {
        let http = reqwest::Client::builder()
            .danger_accept_invalid_certs(true) // 127.0.0.1 self-signed certificate only
            .danger_accept_invalid_hostnames(true)
            .timeout(Duration::from_secs(5))
            .no_proxy()
            .build()?;
        Ok(LocalClient { lock, http })
    }

    pub fn connect() -> Result<LocalClient> {
        let lock = Lockfile::read().context("the Riot Client is not running. Open the Riot Client and sign in.")?;
        LocalClient::new(lock)
    }

    fn url(&self, path: &str) -> String {
        let proto = if self.lock.protocol.eq_ignore_ascii_case("http") { "http" } else { "https" };
        format!("{proto}://127.0.0.1:{}{path}", self.lock.port)
    }

    fn auth(&self) -> String {
        format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("riot:{}", self.lock.password)))
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let resp = self.http.get(self.url(path)).header("Authorization", self.auth()).send().await.context("cannot reach the Riot Client")?;
        let status = resp.status();
        if !status.is_success() {
            bail!("Riot Client answered HTTP {} for {path}", status.as_u16());
        }
        resp.json().await.with_context(|| format!("unexpected Riot Client response for {path}"))
    }

    pub async fn entitlements(&self) -> Result<Entitlements> {
        let e: Entitlements = self.get("/entitlements/v1/token").await.map_err(|e| anyhow::anyhow!("{e:#}. Make sure you are signed in to the Riot Client."))?;
        if e.access_token.is_empty() || e.subject.is_empty() {
            bail!("the Riot Client is open but nobody is signed in");
        }
        Ok(e)
    }

    pub async fn chat_session(&self) -> Option<ChatSession> {
        self.get::<ChatSession>("/chat/v1/session").await.ok().filter(|c| !c.game_name.is_empty())
    }

    /// `(region, shard)` from VALORANT's launch arguments (only while it runs).
    pub async fn region_from_sessions(&self) -> Option<(String, String)> {
        let sessions: HashMap<String, ExternalSession> = self.get("/product-session/v1/external-sessions").await.ok()?;
        let mut region = None;
        let mut shard = None;
        for s in sessions.values().filter(|s| s.product_id == "valorant") {
            for arg in s.launch_configuration.iter().flat_map(|l| l.arguments.iter()) {
                if let Some(v) = arg.strip_prefix("-ares-deployment=") {
                    region = super::normalize_region(v);
                }
                if let Some(v) = arg.strip_prefix("-config-endpoint=") {
                    // https://shared.eu.a.pvp.net
                    shard = v.split('.').nth(1).map(str::to_string);
                }
            }
        }
        let region = region?;
        let shard = shard.unwrap_or_else(|| super::shard_for_region(&region).to_string());
        Some((region, shard))
    }
}

/// `(region, shard)` from the newest `glz-<region>-1.<shard>.a.pvp.net` in the game log.
pub fn region_from_log(log_text: &str) -> Option<(String, String)> {
    let mut found = None;
    let mut rest = log_text;
    while let Some(i) = rest.find("glz-") {
        let tail = &rest[i + 4..];
        let host: String = tail.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '.').collect();
        // <region>-1.<shard>.a.pvp.net
        if let Some((reg, after)) = host.split_once("-1.")
            && let Some(shard) = after.split('.').next()
            && let Some(r) = super::normalize_region(reg)
        {
            found = Some((r, shard.to_string()));
        }
        rest = &tail[host.len().min(tail.len())..];
    }
    found
}

/// `release-XX.YY-shipping-N-NNNNNNN` from `CI server version:` in the game log.
pub fn client_version_from_log(log_text: &str) -> Option<String> {
    let mut found = None;
    for line in log_text.lines() {
        if let Some(i) = line.find("CI server version:") {
            let v = line[i + 18..].split_whitespace().next().unwrap_or("");
            if v.starts_with("release-") {
                found = Some(v.to_string());
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lockfile() {
        let l = Lockfile::parse("Riot Client:12345:54321:s3cr3t:https").unwrap();
        assert_eq!((l.pid, l.port, l.password.as_str(), l.protocol.as_str()), (12345, 54321, "s3cr3t", "https"));
        assert!(Lockfile::parse("garbage").is_none());
    }

    #[test]
    fn parses_log() {
        let log = "x https://glz-na-1.na.a.pvp.net/old\nLogShooter: Display: CI server version: release-11.08-shipping-6-3456789\ny https://glz-eu-1.eu.a.pvp.net/core-game/v1/players/x";
        assert_eq!(region_from_log(log), Some(("eu".into(), "eu".into())));
        assert_eq!(client_version_from_log(log).as_deref(), Some("release-11.08-shipping-6-3456789"));
        assert_eq!(region_from_log("glz-latam-1.na.a.pvp.net"), Some(("latam".into(), "na".into())));
    }
}
