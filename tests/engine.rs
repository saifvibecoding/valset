mod common;

use common::{Env, a, b};
use std::collections::BTreeSet;
use valsync::accounts::{self, DetectMethod};
use valsync::catalog::Category;
use valsync::db::OpStatus;
use valsync::engine::{EngineError, LoadOptions};
use valsync::game::ProcessProbe;
use valsync::ini::IniDoc;
use valsync::profile::Profile;
use valsync::txn::Faults;

fn val(text: &str, section: &str, key: &str) -> Option<String> {
    IniDoc::parse(text).get_first(section, key)
}

const GUS: &str = "/Script/ShooterGame.ShooterGameUserSettings";

// ------------------------------------------------------------------ account switching

#[test]
fn detects_active_account_and_switches() {
    let env = Env::new();
    let e = env.engine();
    let det = e.detection();
    assert_eq!(det.accounts.len(), 2);
    let (active, method) = det.active.clone().unwrap();
    assert_eq!(active.puuid, a());
    assert_eq!(method, DetectMethod::LastKnownUser);
    // First sync records the account but is not a "switch".
    assert!(accounts::sync_with_db(&e.db, &det).unwrap().is_none());
    env.switch_to(b());
    let det = e.detection();
    let sw = accounts::sync_with_db(&e.db, &det).unwrap().expect("switch detected");
    assert_eq!(sw.from.as_deref(), Some(a()));
    assert_eq!(sw.to, b());
    assert_eq!(e.db.switch_events(10).unwrap().len(), 1);
    // Remembered accounts.
    assert_eq!(e.db.accounts().unwrap().len(), 2);
    // No repeated switch on the next poll.
    assert!(accounts::sync_with_db(&e.db, &e.detection()).unwrap().is_none());
}

#[test]
fn manual_fallback_and_guessing() {
    let env = Env::new();
    std::fs::remove_file(env.config.join("RiotLocalMachine.ini")).unwrap();
    let mut e = env.engine();
    let (_, method) = e.detection().active.unwrap();
    assert_eq!(method, DetectMethod::MostRecentFolder);
    assert!(!method.is_confident());
    e.config.manual_active_account = Some(b().to_string());
    let (acc, method) = e.detection().active.unwrap();
    assert_eq!((acc.puuid.as_str(), method), (b(), DetectMethod::Manual));
    // A manual selection that has no local folder is ignored (verified selection).
    e.config.manual_active_account = Some("cccccccc-3333-4333-8333-333333333333".into());
    assert_ne!(e.detection().active.unwrap().1, DetectMethod::Manual);
}

// ------------------------------------------------------------------ cross-account transfer

#[test]
fn cross_account_transfer_and_restore() {
    let env = Env::new();
    let mut e = env.engine();
    let before_b = env.snapshot_bytes(b());

    // 1-4. Account A saves its settings.
    let (ta, _) = e.active_target().unwrap();
    assert_eq!(ta.puuid, a());
    let saved = e.save_profile(&ta, "Main", false, None, None, vec![]).unwrap();
    assert!(saved.report.local > 0 && saved.report.cloud > 0 && saved.report.excluded > 0);
    // Profile never contains identity / machine data.
    let json = saved.profile.to_file_json().unwrap();
    assert!(!json.contains(a()), "profile leaked the PUUID");
    assert!(!json.contains("DefaultMonitorDeviceID"));
    assert!(!json.contains("PlayerUuid"));

    // 5-7. Switch to B and load.
    env.switch_to(b());
    let (tb, m) = e.active_target().unwrap();
    assert_eq!((tb.puuid.as_str(), m), (b(), DetectMethod::LastKnownUser));
    let p = e.db.profile("Main").unwrap().unwrap().profile;
    let out = e.apply_load(&p, &tb, &LoadOptions::default()).unwrap();
    let txn = out.txn.expect("changes applied");

    // 8. B's previous settings were backed up first.
    let pre = e.db.backup(txn.pre_backup).unwrap().unwrap();
    assert_eq!(pre.account_puuid, b());
    assert!(e.db.original_backup(b()).unwrap().is_some());

    // 9-10. Compatible (local) settings applied & verified.
    let gus = env.gus(b());
    assert_eq!(val(&gus, GUS, "ResolutionSizeX").as_deref(), Some("1920"));
    assert_eq!(val(&gus, GUS, "FrameRateLimit").as_deref(), Some("240.000000"));
    assert_eq!(val(&gus, "ScalabilityGroups", "sg.ShadowQuality").as_deref(), Some("0"));
    // Machine/identity keys of B untouched.
    assert!(gus.contains("DefaultMonitorDeviceID=\\\\?\\DISPLAY#GSM5BBF"));
    // Cloud-synced values are NOT written by default and are reported for the checklist.
    assert_eq!(val(&env.rus(b()), "Settings", "EAresFloatSettingName::MouseSensitivity").as_deref(), Some("0.250000"));
    assert!(out.plan.manual.iter().any(|m| m.key.ends_with("MouseSensitivity") && m.values == vec!["0.176000"]));
    assert!(e.verify_changes(&tb, &out.plan.changes).unwrap().is_empty());

    // Restore Previous Settings → byte-identical original files.
    e.undo(b(), false).unwrap();
    assert_eq!(env.snapshot_bytes(b()), before_b);
    // Account A was never modified.
    assert_eq!(val(&env.gus(a()), GUS, "ResolutionSizeX").as_deref(), Some("1920"));
}

