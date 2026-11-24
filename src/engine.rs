//! The VALSYNC settings engine: capture, transfer, backup, restore, undo/redo
//! and crash recovery.
//!
//! ## Transaction protocol
//!
//! Every change to an account's config folder runs through [`Engine::run_txn`]:
//!
//! 1. Refuse if VALORANT is running (it rewrites its config files on exit).
//! 2. Take an *original* snapshot the first time VALSYNC touches the account.
//! 3. Snapshot every affected file (pre-change backup, committed to SQLite).
//! 4. Insert a `pending` row in the operation journal.
//! 5. Write each file atomically (temp → fsync → rename).
//! 6. Re-read and verify every written file byte-for-byte.
//! 7. Snapshot the result (post-change backup, used by redo) and mark the
//!    operation `committed`.
//!
//! Any failure in 5–6 rolls every file back from the pre-change snapshot. If the
//! process dies in the middle, the `pending` journal row survives and
//! [`Engine::recover`] rolls the folder back on the next start.

use crate::accounts::{self, DetectMethod, Detection, LocalAccount};
use crate::appconfig::AppConfig;
use crate::catalog::{Category, Scope};
use crate::db::{self, BackupFile, BackupKind, BackupRow, Db, OpRow, OpStatus};
use crate::game::{self, ProcessProbe};
use crate::ini::IniDoc;
use crate::paths::{self, Paths};
use crate::profile::{CaptureReport, Profile, SettingEntry};
use crate::txn::{self, ApplyError, Faults, FileOp};
use anyhow::{Context, Result, anyhow, bail};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("VALORANT is running. Close the game (you can stay logged in to the Riot Client) and try again: VALORANT rewrites its config files when it exits.")]
    GameRunning,
    #[error("backup #{0} failed integrity verification: {1}. It will not be restored.")]
    Integrity(i64, String),
    #[error("backup #{backup} belongs to account {owner}, but the active account is {active}. Switch accounts in the Riot Client or pass the account explicitly.")]
    AccountMismatch { backup: i64, owner: String, active: String },
    #[error("the transfer failed and every file was rolled back to its previous state: {0}")]
    RolledBack(String),
    #[error("the transfer failed and automatic rollback ALSO failed ({0}). Your previous settings are safe in backup #{1}; run `valsync doctor` (recovery retries on next start).")]
    RollbackFailed(String, i64),
    #[error("simulated crash (test hook)")]
    SimulatedCrash,
    #[error("no active VALORANT account could be detected. Launch VALORANT once with the account, or select one under Manage Riot Accounts.")]
    NoActiveAccount,
    #[error("another VALSYNC instance is applying settings right now (lock file {0}).")]
    Locked(String),
}

#[derive(Debug, Clone)]
pub struct Target {
    pub puuid: String,
    pub folder: String,
    pub dir: PathBuf,
}

