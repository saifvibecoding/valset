//! Direct (non-interactive) command implementations.

use crate::catalog::{Category, Scope};
use crate::cli::*;
use crate::db::{self, BackupKind};
use crate::engine::{Change, Engine, LoadOptions, Target, parse_categories};
use crate::matches;
use crate::profile::{self, ManualValue, Profile};
use crate::rank;
use crate::service;
use crate::term::{self, bold, dim, paint};
use anyhow::{Context, Result, anyhow, bail};
use crossterm::style::Color;
use std::collections::BTreeMap;

pub struct Ctx {
    pub engine: Engine,
    pub yes: bool,
}

impl Ctx {
    fn target(&self, account: Option<&str>) -> Result<(Target, bool)> {
        match account {
            Some(a) => Ok((self.engine.resolve_target(a)?, true)),
            None => {
                let (t, m) = self.engine.active_target()?;
                Ok((t, m.is_confident()))
            }
        }
    }

    fn label(&self, puuid: &str) -> String {
        self.engine.account_label(puuid)
    }

    /// Confirm the target when detection was only a guess.
    fn confirm_target(&self, t: &Target, confident: bool) -> Result<()> {
        if !confident {
            term::warn(&format!("The Riot Client is not open, so VALSYNC is guessing the account: {}.", self.label(&t.puuid)));
            if !term::confirm("Use this account?", false)? {
                bail!("cancelled. Open the Riot Client and sign in to the account you want.");
            }
        }
        Ok(())
    }

    fn session(&self) -> Result<crate::riot::Session> {
        service::runtime().block_on(service::connect(&self.engine.paths.valorant_logs, &self.engine.db, &self.engine.config))
    }
}

pub fn run(cmd: Command, ctx: &mut Ctx) -> Result<()> {
    report_recovery(&ctx.engine);
    match cmd {
        Command::Status => status(ctx),
        Command::Accounts { action } => accounts(ctx, action.unwrap_or(AccountAction::List)),
        Command::Save { name, overwrite, categories, note, account } => save(ctx, &name, overwrite, &categories, note, account.as_deref()),
        Command::Load { name, categories, dry_run, account } => load(ctx, &name, &categories, dry_run, account.as_deref()),
        Command::Restore { backup, original, account, dry_run } => restore(ctx, backup, original, account.as_deref(), dry_run),
        Command::Undo { account } => undo_redo(ctx, true, account.as_deref()),
        Command::Redo { account } => undo_redo(ctx, false, account.as_deref()),
        Command::Profiles { action } => profiles(ctx, action.unwrap_or(ProfileAction::List)),
        Command::Backups { all, action } => backups(ctx, all, action.unwrap_or(BackupAction::List)),
        Command::Compare { a, b } => compare(ctx, &a, &b),
        Command::Export { name, output } => export(ctx, &name, output),
        Command::Import { file, name, overwrite } => import(ctx, &file, name, overwrite),
        Command::Live { once } => {
            if once {
                live_once(ctx)
            } else {
                Err(anyhow!("__tui_live__"))
            }
        }
        Command::History { count, offline } => history(ctx, count, offline),
        Command::Doctor => doctor(ctx),
        Command::Config { action } => config(ctx, action.unwrap_or(ConfigAction::Show)),
        Command::Bench => crate::bench::run(ctx),
    }
}

fn report_recovery(e: &Engine) {
    let r = &e.startup_recovery;
    for id in &r.recovered_ops {
        term::warn(&format!("VALSYNC was interrupted last time (operation #{id}); your previous settings were put back."));
    }
    for (id, err) in &r.failed_ops {
        term::error(&format!("Interrupted operation #{id} could not be undone yet: {err}"));
    }
    if r.deferred {
        term::warn("An interrupted change still needs to be undone: close VALORANT and keep the Riot Client signed in to that account, then run VALSYNC again.");
    }
}

fn riot_line(ctx: &Ctx) -> String {
    match ctx.engine.cloud_account() {
        Some(p) => format!("{} signed in as {}", term::dot(true), bold(&ctx.label(&p))),
        None => format!("{} {}", term::dot(false), ctx.engine.cloud_error.clone().unwrap_or_else(|| "not connected".into())),
    }
}

