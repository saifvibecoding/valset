//! High-level operations shared by the CLI and the TUI.

use crate::accounts::DetectMethod;
use crate::appconfig::AppConfig;
use crate::db::{AccountRow, Db};
use crate::engine::Engine;
use crate::game::ProcessStatus;
use crate::matches::{self, MatchView, OwnerStats};
use crate::riot::models::{Content, Match};
use crate::riot::{self, RiotApi, rso};
use anyhow::{Context, Result, anyhow, bail};
use std::collections::HashSet;

pub fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime")
}

#[derive(Debug, Clone, Default)]
pub struct StatusInfo {
    pub config_dir_exists: bool,
    pub active: Option<(AccountRow, String, DetectMethod)>,
    pub account_count: usize,
    pub processes: ProcessStatus,
    pub patch: Option<String>,
    pub last_saved_profile: Option<String>,
    pub profile_count: usize,
    pub backup_count_active: i64,
    pub backup_count_total: i64,
    pub switch: Option<crate::accounts::AccountSwitch>,
    pub undo_available: bool,
    pub redo_available: bool,
    pub api_key: bool,
}

/// Gather the main-menu / `valsync status` dashboard (cheap: a few file stats
/// and SQLite reads, no network).
pub fn status(engine: &Engine) -> Result<StatusInfo> {
    let det = engine.detection();
    let switch = crate::accounts::sync_with_db(&engine.db, &det)?;
    let mut s = StatusInfo {
        config_dir_exists: det.config_exists,
        account_count: det.accounts.len(),
        processes: engine.probe.status(),
        patch: engine.current_patch(),
        last_saved_profile: engine.db.meta_get("last_saved_profile")?,
        profile_count: engine.db.profiles()?.len(),
        backup_count_total: engine.db.backup_count(None)?,
        switch,
        api_key: engine.config.api_key().is_some(),
        ..Default::default()
    };
    if let Some((a, m)) = det.active {
        let row = engine.db.account(&a.puuid)?.unwrap_or(AccountRow { puuid: a.puuid.clone(), shard: a.shard.clone(), ..Default::default() });
        s.backup_count_active = engine.db.backup_count(Some(&a.puuid))?;
        s.undo_available = engine.db.undo_candidate(&a.puuid)?.is_some();
        s.redo_available = engine.db.redo_candidate(&a.puuid)?.is_some();
        s.active = Some((row, a.folder, m));
    }
    Ok(s)
}

pub fn shard_for(acc: &AccountRow, cfg: &AppConfig) -> String {
    acc.shard.as_deref().and_then(riot::normalize_shard).or_else(|| cfg.default_shard.clone()).unwrap_or_else(|| "na".into())
}

#[derive(Debug, Clone)]
pub enum IdentityResult {
    /// Riot API confirmed the Riot ID belongs to the local PUUID.
    Verified,
    /// Riot ID exists, but the API returns app-encrypted PUUIDs so ownership of
    /// this local config folder could not be proven (use RSO to verify).
    ExistsUnverified,
    /// No API key: stored as entered.
    StoredManual,
}

/// Record a user-entered Riot ID for a local account, verifying it if possible.
pub async fn set_riot_id(api: &RiotApi, db: &mut Db, cfg: &AppConfig, puuid: &str, riot_id: &str) -> Result<IdentityResult> {
    let (name, tag) = crate::accounts::parse_riot_id(riot_id).ok_or_else(|| anyhow!("'{riot_id}' is not a valid Riot ID (expected Name#TAG)"))?;
    let acc = db.account(puuid)?.unwrap_or(AccountRow { puuid: puuid.to_string(), ..Default::default() });
    if !api.has_key() {
        db.set_account_identity(puuid, &name, &tag, "manual", false, None)?;
        return Ok(IdentityResult::StoredManual);
    }
    let region = riot::region_for_shard(&shard_for(&acc, cfg));
    match api.account_by_riot_id(db, region, &name, &tag).await {
        Ok(dto) => {
            let n = dto.game_name.clone().unwrap_or(name);
            let t = dto.tag_line.clone().unwrap_or(tag);
            if dto.puuid.eq_ignore_ascii_case(puuid) {
                db.set_account_identity(puuid, &n, &t, "api", true, Some(&dto.puuid))?;
                Ok(IdentityResult::Verified)
            } else {
                db.set_account_identity(puuid, &n, &t, "manual", false, Some(&dto.puuid))?;
                Ok(IdentityResult::ExistsUnverified)
            }
        }
        Err(riot::ApiError::NotFound) => bail!("Riot ID {name}#{tag} does not exist"),
        Err(e) => {
            db.set_account_identity(puuid, &name, &tag, "manual", false, None)?;
            Err(anyhow!("saved as unverified; verification failed: {e}"))
        }
    }
}

