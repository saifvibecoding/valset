//! Direct (non-interactive) command implementations.

use crate::catalog::{Category, Scope};
use crate::cli::*;
use crate::db::{self, BackupKind};
use crate::engine::{Change, Engine, LoadOptions, Target, parse_categories};
use crate::matches;
use crate::profile::{self, ManualValue, Profile};
use crate::rank;
use crate::riot::RiotApi;
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
    fn api(&self) -> RiotApi {
        RiotApi::new(self.engine.config.api_key())
    }

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
            term::warn(&format!("The active account could not be detected reliably; best guess is {} ({}).", self.label(&t.puuid), t.folder));
            if !term::confirm("Use this account?", false)? {
                bail!("cancelled. Select the account with `valsync accounts select <account>` or pass --account.");
            }
        }
        Ok(())
    }
}

pub fn run(cmd: Command, ctx: &mut Ctx) -> Result<()> {
    report_recovery(&ctx.engine);
    match cmd {
        Command::Status => status(ctx),
        Command::Accounts { action } => accounts(ctx, action.unwrap_or(AccountAction::List)),
        Command::Save { name, overwrite, categories, note, crosshair_code, sens, account } => {
            save(ctx, &name, overwrite, &categories, note, crosshair_code, sens, account.as_deref())
        }
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
        Command::History { count, account, offline } => history(ctx, count, account.as_deref(), offline),
        Command::Doctor => doctor(ctx),
        Command::Config { action } => config(ctx, action.unwrap_or(ConfigAction::Show)),
        Command::Bench => crate::bench::run(ctx),
    }
}

fn report_recovery(e: &Engine) {
    let r = &e.startup_recovery;
    for id in &r.recovered_ops {
        term::warn(&format!("Recovered from an interrupted operation (#{id}): the account's previous settings were restored."));
    }
    for (id, err) in &r.failed_ops {
        term::error(&format!("Interrupted operation #{id} could not be rolled back yet: {err}"));
    }
    if r.deferred {
        term::warn("An interrupted operation needs recovery, but VALORANT is running. Close the game and run any VALSYNC command.");
    }
}

// --------------------------------------------------------------------- status
fn status(ctx: &mut Ctx) -> Result<()> {
    let s = service::status(&ctx.engine)?;
    println!("{}", paint(term::LOGO, Color::Rgb { r: 255, g: 70, b: 85 }));
    println!("{}\n", dim(&format!("  v{} · VALORANT settings manager", env!("CARGO_PKG_VERSION"))));
    let acct = match &s.active {
        Some((row, folder, m)) => format!(
            "{}  {}  {}",
            bold(&row.display()),
            dim(&format!("[{}]", row.shard.clone().unwrap_or_else(|| "?".into()).to_uppercase())),
            dim(&format!("{} · {}", m.label(), folder))
        ),
        None => paint("none detected", Color::Yellow),
    };
    println!("  Active account   {acct}");
    println!("  Riot Client      {} {}", term::dot(s.processes.riot_client), if s.processes.riot_client { "running" } else { "not running" });
    println!(
        "  VALORANT         {} {}",
        if s.processes.valorant { paint("●", Color::Yellow) } else { term::dot(false) },
        if s.processes.valorant { "running (close it before loading/restoring settings)" } else { "closed" }
    );
    println!("  Game patch       {}", s.patch.clone().unwrap_or_else(|| "unknown".into()));
    println!("  Last saved       {}", s.last_saved_profile.clone().unwrap_or_else(|| "—".into()));
    println!("  Profiles         {}", s.profile_count);
    println!("  Backups          {} for this account · {} total", s.backup_count_active, s.backup_count_total);
    println!("  Undo / Redo      {} / {}", if s.undo_available { "available" } else { "—" }, if s.redo_available { "available" } else { "—" });
    println!("  Riot API key     {}", if s.api_key { "configured" } else { "not configured (Riot ID lookups and match data unavailable)" });
    if !s.config_dir_exists {
        term::warn(&format!("VALORANT config folder not found at {}", ctx.engine.paths.valorant_config.display()));
    }
    if let Some(sw) = &s.switch {
        term::info(&format!("Account switch detected: {} → {}", sw.from.as_deref().map(|p| ctx.label(p)).unwrap_or_else(|| "—".into()), ctx.label(&sw.to)));
    }
    Ok(())
}

// --------------------------------------------------------------------- accounts
fn account_puuid(ctx: &Ctx, account: Option<&str>) -> Result<String> {
    Ok(ctx.target(account)?.0.puuid)
}