// --------------------------------------------------------------------- status
fn status(ctx: &mut Ctx) -> Result<()> {
    let s = service::status(&ctx.engine)?;
    println!("{}", paint(term::LOGO, Color::Rgb { r: 255, g: 70, b: 85 }));
    println!("{}\n", dim(&format!("  v{} · VALORANT settings manager", env!("CARGO_PKG_VERSION"))));
    println!("  Riot Client      {}", riot_line(ctx));
    let acct = match &s.active {
        Some((row, _, m)) => format!("{}  {}", bold(&row.display()), dim(&format!("[{}] {}", row.shard.clone().unwrap_or_else(|| "?".into()).to_uppercase(), m.label()))),
        None => paint("none — open the Riot Client and sign in", Color::Yellow),
    };
    println!("  Account          {acct}");
    println!(
        "  VALORANT         {} {}",
        if s.processes.valorant { paint("●", Color::Yellow) } else { term::dot(false) },
        if s.processes.valorant { "running (close it before loading or restoring settings)" } else { "closed" }
    );
    println!("  Game patch       {}", s.patch.clone().unwrap_or_else(|| "unknown".into()));
    println!("  Last saved       {}", s.last_saved_profile.clone().unwrap_or_else(|| "—".into()));
    println!("  Profiles         {}", s.profile_count);
    println!("  Backups          {} for this account · {} total", s.backup_count_active, s.backup_count_total);
    println!("  Undo / Redo      {} / {}", if s.undo_available { "available" } else { "—" }, if s.redo_available { "available" } else { "—" });
    if let Some(sw) = &s.switch {
        term::info(&format!("Account switch detected: {} → {}", sw.from.as_deref().map(|p| ctx.label(p)).unwrap_or_else(|| "—".into()), ctx.label(&sw.to)));
    }
    Ok(())
}

// --------------------------------------------------------------------- accounts
fn accounts(ctx: &mut Ctx, action: AccountAction) -> Result<()> {
    match action {
        AccountAction::List => {
            let det = ctx.engine.detection();
            crate::accounts::sync_with_db(&ctx.engine.db, &det)?;
            let active = det.active.as_ref().map(|a| a.0.puuid.clone());
            let rows: Vec<Vec<String>> = ctx
                .engine
                .db
                .accounts()?
                .into_iter()
                .map(|a| {
                    vec![
                        if active.as_deref() == Some(a.puuid.as_str()) { paint("▶", Color::Green) } else { " ".into() },
                        a.riot_id().unwrap_or_else(|| dim("(name appears after signing in once)")),
                        a.shard.clone().unwrap_or_else(|| "?".into()).to_uppercase(),
                        ctx.engine.db.backup_count(Some(&a.puuid)).unwrap_or(0).to_string(),
                        a.preferred_profile.clone().unwrap_or_else(|| "—".into()),
                    ]
                })
                .collect();
            if rows.is_empty() {
                term::warn("No accounts yet. Open the Riot Client and sign in — VALSYNC picks it up automatically.");
                return Ok(());
            }
            term::table(&["", "Riot ID", "Region", "Backups", "Preferred profile"], &rows);
            println!("\n{}", dim("Switch accounts in the Riot Client as usual; VALSYNC follows automatically."));
        }
        AccountAction::Prefer { profile, account } => {
            let (t, _) = ctx.target(account.as_deref())?;
            let p = ctx.engine.db.profile(&profile)?.with_context(|| format!("profile '{profile}' not found"))?;
            ctx.engine.db.touch_account(&t.puuid, None)?;
            ctx.engine.db.set_preferred_profile(&t.puuid, Some(&p.name))?;
            term::ok(&format!("Preferred profile for {} is now '{}'.", ctx.label(&t.puuid), p.name));
        }
        AccountAction::Forget { account } => {
            let puuid = ctx.engine.resolve_target(&account).map(|t| t.puuid).or_else(|_| {
                ctx.engine.db.accounts()?.into_iter().find(|a| a.puuid.starts_with(&account.to_ascii_lowercase()) || a.riot_id().map(|r| r.eq_ignore_ascii_case(&account)).unwrap_or(false)).map(|a| a.puuid).context("account not found")
            })?;
            if term::confirm(&format!("Forget {}? (Settings and backups are kept.)", ctx.label(&puuid)), ctx.yes)? {
                ctx.engine.db.forget_account(&puuid)?;
                term::ok("Account forgotten.");
            }
        }
        AccountAction::Switches => {
            let rows: Vec<Vec<String>> = ctx
                .engine
                .db
                .switch_events(30)?
                .into_iter()
                .map(|(ts, from, to)| vec![term::local_time(&ts), from.map(|f| ctx.label(&f)).unwrap_or_else(|| "—".into()), ctx.label(&to)])
                .collect();
            if rows.is_empty() {
                term::info("No account switches recorded yet.");
            } else {
                term::table(&["When", "From", "To"], &rows);
            }
        }
    }
    Ok(())
}

// --------------------------------------------------------------------- save / load
fn save(ctx: &mut Ctx, name: &str, overwrite: bool, categories: &str, note: Option<String>, account: Option<&str>) -> Result<()> {
    let cats = parse_categories(categories)?;
    let (t, confident) = ctx.target(account)?;
    ctx.confirm_target(&t, confident)?;
    let mut overwrite = overwrite;
    if !overwrite && ctx.engine.db.profile(name)?.is_some() {
        if term::confirm(&format!("Profile '{name}' already exists. Overwrite it?"), ctx.yes)? {
            overwrite = true;
        } else {
            bail!("cancelled; profile '{name}' was not changed");
        }
    }
    let out = ctx.engine.save_profile(&t, name, overwrite, cats.as_ref(), note, Vec::new())?;
    term::ok(&format!("Saved '{}' from {}", out.profile.name, ctx.label(&t.puuid)));
    print_profile_summary(&out.profile);
    if !out.cloud {
        term::warn("Only video settings were saved: the Riot Client isn't signed in to this account. Sign in and save again to include sensitivity, crosshair and keybinds.");
    }
    Ok(())
}