impl From<&LocalAccount> for Target {
    fn from(a: &LocalAccount) -> Self {
        Target { puuid: a.puuid.clone(), folder: a.folder.clone(), dir: a.path.clone() }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub file: String,
    pub section: String,
    pub key: String,
    pub old: Vec<String>,
    pub new: Vec<String>,
    pub category: Category,
    pub scope: Scope,
}

impl Change {
    pub fn old_display(&self) -> String {
        if self.old.is_empty() { "(unset)".into() } else { short(&self.old.join(" | ")) }
    }
    pub fn new_display(&self) -> String {
        if self.new.is_empty() { "(unset)".into() } else { short(&self.new.join(" | ")) }
    }
}

fn short(s: &str) -> String {
    if s.chars().count() > 48 { format!("{}…", s.chars().take(47).collect::<String>()) } else { s.to_string() }
}

#[derive(Debug, Clone, Default)]
pub struct LoadOptions {
    /// `None` = every supported category.
    pub categories: Option<BTreeSet<Category>>,
    /// Write cloud-synced values into the local cache too (best effort).
    pub include_cloud: bool,
}

#[derive(Debug, Clone, Default)]
pub struct LoadPlan {
    pub changes: Vec<Change>,
    pub ops: Vec<FileOp>,
    /// Cloud-synced settings that need to be set in-game manually.
    pub manual: Vec<SettingEntry>,
    pub unchanged: usize,
    pub new_keys: usize,
    pub new_files: Vec<String>,
    pub patch_warning: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TxnOutcome {
    pub op_id: i64,
    pub pre_backup: i64,
    pub post_backup: i64,
    pub files_written: usize,
    pub verified: usize,
}

#[derive(Debug, Clone)]
pub struct LoadOutcome {
    pub plan: LoadPlan,
    pub txn: Option<TxnOutcome>,
}

#[derive(Debug, Clone)]
pub struct RestoreOutcome {
    pub backup_id: i64,
    pub changes: Vec<Change>,
    pub txn: Option<TxnOutcome>,
}

#[derive(Debug, Clone, Default)]
pub struct RecoveryReport {
    pub recovered_ops: Vec<i64>,
    pub failed_ops: Vec<(i64, String)>,
    pub deferred: bool,
    pub stray_tmp_removed: usize,
}

impl RecoveryReport {
    pub fn is_empty(&self) -> bool {
        self.recovered_ops.is_empty() && self.failed_ops.is_empty() && !self.deferred && self.stray_tmp_removed == 0
    }
}

#[derive(Debug, Clone)]
pub struct IntegrityReport {
    pub backup_id: i64,
    pub ok: bool,
    pub problems: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SaveOutcome {
    pub profile: Profile,
    pub report: CaptureReport,
    pub overwritten: bool,
    pub account: String,
}

pub struct Engine {
    pub paths: Paths,
    pub db: Db,
    pub config: AppConfig,
    pub probe: ProcessProbe,
    pub faults: Faults,
    pub startup_recovery: RecoveryReport,
}

/// Guard for the single-writer lock file.
struct WriteLock(PathBuf);
impl Drop for WriteLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

impl Engine {
    /// Open the data directory, database and run crash recovery.
    pub fn open(paths: Paths) -> Result<Engine> {
        std::fs::create_dir_all(&paths.data_dir).with_context(|| format!("cannot create data dir {}", paths.data_dir.display()))?;
        let config = AppConfig::load(&paths.config_path())?;
        let db = Db::open(&paths.db_path())?;
        let mut e = Engine { paths, db, config, probe: ProcessProbe::from_env(), faults: Faults::default(), startup_recovery: RecoveryReport::default() };
        e.startup_recovery = e.recover()?;
        let _ = e.db.cache_purge();
        Ok(e)
    }

    /// Construct with explicit parts (tests).
    pub fn with_parts(paths: Paths, db: Db, config: AppConfig, probe: ProcessProbe) -> Engine {
        Engine { paths, db, config, probe, faults: Faults::default(), startup_recovery: RecoveryReport::default() }
    }

    pub fn save_config(&self) -> Result<()> {
        self.config.save(&self.paths.config_path())
    }

    // ------------------------------------------------------------------ detection
    pub fn detection(&self) -> Detection {
        accounts::detect(&self.paths.valorant_config, self.config.manual_active_account.as_deref())
    }

    pub fn active_target(&self) -> Result<(Target, DetectMethod)> {
        let det = self.detection();
        match det.active {
            Some((a, m)) => Ok((Target::from(&a), m)),
            None => Err(EngineError::NoActiveAccount.into()),
        }
    }

    /// Resolve an account by PUUID, PUUID prefix, folder name or known Riot ID.
    pub fn resolve_target(&self, query: &str) -> Result<Target> {
        let det = self.detection();
        if let Some(a) = det.find(query) {
            return Ok(Target::from(a));
        }
        if let Some((n, t)) = accounts::parse_riot_id(query) {
            for acc in self.db.accounts()? {
                if acc.game_name.as_deref().map(|x| x.eq_ignore_ascii_case(&n)).unwrap_or(false)
                    && acc.tag_line.as_deref().map(|x| x.eq_ignore_ascii_case(&t)).unwrap_or(false)
                    && let Some(a) = det.find(&acc.puuid)
                {
                    return Ok(Target::from(a));
                }
            }
        }
        bail!("no local VALORANT config folder matches account '{query}'. Known folders: {}", det.accounts.iter().map(|a| a.folder.as_str()).collect::<Vec<_>>().join(", "))
    }

