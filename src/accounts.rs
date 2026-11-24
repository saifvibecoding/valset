//! Local Riot account detection.
//!
//! VALSYNC identifies accounts only through normal configuration files that
//! VALORANT writes for every account that has launched the game on this PC:
//!
//! * `Config\<PUUID>-<shard>\...` — one folder per account (the folder name is
//!   the player's PUUID followed by the shard, e.g. `-eu`).
//! * `Config\RiotLocalMachine.ini` → `[UserInfo] LastKnownUser=<PUUID>` — the
//!   account that most recently ran VALORANT.
//!
//! VALSYNC never reads Riot Client session files, cookies, lockfiles or tokens.
//! The Riot ID (Name#TAG) is filled in by the user (optionally verified against
//! ACCOUNT-V1) or by Riot Sign On.

use crate::db::Db;
use crate::ini::IniDoc;
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalAccount {
    pub puuid: String,
    pub shard: Option<String>,
    /// Folder name inside the Config directory.
    pub folder: String,
    pub path: PathBuf,
    pub modified: Option<SystemTime>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectMethod {
    /// `RiotLocalMachine.ini` LastKnownUser (VALORANT's own record).
    LastKnownUser,
    /// User-selected fallback, validated against a local config folder.
    Manual,
    /// Only one account folder exists.
    SingleFolder,
    /// Heuristic: most recently modified account folder. Must be confirmed.
    MostRecentFolder,
}

impl DetectMethod {
    pub fn label(self) -> &'static str {
        match self {
            DetectMethod::LastKnownUser => "detected (VALORANT last user)",
            DetectMethod::Manual => "manual selection",
            DetectMethod::SingleFolder => "detected (only account)",
            DetectMethod::MostRecentFolder => "guessed (most recent folder) — please confirm",
        }
    }
    pub fn is_confident(self) -> bool {
        !matches!(self, DetectMethod::MostRecentFolder)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Detection {
    pub config_exists: bool,
    pub accounts: Vec<LocalAccount>,
    pub last_known_user: Option<String>,
    pub active: Option<(LocalAccount, DetectMethod)>,
}

impl Detection {
    pub fn find(&self, puuid_or_folder: &str) -> Option<&LocalAccount> {
        let q = puuid_or_folder.trim().to_ascii_lowercase();
        let mut matches: Vec<&LocalAccount> = self.accounts.iter().filter(|a| a.puuid == q || a.folder.to_ascii_lowercase() == q).collect();
        if matches.is_empty() && q.len() >= 6 {
            matches = self.accounts.iter().filter(|a| a.puuid.starts_with(&q)).collect();
            let distinct: std::collections::BTreeSet<&str> = matches.iter().map(|a| a.puuid.as_str()).collect();
            if distinct.len() > 1 {
                return None;
            }
        }
        matches.sort_by_key(|a| std::cmp::Reverse(a.modified));
        matches.first().copied()
    }
}

/// Parse `<uuid>` or `<uuid>-<shard>` folder names.
pub fn parse_account_folder(name: &str) -> Option<(String, Option<String>)> {
    if name.len() < 36 || !name.is_char_boundary(36) {
        return None;
    }
    let (uuid, rest) = name.split_at(36);
    if !crate::catalog::looks_like_uuid(uuid) {
        return None;
    }
    let shard = match rest {
        "" => None,
        r if r.starts_with('-') && r.len() > 1 && r[1..].chars().all(|c| c.is_ascii_alphanumeric()) => Some(r[1..].to_ascii_lowercase()),
        _ => return None,
    };
    Some((uuid.to_ascii_lowercase(), shard))
}

pub fn scan(config_root: &Path) -> Vec<LocalAccount> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(config_root) else { return out };
    for e in rd.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        if !ft.is_dir() {
            continue;
        }
        let name = e.file_name().to_string_lossy().to_string();
        if let Some((puuid, shard)) = parse_account_folder(&name) {
            let modified = newest_mtime(&e.path());
            out.push(LocalAccount { puuid, shard, folder: name, path: e.path(), modified });
        }
    }
    out.sort_by(|a, b| b.modified.cmp(&a.modified).then(a.folder.cmp(&b.folder)));
    out
}