pub fn print_profile_summary(p: &Profile) {
    if let Some(c) = &p.cloud {
        for (k, v) in crate::cloud::highlights(c) {
            println!("  {:<20} {}", k, bold(&v));
        }
    }
    let cats = p.categories();
    let parts: Vec<String> = Category::ALL
        .iter()
        .filter_map(|c| cats.get(c).map(|(l, cl)| (c, l + cl)))
        .filter(|(_, n)| *n > 0)
        .map(|(c, n)| format!("{} ({n})", c.label()))
        .collect();
    if !parts.is_empty() {
        println!("  {}", dim(&format!("Includes: {}", parts.join(", "))));
    }
}

fn print_changes(changes: &[Change], limit: usize) {
    let rows: Vec<Vec<String>> = changes
        .iter()
        .take(limit)
        .map(|c| {
            vec![
                c.category.label().to_string(),
                c.key.clone(),
                paint(&c.old_display(), Color::Red),
                paint(&c.new_display(), Color::Green),
                if c.scope == Scope::Cloud { paint("cloud", Color::Yellow) } else { "local".into() },
            ]
        })
        .collect();
    term::table(&["Category", "Setting", "Current", "New", "Scope"], &rows);
    if changes.len() > limit {
        println!("{}", dim(&format!("… and {} more", changes.len() - limit)));
    }
}

fn print_manual(manual: &[profile::SettingEntry], extra: &[ManualValue]) {
    if manual.is_empty() && extra.is_empty() {
        return;
    }
    term::heading("Manual checklist (account-synced settings — set these in-game)");
    let mut by_cat: BTreeMap<Category, Vec<&profile::SettingEntry>> = BTreeMap::new();
    for m in manual {
        by_cat.entry(m.category).or_default().push(m);
    }
    for (c, items) in by_cat {
        println!("  {}", bold(c.label()));
        for i in items.iter().take(8) {
            println!("    ☐ {} = {}", i.key.rsplit("::").next().unwrap_or(&i.key), i.display_value());
        }
        if items.len() > 8 {
            println!("    {}", dim(&format!("… {} more", items.len() - 8)));
        }
    }
    for e in extra {
        println!("  ☐ {}: {}", bold(&e.label), e.value);
    }
    println!(
        "{}",
        dim("  These live on Riot's servers and are overwritten from the cloud at login; no supported API can write them.")
    );
}

fn load(ctx: &mut Ctx, name: &str, categories: &str, dry_run: bool, account: Option<&str>) -> Result<()> {
    let cats = parse_categories(categories)?;
    let row = ctx.engine.db.profile(name)?.with_context(|| format!("profile '{name}' not found (see `valsync profiles`)"))?;
    let (t, confident) = ctx.target(account)?;
    if !dry_run {
        ctx.engine.ensure_game_closed()?;
    }
    ctx.confirm_target(&t, confident)?;
    let opts = LoadOptions { categories: cats, include_cloud: false };
    let plan = ctx.engine.plan_load(&row.profile, &t, &opts)?;
    term::heading(&format!("Load '{}' → {}", row.name, ctx.label(&t.puuid)));
    for w in plan.warnings.iter().chain(plan.patch_warning.iter()) {
        term::warn(w);
    }
    if plan.changes.is_empty() {
        term::ok("Nothing to change — this account already has these settings.");
        print_manual(&plan.manual, &row.profile.manual);
        return Ok(());
    }
    print_changes(&plan.changes, 40);
    println!("\n  {} setting(s) will change.", plan.changes.len());
    if dry_run {
        print_manual(&plan.manual, &row.profile.manual);
        term::info("Dry run: nothing was changed.");
        return Ok(());
    }
    if !term::confirm(&format!("Apply to {}? Your current settings are backed up first.", ctx.label(&t.puuid)), ctx.yes)? {
        bail!("cancelled; nothing was changed");
    }
    let out = ctx.engine.apply_load(&row.profile, &t, &opts)?;
    if let Some(txn) = &out.txn {
        term::ok(&format!("Done! {} setting(s) applied and verified.", out.plan.changes.len()));
        println!("  Changed your mind? Run {} (backup #{}).", bold("valsync undo"), txn.pre_backup);
    }
    print_manual(&out.plan.manual, &row.profile.manual);
    Ok(())
}