    pub fn current_patch(&self) -> Option<String> {
        game::detect_patch(&self.paths.valorant_logs).or_else(|| self.db.meta_get("content_version").ok().flatten().and_then(|v| game::find_release(&v)))
    }

    pub fn ensure_game_closed(&self) -> Result<()> {
        if self.probe.status().valorant {
            return Err(EngineError::GameRunning.into());
        }
        Ok(())
    }

    fn acquire_lock(&self) -> Result<WriteLock> {
        let p = self.paths.lock_path();
        for _ in 0..2 {
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&p) {
                Ok(mut f) => {
                    use std::io::Write;
                    let _ = writeln!(f, "{}", std::process::id());
                    return Ok(WriteLock(p));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Stale lock (crashed process): older than 2 minutes.
                    let stale = std::fs::metadata(&p)
                        .and_then(|m| m.modified())
                        .map(|t| t.elapsed().map(|d| d.as_secs() > 120).unwrap_or(true))
                        .unwrap_or(true);
                    if stale {
                        let _ = std::fs::remove_file(&p);
                        continue;
                    }
                    return Err(EngineError::Locked(p.display().to_string()).into());
                }
                Err(e) => return Err(e).with_context(|| format!("cannot create lock {}", p.display())),
            }
        }
        Err(EngineError::Locked(p.display().to_string()).into())
    }

    // ------------------------------------------------------------------ file access
    /// All `.ini` files in an account folder, keyed by `/`-separated relative path.
    pub fn read_account_files(dir: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
        let mut out = BTreeMap::new();
        if !dir.is_dir() {
            bail!("account config folder {} does not exist", dir.display());
        }
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).with_context(|| format!("cannot list {}", d.display()))?.flatten() {
                let p = e.path();
                let ft = e.file_type()?;
                if ft.is_dir() {
                    stack.push(p);
                } else if ft.is_file() && p.extension().map(|x| x.eq_ignore_ascii_case("ini")).unwrap_or(false) {
                    let rel = p.strip_prefix(dir).unwrap_or(&p).to_string_lossy().replace('\\', "/");
                    let bytes = std::fs::read(&p).with_context(|| format!("cannot read {}", p.display()))?;
                    out.insert(rel, bytes);
                }
            }
        }
        Ok(out)
    }

    fn parse_files(files: &BTreeMap<String, Vec<u8>>) -> BTreeMap<String, IniDoc> {
        files.iter().map(|(k, v)| (k.clone(), IniDoc::parse_bytes(v))).collect()
    }

    /// Snapshot every `.ini` in the folder plus `extra` paths that may be created.
    pub fn snapshot(&self, t: &Target, extra: &[String]) -> Result<Vec<BackupFile>> {
        let files = Self::read_account_files(&t.dir)?;
        let mut out: Vec<BackupFile> =
            files.into_iter().map(|(rel, data)| BackupFile { sha256: db::sha256_hex(&data), rel_path: rel, existed: true, data }).collect();
        for rel in extra {
            if !out.iter().any(|f| f.rel_path.eq_ignore_ascii_case(rel)) {
                let path = t.dir.join(paths::safe_relative(rel).ok_or_else(|| anyhow!("unsafe path {rel}"))?);
                match std::fs::read(&path) {
                    Ok(data) => out.push(BackupFile { sha256: db::sha256_hex(&data), rel_path: rel.clone(), existed: true, data }),
                    Err(_) => out.push(BackupFile { rel_path: rel.clone(), existed: false, sha256: db::sha256_hex(b""), data: Vec::new() }),
                }
            }
        }
        out.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
        Ok(out)
    }