#[test]
fn sensitivity_example_with_cloud_cache_write() {
    // The README example: B has 0.25, the profile has 0.176; restore must give 0.25 back.
    let env = Env::new();
    let mut e = env.engine();
    let (ta, _) = e.active_target().unwrap();
    e.save_profile(&ta, "Main", false, None, None, vec![]).unwrap();
    env.switch_to(b());
    let (tb, _) = e.active_target().unwrap();
    let p = e.db.profile("Main").unwrap().unwrap().profile;
    let opts = LoadOptions { categories: None, include_cloud: true };
    e.apply_load(&p, &tb, &opts).unwrap();
    assert_eq!(val(&env.rus(b()), "Settings", "EAresFloatSettingName::MouseSensitivity").as_deref(), Some("0.176000"));
    // The identity key in the cloud cache still belongs to B.
    assert!(env.rus(b()).contains(&format!("PlayerUuid={}", b())));
    e.undo(b(), false).unwrap();
    assert_eq!(val(&env.rus(b()), "Settings", "EAresFloatSettingName::MouseSensitivity").as_deref(), Some("0.250000"));
    assert_eq!(val(&env.gus(b()), GUS, "ResolutionSizeX").as_deref(), Some("1280"));
}

#[test]
fn selective_category_transfer() {
    let env = Env::new();
    let mut e = env.engine();
    let (ta, _) = e.active_target().unwrap();
    e.save_profile(&ta, "Main", false, None, None, vec![]).unwrap();
    let tb = env.target(b());
    let p = e.db.profile("Main").unwrap().unwrap().profile;
    let cats: BTreeSet<Category> = [Category::FpsLimits].into_iter().collect();
    let out = e.apply_load(&p, &tb, &LoadOptions { categories: Some(cats), include_cloud: false }).unwrap();
    assert!(out.plan.changes.iter().all(|c| c.category == Category::FpsLimits));
    let gus = env.gus(b());
    assert_eq!(val(&gus, GUS, "FrameRateLimit").as_deref(), Some("240.000000"));
    assert_eq!(val(&gus, GUS, "ResolutionSizeX").as_deref(), Some("1280"), "resolution must not change");
}

#[test]
fn profiles_are_reusable_across_accounts() {
    let env = Env::new();
    let mut e = env.engine();
    let (ta, _) = e.active_target().unwrap();
    e.save_profile(&ta, "Main", false, None, None, vec![]).unwrap();
    // A third account appears later and can use the same profile.
    let c = "cccccccc-3333-4333-8333-333333333333";
    let mut acc = valsync::sample::account_b();
    acc.puuid = c;
    valsync::sample::write_account(&env.config, &acc).unwrap();
    env.switch_to(c);
    let (tc, _) = e.active_target().unwrap();
    let p = e.db.profile("Main").unwrap().unwrap().profile;
    e.apply_load(&p, &tc, &LoadOptions::default()).unwrap();
    assert_eq!(val(&env.gus(c), GUS, "ResolutionSizeX").as_deref(), Some("1920"));
}

