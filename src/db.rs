//! SQLite storage for accounts, profiles, backups, the operation journal,
//! cached API responses and match history.

use crate::profile::Profile;
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

const SCHEMA_VERSION: i64 = 1;

pub struct Db {
    pub conn: Connection,
}

#[derive(Debug, Clone, Default)]
pub struct AccountRow {
    /// PUUID as used by the local VALORANT config folder.
    pub puuid: String,
    pub game_name: Option<String>,
    pub tag_line: Option<String>,
    pub shard: Option<String>,
    /// PUUID as returned by the Riot API for this app (may differ if encrypted).
    pub api_puuid: Option<String>,
    /// How the Riot ID was obtained: local | manual | api | rso
    pub source: String,
    pub verified: bool,
    /// Player opted in via RSO (required for VALORANT match data).
    pub consent: bool,
    pub consent_at: Option<String>,
    pub preferred_profile: Option<String>,
    pub first_seen: String,
    pub last_seen: String,
}

impl AccountRow {
    pub fn riot_id(&self) -> Option<String> {
        match (&self.game_name, &self.tag_line) {
            (Some(n), Some(t)) => Some(format!("{n}#{t}")),
            _ => None,
        }
    }

    pub fn display(&self) -> String {
        self.riot_id().unwrap_or_else(|| format!("Unknown ({})", short_id(&self.puuid)))
    }
}

pub fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

#[derive(Debug, Clone)]
pub struct ProfileRow {
    pub id: i64,
    pub name: String,
    pub created_at: String,
    pub updated_at: String,
    pub favorite: bool,
    pub source: String,
    pub profile: Profile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupKind {
    /// First snapshot VALSYNC ever took of an account ("Restore original settings").
    Original,
    /// Automatic snapshot before a transfer/restore.
    PreChange,
    /// Snapshot after a successful transfer (used by redo).
    PostChange,
    /// User-requested backup.
    Manual,
}

impl BackupKind {
    pub fn as_str(self) -> &'static str {
        match self {
            BackupKind::Original => "original",
            BackupKind::PreChange => "pre-change",
            BackupKind::PostChange => "post-change",
            BackupKind::Manual => "manual",
        }
    }
    pub fn parse(s: &str) -> BackupKind {
        match s {
            "original" => BackupKind::Original,
            "post-change" => BackupKind::PostChange,
            "manual" => BackupKind::Manual,
            _ => BackupKind::PreChange,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BackupRow {
    pub id: i64,
    pub account_puuid: String,
    /// Config folder name (e.g. `<puuid>-eu`) the backup belongs to.
    pub folder: String,
    pub kind: BackupKind,
    pub label: String,
    pub created_at: String,
    pub patch: Option<String>,
    pub manifest_sha: String,
    pub file_count: i64,
    pub total_bytes: i64,
    pub corrupted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupFile {
    pub rel_path: String,
    pub existed: bool,
    pub sha256: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpStatus {
    Pending,
    Committed,
    RolledBack,
    Recovered,
    Failed,
}

impl OpStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            OpStatus::Pending => "pending",
            OpStatus::Committed => "committed",
            OpStatus::RolledBack => "rolled-back",
            OpStatus::Recovered => "recovered",
            OpStatus::Failed => "failed",
        }
    }
    pub fn parse(s: &str) -> OpStatus {
        match s {
            "pending" => OpStatus::Pending,
            "committed" => OpStatus::Committed,
            "rolled-back" => OpStatus::RolledBack,
            "recovered" => OpStatus::Recovered,
            _ => OpStatus::Failed,
        }
    }
}

#[derive(Debug, Clone)]
pub struct OpRow {
    pub id: i64,
    pub account_puuid: String,
    pub folder: String,
    /// load | restore | undo | redo
    pub kind: String,
    pub profile_name: Option<String>,
    pub pre_backup_id: i64,
    pub post_backup_id: Option<i64>,
    pub status: OpStatus,
    pub created_at: String,
    pub undone: bool,
    pub redo_dead: bool,
    pub details: String,
}

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(data))
}