    fn ensure_original(&mut self, t: &Target) -> Result<()> {
        if self.db.original_backup(&t.puuid)?.is_none() {
            let files = self.snapshot(t, &[])?;
            let patch = self.current_patch();
            self.db.insert_backup(&t.puuid, &t.folder, BackupKind::Original, "Original settings (first seen by VALSYNC)", patch.as_deref(), &files)?;
        }
        Ok(())
    }

    // ------------------------------------------------------------------ save
    pub fn capture(&self, t: &Target, name: &str, categories: Option<&BTreeSet<Category>>) -> Result<(Profile, CaptureReport)> {
        let files = Self::read_account_files(&t.dir)?;
        let docs = Self::parse_files(&files);
        let (mut p, rep) = Profile::capture(name, &docs, self.current_patch(), Some(&t.puuid));
        if let Some(cats) = categories {
            p.settings.retain(|s| cats.contains(&s.category));
        }
        Ok((p, rep))
    }

    pub fn save_profile(&mut self, t: &Target, name: &str, overwrite: bool, categories: Option<&BTreeSet<Category>>, notes: Option<String>, manual: Vec<crate::profile::ManualValue>) -> Result<SaveOutcome> {
        let name = crate::profile::validate_name(name)?;
        let existed = self.db.profile(&name)?.is_some();
        let (mut p, report) = self.capture(t, &name, categories)?;
        if p.settings.is_empty() {
            bail!("no transferable settings were found in {} (launch VALORANT once with this account so it writes its config)", t.dir.display());
        }
        p.notes = notes;
        p.manual = manual;
        self.db.save_profile(&p, "captured", overwrite)?;
        self.db.meta_set("last_saved_profile", &name)?;
        // Snapshot the source account's original settings as well; harmless and useful.
        let _ = self.ensure_original(t);
        Ok(SaveOutcome { profile: p, report, overwritten: existed, account: t.puuid.clone() })
    }

    // ------------------------------------------------------------------ load
    pub fn plan_load(&self, profile: &Profile, t: &Target, opts: &LoadOptions) -> Result<LoadPlan> {
        let originals = Self::read_account_files(&t.dir)?;
        let mut docs = Self::parse_files(&originals);
        let mut plan = LoadPlan { patch_warning: game::patch_warning(profile.patch.as_deref(), self.current_patch().as_deref()), ..Default::default() };
        let mut touched: BTreeSet<String> = BTreeSet::new();
        for entry in &profile.settings {
            if let Some(cats) = &opts.categories
                && !cats.contains(&entry.category)
            {
                continue;
            }
            if paths::safe_relative(&entry.file).is_none() {
                continue;
            }
            if entry.scope == Scope::Cloud && !opts.include_cloud {
                plan.manual.push(entry.clone());
                continue;
            }
            // Map the profile's file name onto the destination's actual casing.
            let file_key = docs.keys().find(|k| k.eq_ignore_ascii_case(&entry.file)).cloned().unwrap_or_else(|| entry.file.clone());
            if !docs.contains_key(&file_key) {
                plan.new_files.push(file_key.clone());
            }
            let doc = docs.entry(file_key.clone()).or_default();
            let old = doc.get(&entry.section, &entry.key);
            if old == entry.values {
                plan.unchanged += 1;
                continue;
            }
            if old.is_empty() {
                plan.new_keys += 1;
            }
            doc.set(&entry.section, &entry.key, &entry.values);
            touched.insert(file_key.clone());
            plan.changes.push(Change {
                file: file_key,
                section: entry.section.clone(),
                key: entry.key.clone(),
                old,
                new: entry.values.clone(),
                category: entry.category,
                scope: entry.scope,
            });
        }
        if opts.include_cloud {
            // Cloud values written to the cache still need confirming in-game.
            plan.manual = profile.settings.iter().filter(|s| s.scope == Scope::Cloud && opts.categories.as_ref().map(|c| c.contains(&s.category)).unwrap_or(true)).cloned().collect();
        }
        plan.new_files.sort();
        plan.new_files.dedup();
        for rel in touched {
            let bytes = docs[&rel].to_bytes();
            if originals.get(&rel) != Some(&bytes) {
                plan.ops.push(FileOp { rel_path: rel, contents: Some(bytes) });
            }
        }
        Ok(plan)
    }

