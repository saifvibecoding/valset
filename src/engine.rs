//! The VALSYNC settings engine: capture, transfer, backup, restore, undo/redo
//! and crash recovery — for both kinds of VALORANT settings:
//!
//! * **account settings** stored by Riot (sensitivity, crosshair, keybinds, HUD,
//!   audio, minimap, …), read and written through the Riot Client session;
//! * **video settings** in the local `.ini` files of the account's config folder.
//!
//! ## Transaction protocol
//!
//! Every change goes through [`Engine::run_txn`]:
//!
//! 1. Refuse if VALORANT is running (it rewrites settings when it exits).
//! 2. Take an *original* snapshot the first time VALSYNC touches the account.
//! 3. Snapshot the account (cloud document + every `.ini`) → committed to SQLite.
//! 4. Insert a `pending` row in the operation journal.
//! 5. Write files atomically (temp → fsync → rename), then upload the settings.
//! 6. Re-read everything and verify it.
//! 7. Snapshot the result (for redo) and mark the operation `committed`.
//!
//! Any failure in 5–6 rolls back files *and* account settings. If the process
//! dies midway, the `pending` row survives and [`Engine::recover`] rolls back on
//! the next start.

use crate::accounts::{self, DetectMethod, Detection, LocalAccount};
use crate::appconfig::AppConfig;
use crate::catalog::{Category, Scope};
use crate::cloud::{self, CLOUD_REL, CloudSettings};
use crate::db::{self, BackupFile, BackupKind, BackupRow, Db, OpRow, OpStatus};
use crate::game::{self, ProcessProbe};
use crate::ini::IniDoc;
use crate::paths::{self, Paths};
use crate::profile::{CaptureReport, Profile, SettingEntry};
use crate::txn::{self, ApplyError, Faults, FileOp};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("VALORANT is running. Close the game first (keep the Riot Client open) — VALORANT overwrites settings when it exits.")]
    GameRunning,
    #[error("backup #{0} is damaged ({1}) and will not be restored.")]
    Integrity(i64, String),
    #[error("backup #{backup} belongs to {owner}, but the Riot Client is signed in as {active}. Sign in to {owner} first.")]
    AccountMismatch { backup: i64, owner: String, active: String },
    #[error("the transfer failed and everything was put back the way it was: {0}")]
    RolledBack(String),
    #[error("the transfer failed and undoing it ALSO failed ({0}). Your previous settings are safe in backup #{1}; VALSYNC retries automatically on next start.")]
    RollbackFailed(String, i64),
    #[error("simulated crash (test hook)")]
    SimulatedCrash,
    #[error("no account found. Open the Riot Client and sign in.")]
    NoActiveAccount,
    #[error("another VALSYNC window is applying settings right now.")]
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
    /// `None` = everything.
    pub categories: Option<BTreeSet<Category>>,
    /// Also write old-style local cache keys (profiles saved without the Riot Client).
    pub include_cloud: bool,
}

#[derive(Debug, Clone, Default)]
pub struct LoadPlan {
    pub changes: Vec<Change>,
    pub ops: Vec<FileOp>,
    /// New account-settings document to upload.
    pub cloud: Option<Value>,
    /// Settings that could not be applied automatically (shown as a checklist).
    pub manual: Vec<SettingEntry>,
    pub warnings: Vec<String>,
    pub unchanged: usize,
    pub new_keys: usize,
    pub new_files: Vec<String>,
    pub patch_warning: Option<String>,
    /// The account has never started VALORANT on this PC, so video settings are skipped.
    pub video_skipped: bool,
}

pub const VIDEO_SKIPPED_WARNING: &str = "Video settings will be skipped: this account hasn't started VALORANT on this PC yet. \
Only your account settings (sensitivity, crosshair, keybinds, HUD, audio and more) will be copied. \
To copy resolution, graphics and FPS settings too, start VALORANT once on this account, close it, and load again.";

impl LoadPlan {
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty() && self.cloud.is_none()
    }
}