pub fn manifest_hash(files: &[BackupFile]) -> String {
    let mut sorted: Vec<&BackupFile> = files.iter().collect();
    sorted.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    let mut s = String::new();
    for f in sorted {
        s.push_str(&format!("{}\t{}\t{}\n", f.rel_path, f.existed as u8, f.sha256));
    }
    sha256_hex(s.as_bytes())
}

impl Db {
    pub fn open(path: &Path) -> Result<Db> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).with_context(|| format!("cannot create {}", p.display()))?;
        }
        let conn = Connection::open(path).with_context(|| format!("cannot open database {}", path.display()))?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Db> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Db> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if v > SCHEMA_VERSION {
            bail!("database was created by a newer VALSYNC (schema v{v}); please update VALSYNC");
        }
        if v < 1 {
            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS accounts (
                    puuid TEXT PRIMARY KEY,
                    game_name TEXT, tag_line TEXT, shard TEXT, api_puuid TEXT,
                    source TEXT NOT NULL DEFAULT 'local',
                    verified INTEGER NOT NULL DEFAULT 0,
                    consent INTEGER NOT NULL DEFAULT 0,
                    consent_at TEXT,
                    preferred_profile TEXT,
                    first_seen TEXT NOT NULL, last_seen TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS account_events (
                    id INTEGER PRIMARY KEY, ts TEXT NOT NULL, from_puuid TEXT, to_puuid TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS profiles (
                    id INTEGER PRIMARY KEY,
                    name TEXT NOT NULL UNIQUE COLLATE NOCASE,
                    created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
                    favorite INTEGER NOT NULL DEFAULT 0,
                    source TEXT NOT NULL DEFAULT 'captured',
                    data TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS backups (
                    id INTEGER PRIMARY KEY,
                    account_puuid TEXT NOT NULL,
                    folder TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    label TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    patch TEXT,
                    manifest_sha TEXT NOT NULL,
                    file_count INTEGER NOT NULL,
                    total_bytes INTEGER NOT NULL,
                    corrupted INTEGER NOT NULL DEFAULT 0
                );
                CREATE INDEX IF NOT EXISTS backups_account ON backups(account_puuid, id);
                CREATE TABLE IF NOT EXISTS backup_files (
                    backup_id INTEGER NOT NULL REFERENCES backups(id) ON DELETE CASCADE,
                    rel_path TEXT NOT NULL,
                    existed INTEGER NOT NULL,
                    sha256 TEXT NOT NULL,
                    data BLOB NOT NULL,
                    PRIMARY KEY (backup_id, rel_path)
                );
                CREATE TABLE IF NOT EXISTS operations (
                    id INTEGER PRIMARY KEY,
                    account_puuid TEXT NOT NULL,
                    folder TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    profile_name TEXT,
                    pre_backup_id INTEGER NOT NULL REFERENCES backups(id),
                    post_backup_id INTEGER REFERENCES backups(id),
                    status TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    finished_at TEXT,
                    undone INTEGER NOT NULL DEFAULT 0,
                    redo_dead INTEGER NOT NULL DEFAULT 0,
                    details TEXT NOT NULL DEFAULT ''
                );
                CREATE INDEX IF NOT EXISTS ops_account ON operations(account_puuid, id);
                CREATE TABLE IF NOT EXISTS matches (
                    match_id TEXT NOT NULL,
                    owner_puuid TEXT NOT NULL,
                    started_at INTEGER NOT NULL,
                    queue TEXT,
                    data TEXT NOT NULL,
                    fetched_at TEXT NOT NULL,
                    PRIMARY KEY (match_id, owner_puuid)
                );
                CREATE TABLE IF NOT EXISTS cache (
                    key TEXT PRIMARY KEY, value TEXT NOT NULL, expires_at INTEGER NOT NULL
                );
                CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                PRAGMA user_version = 1;
                "#,
            )?;
        }
        Ok(Db { conn })
    }

    // ---------------------------------------------------------------- meta
    pub fn meta_get(&self, key: &str) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0)).optional()?)
    }
    pub fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute("INSERT INTO meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key, value])?;
        Ok(())
    }

    // ---------------------------------------------------------------- cache
    pub fn cache_get(&self, key: &str) -> Result<Option<String>> {
        let now = chrono::Utc::now().timestamp();
        Ok(self.conn.query_row("SELECT value FROM cache WHERE key=?1 AND expires_at > ?2", params![key, now], |r| r.get(0)).optional()?)
    }
    pub fn cache_put(&self, key: &str, value: &str, ttl_secs: i64) -> Result<()> {
        let exp = chrono::Utc::now().timestamp() + ttl_secs;
        self.conn.execute(
            "INSERT INTO cache(key,value,expires_at) VALUES(?1,?2,?3) ON CONFLICT(key) DO UPDATE SET value=excluded.value, expires_at=excluded.expires_at",
            params![key, value, exp],
        )?;
        Ok(())
    }
    pub fn cache_purge(&self) -> Result<usize> {
        Ok(self.conn.execute("DELETE FROM cache WHERE expires_at <= ?1", [chrono::Utc::now().timestamp()])?)
    }

    // ---------------------------------------------------------------- accounts
    fn account_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<AccountRow> {
        Ok(AccountRow {
            puuid: r.get(0)?,
            game_name: r.get(1)?,
            tag_line: r.get(2)?,
            shard: r.get(3)?,
            api_puuid: r.get(4)?,
            source: r.get(5)?,
            verified: r.get::<_, i64>(6)? != 0,
            consent: r.get::<_, i64>(7)? != 0,
            consent_at: r.get(8)?,
            preferred_profile: r.get(9)?,
            first_seen: r.get(10)?,
            last_seen: r.get(11)?,
        })
    }

    const ACC_COLS: &'static str =
        "puuid,game_name,tag_line,shard,api_puuid,source,verified,consent,consent_at,preferred_profile,first_seen,last_seen";

    pub fn accounts(&self) -> Result<Vec<AccountRow>> {
        let mut st = self.conn.prepare(&format!("SELECT {} FROM accounts ORDER BY last_seen DESC", Self::ACC_COLS))?;
        let rows = st.query_map([], Self::account_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn account(&self, puuid: &str) -> Result<Option<AccountRow>> {
        Ok(self
            .conn
            .query_row(&format!("SELECT {} FROM accounts WHERE puuid=?1", Self::ACC_COLS), [puuid], Self::account_from_row)
            .optional()?)
    }

    /// Insert an account seen locally, or refresh its last-seen time.
    pub fn touch_account(&self, puuid: &str, shard: Option<&str>) -> Result<bool> {
        let n = now();
        let inserted = self.conn.execute(
            "INSERT INTO accounts(puuid, shard, source, first_seen, last_seen) VALUES(?1,?2,'local',?3,?3) ON CONFLICT(puuid) DO NOTHING",
            params![puuid, shard, n],
        )? > 0;
        if !inserted {
            self.conn.execute("UPDATE accounts SET shard=COALESCE(shard, ?2) WHERE puuid=?1", params![puuid, shard])?;
        }
        Ok(inserted)
    }

    pub fn mark_account_seen(&self, puuid: &str) -> Result<()> {
        self.conn.execute("UPDATE accounts SET last_seen=?2 WHERE puuid=?1", params![puuid, now()])?;
        Ok(())
    }

    pub fn set_account_identity(&self, puuid: &str, name: &str, tag: &str, source: &str, verified: bool, api_puuid: Option<&str>) -> Result<()> {
        self.touch_account(puuid, None)?;
        self.conn.execute(
            "UPDATE accounts SET game_name=?2, tag_line=?3, source=?4, verified=?5, api_puuid=COALESCE(?6, api_puuid) WHERE puuid=?1",
            params![puuid, name, tag, source, verified as i64, api_puuid],
        )?;
        Ok(())
    }

    pub fn set_account_shard(&self, puuid: &str, shard: &str) -> Result<()> {
        self.conn.execute("UPDATE accounts SET shard=?2 WHERE puuid=?1", params![puuid, shard])?;
        Ok(())
    }

    pub fn set_account_consent(&self, puuid: &str, consent: bool) -> Result<()> {
        let at = if consent { Some(now()) } else { None };
        self.conn.execute("UPDATE accounts SET consent=?2, consent_at=?3 WHERE puuid=?1", params![puuid, consent as i64, at])?;
        Ok(())
    }

    pub fn set_preferred_profile(&self, puuid: &str, profile: Option<&str>) -> Result<()> {
        self.conn.execute("UPDATE accounts SET preferred_profile=?2 WHERE puuid=?1", params![puuid, profile])?;
        Ok(())
    }

    pub fn forget_account(&self, puuid: &str) -> Result<()> {
        self.conn.execute("DELETE FROM accounts WHERE puuid=?1", [puuid])?;
        self.conn.execute("DELETE FROM matches WHERE owner_puuid=?1", [puuid])?;
        Ok(())
    }

    pub fn record_switch(&self, from: Option<&str>, to: &str) -> Result<()> {
        self.conn.execute("INSERT INTO account_events(ts, from_puuid, to_puuid) VALUES(?1,?2,?3)", params![now(), from, to])?;
        Ok(())
    }

    pub fn switch_events(&self, limit: usize) -> Result<Vec<(String, Option<String>, String)>> {
        let mut st = self.conn.prepare("SELECT ts, from_puuid, to_puuid FROM account_events ORDER BY id DESC LIMIT ?1")?;
        let rows = st.query_map([limit as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // ---------------------------------------------------------------- profiles
    fn profile_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<(i64, String, String, String, bool, String, String)> {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get::<_, i64>(4)? != 0, r.get(5)?, r.get(6)?))
    }

    fn build_profile_row(t: (i64, String, String, String, bool, String, String)) -> Result<ProfileRow> {
        let mut profile: Profile = serde_json::from_str(&t.6).with_context(|| format!("profile '{}' is corrupted in the database", t.1))?;
        profile.name = t.1.clone();
        Ok(ProfileRow { id: t.0, name: t.1, created_at: t.2, updated_at: t.3, favorite: t.4, source: t.5, profile })
    }

    pub fn profiles(&self) -> Result<Vec<ProfileRow>> {
        let mut st = self.conn.prepare(
            "SELECT id,name,created_at,updated_at,favorite,source,data FROM profiles ORDER BY favorite DESC, updated_at DESC",
        )?;
        let rows = st.query_map([], Self::profile_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter().map(Self::build_profile_row).collect()
    }

    pub fn profile(&self, name: &str) -> Result<Option<ProfileRow>> {
        let row = self
            .conn
            .query_row(
                "SELECT id,name,created_at,updated_at,favorite,source,data FROM profiles WHERE name=?1 COLLATE NOCASE",
                [name],
                Self::profile_from_row,
            )
            .optional()?;
        row.map(Self::build_profile_row).transpose()
    }

    pub fn save_profile(&self, p: &Profile, source: &str, overwrite: bool) -> Result<i64> {
        let data = serde_json::to_string(p)?;
        let n = now();
        if let Some(existing) = self.profile(&p.name)? {
            if !overwrite {
                bail!("a profile named '{}' already exists (use --overwrite to replace it)", existing.name);
            }
            self.conn.execute("UPDATE profiles SET data=?2, updated_at=?3, source=?4 WHERE id=?1", params![existing.id, data, n, source])?;
            return Ok(existing.id);
        }
        self.conn.execute(
            "INSERT INTO profiles(name, created_at, updated_at, favorite, source, data) VALUES(?1,?2,?2,0,?3,?4)",
            params![p.name, n, source, data],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn rename_profile(&self, old: &str, new: &str) -> Result<()> {
        let new = crate::profile::validate_name(new)?;
        let row = self.profile(old)?.with_context(|| format!("profile '{old}' not found"))?;
        if let Some(other) = self.profile(&new)?
            && other.id != row.id
        {
            bail!("a profile named '{}' already exists", other.name);
        }
        let mut p = row.profile.clone();
        p.name = new.clone();
        self.conn.execute(
            "UPDATE profiles SET name=?2, data=?3, updated_at=?4 WHERE id=?1",
            params![row.id, new, serde_json::to_string(&p)?, now()],
        )?;
        self.conn.execute("UPDATE accounts SET preferred_profile=?2 WHERE preferred_profile=?1 COLLATE NOCASE", params![row.name, new])?;
        Ok(())
    }

    pub fn delete_profile(&self, name: &str) -> Result<()> {
        let n = self.conn.execute("DELETE FROM profiles WHERE name=?1 COLLATE NOCASE", [name])?;
        if n == 0 {
            bail!("profile '{name}' not found");
        }
        self.conn.execute("UPDATE accounts SET preferred_profile=NULL WHERE preferred_profile=?1 COLLATE NOCASE", [name])?;
        Ok(())
    }

    pub fn set_favorite(&self, name: &str, fav: bool) -> Result<()> {
        let n = self.conn.execute("UPDATE profiles SET favorite=?2 WHERE name=?1 COLLATE NOCASE", params![name, fav as i64])?;
        if n == 0 {
            bail!("profile '{name}' not found");
        }
        Ok(())
    }

    // ---------------------------------------------------------------- backups
    pub fn insert_backup(
        &mut self,
        account_puuid: &str,
        folder: &str,
        kind: BackupKind,
        label: &str,
        patch: Option<&str>,
        files: &[BackupFile],
    ) -> Result<i64> {
        let manifest = manifest_hash(files);
        let total: i64 = files.iter().map(|f| f.data.len() as i64).sum();
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO backups(account_puuid, folder, kind, label, created_at, patch, manifest_sha, file_count, total_bytes) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![account_puuid, folder, kind.as_str(), label, now(), patch, manifest, files.len() as i64, total],
        )?;
        let id = tx.last_insert_rowid();
        {
            let mut st = tx.prepare("INSERT INTO backup_files(backup_id, rel_path, existed, sha256, data) VALUES(?1,?2,?3,?4,?5)")?;
            for f in files {
                st.execute(params![id, f.rel_path, f.existed as i64, f.sha256, f.data])?;
            }
        }
        tx.commit()?;
        Ok(id)
    }

    fn backup_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<BackupRow> {
        Ok(BackupRow {
            id: r.get(0)?,
            account_puuid: r.get(1)?,
            folder: r.get(2)?,
            kind: BackupKind::parse(&r.get::<_, String>(3)?),
            label: r.get(4)?,
            created_at: r.get(5)?,
            patch: r.get(6)?,
            manifest_sha: r.get(7)?,
            file_count: r.get(8)?,
            total_bytes: r.get(9)?,
            corrupted: r.get::<_, i64>(10)? != 0,
        })
    }

    const BK_COLS: &'static str = "id,account_puuid,folder,kind,label,created_at,patch,manifest_sha,file_count,total_bytes,corrupted";

    pub fn backup(&self, id: i64) -> Result<Option<BackupRow>> {
        Ok(self.conn.query_row(&format!("SELECT {} FROM backups WHERE id=?1", Self::BK_COLS), [id], Self::backup_from_row).optional()?)
    }

    pub fn backups(&self, account: Option<&str>) -> Result<Vec<BackupRow>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {} FROM backups WHERE (?1 IS NULL OR account_puuid=?1) ORDER BY id DESC",
            Self::BK_COLS
        ))?;
        let rows = st.query_map([account], Self::backup_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn backup_count(&self, account: Option<&str>) -> Result<i64> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM backups WHERE (?1 IS NULL OR account_puuid=?1)", [account], |r| r.get(0))?)
    }

    pub fn original_backup(&self, account: &str) -> Result<Option<BackupRow>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {} FROM backups WHERE account_puuid=?1 AND kind='original' ORDER BY id ASC LIMIT 1", Self::BK_COLS),
                [account],
                Self::backup_from_row,
            )
            .optional()?)
    }

    pub fn backup_files(&self, id: i64) -> Result<Vec<BackupFile>> {
        let mut st = self.conn.prepare("SELECT rel_path, existed, sha256, data FROM backup_files WHERE backup_id=?1 ORDER BY rel_path")?;
        let rows = st
            .query_map([id], |r| {
                Ok(BackupFile { rel_path: r.get(0)?, existed: r.get::<_, i64>(1)? != 0, sha256: r.get(2)?, data: r.get(3)? })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn mark_backup_corrupted(&self, id: i64, corrupted: bool) -> Result<()> {
        self.conn.execute("UPDATE backups SET corrupted=?2 WHERE id=?1", params![id, corrupted as i64])?;
        Ok(())
    }

    pub fn backup_referenced(&self, id: i64) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM operations WHERE pre_backup_id=?1 OR post_backup_id=?1",
            [id],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    pub fn delete_backup(&mut self, id: i64) -> Result<()> {
        let tx = self.conn.transaction()?;
        // Operations that reference the backup lose undo/redo ability; remove them.
        tx.execute("DELETE FROM operations WHERE pre_backup_id=?1 OR post_backup_id=?1", [id])?;
        tx.execute("DELETE FROM backup_files WHERE backup_id=?1", [id])?;
        let n = tx.execute("DELETE FROM backups WHERE id=?1", [id])?;
        tx.commit()?;
        if n == 0 {
            bail!("backup #{id} not found");
        }
        Ok(())
    }

    // ---------------------------------------------------------------- operations
    fn op_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<OpRow> {
        Ok(OpRow {
            id: r.get(0)?,
            account_puuid: r.get(1)?,
            folder: r.get(2)?,
            kind: r.get(3)?,
            profile_name: r.get(4)?,
            pre_backup_id: r.get(5)?,
            post_backup_id: r.get(6)?,
            status: OpStatus::parse(&r.get::<_, String>(7)?),
            created_at: r.get(8)?,
            undone: r.get::<_, i64>(9)? != 0,
            redo_dead: r.get::<_, i64>(10)? != 0,
            details: r.get(11)?,
        })
    }

    const OP_COLS: &'static str =
        "id,account_puuid,folder,kind,profile_name,pre_backup_id,post_backup_id,status,created_at,undone,redo_dead,details";

    pub fn begin_op(&self, account: &str, folder: &str, kind: &str, profile: Option<&str>, pre_backup: i64, details: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO operations(account_puuid, folder, kind, profile_name, pre_backup_id, status, created_at, details) VALUES(?1,?2,?3,?4,?5,'pending',?6,?7)",
            params![account, folder, kind, profile, pre_backup, now(), details],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn finish_op(&self, id: i64, status: OpStatus, post_backup: Option<i64>) -> Result<()> {
        self.conn.execute(
            "UPDATE operations SET status=?2, post_backup_id=COALESCE(?3, post_backup_id), finished_at=?4 WHERE id=?1",
            params![id, status.as_str(), post_backup, now()],
        )?;
        Ok(())
    }

    pub fn op(&self, id: i64) -> Result<Option<OpRow>> {
        Ok(self.conn.query_row(&format!("SELECT {} FROM operations WHERE id=?1", Self::OP_COLS), [id], Self::op_from_row).optional()?)
    }

    pub fn ops(&self, account: Option<&str>, limit: usize) -> Result<Vec<OpRow>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {} FROM operations WHERE (?1 IS NULL OR account_puuid=?1) ORDER BY id DESC LIMIT ?2",
            Self::OP_COLS
        ))?;
        let rows = st.query_map(params![account, limit as i64], Self::op_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn pending_ops(&self) -> Result<Vec<OpRow>> {
        let mut st = self.conn.prepare(&format!("SELECT {} FROM operations WHERE status='pending' ORDER BY id", Self::OP_COLS))?;
        let rows = st.query_map([], Self::op_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Latest committed, not-yet-undone change for the account (undo target).
    pub fn undo_candidate(&self, account: &str) -> Result<Option<OpRow>> {
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT {} FROM operations WHERE account_puuid=?1 AND status='committed' AND undone=0 AND kind IN ('load','restore') AND post_backup_id IS NOT NULL ORDER BY id DESC LIMIT 1",
                    Self::OP_COLS
                ),
                [account],
                Self::op_from_row,
            )
            .optional()?)
    }

    /// Most recently undone change that can still be redone.
    pub fn redo_candidate(&self, account: &str) -> Result<Option<OpRow>> {
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT {} FROM operations WHERE account_puuid=?1 AND undone=1 AND redo_dead=0 AND status='committed' AND kind IN ('load','restore') AND post_backup_id IS NOT NULL ORDER BY id ASC LIMIT 1",
                    Self::OP_COLS
                ),
                [account],
                Self::op_from_row,
            )
            .optional()?)
    }

    pub fn set_op_undone(&self, id: i64, undone: bool) -> Result<()> {
        self.conn.execute("UPDATE operations SET undone=?2 WHERE id=?1", params![id, undone as i64])?;
        Ok(())
    }

    /// A new change invalidates the redo stack of the account.
    pub fn kill_redo(&self, account: &str) -> Result<()> {
        self.conn.execute("UPDATE operations SET redo_dead=1 WHERE account_puuid=?1 AND undone=1", [account])?;
        Ok(())
    }

    /// Keep only the newest `keep` automatic pre/post-change backups of the account.
    /// Original and manual backups are never pruned.
    pub fn prune_auto_backups(&mut self, account: &str, keep: u32) -> Result<usize> {
        if keep == 0 {
            return Ok(0);
        }
        let ids: Vec<i64> = {
            let mut st = self.conn.prepare(
                "SELECT id FROM backups WHERE account_puuid=?1 AND kind='pre-change' ORDER BY id DESC",
            )?;
            st.query_map([account], |r| r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut removed = 0;
        for id in ids.into_iter().skip(keep as usize) {
            // Post-change backups belonging to the same operations go too.
            let posts: Vec<i64> = {
                let mut st = self.conn.prepare("SELECT post_backup_id FROM operations WHERE pre_backup_id=?1 AND post_backup_id IS NOT NULL")?;
                st.query_map([id], |r| r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?
            };
            self.delete_backup(id)?;
            for p in posts {
                if self.backup(p)?.map(|b| b.kind == BackupKind::PostChange).unwrap_or(false) {
                    let _ = self.delete_backup(p);
                }
            }
            removed += 1;
        }
        Ok(removed)
    }

    // ---------------------------------------------------------------- matches
    pub fn store_match(&self, match_id: &str, owner: &str, started_at: i64, queue: &str, data: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO matches(match_id, owner_puuid, started_at, queue, data, fetched_at) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(match_id, owner_puuid) DO UPDATE SET data=excluded.data",
            params![match_id, owner, started_at, queue, data, now()],
        )?;
        Ok(())
    }

    pub fn has_match(&self, match_id: &str, owner: &str) -> Result<bool> {
        let n: i64 = self.conn.query_row("SELECT COUNT(*) FROM matches WHERE match_id=?1 AND owner_puuid=?2", params![match_id, owner], |r| r.get(0))?;
        Ok(n > 0)
    }

    pub fn matches(&self, owner: &str, limit: usize) -> Result<Vec<String>> {
        let mut st = self.conn.prepare("SELECT data FROM matches WHERE owner_puuid=?1 ORDER BY started_at DESC LIMIT ?2")?;
        let rows = st.query_map(params![owner, limit as i64], |r| r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}