// --------------------------------------------------------------------- restore / undo / redo
fn restore(ctx: &mut Ctx, backup: Option<i64>, original: bool, account: Option<&str>, dry_run: bool) -> Result<()> {
    let explicit_account = account.is_some();
    let (t, _) = ctx.target(account)?;
    let id = if let Some(id) = backup {
        id
    } else if original {
        ctx.engine.db.original_backup(&t.puuid)?.context("no original snapshot exists for this account yet")?.id
    } else {
        // Default: restore what the account had before the most recent change (undo).
        if dry_run {
            let op = ctx.engine.undo_candidate(&t.puuid)?.context("no previous settings to restore for this account")?;
            op.pre_backup_id
        } else {
            return undo_redo(ctx, true, account);
        }
    };
    let b = ctx.engine.db.backup(id)?.with_context(|| format!("backup #{id} not found"))?;
    if b.account_puuid != t.puuid {
        bail!(
            "backup #{id} belongs to {}, not {}. VALSYNC never restores one account's backup onto another account; pass --account to select its owner.",
            ctx.label(&b.account_puuid),
            ctx.label(&t.puuid)
        );
    }
    let rep = ctx.engine.verify_backup(id)?;
    if !rep.ok {
        bail!("backup #{id} is corrupted ({}); choose another backup", rep.problems.join("; "));
    }
    let changes = ctx.engine.preview_backup(id)?;
    term::heading(&format!("Restore backup #{id} ({}, {}) → {}", b.kind.as_str(), term::local_time(&b.created_at), ctx.label(&b.account_puuid)));
    if changes.is_empty() {
        term::ok("Current settings already match this backup.");
        return Ok(());
    }
    print_changes(&changes, 40);
    if dry_run {
        term::info("Dry run: nothing was changed.");
        return Ok(());
    }
    ctx.engine.ensure_game_closed()?;
    if !term::confirm(&format!("Restore {} setting(s)?", changes.len()), ctx.yes)? {
        bail!("cancelled");
    }
    let out = ctx.engine.restore_backup(id, explicit_account)?;
    if let Some(txn) = out.txn {
        term::ok(&format!("Restored backup #{id} and verified {} file(s). Undo with `valsync undo`.", txn.verified));
    }
    Ok(())
}

fn undo_redo(ctx: &mut Ctx, undo: bool, account: Option<&str>) -> Result<()> {
    let explicit = account.is_some();
    let (t, confident) = ctx.target(account)?;
    ctx.confirm_target(&t, confident)?;
    let op = if undo { ctx.engine.undo_candidate(&t.puuid)? } else { ctx.engine.redo_candidate(&t.puuid)? };
    let op = op.with_context(|| format!("nothing to {} for {}", if undo { "undo" } else { "redo" }, ctx.label(&t.puuid)))?;
    let backup_id = if undo { op.pre_backup_id } else { op.post_backup_id.context("missing redo snapshot")? };
    let changes = ctx.engine.preview_backup(backup_id)?;
    let what = match &op.profile_name {
        Some(p) => format!("{} of {} on {}", op.kind, p, term::local_time(&op.created_at)),
        None => format!("{} on {}", op.kind, term::local_time(&op.created_at)),
    };
    term::heading(&format!("{} {what}", if undo { "Undo" } else { "Redo" }));
    if changes.is_empty() {
        term::info("Settings already match; history pointer will move without file changes.");
    } else {
        print_changes(&changes, 40);
    }
    ctx.engine.ensure_game_closed()?;
    if !term::confirm(&format!("{} {} setting(s) on {}?", if undo { "Revert" } else { "Re-apply" }, changes.len(), ctx.label(&t.puuid)), ctx.yes)? {
        bail!("cancelled");
    }
    let (_, out) = if undo { ctx.engine.undo(&t.puuid, explicit)? } else { ctx.engine.redo(&t.puuid, explicit)? };
    term::ok(&format!("{} complete ({} setting(s), verified).", if undo { "Undo" } else { "Redo" }, out.changes.len()));
    Ok(())
}