    pub fn apply_load(&mut self, profile: &Profile, t: &Target, opts: &LoadOptions) -> Result<LoadOutcome> {
        let plan = self.plan_load(profile, t, opts)?;
        if plan.ops.is_empty() {
            return Ok(LoadOutcome { plan, txn: None });
        }
        let cats: Vec<&str> = opts.categories.as_ref().map(|c| c.iter().map(|c| c.slug()).collect()).unwrap_or_else(|| vec!["all"]);
        let details = format!("{} change(s); categories: {}", plan.changes.len(), cats.join(","));
        let txn = self.run_txn(t, "load", Some(&profile.name), &plan.ops, &details)?;
        let verified = self.verify_changes(t, &plan.changes)?;
        if !verified.is_empty() {
            // Should be impossible after byte verification, but never leave a half-applied state.
            self.restore_files_raw(t, txn.pre_backup)?;
            self.db.finish_op(txn.op_id, OpStatus::RolledBack, None)?;
            return Err(EngineError::RolledBack(format!("verification failed for {}", verified.join(", "))).into());
        }
        self.db.meta_set(&format!("last_loaded_profile:{}", t.puuid), &profile.name)?;
        Ok(LoadOutcome { plan, txn: Some(txn) })
    }

    /// Re-read files and check each change landed. Returns failing keys.
    pub fn verify_changes(&self, t: &Target, changes: &[Change]) -> Result<Vec<String>> {
        let files = Self::read_account_files(&t.dir)?;
        let docs = Self::parse_files(&files);
        let mut bad = Vec::new();
        for c in changes {
            let ok = docs.iter().find(|(k, _)| k.eq_ignore_ascii_case(&c.file)).map(|(_, d)| d.get(&c.section, &c.key) == c.new).unwrap_or(false);
            if !ok {
                bad.push(c.key.clone());
            }
        }
        Ok(bad)
    }

    // ------------------------------------------------------------------ transactions
    fn run_txn(&mut self, t: &Target, kind: &str, profile: Option<&str>, ops: &[FileOp], details: &str) -> Result<TxnOutcome> {
        self.ensure_game_closed()?;
        let _lock = self.acquire_lock()?;
        self.ensure_original(t)?;
        let rels: Vec<String> = ops.iter().map(|o| o.rel_path.clone()).collect();
        let pre_files = self.snapshot(t, &rels)?;
        let patch = self.current_patch();
        let label = match (kind, profile) {
            ("load", Some(p)) => format!("Before loading profile '{p}'"),
            ("undo", _) => "Before undo".to_string(),
            ("redo", _) => "Before redo".to_string(),
            (_, Some(p)) => format!("Before {kind} ({p})"),
            _ => format!("Before {kind}"),
        };
        let pre = self.db.insert_backup(&t.puuid, &t.folder, BackupKind::PreChange, &label, patch.as_deref(), &pre_files)?;
        let op_id = self.db.begin_op(&t.puuid, &t.folder, kind, profile, pre, details)?;

        match txn::apply_ops(&t.dir, ops, &self.faults) {
            Ok(()) => {}
            Err(ApplyError::Crashed) => return Err(EngineError::SimulatedCrash.into()),
            Err(ApplyError::Failed { error, .. }) => return Err(self.rollback(t, op_id, pre, format!("{error:#}"))),
        }
        // Byte-level verification of every written/deleted file.
        let mut problems = Vec::new();
        for op in ops {
            let path = t.dir.join(paths::safe_relative(&op.rel_path).expect("validated"));
            let actual = std::fs::read(&path).ok();
            if actual.as_deref() != op.contents.as_deref() {
                problems.push(op.rel_path.clone());
            }
        }
        if !problems.is_empty() {
            return Err(self.rollback(t, op_id, pre, format!("verification failed for {}", problems.join(", "))));
        }
        let post_files = self.snapshot(t, &rels)?;
        let post_label = match profile {
            Some(p) => format!("After {kind} '{p}'"),
            None => format!("After {kind}"),
        };
        let post = self.db.insert_backup(&t.puuid, &t.folder, BackupKind::PostChange, &post_label, patch.as_deref(), &post_files)?;
        self.db.finish_op(op_id, OpStatus::Committed, Some(post))?;
        if kind == "load" || kind == "restore" {
            self.db.kill_redo(&t.puuid)?;
        }
        let retention = self.config.auto_backup_retention;
        let _ = self.db.prune_auto_backups(&t.puuid, retention);
        Ok(TxnOutcome { op_id, pre_backup: pre, post_backup: post, files_written: ops.len(), verified: ops.len() })
    }

