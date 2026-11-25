//! High-level operations shared by the CLI and the TUI.

use crate::accounts::DetectMethod;
use crate::appconfig::AppConfig;
use crate::db::{AccountRow, Db};
use crate::engine::Engine;
use crate::game::ProcessStatus;
use crate::matches::{self, LiveView, MatchView, OwnerStats};
use crate::riot::models::{Match, Peak};
use crate::riot::{Session, content};
use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;

pub fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime")
}

#[derive(Debug, Clone, Default)]
pub struct StatusInfo {
    pub config_dir_exists: bool,
    pub active: Option<(AccountRow, String, DetectMethod)>,
    pub account_count: usize,
    pub processes: ProcessStatus,
    /// Signed in to the Riot Client (account settings available).
    pub riot_signed_in: bool,
    pub riot_error: Option<String>,
    pub patch: Option<String>,
    pub last_saved_profile: Option<String>,
    pub profile_count: usize,
    pub backup_count_active: i64,
    pub backup_count_total: i64,
    pub switch: Option<crate::accounts::AccountSwitch>,
    pub undo_available: bool,
    pub redo_available: bool,
}

/// Gather the main-menu / `valsync status` dashboard (local only, no network).
pub fn status(engine: &Engine) -> Result<StatusInfo> {
    let det = engine.detection();
    let switch = crate::accounts::sync_with_db(&engine.db, &det)?;
    let mut s = StatusInfo {
        config_dir_exists: det.config_exists,
        account_count: engine.db.accounts()?.len(),
        processes: engine.probe.status(),
        riot_signed_in: engine.cloud_account().is_some(),
        riot_error: engine.cloud_error.clone(),
        patch: engine.current_patch(),
        last_saved_profile: engine.db.meta_get("last_saved_profile")?,
        profile_count: engine.db.profiles()?.len(),
        backup_count_total: engine.db.backup_count(None)?,
        switch,
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

pub async fn connect(logs_dir: &Path, db: &Db, cfg: &AppConfig) -> Result<Session> {
    let known = cfg.region.clone().or_else(|| db.meta_get("last_region").ok().flatten());
    Session::connect(logs_dir, known.as_deref()).await
}

/// Stored matches of an account, newest first.
pub fn stored_matches(db: &Db, puuid: &str, limit: usize) -> Result<Vec<Match>> {
    Ok(db.matches(puuid, limit)?.into_iter().filter_map(|s| serde_json::from_str(&s).ok()).collect())
}

#[derive(Debug, Clone)]
pub struct HistoryData {
    pub account: String,
    pub views: Vec<MatchView>,
    pub stats: OwnerStats,
    pub new_matches: usize,
    pub warning: Option<String>,
}

fn stored_peak(db: &Db, puuid: &str) -> Option<Peak> {
    db.meta_get(&format!("peak:{puuid}")).ok().flatten().and_then(|v| serde_json::from_str(&v).ok())
}

fn build_history(db: &Db, cfg: &AppConfig, puuid: &str, label: String, limit: usize, extra: Option<&crate::riot::models::Mmr>) -> Result<HistoryData> {
    let c = content::cached(db);
    let ms = stored_matches(db, puuid, limit)?;
    let mut stats = matches::owner_stats(&ms, puuid);
    if let Some(m) = extra {
        stats.apply_mmr(m);
    }
    if let Some(p) = stored_peak(db, puuid) {
        stats.apply_peak(p);
    }
    let views = ms.iter().map(|m| matches::build_view(m, c.as_ref(), puuid, cfg.hide_other_names)).collect();
    Ok(HistoryData { account: label, views, stats, new_matches: 0, warning: None })
}

/// Stored history only (instant, offline).
pub fn history_offline(db: &Db, cfg: &AppConfig, puuid: &str, limit: usize) -> Result<HistoryData> {
    let label = db.account(puuid)?.map(|a| a.display()).unwrap_or_else(|| crate::db::short_id(puuid));
    let mut h = build_history(db, cfg, puuid, label, limit, None)?;
    if let Ok(Some(raw)) = db.meta_get(&format!("mmr:{puuid}"))
        && let Ok(m) = serde_json::from_str(&raw)
    {
        h.stats.apply_mmr(&m);
    }
    Ok(h)
}

/// Download new matches + rank for the signed-in account, then build the history.
pub async fn history(session: &Session, db: &Db, cfg: &AppConfig, limit: usize) -> Result<HistoryData> {
    let content = content::load(db).await;
    let _ = content; // cached for build_history
    let mut warning = None;
    let mut new = 0;
    match session.match_history(limit.min(20)).await {
        Ok(list) => {
            for e in list.history.iter().take(limit) {
                if db.has_match(&e.match_id, &session.puuid).unwrap_or(false) {
                    continue;
                }
                match session.match_details(&e.match_id).await {
                    Ok((m, _)) => {
                        let compact = serde_json::to_string(&m)?;
                        db.store_match(&e.match_id, &session.puuid, m.match_info.game_start_millis, &m.match_info.queue_id, &compact)?;
                        new += 1;
                    }
                    Err(err) => {
                        warning = Some(format!("{err:#}"));
                        break;
                    }
                }
            }
        }
        Err(e) => warning = Some(format!("{e:#}")),
    }
    let mmr = match session.mmr().await {
        Ok(m) => Some(m),
        Err(e) => {
            warning.get_or_insert(format!("rank: {e:#}"));
            None
        }
    };
    if let Some(m) = &mmr
        && let Ok(j) = serde_json::to_string(m)
    {
        let _ = db.meta_set(&format!("mmr:{}", session.puuid), &j);
    }
    // Rank history: the full history once (up to 100 matches), then the newest page.
    let mut updates = Vec::new();
    let pages = if stored_peak(db, &session.puuid).is_some() { 1 } else { 5 };
    for page in 0..pages {
        match session.competitive_updates(page * 20).await {
            Ok(u) if !u.matches.is_empty() => updates.extend(u.matches),
            _ => break,
        }
    }
    let label = session.riot_id().unwrap_or_else(|| crate::db::short_id(&session.puuid));
    let mut h = build_history(db, cfg, &session.puuid, label, limit, mmr.as_ref())?;
    h.stats.apply_updates(&updates);
    if let Some(t) = h.stats.peak_tier
        && let Ok(j) = serde_json::to_string(&Peak { tier: t, rr: h.stats.peak_rr })
    {
        let _ = db.meta_set(&format!("peak:{}", session.puuid), &j);
    }
    if let Some(l) = session.account_level().await {
        h.stats.level = Some(l);
    }
    h.new_matches = new;
    h.warning = warning;
    Ok(h)
}

#[derive(Debug, Clone)]
pub struct LiveData {
    pub account: String,
    pub view: Option<LiveView>,
    pub stats: OwnerStats,
    pub last_match: Option<MatchView>,
    pub warning: Option<String>,
}

async fn names_for(session: &Session, db: &Db, puuids: Vec<String>) -> HashMap<String, (String, String)> {
    let mut out = HashMap::new();
    let mut missing = Vec::new();
    for p in puuids {
        match db.cache_get(&format!("name:{p}")).ok().flatten().and_then(|v| v.split_once('#').map(|(a, b)| (a.to_string(), b.to_string()))) {
            Some(n) => {
                out.insert(p, n);
            }
            None => missing.push(p),
        }
    }
    if let Ok(list) = session.names(&missing).await {
        for n in list {
            if !n.game_name.is_empty() {
                let _ = db.cache_put(&format!("name:{}", n.subject), &format!("{}#{}", n.game_name, n.tag_line), 86_400);
            }
            out.insert(n.subject, (n.game_name, n.tag_line));
        }
    }
    out
}

/// Current agent select / match for the signed-in account, plus own stats.
/// `cached_loadouts` avoids re-downloading loadouts for the same match.
pub async fn live(session: &Session, db: &Db, cfg: &AppConfig, cached: Option<&LiveView>) -> Result<LiveData> {
    let content = content::load(db).await;
    let mut warning = None;
    let mut view = None;
    match session.current_game().await {
        Ok(Some(m)) => {
            if let Some(c) = cached.filter(|c| c.match_id == m.match_id && c.phase == matches::LivePhase::InGame) {
                view = Some(c.clone());
            } else {
                let loadouts = session.current_loadouts(&m.match_id).await.ok();
                let visible: Vec<String> = m.players.iter().filter(|p| !p.player_identity.incognito || p.subject == session.puuid).map(|p| p.subject.clone()).collect();
                let names = names_for(session, db, visible).await;
                view = Some(matches::build_live(&m, loadouts.as_ref(), &names, Some(&content), &session.puuid, cfg.hide_other_names));
            }
        }
        Ok(None) => match session.pregame().await {
            Ok(Some(p)) => {
                let visible: Vec<String> = p
                    .ally_team
                    .iter()
                    .flat_map(|t| t.players.iter())
                    .filter(|x| !x.player_identity.incognito || x.subject == session.puuid)
                    .map(|x| x.subject.clone())
                    .collect();
                let names = names_for(session, db, visible).await;
                view = Some(matches::build_pregame(&p, &names, Some(&content), &session.puuid, cfg.hide_other_names));
            }
            Ok(None) => {}
            Err(e) => warning = Some(format!("{e:#}")),
        },
        Err(e) => warning = Some(format!("{e:#}")),
    }
    // Own stats (rank cached; refreshed when not in a match to keep requests low).
    let mut h = history_offline(db, cfg, &session.puuid, 20)?;
    if view.is_none()
        && let Ok(full) = history(session, db, cfg, 10).await
    {
        h = full;
    }
    Ok(LiveData {
        account: session.riot_id().unwrap_or_else(|| crate::db::short_id(&session.puuid)),
        view,
        stats: h.stats,
        last_match: h.views.into_iter().next(),
        warning: warning.or(h.warning),
    })
}