#[test]
fn refuses_while_game_running() {
    let env = Env::new();
    let mut e = env.engine();
    let (ta, _) = e.active_target().unwrap();
    e.save_profile(&ta, "Main", false, None, None, vec![]).unwrap();
    e.probe = ProcessProbe::Fixed(vec!["valorant-win64-shipping.exe".into()]);
    let p = e.db.profile("Main").unwrap().unwrap().profile;
    let before = env.snapshot_bytes(b());
    let err = e.apply_load(&p, &env.target(b()), &LoadOptions::default()).unwrap_err();
    assert!(matches!(err.downcast_ref::<EngineError>(), Some(EngineError::GameRunning)));
    assert_eq!(env.snapshot_bytes(b()), before);
}

// ------------------------------------------------------------------ undo / redo / original

#[test]
fn undo_redo_stack_semantics() {
    let env = Env::new();
    let mut e = env.engine();
    let (ta, _) = e.active_target().unwrap();
    e.save_profile(&ta, "Main", false, None, None, vec![]).unwrap();
    env.switch_to(b());
    let tb = env.target(b());
    let main = e.db.profile("Main").unwrap().unwrap().profile;
    let original = env.snapshot_bytes(b());

    // Second profile: only FPS 144.
    let mut fps = main.clone();
    fps.name = "FPS144".into();
    fps.settings.retain(|s| s.key == "FrameRateLimit");
    fps.settings[0].values = vec!["144.000000".into()];
    e.db.save_profile(&fps, "test", false).unwrap();

    e.apply_load(&main, &tb, &LoadOptions::default()).unwrap(); // op1
    let after_main = env.snapshot_bytes(b());
    e.apply_load(&fps, &tb, &LoadOptions::default()).unwrap(); // op2
    let after_fps = env.snapshot_bytes(b());
    assert_eq!(val(&env.gus(b()), GUS, "FrameRateLimit").as_deref(), Some("144.000000"));

    e.undo(b(), false).unwrap();
    assert_eq!(env.snapshot_bytes(b()), after_main);
    e.undo(b(), false).unwrap();
    assert_eq!(env.snapshot_bytes(b()), original);
    assert!(e.undo_candidate(b()).unwrap().is_none());

    e.redo(b(), false).unwrap();
    assert_eq!(env.snapshot_bytes(b()), after_main);
    e.redo(b(), false).unwrap();
    assert_eq!(env.snapshot_bytes(b()), after_fps);
    assert!(e.redo_candidate(b()).unwrap().is_none());

    // A new change clears the redo stack.
    e.undo(b(), false).unwrap();
    assert!(e.redo_candidate(b()).unwrap().is_some());
    e.apply_load(&fps, &tb, &LoadOptions::default()).unwrap();
    assert!(e.redo_candidate(b()).unwrap().is_none());

    // Restore original settings.
    e.restore_original(b(), false).unwrap();
    assert_eq!(env.snapshot_bytes(b()), original);
    // ...which itself can be undone.
    e.undo(b(), false).unwrap();
    assert_eq!(val(&env.gus(b()), GUS, "FrameRateLimit").as_deref(), Some("144.000000"));
}

#[test]
fn never_restores_another_accounts_backup() {
    let env = Env::new();
    let mut e = env.engine();
    let (ta, _) = e.active_target().unwrap();
    let id_a = e.create_manual_backup(&ta, "A backup").unwrap();
    env.switch_to(b());
    let before_b = env.snapshot_bytes(b());
    let err = e.restore_backup(id_a, false).unwrap_err();
    assert!(matches!(err.downcast_ref::<EngineError>(), Some(EngineError::AccountMismatch { .. })), "{err:#}");
    assert_eq!(env.snapshot_bytes(b()), before_b, "B must be untouched");
    // Even when explicitly allowed, the backup only ever targets its own account's folder.
    std::fs::write(env.folder(a()).join("Windows").join("GameUserSettings.ini"), "[x]\r\ny=1\r\n").unwrap();
    e.restore_backup(id_a, true).unwrap();
    assert_eq!(env.snapshot_bytes(b()), before_b);
    assert!(env.gus(a()).contains("ResolutionSizeX=1920"));
}