    fn rollback(&mut self, t: &Target, op_id: i64, pre: i64, reason: String) -> anyhow::Error {
        match self.restore_files_raw(t, pre) {
            Ok(()) => {
                let _ = self.db.finish_op(op_id, OpStatus::RolledBack, None);
                EngineError::RolledBack(reason).into()
            }
            Err(e) => {
                // Leave the journal entry pending so recovery retries on next start.
                EngineError::RollbackFailed(format!("{reason}; rollback: {e:#}"), pre).into()
            }
        }
    }

    /// Write a backup's files back verbatim (no journal, used by rollback/recovery).
    fn restore_files_raw(&self, t: &Target, backup_id: i64) -> Result<()> {
        let files = self.verified_backup_files(backup_id)?;
        let ops: Vec<FileOp> = files.iter().map(|f| FileOp { rel_path: f.rel_path.clone(), contents: if f.existed { Some(f.data.clone()) } else { None } }).collect();
        txn::apply_ops(&t.dir, &ops, &Faults::default()).map_err(|e| anyhow!("{e}"))?;
        for f in &files {
            let path = t.dir.join(paths::safe_relative(&f.rel_path).ok_or_else(|| anyhow!("unsafe path"))?);
            let now = std::fs::read(&path).ok();
            let expect = if f.existed { Some(f.data.as_slice()) } else { None };
            if now.as_deref() != expect {
                bail!("restored file {} does not match the backup", f.rel_path);
            }
        }
        txn::clean_stray_tmp(&t.dir);
        Ok(())
    }

    // ------------------------------------------------------------------ backups
    pub fn create_manual_backup(&mut self, t: &Target, label: &str) -> Result<i64> {
        let files = self.snapshot(t, &[])?;
        if files.is_empty() {
            bail!("no config files found in {}", t.dir.display());
        }
        let patch = self.current_patch();
        self.ensure_original(t)?;
        let label = if label.trim().is_empty() { "Manual backup".to_string() } else { label.trim().to_string() };
        self.db.insert_backup(&t.puuid, &t.folder, BackupKind::Manual, &label, patch.as_deref(), &files)
    }

    pub fn verify_backup(&self, id: i64) -> Result<IntegrityReport> {
        let row = self.db.backup(id)?.with_context(|| format!("backup #{id} not found"))?;
        let files = self.db.backup_files(id)?;
        let mut problems = Vec::new();
        if files.len() as i64 != row.file_count {
            problems.push(format!("expected {} file(s), found {}", row.file_count, files.len()));
        }
        for f in &files {
            if paths::safe_relative(&f.rel_path).is_none() {
                problems.push(format!("unsafe path '{}'", f.rel_path));
            }
            if f.existed && db::sha256_hex(&f.data) != f.sha256 {
                problems.push(format!("{} checksum mismatch", f.rel_path));
            }
        }
        if db::manifest_hash(&files) != row.manifest_sha {
            problems.push("manifest checksum mismatch".to_string());
        }
        let ok = problems.is_empty();
        let _ = self.db.mark_backup_corrupted(id, !ok);
        Ok(IntegrityReport { backup_id: id, ok, problems })
    }

    fn verified_backup_files(&self, id: i64) -> Result<Vec<BackupFile>> {
        let rep = self.verify_backup(id)?;
        if !rep.ok {
            return Err(EngineError::Integrity(id, rep.problems.join("; ")).into());
        }
        self.db.backup_files(id)
    }