// --------------------------------------------------------------------- profiles
fn profiles(ctx: &mut Ctx, action: ProfileAction) -> Result<()> {
    let db = &ctx.engine.db;
    match action {
        ProfileAction::List => {
            let accounts = db.accounts()?;
            let rows: Vec<Vec<String>> = db
                .profiles()?
                .into_iter()
                .map(|p| {
                    let prefs: Vec<String> =
                        accounts.iter().filter(|a| a.preferred_profile.as_deref().map(|x| x.eq_ignore_ascii_case(&p.name)).unwrap_or(false)).map(|a| a.display()).collect();
                    vec![
                        if p.favorite { paint("★", Color::Yellow) } else { " ".into() },
                        bold(&p.name),
                        p.profile.local_count().to_string(),
                        (p.profile.settings.len() - p.profile.local_count()).to_string(),
                        p.profile.patch.clone().unwrap_or_else(|| "?".into()),
                        term::local_time(&p.updated_at),
                        if prefs.is_empty() { "—".into() } else { prefs.join(", ") },
                    ]
                })
                .collect();
            if rows.is_empty() {
                term::info("No profiles yet. Save one with `valsync save \"Main\"`.");
            } else {
                term::table(&["", "Profile", "Local", "Cloud", "Patch", "Updated", "Preferred by"], &rows);
            }
        }
        ProfileAction::Show { name } => {
            let p = db.profile(&name)?.with_context(|| format!("profile '{name}' not found"))?;
            term::heading(&format!("Profile '{}'", p.name));
            println!("  Created {} · patch {}", term::local_time(&p.created_at), p.profile.patch.clone().unwrap_or_else(|| "?".into()));
            if let Some(n) = &p.profile.notes {
                println!("  Note: {n}");
            }
            let rows: Vec<Vec<String>> = p
                .profile
                .settings
                .iter()
                .map(|s| vec![s.category.label().into(), s.key.clone(), s.display_value(), s.scope.label().into()])
                .collect();
            term::table(&["Category", "Setting", "Value", "Scope"], &rows);
            for m in &p.profile.manual {
                println!("  ☐ {}: {}", bold(&m.label), m.value);
            }
        }
        ProfileAction::Rename { old, new } => {
            db.rename_profile(&old, &new)?;
            term::ok(&format!("Renamed '{old}' → '{new}'"));
        }
        ProfileAction::Duplicate { name, new } => {
            let new = profile::validate_name(&new)?;
            let mut p = db.profile(&name)?.with_context(|| format!("profile '{name}' not found"))?.profile;
            p.name = new.clone();
            p.created_at = db::now();
            db.save_profile(&p, "duplicate", false)?;
            term::ok(&format!("Duplicated '{name}' as '{new}'"));
        }
        ProfileAction::Delete { name } => {
            let p = db.profile(&name)?.with_context(|| format!("profile '{name}' not found"))?;
            if term::confirm(&format!("Delete profile '{}'? Backups are not affected.", p.name), ctx.yes)? {
                ctx.engine.db.delete_profile(&p.name)?;
                term::ok("Deleted.");
            }
        }
        ProfileAction::Favorite { name } => {
            let p = db.profile(&name)?.with_context(|| format!("profile '{name}' not found"))?;
            db.set_favorite(&p.name, !p.favorite)?;
            term::ok(&format!("'{}' {}", p.name, if p.favorite { "removed from favorites" } else { "added to favorites ★" }));
        }
        ProfileAction::Check { name } => {
            let p = db.profile(&name)?.with_context(|| format!("profile '{name}' not found"))?;
            let (t, _) = ctx.target(None)?;
            let plan = ctx.engine.plan_load(&p.profile, &t, &LoadOptions::default())?;
            term::heading(&format!("Compatibility of '{}' with {}", p.name, ctx.label(&t.puuid)));
            match &plan.patch_warning {
                Some(w) => term::warn(w),
                None => term::ok(&format!("Patch: {} (matches or unknown)", p.profile.patch.clone().unwrap_or_else(|| "?".into()))),
            }
            term::info(&format!("{} local change(s) would be applied, {} already match", plan.changes.len(), plan.unchanged));
            if plan.new_keys > 0 {
                term::warn(&format!("{} key(s) do not exist in this account's files yet (possibly from another patch); they will be added.", plan.new_keys));
            }
            if !plan.new_files.is_empty() {
                term::warn(&format!("Files that will be created: {}", plan.new_files.join(", ")));
            }
            term::info(&format!("{} cloud-synced setting(s) go to the manual checklist", plan.manual.len()));
        }
        ProfileAction::Note { name, label, value } => {
            let mut p = db.profile(&name)?.with_context(|| format!("profile '{name}' not found"))?.profile;
            p.manual.retain(|m| !m.label.eq_ignore_ascii_case(&label));
            p.manual.push(ManualValue { label: label.clone(), value });
            db.save_profile(&p, "captured", true)?;
            term::ok(&format!("Added '{label}' to the manual checklist of '{name}'"));
        }
    }
    Ok(())
}