// ------------------------------------------------------------------ integrity

#[test]
fn corrupted_backup_is_detected_and_refused() {
    let env = Env::new();
    let mut e = env.engine();
    let (ta, _) = e.active_target().unwrap();
    let id = e.create_manual_backup(&ta, "good").unwrap();
    assert!(e.verify_backup(id).unwrap().ok);
    // Flip bytes inside the stored blob.
    e.db.conn
        .execute("UPDATE backup_files SET data = CAST(REPLACE(CAST(data AS TEXT), '1920', '9999') AS BLOB) WHERE backup_id=?1", [id])
        .unwrap();
    let rep = e.verify_backup(id).unwrap();
    assert!(!rep.ok);
    assert!(rep.problems.iter().any(|p| p.contains("checksum")));
    assert!(e.db.backup(id).unwrap().unwrap().corrupted);
    std::fs::write(env.folder(a()).join("Windows").join("GameUserSettings.ini"), "changed").unwrap();
    let err = e.restore_backup(id, false).unwrap_err();
    assert!(matches!(err.downcast_ref::<EngineError>(), Some(EngineError::Integrity(..))), "{err:#}");
    assert_eq!(std::fs::read_to_string(env.folder(a()).join("Windows").join("GameUserSettings.ini")).unwrap(), "changed");
}

#[test]
fn missing_backup_file_row_is_detected() {
    let env = Env::new();
    let mut e = env.engine();
    let (ta, _) = e.active_target().unwrap();
    let id = e.create_manual_backup(&ta, "x").unwrap();
    e.db.conn.execute("DELETE FROM backup_files WHERE backup_id=?1 AND rel_path LIKE '%Riot%'", [id]).unwrap();
    let rep = e.verify_backup(id).unwrap();
    assert!(!rep.ok);
    assert!(rep.problems.iter().any(|p| p.contains("expected 2 file")));
}

// ------------------------------------------------------------------ interrupted writes

fn setup_transfer(env: &Env) -> (valsync::engine::Engine, Profile) {
    let mut e = env.engine();
    let (ta, _) = e.active_target().unwrap();
    e.save_profile(&ta, "Main", false, None, None, vec![]).unwrap();
    env.switch_to(b());
    let p = e.db.profile("Main").unwrap().unwrap().profile;
    (e, p)
}

#[test]
fn failed_write_rolls_back_every_file() {
    let env = Env::new();
    let (mut e, p) = setup_transfer(&env);
    let before = env.snapshot_bytes(b());
    // Two files change (cloud write enabled) and the second write fails.
    e.faults = Faults { fail_before_write: Some(1), ..Default::default() };
    let err = e.apply_load(&p, &env.target(b()), &LoadOptions { categories: None, include_cloud: true }).unwrap_err();
    assert!(matches!(err.downcast_ref::<EngineError>(), Some(EngineError::RolledBack(_))), "{err:#}");
    assert_eq!(env.snapshot_bytes(b()), before, "first file must have been rolled back");
    let ops = e.db.ops(Some(b()), 5).unwrap();
    assert_eq!(ops[0].status, OpStatus::RolledBack);
    assert!(e.undo_candidate(b()).unwrap().is_none(), "a rolled-back transfer is not undoable");
}

#[test]
fn corrupted_write_is_caught_by_verification() {
    let env = Env::new();
    let (mut e, p) = setup_transfer(&env);
    let before = env.snapshot_bytes(b());
    e.faults = Faults { corrupt_write: Some(0), ..Default::default() };
    let err = e.apply_load(&p, &env.target(b()), &LoadOptions::default()).unwrap_err();
    assert!(format!("{err:#}").contains("verification failed"), "{err:#}");
    assert_eq!(env.snapshot_bytes(b()), before);
}