#[derive(Debug, Clone)]
pub struct TxnOutcome {
    pub op_id: i64,
    pub pre_backup: i64,
    pub post_backup: i64,
    pub files_written: usize,
    pub cloud_written: bool,
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
    /// Account settings were captured from Riot.
    pub cloud: bool,
}

pub struct Engine {
    pub paths: Paths,
    pub db: Db,
    pub config: AppConfig,
    pub probe: ProcessProbe,
    pub faults: Faults,
    pub startup_recovery: RecoveryReport,
    pub cloud: Option<Box<dyn CloudSettings>>,
    /// Why the Riot Client connection is unavailable.
    pub cloud_error: Option<String>,
}

/// Guard for the single-writer lock file.
struct WriteLock(PathBuf);
impl Drop for WriteLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

impl Engine {
    /// Open the data directory and database, connect to the Riot Client and run crash recovery.
    pub fn open(paths: Paths) -> Result<Engine> {
        std::fs::create_dir_all(&paths.data_dir).with_context(|| format!("cannot create data dir {}", paths.data_dir.display()))?;
        let config = AppConfig::load(&paths.config_path())?;
        let db = Db::open(&paths.db_path())?;
        let mut e = Engine::with_parts(paths, db, config, ProcessProbe::from_env());
        e.connect_riot();
        e.startup_recovery = e.recover()?;
        let _ = e.db.cache_purge();
        Ok(e)
    }

    /// Construct with explicit parts (tests).
    pub fn with_parts(paths: Paths, db: Db, config: AppConfig, probe: ProcessProbe) -> Engine {
        Engine { paths, db, config, probe, faults: Faults::default(), startup_recovery: RecoveryReport::default(), cloud: None, cloud_error: None }
    }

    pub fn save_config(&self) -> Result<()> {
        self.config.save(&self.paths.config_path())
    }

    /// (Re)connect to the Riot Client. Cheap when it is not running.
    pub fn connect_riot(&mut self) {
        if crate::riot::local::Lockfile::read().is_none() {
            self.cloud = None;
            self.cloud_error = Some("Riot Client is not running".into());
            return;
        }
        let known = self.config.region.clone().or_else(|| self.db.meta_get("last_region").ok().flatten());
        match cloud::RiotCloud::connect(&self.paths.valorant_logs, known.as_deref()) {
            Ok(c) => {
                {
                    let s = c.session();
                    let _ = self.remember_account(&s.puuid, Some(&s.region), s.game_name.as_deref(), s.tag_line.as_deref());
                    let _ = self.db.meta_set("last_region", &s.region);
                }
                self.cloud = Some(Box::new(c));
                self.cloud_error = None;
            }
            Err(e) => {
                self.cloud = None;
                self.cloud_error = Some(format!("{e:#}"));
            }
        }
    }

    /// Follow sign-ins, sign-outs and account switches in the Riot Client.
    /// Cheap: one local request. Returns true when the signed-in account changed.
    pub fn refresh_riot(&mut self) -> bool {
        let before = self.cloud_account();
        let now = crate::riot::local::Lockfile::read()
            .and_then(|l| crate::riot::local::LocalClient::new(l).ok())
            .and_then(|c| crate::riot::block_on(c.entitlements()).ok())
            .map(|e| e.subject.to_ascii_lowercase());
        match &now {
            Some(p) if before.as_deref() != Some(p.as_str()) => self.connect_riot(),
            Some(_) => {}
            None => {
                if self.cloud.is_some() || before.is_some() {
                    self.cloud = None;
                }
                self.cloud_error = Some(if crate::riot::local::Lockfile::read().is_some() {
                    "Riot Client is open, but nobody is signed in".into()
                } else {
                    "Riot Client is not running".into()
                });
            }
        }
        before != self.cloud_account()
    }

    /// Use a custom backend (tests, benchmarks).
    pub fn set_cloud(&mut self, c: Box<dyn CloudSettings>) {
        if let Some(p) = c.account() {
            let _ = self.remember_account(&p, None, None, None);
        }
        self.cloud = Some(c);
        self.cloud_error = None;
    }