// --------------------------------------------------------------------- backups
fn backups(ctx: &mut Ctx, all: bool, action: BackupAction) -> Result<()> {
    match action {
        BackupAction::List => {
            let account = if all { None } else { ctx.target(None).ok().map(|t| t.0.puuid) };
            let list = ctx.engine.db.backups(account.as_deref())?;
            if list.is_empty() {
                term::info("No backups yet. They are created automatically before every load/restore, or manually with `valsync backups create`.");
                return Ok(());
            }
            let rows: Vec<Vec<String>> = list
                .iter()
                .map(|b| {
                    let mut r = vec![
                        format!("#{}", b.id),
                        match b.kind {
                            BackupKind::Original => paint("original", Color::Cyan),
                            BackupKind::Manual => paint("manual", Color::Green),
                            BackupKind::PreChange => "pre-change".into(),
                            BackupKind::PostChange => dim("post-change"),
                        },
                        term::local_time(&b.created_at),
                        b.label.clone(),
                        format!("{} ({})", b.file_count, term::human_bytes(b.total_bytes)),
                        if b.corrupted { paint("CORRUPTED", Color::Red) } else { paint("ok", Color::Green) },
                    ];
                    if all {
                        r.push(ctx.label(&b.account_puuid));
                    }
                    r
                })
                .collect();
            let mut headers = vec!["ID", "Kind", "Created", "Label", "Files", "Integrity"];
            if all {
                headers.push("Account");
            }
            term::table(&headers, &rows);
            if !all {
                println!("{}", dim("Showing the active account. Use --all for every account."));
            }
        }
        BackupAction::Create { label } => {
            let (t, confident) = ctx.target(None)?;
            ctx.confirm_target(&t, confident)?;
            let id = ctx.engine.create_manual_backup(&t, label.as_deref().unwrap_or("Manual backup"))?;
            term::ok(&format!("Created manual backup #{id} of {}", ctx.label(&t.puuid)));
        }
        BackupAction::Verify { id } => {
            let ids: Vec<i64> = match id {
                Some(i) => vec![i],
                None => ctx.engine.db.backups(None)?.into_iter().map(|b| b.id).collect(),
            };
            let mut bad = 0;
            for i in &ids {
                let r = ctx.engine.verify_backup(*i)?;
                if r.ok {
                    if id.is_some() {
                        term::ok(&format!("Backup #{i} is intact"));
                    }
                } else {
                    bad += 1;
                    term::error(&format!("Backup #{i}: {}", r.problems.join("; ")));
                }
            }
            if id.is_none() {
                if bad == 0 { term::ok(&format!("All {} backup(s) verified", ids.len())) } else { bail!("{bad} of {} backup(s) failed verification", ids.len()) }
            } else if bad > 0 {
                bail!("backup failed verification");
            }
        }
        BackupAction::Show { id } => {
            let b = ctx.engine.db.backup(id)?.with_context(|| format!("backup #{id} not found"))?;
            term::heading(&format!("Backup #{id} — {} ({})", b.label, ctx.label(&b.account_puuid)));
            for f in ctx.engine.db.backup_files(id)? {
                println!("  {} {}", f.rel_path, dim(&format!("{} · sha256 {}…", if f.existed { term::human_bytes(f.data.len() as i64) } else { "did not exist".into() }, &f.sha256[..12])));
            }
            let changes = ctx.engine.preview_backup(id)?;
            if changes.is_empty() {
                term::ok("Current settings match this backup.");
            } else {
                println!();
                print_changes(&changes, 60);
            }
        }
        BackupAction::Delete { id } => {
            let b = ctx.engine.db.backup(id)?.with_context(|| format!("backup #{id} not found"))?;
            let mut q = format!("Delete backup #{id} ({})?", b.label);
            if b.kind == BackupKind::Original {
                q = format!("Backup #{id} holds the ORIGINAL settings of {}. Delete it permanently?", ctx.label(&b.account_puuid));
            } else if ctx.engine.db.backup_referenced(id)? {
                q.push_str(" Undo/redo history that depends on it will be removed.");
            }
            if term::confirm(&q, ctx.yes)? {
                ctx.engine.db.delete_backup(id)?;
                term::ok("Deleted.");
            }
        }
    }
    Ok(())
}

// --------------------------------------------------------------------- compare / export / import
fn compare(ctx: &mut Ctx, a: &str, b: &str) -> Result<()> {
    let pa = ctx.engine.db.profile(a)?.with_context(|| format!("profile '{a}' not found"))?;
    let pb = ctx.engine.db.profile(b)?.with_context(|| format!("profile '{b}' not found"))?;
    let d = profile::compare(&pa.profile, &pb.profile);
    term::heading(&format!("'{}' vs '{}'", pa.name, pb.name));
    println!("  {} identical · {} different", d.same, d.changed.len() + d.cloud.len());
    let mut rows: Vec<Vec<String>> = d
        .cloud
        .iter()
        .map(|c| vec![c.category.label().into(), c.key.clone(), paint(&c.old_display(), Color::Cyan), paint(&c.new_display(), Color::Magenta)])
        .collect();
    rows.extend(
        d.changed
            .iter()
            .map(|(x, y)| vec![x.category.label().into(), x.key.clone(), paint(&x.display_value(), Color::Cyan), paint(&y.display_value(), Color::Magenta)]),
    );
    if !rows.is_empty() {
        println!();
        term::table(&["Category", "Setting", &pa.name, &pb.name], &rows);
    }
    for (label, list) in [(pa.name.as_str(), &d.only_a), (pb.name.as_str(), &d.only_b)] {
        if !list.is_empty() {
            println!("\n  {}", bold(&format!("Only in '{label}':")));
            for s in list.iter().take(20) {
                println!("    {} = {}", s.key, s.display_value());
            }
        }
    }
    Ok(())
}

fn export(ctx: &mut Ctx, name: &str, output: Option<std::path::PathBuf>) -> Result<()> {
    let p = ctx.engine.db.profile(name)?.with_context(|| format!("profile '{name}' not found"))?;
    let path = output.unwrap_or_else(|| std::path::PathBuf::from(format!("{}.valsync", profile::file_stem(&p.name))));
    if path.exists() && !term::confirm(&format!("{} exists. Overwrite?", path.display()), ctx.yes)? {
        bail!("cancelled");
    }
    let json = p.profile.to_file_json()?;
    crate::txn::atomic_write(&path, json.as_bytes())?;
    term::ok(&format!("Exported '{}' to {} ({} settings, no account data)", p.name, path.display(), p.profile.settings.len()));
    Ok(())
}