/// Try to fill in the Riot ID of a local account from ACCOUNT-V1 (by PUUID).
pub async fn refresh_identity(api: &RiotApi, db: &mut Db, cfg: &AppConfig, puuid: &str) -> Result<Option<String>> {
    if !api.has_key() {
        return Ok(None);
    }
    db.touch_account(puuid, None)?;
    let acc = db.account(puuid)?.context("unknown account")?;
    let region = riot::region_for_shard(&shard_for(&acc, cfg));
    let lookup = acc.api_puuid.clone().unwrap_or_else(|| puuid.to_string());
    match api.account_by_puuid(db, region, &lookup).await {
        Ok(dto) => {
            if let (Some(n), Some(t)) = (dto.game_name.clone(), dto.tag_line.clone()) {
                let verified = acc.verified || dto.puuid.eq_ignore_ascii_case(puuid);
                let source = if acc.source == "rso" { "rso" } else { "api" };
                db.set_account_identity(puuid, &n, &t, source, verified, Some(&dto.puuid))?;
                if let Ok(sh) = api.active_shard(db, region, &dto.puuid).await
                    && let Some(s) = riot::normalize_shard(&sh.active_shard)
                {
                    db.set_account_shard(puuid, &s)?;
                }
                return Ok(Some(format!("{n}#{t}")));
            }
            Ok(None)
        }
        Err(riot::ApiError::NotFound) => Ok(None),
        Err(e) => Err(anyhow!("{e}")),
    }
}

/// Riot Sign On for the currently active local account.
pub async fn rso_link(api: &RiotApi, db: &mut Db, cfg: &AppConfig, local_puuid: &str, open: bool, on_url: impl Fn(&str)) -> Result<String> {
    let client_id = cfg.rso_client_id.clone().context("RSO requires a client id issued by Riot for an approved product (`valsync config set rso_client_id <id>`)")?;
    let client_secret = cfg.rso_secret().context("RSO requires a client secret (set VALSYNC_RSO_CLIENT_SECRET or rso_client_secret)")?;
    let acc = db.account(local_puuid)?.unwrap_or(AccountRow { puuid: local_puuid.to_string(), ..Default::default() });
    let region = riot::region_for_shard(&shard_for(&acc, cfg)).to_string();
    let rcfg = rso::RsoConfig { client_id, client_secret, port: cfg.rso_redirect_port, region };
    let dto = rso::login(&rcfg, api, open, on_url).await?;
    let (n, t) = (dto.game_name.clone().unwrap_or_default(), dto.tag_line.clone().unwrap_or_default());
    db.touch_account(local_puuid, None)?;
    db.set_account_identity(local_puuid, &n, &t, "rso", true, Some(&dto.puuid))?;
    db.set_account_consent(local_puuid, true)?;
    Ok(format!("{n}#{t}"))
}

/// Stored matches of a consenting account, newest first.
pub fn stored_matches(db: &Db, local_puuid: &str, limit: usize) -> Result<Vec<Match>> {
    Ok(db.matches(local_puuid, limit)?.into_iter().filter_map(|s| serde_json::from_str(&s).ok()).collect())
}

pub fn cached_content(db: &Db, cfg: &AppConfig, shard: &str) -> Option<Content> {
    // Content is cached by RiotApi::content; read without network.
    db.conn
        .query_row("SELECT value FROM cache WHERE key=?1", [format!("content:{shard}:{}", cfg.locale)], |r| r.get::<_, String>(0))
        .ok()
        .and_then(|v| serde_json::from_str(&v).ok())
}

pub struct HistoryData {
    pub account: AccountRow,
    pub views: Vec<MatchView>,
    pub stats: OwnerStats,
    pub new_matches: usize,
    pub warning: Option<String>,
}

/// Sync (if allowed) and build match history for a local account.
pub async fn history(api: &RiotApi, db: &mut Db, cfg: &AppConfig, local_puuid: &str, limit: usize, fetch: bool) -> Result<HistoryData> {
    db.touch_account(local_puuid, None)?;
    let account = db.account(local_puuid)?.context("unknown account")?;
    if !account.consent {
        bail!(
            "{} has not opted in through Riot Sign On. Riot requires RSO consent before VALORANT match data can be shown (Manage Riot Accounts → Link with RSO, or `valsync accounts link`).",
            account.display()
        );
    }
    let api_puuid = account.api_puuid.clone().unwrap_or_else(|| local_puuid.to_string());
    let shard = shard_for(&account, cfg);
    let mut warning = None;
    let mut new_matches = 0;
    if fetch {
        if !api.has_key() {
            warning = Some(riot::ApiError::NoKey.to_string());
        } else {
            if let Err(e) = api.content(db, &shard, &cfg.locale).await {
                warning = Some(format!("content: {e}"));
            }
            match api.sync_matches(db, &shard, &api_puuid, local_puuid, limit.min(20)).await {
                Ok(n) => new_matches = n,
                Err(e) => warning = Some(e.to_string()),
            }
        }
    }
    let content = cached_content(db, cfg, &shard);
    let consented: HashSet<String> = db.accounts()?.into_iter().filter(|a| a.consent).filter_map(|a| a.api_puuid).collect();
    let ms = stored_matches(db, local_puuid, limit)?;
    let stats = matches::owner_stats(&ms, &api_puuid);
    let views = ms.iter().map(|m| matches::build_view(m, content.as_ref(), &api_puuid, &consented, cfg.hide_other_names)).collect();
    Ok(HistoryData { account, views, stats, new_matches, warning })
}