fn newest_mtime(dir: &Path) -> Option<SystemTime> {
    let mut best = std::fs::metadata(dir).and_then(|m| m.modified()).ok();
    let mut stack = vec![dir.to_path_buf()];
    let mut budget = 200usize;
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            budget = budget.saturating_sub(1);
            if budget == 0 {
                return best;
            }
            if let Ok(m) = e.metadata() {
                if m.is_dir() {
                    stack.push(e.path());
                } else if let Ok(t) = m.modified() {
                    best = Some(best.map_or(t, |b: SystemTime| b.max(t)));
                }
            }
        }
    }
    best
}

pub fn read_last_known_user(config_root: &Path) -> Option<String> {
    for p in [config_root.join("RiotLocalMachine.ini"), config_root.join("Windows").join("RiotLocalMachine.ini")] {
        if let Ok(bytes) = std::fs::read(&p) {
            let doc = IniDoc::parse_bytes(&bytes);
            if let Some(v) = doc.get_first("UserInfo", "LastKnownUser") {
                let v = v.trim().to_ascii_lowercase();
                if crate::catalog::looks_like_uuid(&v) {
                    return Some(v);
                }
            }
        }
    }
    None
}

pub fn detect(config_root: &Path, manual: Option<&str>) -> Detection {
    let accounts = scan(config_root);
    let last_known_user = read_last_known_user(config_root);
    let mut det = Detection { config_exists: config_root.is_dir(), accounts, last_known_user, active: None };
    if let Some(lku) = det.last_known_user.clone()
        && let Some(a) = det.find(&lku)
    {
        det.active = Some((a.clone(), DetectMethod::LastKnownUser));
        return det;
    }
    if let Some(m) = manual
        && let Some(a) = det.find(m)
    {
        det.active = Some((a.clone(), DetectMethod::Manual));
        return det;
    }
    let distinct: std::collections::BTreeSet<&str> = det.accounts.iter().map(|a| a.puuid.as_str()).collect();
    if distinct.len() == 1 {
        det.active = Some((det.accounts[0].clone(), DetectMethod::SingleFolder));
    } else if let Some(a) = det.accounts.first() {
        det.active = Some((a.clone(), DetectMethod::MostRecentFolder));
    }
    det
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountSwitch {
    pub from: Option<String>,
    pub to: String,
}

/// Remember every locally detected account and detect account switches.
pub fn sync_with_db(db: &Db, det: &Detection) -> Result<Option<AccountSwitch>> {
    for a in &det.accounts {
        db.touch_account(&a.puuid, a.shard.as_deref())?;
    }
    let Some((active, method)) = &det.active else { return Ok(None) };
    if !method.is_confident() {
        return Ok(None);
    }
    db.mark_account_seen(&active.puuid)?;
    let prev = db.meta_get("last_active_puuid")?;
    if prev.as_deref() != Some(active.puuid.as_str()) {
        db.meta_set("last_active_puuid", &active.puuid)?;
        if prev.is_some() {
            db.record_switch(prev.as_deref(), &active.puuid)?;
            return Ok(Some(AccountSwitch { from: prev, to: active.puuid.clone() }));
        }
    }
    Ok(None)
}

/// Parse `Name#TAG`.
pub fn parse_riot_id(s: &str) -> Option<(String, String)> {
    let (n, t) = s.trim().rsplit_once('#')?;
    let (n, t) = (n.trim(), t.trim());
    if n.is_empty() || t.is_empty() || n.chars().count() > 16 || t.chars().count() > 5 || t.chars().count() < 2 {
        return None;
    }
    Some((n.to_string(), t.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_folder_names() {
        let (p, s) = parse_account_folder("0d1a7e4b-1111-2222-3333-444455556666-eu").unwrap();
        assert_eq!(p, "0d1a7e4b-1111-2222-3333-444455556666");
        assert_eq!(s.as_deref(), Some("eu"));
        assert!(parse_account_folder("Windows").is_none());
        assert!(parse_account_folder("0d1a7e4b-1111-2222-3333-444455556666").unwrap().1.is_none());
        assert!(parse_account_folder("0d1a7e4b-1111-2222-3333-444455556666_x").is_none());
    }

    #[test]
    fn parses_riot_ids() {
        assert_eq!(parse_riot_id("Player One#EUW"), Some(("Player One".into(), "EUW".into())));
        assert!(parse_riot_id("noTag").is_none());
        assert!(parse_riot_id("#TAG").is_none());
    }
}