fn import(ctx: &mut Ctx, file: &std::path::Path, name: Option<String>, overwrite: bool) -> Result<()> {
    let meta = std::fs::metadata(file).with_context(|| format!("cannot read {}", file.display()))?;
    if meta.len() > 16 * 1024 * 1024 {
        bail!("{} is too large to be a VALSYNC profile", file.display());
    }
    let text = std::fs::read_to_string(file).with_context(|| format!("cannot read {}", file.display()))?;
    let (mut p, removed) = Profile::from_file_json(&text)?;
    if let Some(n) = name {
        p.name = profile::validate_name(&n)?;
    } else {
        p.name = profile::validate_name(&p.name)?;
    }
    let mut overwrite = overwrite;
    if !overwrite && ctx.engine.db.profile(&p.name)?.is_some() {
        if term::confirm(&format!("Profile '{}' exists. Overwrite?", p.name), ctx.yes)? {
            overwrite = true;
        } else {
            bail!("cancelled (use --name to import under another name)");
        }
    }
    ctx.engine.db.save_profile(&p, "imported", overwrite)?;
    term::ok(&format!("Imported '{}' ({} settings)", p.name, p.settings.len()));
    if removed > 0 {
        term::warn(&format!("{removed} entr(ies) were dropped (identifiers, unsafe paths or unsupported data)."));
    }
    if let Some(w) = crate::game::patch_warning(p.patch.as_deref(), ctx.engine.current_patch().as_deref()) {
        term::warn(&w);
    }
    Ok(())
}

// --------------------------------------------------------------------- live / history
fn tier_text(t: Option<i32>) -> String {
    match t {
        Some(t) => paint(&rank::tier_name(t), rank::term_color(t)),
        None => dim("N/A"),
    }
}

pub fn print_stats(s: &matches::OwnerStats) {
    println!(
        "  Rank {} {} · Peak {} · Level {} · K/D {} · HS {} · Win rate {} ({} games)",
        tier_text(s.current_tier),
        s.rr.map(|r| format!("{r} RR")).unwrap_or_default(),
        tier_text(s.peak_tier),
        s.level.map(|l| l.to_string()).unwrap_or_else(|| "N/A".into()),
        s.kd().map(|k| format!("{k:.2}")).unwrap_or_else(|| "N/A".into()),
        matches::fmt_opt_pct(s.hs_pct()),
        matches::fmt_opt_pct(s.win_rate()),
        s.games
    );
}

pub fn print_match(v: &matches::MatchView) {
    term::heading(&format!("{} · {} · {} {}", v.map, v.mode, v.result_label(), v.score_line()));
    println!("  {} · duration {}", matches::fmt_time(v.started_ms), matches::fmt_duration(v.duration_ms));
    let rows: Vec<Vec<String>> = v
        .players
        .iter()
        .map(|p| {
            vec![
                if p.is_ally { paint("ALLY", Color::Cyan) } else { paint("ENEMY", Color::Red) },
                if p.is_owner { bold(&p.name) } else { p.name.clone() },
                p.agent.clone(),
                format!("{}/{}/{}", p.kills, p.deaths, p.assists),
                format!("{:.2}", p.kills as f64 / p.deaths.max(1) as f64),
                matches::fmt_opt_pct(p.hs_pct),
            ]
        })
        .collect();
    term::table(&["Team", "Player", "Agent", "K/D/A", "K/D", "HS%"], &rows);
}

fn history(ctx: &mut Ctx, count: usize, offline: bool) -> Result<()> {
    let h = if offline {
        let (t, _) = ctx.target(None)?;
        service::history_offline(&ctx.engine.db, &ctx.engine.config, &t.puuid, count.clamp(1, 50))?
    } else {
        let session = ctx.session().context("match history needs the Riot Client open and signed in")?;
        service::runtime().block_on(service::history(&session, &ctx.engine.db, &ctx.engine.config, count.clamp(1, 20)))?
    };
    if let Some(w) = &h.warning {
        term::warn(w);
    }
    term::heading(&format!("Match history — {}", h.account));
    print_stats(&h.stats);
    let rows: Vec<Vec<String>> = h
        .views
        .iter()
        .map(|v| {
            let me = v.owner_line.as_ref();
            vec![
                matches::fmt_time(v.started_ms),
                v.map.clone(),
                v.mode.clone(),
                match v.owner_won {
                    Some(true) => paint("WIN", Color::Green),
                    Some(false) => paint("LOSS", Color::Red),
                    None => "—".into(),
                },
                v.score_line(),
                me.map(|m| m.agent.clone()).unwrap_or_default(),
                me.map(|m| format!("{}/{}/{}", m.kills, m.deaths, m.assists)).unwrap_or_default(),
                matches::fmt_opt_pct(me.and_then(|m| m.hs_pct)),
            ]
        })
        .collect();
    if rows.is_empty() {
        term::info("No matches yet.");
    } else {
        println!();
        term::table(&["Date", "Map", "Mode", "Result", "Score", "Agent", "K/D/A", "HS%"], &rows);
    }
    Ok(())
}