    /// Diff between the account's current files and a backup.
    pub fn preview_backup(&self, backup_id: i64) -> Result<Vec<Change>> {
        let row = self.db.backup(backup_id)?.with_context(|| format!("backup #{backup_id} not found"))?;
        let dir = self.paths.account_dir(&row.folder);
        let current = if dir.is_dir() { Self::read_account_files(&dir)? } else { BTreeMap::new() };
        let files = self.db.backup_files(backup_id)?;
        let mut changes = Vec::new();
        for f in &files {
            let cur = current.get(&f.rel_path).map(|b| IniDoc::parse_bytes(b)).unwrap_or_default();
            let target = if f.existed { IniDoc::parse_bytes(&f.data) } else { IniDoc::default() };
            changes.extend(diff_docs(&f.rel_path, &cur, &target));
        }
        Ok(changes)
    }

    fn check_backup_account(&self, row: &BackupRow, allow_inactive: bool) -> Result<Target> {
        let det = self.detection();
        let local = det.accounts.iter().find(|a| a.folder == row.folder).cloned().ok_or_else(|| {
            anyhow!("the config folder '{}' for backup #{} no longer exists. Launch VALORANT once with that account first.", row.folder, row.id)
        })?;
        if local.puuid != row.account_puuid {
            bail!("backup #{} folder/account mismatch; refusing to restore", row.id);
        }
        if !allow_inactive {
            match &det.active {
                Some((a, m)) if a.puuid == row.account_puuid && m.is_confident() => {}
                Some((a, _)) => {
                    return Err(EngineError::AccountMismatch {
                        backup: row.id,
                        owner: self.account_label(&row.account_puuid),
                        active: self.account_label(&a.puuid),
                    }
                    .into());
                }
                None => return Err(EngineError::NoActiveAccount.into()),
            }
        }
        Ok(Target::from(&local))
    }

    pub fn account_label(&self, puuid: &str) -> String {
        self.db.account(puuid).ok().flatten().map(|a| a.display()).unwrap_or_else(|| db::short_id(puuid))
    }

    fn restore_internal(&mut self, backup_id: i64, kind: &str, allow_inactive: bool) -> Result<RestoreOutcome> {
        let row = self.db.backup(backup_id)?.with_context(|| format!("backup #{backup_id} not found"))?;
        let t = self.check_backup_account(&row, allow_inactive)?;
        let files = self.verified_backup_files(backup_id)?;
        let changes = self.preview_backup(backup_id)?;
        let mut ops = Vec::new();
        for f in &files {
            let path = t.dir.join(paths::safe_relative(&f.rel_path).expect("verified"));
            let cur = std::fs::read(&path).ok();
            let want = if f.existed { Some(f.data.clone()) } else { None };
            if cur != want {
                ops.push(FileOp { rel_path: f.rel_path.clone(), contents: want });
            }
        }
        if ops.is_empty() {
            return Ok(RestoreOutcome { backup_id, changes, txn: None });
        }
        let details = format!("restore backup #{backup_id} ({} change(s))", changes.len());
        let txn = self.run_txn(&t, kind, Some(&format!("backup #{backup_id}")), &ops, &details)?;
        Ok(RestoreOutcome { backup_id, changes, txn: Some(txn) })
    }

    /// Restore an arbitrary backup (counts as a new change; can be undone).
    pub fn restore_backup(&mut self, backup_id: i64, allow_inactive: bool) -> Result<RestoreOutcome> {
        self.restore_internal(backup_id, "restore", allow_inactive)
    }

    pub fn restore_original(&mut self, account: &str, allow_inactive: bool) -> Result<RestoreOutcome> {
        let b = self.db.original_backup(account)?.context("no original snapshot exists for this account yet")?;
        self.restore_backup(b.id, allow_inactive)
    }

    pub fn undo_candidate(&self, account: &str) -> Result<Option<OpRow>> {
        self.db.undo_candidate(account)
    }

    pub fn redo_candidate(&self, account: &str) -> Result<Option<OpRow>> {
        self.db.redo_candidate(account)
    }