    pub fn remember_account(&self, puuid: &str, region: Option<&str>, name: Option<&str>, tag: Option<&str>) -> Result<()> {
        self.db.touch_account(puuid, region)?;
        if let Some(r) = region {
            self.db.set_account_shard(puuid, r)?;
        }
        if let (Some(n), Some(t)) = (name, tag)
            && !n.is_empty()
        {
            self.db.set_account_identity(puuid, n, t, "riot-client", true, None)?;
        }
        self.db.mark_account_seen(puuid)?;
        Ok(())
    }

    pub fn cloud_account(&self) -> Option<String> {
        self.cloud.as_ref().and_then(|c| c.account())
    }

    /// The cloud backend, only if it is signed into `puuid`.
    pub fn cloud_for(&self, puuid: &str) -> Option<&dyn CloudSettings> {
        let c = self.cloud.as_deref()?;
        (c.account().as_deref() == Some(puuid)).then_some(c)
    }

    // ------------------------------------------------------------------ detection
    pub fn detection(&self) -> Detection {
        let mut det = accounts::detect(&self.paths.valorant_config, self.config.manual_active_account.as_deref());
        if let Some(p) = self.cloud_account() {
            let local = det.find(&p).cloned().unwrap_or_else(|| {
                let region = self.db.account(&p).ok().flatten().and_then(|a| a.shard).unwrap_or_else(|| "na".into());
                let folder = format!("{p}-{region}");
                LocalAccount { puuid: p.clone(), shard: Some(region), path: self.paths.valorant_config.join(&folder), folder, modified: None }
            });
            det.active = Some((local, DetectMethod::RiotClient));
        }
        det
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
        if let Some((a, _)) = &det.active
            && (a.puuid.starts_with(&query.to_ascii_lowercase()) && query.len() >= 6)
        {
            return Ok(Target::from(a));
        }
        if let Some((n, t)) = accounts::parse_riot_id(query) {
            for acc in self.db.accounts()? {
                if acc.game_name.as_deref().map(|x| x.eq_ignore_ascii_case(&n)).unwrap_or(false)
                    && acc.tag_line.as_deref().map(|x| x.eq_ignore_ascii_case(&t)).unwrap_or(false)
                {
                    if let Some(a) = det.find(&acc.puuid) {
                        return Ok(Target::from(a));
                    }
                    if let Some((a, _)) = &det.active
                        && a.puuid == acc.puuid
                    {
                        return Ok(Target::from(a));
                    }
                }
            }
        }
        bail!("no account matches '{query}'")
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
    /// All `.ini` files in an account folder (empty when the folder does not exist yet).
    pub fn read_account_files(dir: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
        let mut out = BTreeMap::new();
        if !dir.is_dir() {
            return Ok(out);
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

    /// Snapshot the account: its cloud settings (when signed in) and every `.ini`,
    /// plus `extra` paths that may be created.
    pub fn snapshot(&self, t: &Target, extra: &[String]) -> Result<Vec<BackupFile>> {
        let files = Self::read_account_files(&t.dir)?;
        let mut out: Vec<BackupFile> =
            files.into_iter().map(|(rel, data)| BackupFile { sha256: db::sha256_hex(&data), rel_path: rel, existed: true, data }).collect();
        for rel in extra {
            if rel == CLOUD_REL || out.iter().any(|f| f.rel_path.eq_ignore_ascii_case(rel)) {
                continue;
            }
            let path = t.dir.join(paths::safe_relative(rel).ok_or_else(|| anyhow!("unsafe path {rel}"))?);
            match std::fs::read(&path) {
                Ok(data) => out.push(BackupFile { sha256: db::sha256_hex(&data), rel_path: rel.clone(), existed: true, data }),
                Err(_) => out.push(BackupFile { rel_path: rel.clone(), existed: false, sha256: db::sha256_hex(b""), data: Vec::new() }),
            }
        }
        if let Some(c) = self.cloud_for(&t.puuid) {
            let doc = c.fetch().context("could not back up your account settings")?;
            let data = serde_json::to_vec_pretty(&doc)?;
            out.push(BackupFile { sha256: db::sha256_hex(&data), rel_path: CLOUD_REL.into(), existed: true, data });
        }
        out.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
        Ok(out)
    }

    fn ensure_original(&mut self, t: &Target) -> Result<()> {
        let has_cloud = self.cloud_for(&t.puuid).is_some();
        let existing = self.db.original_backup(&t.puuid)?;
        let needs = match &existing {
            None => true,
            // An original taken without the Riot Client lacks account settings; take a fuller one.
            Some(b) => has_cloud && !self.db.backup_files(b.id)?.iter().any(|f| f.rel_path == CLOUD_REL),
        };
        if needs {
            let files = self.snapshot(t, &[])?;
            if files.is_empty() {
                return Ok(());
            }
            let patch = self.current_patch();
            self.db.insert_backup(&t.puuid, &t.folder, BackupKind::Original, "Original settings (first seen by VALSYNC)", patch.as_deref(), &files)?;
        }
        Ok(())
    }

    // ------------------------------------------------------------------ save
    pub fn capture(&self, t: &Target, name: &str, categories: Option<&BTreeSet<Category>>) -> Result<(Profile, CaptureReport)> {
        let files = Self::read_account_files(&t.dir)?;
        let docs = Self::parse_files(&files);
        let (mut p, mut rep) = Profile::capture(name, &docs, self.current_patch(), Some(&t.puuid));
        if let Some(c) = self.cloud_for(&t.puuid) {
            let doc = c.fetch().context("could not read your account settings from Riot")?;
            p.set_cloud(&doc);
            rep.cloud = p.cloud_count();
        }
        if let Some(cats) = categories {
            p.settings.retain(|s| cats.contains(&s.category));
            if let Some(c) = &p.cloud {
                p.cloud = Some(cloud::merge(c, &Value::Object(Default::default()), Some(cats)));
            }
            rep.cloud = p.cloud_count();
        }
        rep.local = p.local_count();
        Ok((p, rep))
    }

    pub fn save_profile(
        &mut self,
        t: &Target,
        name: &str,
        overwrite: bool,
        categories: Option<&BTreeSet<Category>>,
        notes: Option<String>,
        manual: Vec<crate::profile::ManualValue>,
    ) -> Result<SaveOutcome> {
        let name = crate::profile::validate_name(name)?;
        let existed = self.db.profile(&name)?.is_some();
        let (mut p, report) = self.capture(t, &name, categories)?;
        if p.settings.is_empty() && p.cloud.is_none() {
            bail!("no settings found for this account. Open the Riot Client and sign in, then try again.");
        }
        p.notes = notes;
        p.manual = manual;
        self.db.save_profile(&p, "captured", overwrite)?;
        self.db.meta_set("last_saved_profile", &name)?;
        let _ = self.ensure_original(t);
        let cloud = p.cloud.is_some();
        Ok(SaveOutcome { profile: p, report, overwritten: existed, account: t.puuid.clone(), cloud })
    }

    // ------------------------------------------------------------------ load
    pub fn plan_load(&self, profile: &Profile, t: &Target, opts: &LoadOptions) -> Result<LoadPlan> {
        let mut plan = LoadPlan { patch_warning: game::patch_warning(profile.patch.as_deref(), self.current_patch().as_deref()), ..Default::default() };
        let wants = |c: Category| opts.categories.as_ref().map(|s| s.contains(&c)).unwrap_or(true);

        // ---- account settings (Riot)
        if let Some(preset) = &profile.cloud {
            match self.cloud_for(&t.puuid) {
                Some(c) => {
                    let current = c.fetch().context("could not read the current account settings")?;
                    let merged = cloud::merge(preset, &current, opts.categories.as_ref());
                    let changes = cloud::diff(&current, &merged);
                    plan.unchanged += cloud::entries(preset).len().saturating_sub(changes.len());
                    if !changes.is_empty() {
                        plan.changes.extend(changes);
                        plan.cloud = Some(merged);
                    }
                }
                None => plan.warnings.push(match self.cloud_account() {
                    Some(other) => format!(
                        "Sensitivity, crosshair and keybinds were skipped: the Riot Client is signed in as {}, not {}.",
                        self.account_label(&other),
                        self.account_label(&t.puuid)
                    ),
                    None => "Sensitivity, crosshair and keybinds were skipped: open the Riot Client and sign in to this account.".into(),
                }),
            }
        }

        // ---- video settings (local files)
        let local_entries: Vec<&SettingEntry> = profile
            .settings
            .iter()
            .filter(|e| wants(e.category) && paths::safe_relative(&e.file).is_some())
            .collect();
        if !t.dir.is_dir() {
            if local_entries.iter().any(|e| e.scope == Scope::Local) {
                plan.video_skipped = true;
                plan.warnings.push(VIDEO_SKIPPED_WARNING.into());
            }
            return Ok(plan);
        }
        let originals = Self::read_account_files(&t.dir)?;
        let mut docs = Self::parse_files(&originals);
        let mut touched: BTreeSet<String> = BTreeSet::new();
        for entry in local_entries {
            if entry.scope == Scope::Cloud && !opts.include_cloud {
                if profile.cloud.is_none() {
                    plan.manual.push(entry.clone());
                }
                continue;
            }
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
        if plan.is_empty() {
            return Ok(LoadOutcome { plan, txn: None });
        }
        let cats: Vec<&str> = opts.categories.as_ref().map(|c| c.iter().map(|c| c.slug()).collect()).unwrap_or_else(|| vec!["all"]);
        let details = format!("{} change(s); categories: {}", plan.changes.len(), cats.join(","));
        let txn = self.run_txn(t, "load", Some(&profile.name), &plan.ops, plan.cloud.as_ref(), &details)?;
        let local: Vec<Change> = plan.changes.iter().filter(|c| c.file != CLOUD_REL).cloned().collect();
        let bad = self.verify_changes(t, &local)?;
        if !bad.is_empty() {
            self.restore_raw(t, txn.pre_backup)?;
            self.db.finish_op(txn.op_id, OpStatus::RolledBack, None)?;
            return Err(EngineError::RolledBack(format!("verification failed for {}", bad.join(", "))).into());
        }
        self.db.meta_set(&format!("last_loaded_profile:{}", t.puuid), &profile.name)?;
        Ok(LoadOutcome { plan, txn: Some(txn) })
    }

    /// Re-read files and check each local change landed. Returns failing keys.
    pub fn verify_changes(&self, t: &Target, changes: &[Change]) -> Result<Vec<String>> {
        let files = Self::read_account_files(&t.dir)?;
        let docs = Self::parse_files(&files);
        let mut bad = Vec::new();
        for c in changes.iter().filter(|c| c.file != CLOUD_REL) {
            let ok = docs.iter().find(|(k, _)| k.eq_ignore_ascii_case(&c.file)).map(|(_, d)| d.get(&c.section, &c.key) == c.new).unwrap_or(false);
            if !ok {
                bad.push(c.key.clone());
            }
        }
        Ok(bad)
    }

    // ------------------------------------------------------------------ transactions
    fn run_txn(&mut self, t: &Target, kind: &str, profile: Option<&str>, ops: &[FileOp], cloud_new: Option<&Value>, details: &str) -> Result<TxnOutcome> {
        self.ensure_game_closed()?;
        if cloud_new.is_some() && self.cloud_for(&t.puuid).is_none() {
            bail!("the Riot Client is no longer signed in to {}", self.account_label(&t.puuid));
        }
        let _lock = self.acquire_lock()?;
        self.ensure_original(t)?;
        let rels: Vec<String> = ops.iter().map(|o| o.rel_path.clone()).collect();
        let pre_files = self.snapshot(t, &rels)?;
        let patch = self.current_patch();
        let label = match (kind, profile) {
            ("load", Some(p)) => format!("Before loading '{p}'"),
            ("undo", _) => "Before undo".to_string(),
            ("redo", _) => "Before redo".to_string(),
            (_, Some(p)) => format!("Before {kind} ({p})"),
            _ => format!("Before {kind}"),
        };
        let pre = self.db.insert_backup(&t.puuid, &t.folder, BackupKind::PreChange, &label, patch.as_deref(), &pre_files)?;
        let op_id = self.db.begin_op(&t.puuid, &t.folder, kind, profile, pre, details)?;

        // 1. local files
        match txn::apply_ops(&t.dir, ops, &self.faults) {
            Ok(()) => {}
            Err(ApplyError::Crashed) => return Err(EngineError::SimulatedCrash.into()),
            Err(ApplyError::Failed { error, .. }) => return Err(self.rollback(t, op_id, pre, format!("{error:#}"))),
        }
        let mut problems = Vec::new();
        for op in ops {
            let path = t.dir.join(paths::safe_relative(&op.rel_path).expect("validated"));
            if std::fs::read(&path).ok().as_deref() != op.contents.as_deref() {
                problems.push(op.rel_path.clone());
            }
        }
        if !problems.is_empty() {
            return Err(self.rollback(t, op_id, pre, format!("verification failed for {}", problems.join(", "))));
        }
        // 2. account settings
        if let Some(doc) = cloud_new {
            let c = self.cloud_for(&t.puuid).expect("checked");
            if let Err(e) = c.store(doc) {
                return Err(self.rollback(t, op_id, pre, format!("uploading settings failed: {e:#}")));
            }
            if self.faults.crash_after_cloud {
                return Err(EngineError::SimulatedCrash.into());
            }
            match c.fetch() {
                Ok(now) if cloud::equivalent(doc, &now) => {}
                Ok(_) => return Err(self.rollback(t, op_id, pre, "Riot did not keep the uploaded settings".into())),
                Err(e) => return Err(self.rollback(t, op_id, pre, format!("could not verify the uploaded settings: {e:#}"))),
            }
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
        Ok(TxnOutcome { op_id, pre_backup: pre, post_backup: post, files_written: ops.len(), cloud_written: cloud_new.is_some(), verified: ops.len() + cloud_new.is_some() as usize })
    }

    fn rollback(&mut self, t: &Target, op_id: i64, pre: i64, reason: String) -> anyhow::Error {
        match self.restore_raw(t, pre) {
            Ok(()) => {
                let _ = self.db.finish_op(op_id, OpStatus::RolledBack, None);
                EngineError::RolledBack(reason).into()
            }
            Err(e) => EngineError::RollbackFailed(format!("{reason}; rollback: {e:#}"), pre).into(),
        }
    }

    /// Put a backup back verbatim (rollback / recovery): files, and account
    /// settings when the Riot Client is signed in to that account.
    fn restore_raw(&self, t: &Target, backup_id: i64) -> Result<()> {
        let files = self.verified_backup_files(backup_id)?;
        let (cloud_files, local): (Vec<&BackupFile>, Vec<&BackupFile>) = files.iter().partition(|f| f.rel_path == CLOUD_REL);
        let ops: Vec<FileOp> = local.iter().map(|f| FileOp { rel_path: f.rel_path.clone(), contents: if f.existed { Some(f.data.clone()) } else { None } }).collect();
        txn::apply_ops(&t.dir, &ops, &Faults::default()).map_err(|e| anyhow!("{e}"))?;
        for f in &local {
            let path = t.dir.join(paths::safe_relative(&f.rel_path).ok_or_else(|| anyhow!("unsafe path"))?);
            let now = std::fs::read(&path).ok();
            let expect = if f.existed { Some(f.data.as_slice()) } else { None };
            if now.as_deref() != expect {
                bail!("restored file {} does not match the backup", f.rel_path);
            }
        }
        txn::clean_stray_tmp(&t.dir);
        if let Some(cf) = cloud_files.first() {
            let doc: Value = serde_json::from_slice(&cf.data).context("backup settings are unreadable")?;
            let c = self.cloud_for(&t.puuid).ok_or_else(|| anyhow!("sign in to {} in the Riot Client to restore its account settings", self.account_label(&t.puuid)))?;
            let current = c.fetch()?;
            if !cloud::equivalent(&doc, &current) {
                c.store(&doc)?;
                if !cloud::equivalent(&doc, &c.fetch()?) {
                    bail!("Riot did not keep the restored settings");
                }
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------ backups
    pub fn create_manual_backup(&mut self, t: &Target, label: &str) -> Result<i64> {
        let files = self.snapshot(t, &[])?;
        if files.is_empty() {
            bail!("nothing to back up yet: open the Riot Client and sign in, or start VALORANT once on this account");
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
            if f.rel_path == CLOUD_REL && serde_json::from_slice::<Value>(&f.data).is_err() {
                problems.push("account settings are not valid JSON".into());
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

    /// What restoring a backup would change.
    pub fn preview_backup(&self, backup_id: i64) -> Result<Vec<Change>> {
        let row = self.db.backup(backup_id)?.with_context(|| format!("backup #{backup_id} not found"))?;
        let dir = self.paths.account_dir(&row.folder);
        let current = Self::read_account_files(&dir)?;
        let files = self.db.backup_files(backup_id)?;
        let mut changes = Vec::new();
        for f in &files {
            if f.rel_path == CLOUD_REL {
                let target: Value = serde_json::from_slice(&f.data).unwrap_or(Value::Null);
                match self.cloud_for(&row.account_puuid).map(|c| c.fetch()) {
                    Some(Ok(cur)) => changes.extend(cloud::diff(&cur, &target)),
                    _ => changes.extend(cloud::diff(&Value::Object(Default::default()), &target).into_iter().map(|mut c| {
                        c.old = vec!["?".into()];
                        c
                    })),
                }
                continue;
            }
            let cur = current.get(&f.rel_path).map(|b| IniDoc::parse_bytes(b)).unwrap_or_default();
            let target = if f.existed { IniDoc::parse_bytes(&f.data) } else { IniDoc::default() };
            changes.extend(diff_docs(&f.rel_path, &cur, &target));
        }
        Ok(changes)
    }

    fn check_backup_account(&self, row: &BackupRow, allow_inactive: bool) -> Result<Target> {
        let det = self.detection();
        let t = match det.accounts.iter().find(|a| a.folder == row.folder) {
            Some(local) => {
                if local.puuid != row.account_puuid {
                    bail!("backup #{} folder/account mismatch; refusing to restore", row.id);
                }
                Target::from(local)
            }
            None => Target { puuid: row.account_puuid.clone(), folder: row.folder.clone(), dir: self.paths.account_dir(&row.folder) },
        };
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
        Ok(t)
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
        let mut cloud_new = None;
        for f in &files {
            if f.rel_path == CLOUD_REL {
                let doc: Value = serde_json::from_slice(&f.data).context("backup settings are unreadable")?;
                let c = self.cloud_for(&t.puuid).ok_or_else(|| {
                    anyhow!("this backup includes sensitivity/crosshair/keybinds. Sign in to {} in the Riot Client to restore it.", self.account_label(&t.puuid))
                })?;
                if !cloud::equivalent(&doc, &c.fetch()?) {
                    cloud_new = Some(doc);
                }
                continue;
            }
            let path = t.dir.join(paths::safe_relative(&f.rel_path).expect("verified"));
            let cur = std::fs::read(&path).ok();
            let want = if f.existed { Some(f.data.clone()) } else { None };
            if cur != want {
                ops.push(FileOp { rel_path: f.rel_path.clone(), contents: want });
            }
        }
        if ops.is_empty() && cloud_new.is_none() {
            return Ok(RestoreOutcome { backup_id, changes, txn: None });
        }
        let details = format!("restore backup #{backup_id} ({} change(s))", changes.len());
        let txn = self.run_txn(&t, kind, Some(&format!("backup #{backup_id}")), &ops, cloud_new.as_ref(), &details)?;
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
            let has_cloud = self.db.backup_files(op.pre_backup_id)?.iter().any(|f| f.rel_path == CLOUD_REL);
            if has_cloud && self.cloud_for(&op.account_puuid).is_none() {
                // Needs the Riot Client signed in as that account; retry next start.
                rep.deferred = true;
                continue;
            }
            let dir = self.paths.account_dir(&op.folder);
            let t = Target { puuid: op.account_puuid.clone(), folder: op.folder.clone(), dir };
            match self.restore_raw(&t, op.pre_backup_id) {
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