fn live_once(ctx: &mut Ctx) -> Result<()> {
    let session = ctx.session().context("Live Match needs the Riot Client open and signed in")?;
    let d = service::runtime().block_on(service::live(&session, &ctx.engine.db, &ctx.engine.config, None))?;
    if let Some(w) = &d.warning {
        term::warn(w);
    }
    term::heading(&format!("Live match — {}", d.account));
    print_stats(&d.stats);
    match &d.view {
        Some(v) => {
            println!(
                "\n  {} · {} · {}",
                bold(&v.map),
                v.mode,
                if v.phase == matches::LivePhase::AgentSelect { "agent select" } else { "in game" }
            );
            let rows: Vec<Vec<String>> = v
                .players
                .iter()
                .map(|p| {
                    vec![
                        if p.is_ally { paint("ALLY", Color::Cyan) } else { paint("ENEMY", Color::Red) },
                        if p.is_owner { bold(&p.name) } else { p.name.clone() },
                        p.agent.clone(),
                        p.level.map(|l| l.to_string()).unwrap_or_else(|| dim("hidden")),
                        p.vandal.clone().unwrap_or_else(|| dim("N/A")),
                        p.phantom.clone().unwrap_or_else(|| dim("N/A")),
                    ]
                })
                .collect();
            term::table(&["Team", "Player", "Agent", "Level", "Vandal", "Phantom"], &rows);
        }
        None => {
            term::info("You're not in agent select or a match right now.");
            if let Some(m) = &d.last_match {
                print_match(m);
            }
        }
    }
    Ok(())
}

// --------------------------------------------------------------------- doctor / config
fn check(okay: bool, label: &str, detail: &str) {
    if okay {
        println!("  {} {label} {}", paint("✔", Color::Green), dim(detail));
    } else {
        println!("  {} {label} {}", paint("✘", Color::Red), detail);
    }
}

fn doctor(ctx: &mut Ctx) -> Result<()> {
    term::heading("VALSYNC doctor");
    let lock = crate::riot::local::Lockfile::read();
    check(lock.is_some(), "Riot Client running", &crate::riot::local::lockfile_path().display().to_string());
    match ctx.engine.cloud_account() {
        Some(p) => check(true, "Signed in", &ctx.label(&p)),
        None => check(false, "Signed in", &ctx.engine.cloud_error.clone().unwrap_or_default()),
    }
    if let Some(c) = ctx.engine.cloud.as_ref() {
        match c.fetch() {
            Ok(doc) => check(true, "Account settings readable", &format!("{} settings", crate::cloud::entries(&doc).len())),
            Err(e) => check(false, "Account settings readable", &format!("{e:#}")),
        }
    }
    let e = &ctx.engine;
    let cfg_dir = &e.paths.valorant_config;
    check(cfg_dir.is_dir(), "VALORANT settings folder", &cfg_dir.display().to_string());
    let det = e.detection();
    for a in &det.accounts {
        let probe = a.path.join(".valsync-write-test");
        let writable = std::fs::write(&probe, b"x").is_ok() && std::fs::remove_file(&probe).is_ok();
        check(writable, &format!("Video settings of {}", e.account_label(&a.puuid)), if writable { "writable" } else { "NOT writable" });
    }
    let st = e.probe.status();
    check(!st.valorant, "VALORANT closed", if st.valorant { "running — close it to load or restore settings" } else { "yes" });
    let patch = e.current_patch();
    check(patch.is_some(), "Game patch", &patch.unwrap_or_else(|| "unknown (start VALORANT once)".into()));
    let integrity: String = e.db.conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    check(integrity == "ok", "Database", &e.paths.db_path().display().to_string());
    let pending = e.db.pending_ops()?;
    check(pending.is_empty(), "Interrupted operations", &if pending.is_empty() { "none".to_string() } else { format!("{} waiting to be undone", pending.len()) });
    let all = e.db.backups(None)?;
    let bad = all.iter().filter(|b| !e.verify_backup(b.id).map(|r| r.ok).unwrap_or(false)).count();
    check(bad == 0, "Backups", &format!("{} backup(s), {bad} damaged", all.len()));
    Ok(())
}

fn config(ctx: &mut Ctx, action: ConfigAction) -> Result<()> {
    use crate::appconfig::AppConfig;
    match action {
        ConfigAction::Show => {
            let rows: Vec<Vec<String>> =
                AppConfig::KEYS.iter().map(|k| vec![k.to_string(), ctx.engine.config.get_display(k), dim(AppConfig::describe(k))]).collect();
            term::table(&["Setting", "Value", "Description"], &rows);
        }
        ConfigAction::Get { key } => {
            anyhow::ensure!(AppConfig::KEYS.contains(&key.as_str()), "unknown setting '{key}'");
            println!("{}", ctx.engine.config.get_display(&key));
        }
        ConfigAction::Set { key, value } => {
            ctx.engine.config.set_field(&key, &value)?;
            ctx.engine.save_config()?;
            term::ok(&format!("{key} = {}", ctx.engine.config.get_display(&key)));
        }
        ConfigAction::Path => {
            println!("VALORANT settings : {}", ctx.engine.paths.valorant_config.display());
            println!("VALSYNC data      : {}", ctx.engine.paths.data_dir.display());
        }
    }
    Ok(())
}