fn accounts(ctx: &mut Ctx, action: AccountAction) -> Result<()> {
    let rt = service::runtime();
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
                    let local = det.accounts.iter().any(|l| l.puuid == a.puuid);
                    vec![
                        if active.as_deref() == Some(a.puuid.as_str()) { paint("▶", Color::Green) } else { " ".into() },
                        a.riot_id().unwrap_or_else(|| dim("(Riot ID unknown)")),
                        a.shard.clone().unwrap_or_else(|| "?".into()).to_uppercase(),
                        db::short_id(&a.puuid),
                        if local { "yes".into() } else { dim("no") },
                        match (a.source.as_str(), a.verified) {
                            ("rso", _) => paint("RSO verified", Color::Green),
                            (_, true) => paint("API verified", Color::Green),
                            ("manual", false) => paint("manual (unverified)", Color::Yellow),
                            _ => dim("local folder"),
                        },
                        if a.consent { paint("yes", Color::Green) } else { dim("no") },
                        a.preferred_profile.clone().unwrap_or_else(|| "—".into()),
                    ]
                })
                .collect();
            if rows.is_empty() {
                term::warn("No accounts found. Launch VALORANT once with each account so it creates its config folder.");
                return Ok(());
            }
            term::table(&["", "Riot ID", "Shard", "PUUID", "On this PC", "Identity", "RSO consent", "Preferred profile"], &rows);
            if let Some((_, m)) = &det.active {
                println!("\n{}", dim(&format!("Active account: {}", m.label())));
            }
        }
        AccountAction::Name { riot_id, account } => {
            let puuid = account_puuid(ctx, account.as_deref())?;
            let api = ctx.api();
            let cfg = ctx.engine.config.clone();
            match rt.block_on(service::set_riot_id(&api, &mut ctx.engine.db, &cfg, &puuid, &riot_id)) {
                Ok(service::IdentityResult::Verified) => term::ok(&format!("{riot_id} verified via ACCOUNT-V1 and linked to {}", db::short_id(&puuid))),
                Ok(service::IdentityResult::ExistsUnverified) => {
                    term::ok(&format!("{riot_id} exists (ACCOUNT-V1). Saved for {}.", db::short_id(&puuid)));
                    term::info("Ownership of this config folder can only be proven with Riot Sign On (`valsync accounts link`).");
                }
                Ok(service::IdentityResult::StoredManual) => term::ok(&format!("Saved {riot_id} for {} (unverified: no API key).", db::short_id(&puuid))),
                Err(e) => term::warn(&format!("{e:#}")),
            }
        }
        AccountAction::Select { account } => {
            let t = ctx.engine.resolve_target(&account)?;
            ctx.engine.config.manual_active_account = Some(t.puuid.clone());
            ctx.engine.save_config()?;
            term::ok(&format!("Manual fallback account set to {} ({}).", ctx.label(&t.puuid), t.folder));
            term::info("VALORANT's own last-user record still takes priority when present.");
        }
        AccountAction::Auto => {
            ctx.engine.config.manual_active_account = None;
            ctx.engine.save_config()?;
            term::ok("Using automatic account detection.");
        }
        AccountAction::Link { account, no_browser } => {
            let puuid = account_puuid(ctx, account.as_deref())?;
            let api = ctx.api();
            let cfg = ctx.engine.config.clone();
            term::info("Opening Riot Sign On. Sign in on Riot's page; VALSYNC never sees your password.");
            let id = rt.block_on(service::rso_link(&api, &mut ctx.engine.db, &cfg, &puuid, !no_browser, |u| {
                println!("  {}", dim(&format!("Login URL: {u}")));
            }))?;
            term::ok(&format!("Linked {id} with RSO consent. Match history is now available for this account."));
        }
        AccountAction::Unlink { account } => {
            let puuid = account_puuid(ctx, account.as_deref())?;
            ctx.engine.db.set_account_consent(&puuid, false)?;
            ctx.engine.db.conn.execute("DELETE FROM matches WHERE owner_puuid=?1", [&puuid])?;
            term::ok("Consent withdrawn and stored match data deleted.");
        }
        AccountAction::Prefer { profile, account } => {
            let puuid = account_puuid(ctx, account.as_deref())?;
            let p = ctx.engine.db.profile(&profile)?.with_context(|| format!("profile '{profile}' not found"))?;
            ctx.engine.db.touch_account(&puuid, None)?;
            ctx.engine.db.set_preferred_profile(&puuid, Some(&p.name))?;
            term::ok(&format!("Preferred profile for {} is now '{}'.", ctx.label(&puuid), p.name));
        }
        AccountAction::Forget { account } => {
            let puuid = ctx.engine.resolve_target(&account).map(|t| t.puuid).or_else(|_| {
                ctx.engine.db.accounts()?.into_iter().find(|a| a.puuid.starts_with(&account.to_ascii_lowercase())).map(|a| a.puuid).context("account not found")
            })?;
            if term::confirm(&format!("Forget {}? (Game files and backups are kept.)", ctx.label(&puuid)), ctx.yes)? {
                ctx.engine.db.forget_account(&puuid)?;
                term::ok("Account forgotten. It will reappear if its config folder is still on this PC.");
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
#[allow(clippy::too_many_arguments)]
fn save(ctx: &mut Ctx, name: &str, overwrite: bool, categories: &str, note: Option<String>, crosshair: Option<String>, sens: Option<String>, account: Option<&str>) -> Result<()> {
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
    let mut manual = Vec::new();
    if let Some(c) = crosshair {
        manual.push(ManualValue { label: "Crosshair code".into(), value: c });
    }
    if let Some(s) = sens {
        manual.push(ManualValue { label: "Sensitivity".into(), value: s });
    }
    let out = ctx.engine.save_profile(&t, name, overwrite, cats.as_ref(), note, manual)?;
    term::ok(&format!("Saved profile '{}' from {}", out.profile.name, ctx.label(&t.puuid)));
    println!(
        "  {} local settings (transferable) · {} cloud-synced (manual checklist) · {} excluded",
        out.report.local, out.report.cloud, out.report.excluded
    );
    for (reason, n) in &out.report.excluded_reasons {
        println!("  {}", dim(&format!("excluded {n}: {reason}")));
    }
    print_category_summary(&out.profile);
    Ok(())
}

fn print_category_summary(p: &Profile) {
    let cats = p.categories();
    let rows: Vec<Vec<String>> = Category::ALL
        .iter()
        .map(|c| {
            let (l, cl) = cats.get(c).copied().unwrap_or((0, 0));
            let support = if l > 0 {
                paint("transfer", Color::Green)
            } else if cl > 0 {
                paint("manual (cloud)", Color::Yellow)
            } else if c.typical_scope() == Scope::Cloud {
                dim("cloud-synced — not stored locally")
            } else {
                dim("not present in files")
            };
            vec![c.label().to_string(), l.to_string(), cl.to_string(), support]
        })
        .collect();
    term::table(&["Category", "Local", "Cloud", "Support"], &rows);
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
    let opts = LoadOptions { categories: cats, include_cloud: ctx.engine.config.write_cloud_cache };
    let plan = ctx.engine.plan_load(&row.profile, &t, &opts)?;
    term::heading(&format!("Load '{}' → {}", row.name, ctx.label(&t.puuid)));
    if let Some(w) = &plan.patch_warning {
        term::warn(w);
    }
    if plan.changes.is_empty() {
        term::ok(&format!("Nothing to change: {} setting(s) already match.", plan.unchanged));
        print_manual(&plan.manual, &row.profile.manual);
        return Ok(());
    }
    print_changes(&plan.changes, 40);
    println!("\n  {} change(s) in {} file(s); {} already matching; {} key(s) new to this account", plan.changes.len(), plan.ops.len(), plan.unchanged, plan.new_keys);
    if dry_run {
        print_manual(&plan.manual, &row.profile.manual);
        term::info("Dry run: nothing was changed.");
        return Ok(());
    }
    if !term::confirm(&format!("Apply {} change(s) to {}? A backup is created first.", plan.changes.len(), ctx.label(&t.puuid)), ctx.yes)? {
        bail!("cancelled; nothing was changed");
    }
    let out = ctx.engine.apply_load(&row.profile, &t, &opts)?;
    if let Some(txn) = &out.txn {
        term::ok(&format!("Applied and verified {} change(s) ({} file(s) written).", out.plan.changes.len(), txn.files_written));
        println!("  Previous settings saved as backup #{} · undo with {}", txn.pre_backup, bold("valsync undo"));
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
    println!("  {} identical · {} different · {} only in '{}' · {} only in '{}'", d.same, d.changed.len(), d.only_a.len(), pa.name, d.only_b.len(), pb.name);
    if !d.changed.is_empty() {
        let rows: Vec<Vec<String>> = d
            .changed
            .iter()
            .map(|(x, y)| vec![x.category.label().into(), x.key.clone(), paint(&x.display_value(), Color::Cyan), paint(&y.display_value(), Color::Magenta)])
            .collect();
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
pub fn print_match(v: &matches::MatchView) {
    term::heading(&format!("{} · {} · {} {}", v.map, v.mode, v.result_label(), v.score_line()));
    println!("  {} · duration {}", matches::fmt_time(v.started_ms), matches::fmt_duration(v.duration_ms));
    let rows: Vec<Vec<String>> = v
        .players
        .iter()
        .map(|p| {
            let tier = match p.tier {
                Some(t) => paint(&rank::tier_name(t), rank::term_color(t)),
                None => dim("N/A"),
            };
            vec![
                if p.is_ally { paint("ALLY", Color::Cyan) } else { paint("ENEMY", Color::Red) },
                if p.is_owner { bold(&p.name) } else { p.name.clone() },
                p.agent.clone(),
                tier,
                p.level.map(|l| l.to_string()).unwrap_or_else(|| dim("N/A")),
                format!("{}/{}/{}", p.kills, p.deaths, p.assists),
                format!("{:.2}", p.kills as f64 / p.deaths.max(1) as f64),
                matches::fmt_opt_pct(p.hs_pct),
            ]
        })
        .collect();
    term::table(&["Team", "Riot ID", "Agent", "Rank (match)", "Level", "K/D/A", "K/D", "HS%"], &rows);
}

fn history(ctx: &mut Ctx, count: usize, account: Option<&str>, offline: bool) -> Result<()> {
    let (t, _) = ctx.target(account)?;
    let api = ctx.api();
    let cfg = ctx.engine.config.clone();
    let rt = service::runtime();
    let h = rt.block_on(service::history(&api, &mut ctx.engine.db, &cfg, &t.puuid, count.clamp(1, 50), !offline))?;
    if let Some(w) = &h.warning {
        term::warn(w);
    }
    term::heading(&format!("Match history — {}", h.account.display()));
    let s = &h.stats;
    println!(
        "  Games {} · Win rate {} · K/D {} · HS {} · Current rank {} · Peak recorded {} · Level {}",
        s.games,
        matches::fmt_opt_pct(s.win_rate()),
        s.kd().map(|k| format!("{k:.2}")).unwrap_or_else(|| "N/A".into()),
        matches::fmt_opt_pct(s.hs_pct()),
        matches::fmt_tier(s.current_tier),
        matches::fmt_tier(s.peak_tier),
        s.level.map(|l| l.to_string()).unwrap_or_else(|| "N/A".into())
    );
    println!("{}", dim("  RR and equipped skins are not available through supported Riot APIs (N/A)."));
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
        term::info("No matches stored yet.");
    } else {
        println!();
        term::table(&["Date", "Map", "Mode", "Result", "Score", "Agent", "K/D/A", "HS%"], &rows);
    }
    Ok(())
}

fn live_once(ctx: &mut Ctx) -> Result<()> {
    let st = ctx.engine.probe.status();
    term::heading("Live match");
    println!("  VALORANT {}", if st.valorant { paint("running", Color::Green) } else { dim("not running") });
    if st.valorant
        && let Some(m) = crate::game::current_map_from_log(&ctx.engine.paths.valorant_logs)
    {
        println!("  Map (your client, local log): {}", crate::game::map_display_name(&m));
    }
    term::warn("Riot provides no supported live-match API for VALORANT. Rosters, ranks and loadouts of a match in progress are N/A.");
    term::info("The latest completed match (VAL-MATCH-V1, RSO-linked accounts only):");
    let (t, _) = ctx.target(None)?;
    let api = ctx.api();
    let cfg = ctx.engine.config.clone();
    let rt = service::runtime();
    match rt.block_on(service::history(&api, &mut ctx.engine.db, &cfg, &t.puuid, 1, true)) {
        Ok(h) => {
            if let Some(w) = &h.warning {
                term::warn(w);
            }
            match h.views.first() {
                Some(v) => print_match(v),
                None => term::info("No completed matches stored yet."),
            }
        }
        Err(e) => term::warn(&format!("{e:#}")),
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
    let e = &ctx.engine;
    term::heading("VALSYNC doctor");
    let cfg_dir = &e.paths.valorant_config;
    check(cfg_dir.is_dir(), "VALORANT config folder", &cfg_dir.display().to_string());
    let det = e.detection();
    check(!det.accounts.is_empty(), "Account config folders", &format!("{} found", det.accounts.len()));
    check(det.last_known_user.is_some(), "VALORANT last-user record (RiotLocalMachine.ini)", if det.last_known_user.is_some() { "present" } else { "missing — active account will be guessed or must be selected" });
    match &det.active {
        Some((a, m)) => check(m.is_confident(), "Active account", &format!("{} · {}", e.account_label(&a.puuid), m.label())),
        None => check(false, "Active account", "none"),
    }
    for a in &det.accounts {
        let probe = a.path.join(".valsync-write-test");
        let writable = std::fs::write(&probe, b"x").is_ok() && std::fs::remove_file(&probe).is_ok();
        let files = Engine::read_account_files(&a.path).map(|f| f.len()).unwrap_or(0);
        check(writable, &format!("Folder {}", a.folder), &format!("{files} ini file(s), {}", if writable { "writable" } else { "NOT writable" }));
    }
    let st = e.probe.status();
    check(true, "Riot Client", if st.riot_client { "running" } else { "not running" });
    check(!st.valorant, "VALORANT closed", if st.valorant { "running — loads/restores are blocked until it is closed" } else { "yes" });
    check(true, "Vanguard", if st.vanguard { "running (VALSYNC does not interact with it)" } else { "not detected (VALSYNC does not interact with it)" });
    let patch = e.current_patch();
    check(patch.is_some(), "Game patch", &patch.unwrap_or_else(|| "unknown (no ShooterGame.log yet)".into()));
    let integrity: String = e.db.conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    check(integrity == "ok", "Database integrity", &e.paths.db_path().display().to_string());
    let pending = e.db.pending_ops()?;
    check(pending.is_empty(), "Operation journal", &if pending.is_empty() { "no interrupted operations".to_string() } else { format!("{} interrupted operation(s) awaiting recovery", pending.len()) });
    let mut bad = 0;
    let all = e.db.backups(None)?;
    for b in &all {
        if !e.verify_backup(b.id)?.ok {
            bad += 1;
        }
    }
    check(bad == 0, "Backup integrity", &format!("{} backup(s), {bad} corrupted", all.len()));
    let key = e.config.api_key();
    check(key.is_some(), "Riot API key", if key.is_some() { "configured" } else { "not set — Riot ID verification and match data disabled" });
    if key.is_some() {
        let api = ctx.api();
        let shard = e.config.default_shard.clone().unwrap_or_else(|| "na".into());
        let rt = service::runtime();
        let mut db = crate::db::Db::open(&e.paths.db_path())?;
        match rt.block_on(api.platform_status(&mut db, &shard)) {
            Ok(p) => check(true, "VAL-STATUS-V1", &format!("{} · {} maintenance(s), {} incident(s)", p.name, p.maintenances.len(), p.incidents.len())),
            Err(err) => check(false, "VAL-STATUS-V1", &err.to_string()),
        }
    }
    check(e.config.rso_client_id.is_some(), "RSO client", if e.config.rso_client_id.is_some() { "configured" } else { "not configured — required by Riot for match data (approved products only)" });
    term::heading("Settings support");
    for c in Category::ALL {
        println!("  {:<26} {}", c.label(), dim(crate::catalog::category_support_note(c)));
    }
    Ok(())
}

fn config(ctx: &mut Ctx, action: ConfigAction) -> Result<()> {
    match action {
        ConfigAction::Show => {
            let rows: Vec<Vec<String>> = crate::appconfig::AppConfig::KEYS
                .iter()
                .map(|k| vec![k.to_string(), ctx.engine.config.get_display(k), dim(crate::appconfig::AppConfig::describe(k))])
                .collect();
            term::table(&["Setting", "Value", "Description"], &rows);
        }
        ConfigAction::Get { key } => {
            anyhow::ensure!(crate::appconfig::AppConfig::KEYS.contains(&key.as_str()), "unknown setting '{key}'");
            println!("{}", ctx.engine.config.get_display(&key));
        }
        ConfigAction::Set { key, value } => {
            ctx.engine.config.set_field(&key, &value)?;
            ctx.engine.save_config()?;
            term::ok(&format!("{key} = {}", ctx.engine.config.get_display(&key)));
        }
        ConfigAction::Path => {
            println!("VALORANT config : {}", ctx.engine.paths.valorant_config.display());
            println!("VALORANT logs   : {}", ctx.engine.paths.valorant_logs.display());
            println!("VALSYNC data    : {}", ctx.engine.paths.data_dir.display());
            println!("Database        : {}", ctx.engine.paths.db_path().display());
            println!("Config file     : {}", ctx.engine.paths.config_path().display());
        }
    }
    Ok(())
}