#[test]
fn crash_mid_transfer_is_recovered_on_next_start() {
    let env = Env::new();
    let (mut e, p) = setup_transfer(&env);
    let before = env.snapshot_bytes(b());
    e.faults = Faults { crash_during_write: Some(1), ..Default::default() };
    let err = e.apply_load(&p, &env.target(b()), &LoadOptions { categories: None, include_cloud: true }).unwrap_err();
    assert!(matches!(err.downcast_ref::<EngineError>(), Some(EngineError::SimulatedCrash)));
    drop(e); // "process died"
    // The folder is now torn: one file new, one half-written, plus a temp file.
    assert_ne!(env.snapshot_bytes(b()), before);
    let tmp_exists = std::fs::read_dir(env.folder(b()).join("Windows")).unwrap().flatten().any(|f| f.file_name().to_string_lossy().ends_with(".valsync-tmp"));
    assert!(tmp_exists);

    // Next start: recovery rolls everything back.
    let e = env.reopen();
    assert_eq!(e.startup_recovery.recovered_ops.len(), 1);
    assert!(e.startup_recovery.stray_tmp_removed >= 1);
    assert_eq!(env.snapshot_bytes(b()), before);
    assert!(e.db.pending_ops().unwrap().is_empty());
    let ops = e.db.ops(Some(b()), 1).unwrap();
    assert_eq!(ops[0].status, OpStatus::Recovered);
}

#[test]
fn recovery_waits_while_game_running() {
    let env = Env::new();
    let (mut e, p) = setup_transfer(&env);
    e.faults = Faults { crash_during_write: Some(0), ..Default::default() };
    let _ = e.apply_load(&p, &env.target(b()), &LoadOptions::default());
    drop(e);
    let mut e = env.engine();
    e.probe = ProcessProbe::Fixed(vec!["valorant-win64-shipping.exe".into()]);
    let rep = e.recover().unwrap();
    assert!(rep.deferred);
    assert_eq!(e.db.pending_ops().unwrap().len(), 1);
    e.probe = ProcessProbe::Fixed(vec![]);
    let rep = e.recover().unwrap();
    assert_eq!(rep.recovered_ops.len(), 1);
}

#[test]
fn readonly_config_file_is_handled() {
    let env = Env::new();
    let (mut e, p) = setup_transfer(&env);
    let f = env.folder(b()).join("Windows").join("GameUserSettings.ini");
    let mut perms = std::fs::metadata(&f).unwrap().permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&f, perms).unwrap();
    e.apply_load(&p, &env.target(b()), &LoadOptions::default()).unwrap();
    assert!(std::fs::metadata(&f).unwrap().permissions().readonly(), "read-only flag preserved");
    assert!(env.gus(b()).contains("ResolutionSizeX=1920"));
    e.undo(b(), false).unwrap();
    assert!(env.gus(b()).contains("ResolutionSizeX=1280"));
}

// ------------------------------------------------------------------ profiles / files

#[test]
fn export_import_roundtrip_and_tamper_detection() {
    let env = Env::new();
    let mut e = env.engine();
    let (ta, _) = e.active_target().unwrap();
    let saved = e.save_profile(&ta, "Main", false, None, None, vec![]).unwrap();
    let json = saved.profile.to_file_json().unwrap();
    let (back, removed) = Profile::from_file_json(&json).unwrap();
    assert_eq!(removed, 0);
    assert_eq!(back, saved.profile);
    let tampered = json.replace("1920", "1921");
    assert!(Profile::from_file_json(&tampered).unwrap_err().to_string().contains("checksum"));
    assert!(Profile::from_file_json("{not json").is_err());
}

#[test]
fn import_sanitizes_hostile_profiles() {
    use valsync::catalog::Scope;
    use valsync::profile::SettingEntry;
    let mut p = Profile { name: "Evil".into(), created_at: "x".into(), patch: None, notes: None, settings: vec![], manual: vec![] };
    let entry = |file: &str, key: &str, v: &str| SettingEntry {
        file: file.into(),
        section: "S".into(),
        key: key.into(),
        values: vec![v.into()],
        category: Category::Resolution,
        scope: Scope::Local,
    };
    p.settings.push(entry("../../evil.ini", "ResolutionSizeX", "1"));
    p.settings.push(entry("Windows/GameUserSettings.ini", "AuthToken", "secret"));
    p.settings.push(entry("Windows/GameUserSettings.ini", "Owner", "aaaaaaaa-1111-4111-8111-111111111111"));
    p.settings.push(entry("Windows/GameUserSettings.ini", "Inject", "1\r\n[Other]\r\nx=1"));
    p.settings.push(entry("Windows/GameUserSettings.ini", "FrameRateLimit", "144"));
    let json = p.to_file_json().unwrap(); // valid checksum, hostile content
    let (clean, removed) = Profile::from_file_json(&json).unwrap();
    assert_eq!(removed, 4);
    assert_eq!(clean.settings.len(), 1);
    assert_eq!(clean.settings[0].category, Category::FpsLimits, "category is re-derived, not trusted");
}