    /// Undo the most recent transfer/restore of the account.
    pub fn undo(&mut self, account: &str, allow_inactive: bool) -> Result<(OpRow, RestoreOutcome)> {
        let op = self.db.undo_candidate(account)?.context("nothing to undo for this account")?;
        let out = self.restore_internal(op.pre_backup_id, "undo", allow_inactive)?;
        self.db.set_op_undone(op.id, true)?;
        Ok((op, out))
    }

    /// Re-apply the most recently undone change.
    pub fn redo(&mut self, account: &str, allow_inactive: bool) -> Result<(OpRow, RestoreOutcome)> {
        let op = self.db.redo_candidate(account)?.context("nothing to redo for this account")?;
        let post = op.post_backup_id.context("operation has no post-change snapshot")?;
        let out = self.restore_internal(post, "redo", allow_inactive)?;
        self.db.set_op_undone(op.id, false)?;
        Ok((op, out))
    }

    // ------------------------------------------------------------------ recovery
    /// Roll back any operation that was interrupted (process killed, power loss).
    pub fn recover(&mut self) -> Result<RecoveryReport> {
        let mut rep = RecoveryReport::default();
        let pending = self.db.pending_ops()?;
        let det = self.detection();
        for a in &det.accounts {
            rep.stray_tmp_removed += txn::clean_stray_tmp(&a.path);
        }
        if pending.is_empty() {
            return Ok(rep);
        }
        if self.probe.status().valorant {
            rep.deferred = true;
            return Ok(rep);
        }
        for op in pending {
            let dir = self.paths.account_dir(&op.folder);
            let t = Target { puuid: op.account_puuid.clone(), folder: op.folder.clone(), dir };
            match self.restore_files_raw(&t, op.pre_backup_id) {
                Ok(()) => {
                    self.db.finish_op(op.id, OpStatus::Recovered, None)?;
                    rep.recovered_ops.push(op.id);
                }
                Err(e) => rep.failed_ops.push((op.id, format!("{e:#}"))),
            }
        }
        Ok(rep)
    }
}

/// Key-level differences that turn `from` into `to`.
pub fn diff_docs(file: &str, from: &IniDoc, to: &IniDoc) -> Vec<Change> {
    let mut out = Vec::new();
    let a = from.grouped();
    let b = to.grouped();
    let classify = |s: &str, k: &str, v: &str| match crate::catalog::classify(file, s, k, v) {
        crate::catalog::Classification::Setting { category, scope } => (category, scope),
        crate::catalog::Classification::Excluded(_) => (Category::Other, Scope::Local),
    };
    for (s, k, vals) in &b {
        let old = a.iter().find(|(s2, k2, _)| s2.eq_ignore_ascii_case(s) && k2.eq_ignore_ascii_case(k)).map(|e| e.2.clone()).unwrap_or_default();
        if &old != vals {
            let (category, scope) = classify(s, k, vals.first().map(String::as_str).unwrap_or(""));
            out.push(Change { file: file.to_string(), section: s.clone(), key: k.clone(), old, new: vals.clone(), category, scope });
        }
    }
    for (s, k, vals) in &a {
        if !b.iter().any(|(s2, k2, _)| s2.eq_ignore_ascii_case(s) && k2.eq_ignore_ascii_case(k)) {
            let (category, scope) = classify(s, k, vals.first().map(String::as_str).unwrap_or(""));
            out.push(Change { file: file.to_string(), section: s.clone(), key: k.clone(), old: vals.clone(), new: Vec::new(), category, scope });
        }
    }
    out
}

/// Parse a comma separated category list (`sensitivity,crosshair` or `all`).
pub fn parse_categories(s: &str) -> Result<Option<BTreeSet<Category>>> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("all") {
        return Ok(None);
    }
    let mut set = BTreeSet::new();
    for part in s.split(',') {
        let c = Category::from_slug(part).ok_or_else(|| {
            anyhow!("unknown category '{}'. Valid: {}", part.trim(), Category::ALL.iter().map(|c| c.slug()).collect::<Vec<_>>().join(", "))
        })?;
        set.insert(c);
    }
    Ok(Some(set))
}