#[test]
fn compare_profiles() {
    let env = Env::new();
    let mut e = env.engine();
    let (ta, _) = e.active_target().unwrap();
    e.save_profile(&ta, "A", false, None, None, vec![]).unwrap();
    e.save_profile(&env.target(b()), "B", false, None, None, vec![]).unwrap();
    let pa = e.db.profile("A").unwrap().unwrap().profile;
    let pb = e.db.profile("B").unwrap().unwrap().profile;
    let d = valsync::profile::compare(&pa, &pb);
    assert!(d.changed.iter().any(|(x, _)| x.key == "ResolutionSizeX"));
    assert!(d.same > 0);
}

#[test]
fn profile_management_operations() {
    let env = Env::new();
    let mut e = env.engine();
    let (ta, _) = e.active_target().unwrap();
    e.save_profile(&ta, "Main", false, None, None, vec![]).unwrap();
    assert!(e.save_profile(&ta, "main", false, None, None, vec![]).is_err(), "names are case-insensitive unique");
    e.db.touch_account(a(), None).unwrap();
    e.db.set_preferred_profile(a(), Some("Main")).unwrap();
    e.db.rename_profile("Main", "Primary").unwrap();
    assert_eq!(e.db.account(a()).unwrap().unwrap().preferred_profile.as_deref(), Some("Primary"));
    e.db.set_favorite("Primary", true).unwrap();
    assert!(e.db.profile("primary").unwrap().unwrap().favorite);
    e.db.delete_profile("Primary").unwrap();
    assert!(e.db.account(a()).unwrap().unwrap().preferred_profile.is_none());
}

#[test]
fn patch_compatibility_warning() {
    let env = Env::new();
    let logs = env.paths.valorant_logs.clone();
    std::fs::create_dir_all(&logs).unwrap();
    std::fs::write(logs.join("ShooterGame.log"), "LogInit: x\nLogShooter: Display: Branch: release-11.08\n").unwrap();
    let mut e = env.engine();
    let (ta, _) = e.active_target().unwrap();
    let mut out = e.save_profile(&ta, "Main", false, None, None, vec![]).unwrap();
    assert_eq!(out.profile.patch.as_deref(), Some("release-11.08"));
    out.profile.patch = Some("release-10.01".into());
    let plan = e.plan_load(&out.profile, &env.target(b()), &LoadOptions::default()).unwrap();
    assert!(plan.patch_warning.unwrap().contains("major"));
}

#[test]
fn backup_retention_prunes_only_automatic_backups() {
    let env = Env::new();
    let (mut e, p) = setup_transfer(&env);
    e.config.auto_backup_retention = 2;
    let tb = env.target(b());
    let manual = e.create_manual_backup(&tb, "keep me").unwrap();
    let mut fps = p.clone();
    for i in 0..5 {
        fps.settings.iter_mut().filter(|s| s.key == "FrameRateLimit").for_each(|s| s.values = vec![format!("{}.0", 100 + i)]);
        e.apply_load(&fps, &tb, &LoadOptions::default()).unwrap();
    }
    let backups = e.db.backups(Some(b())).unwrap();
    let pre = backups.iter().filter(|b| b.kind == valsync::db::BackupKind::PreChange).count();
    assert_eq!(pre, 2);
    assert!(backups.iter().any(|b| b.id == manual));
    assert!(e.db.original_backup(b()).unwrap().is_some());
    // Undo still works for the most recent change.
    e.undo(b(), false).unwrap();
}
