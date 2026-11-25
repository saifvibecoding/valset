//! End-to-end tests of the `valsync` binary's direct commands.

mod common;

use common::{Env, a, b};
use std::process::{Command, Output, Stdio};

fn run(env: &Env, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_valsync"))
        .args(["--no-color"])
        .args(args)
        .env("VALSYNC_HOME", &env.paths.data_dir)
        .env("VALSYNC_VALORANT_CONFIG", &env.paths.valorant_config)
        .env("VALSYNC_FAKE_PROCESSES", "riotclientservices.exe")
        .env("VALSYNC_LOCKFILE", env.dir.path().join("no-riot-client"))
        .current_dir(env.dir.path())
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn ok(o: &Output) -> String {
    let s = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "command failed:\n{s}");
    s
}

fn fails(o: &Output) -> String {
    let s = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success(), "command unexpectedly succeeded:\n{s}");
    s
}

#[test]
fn full_cli_workflow() {
    let env = Env::new();
    let s = ok(&run(&env, &["status"]));
    assert!(s.contains("Account") && s.contains("Riot Client is not running"), "{s}");
    let s = ok(&run(&env, &["save", "Main"]));
    assert!(s.contains("Saved 'Main'"), "{s}");
    assert!(s.contains("Only video settings"), "offline save explains what is missing");
    // Overwrite needs confirmation; non-interactive without --yes refuses.
    let s = fails(&run(&env, &["save", "Main"]));
    assert!(s.contains("--yes"));
    env.switch_to(b());
    let s = ok(&run(&env, &["load", "Main", "--dry-run"]));
    assert!(s.contains("Dry run"));
    assert!(env.gus(b()).contains("ResolutionSizeX=1280"));
    let s = fails(&run(&env, &["load", "Main"]));
    assert!(s.contains("--yes"), "{s}");
    let s = ok(&run(&env, &["-y", "load", "Main"]));
    assert!(s.contains("Done!"), "{s}");
    assert!(env.gus(b()).contains("ResolutionSizeX=1920"));
    ok(&run(&env, &["-y", "restore"]));
    assert!(env.gus(b()).contains("ResolutionSizeX=1280"));
    ok(&run(&env, &["-y", "redo"]));
    assert!(env.gus(b()).contains("ResolutionSizeX=1920"));
    ok(&run(&env, &["-y", "undo"]));
    assert!(env.gus(b()).contains("ResolutionSizeX=1280"));
    let s = ok(&run(&env, &["backups"]));
    assert!(s.contains("original") && s.contains("pre-change"));
    let s = ok(&run(&env, &["backups", "verify"]));
    assert!(s.contains("verified"));
    ok(&run(&env, &["-y", "restore", "--original"]));
    let s = ok(&run(&env, &["accounts"]));
    assert_eq!(s.matches("EU").count(), 2, "{s}");
    let s = ok(&run(&env, &["accounts", "switches"]));
    assert!(s.contains("aaaaaaaa") || s.contains("Unknown"), "{s}");
}

#[test]
fn profile_commands() {
    let env = Env::new();
    ok(&run(&env, &["save", "Main"]));
    ok(&run(&env, &["save", "Old", "--account", b()]));
    let s = ok(&run(&env, &["compare", "Main", "Old"]));
    assert!(s.contains("ResolutionSizeX"));
    let s = ok(&run(&env, &["export", "Main"]));
    assert!(s.contains("main.valsync"));
    let file = env.dir.path().join("main.valsync");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(!text.contains(a()), "export must not contain the PUUID");
    ok(&run(&env, &["import", file.to_str().unwrap(), "--name", "Imported"]));
    ok(&run(&env, &["profiles", "rename", "Imported", "Copy"]));
    ok(&run(&env, &["profiles", "duplicate", "Copy", "Copy2"]));
    ok(&run(&env, &["profiles", "favorite", "Copy2"]));
    ok(&run(&env, &["profiles", "check", "Main"]));
    ok(&run(&env, &["profiles", "note", "Main", "Crosshair code", "0;P;h;0"]));
    let s = ok(&run(&env, &["profiles"]));
    assert!(s.contains("Copy2") && s.contains("★"));
    ok(&run(&env, &["-y", "profiles", "delete", "Copy"]));
    let s = fails(&run(&env, &["load", "Nope"]));
    assert!(s.contains("not found"));
    std::fs::write(&file, text.replace("1920", "1921")).unwrap();
    let s = fails(&run(&env, &["import", file.to_str().unwrap(), "--name", "X"]));
    assert!(s.contains("checksum"));
}

#[test]
fn refuses_when_game_running_and_doctor_reports() {
    let env = Env::new();
    ok(&run(&env, &["save", "Main"]));
    env.switch_to(b());
    let o = Command::new(env!("CARGO_BIN_EXE_valsync"))
        .args(["--no-color", "-y", "load", "Main"])
        .env("VALSYNC_HOME", &env.paths.data_dir)
        .env("VALSYNC_VALORANT_CONFIG", &env.paths.valorant_config)
        .env("VALSYNC_FAKE_PROCESSES", "valorant-win64-shipping.exe")
        .env("VALSYNC_LOCKFILE", env.dir.path().join("no-riot-client"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let s = fails(&o);
    assert!(s.contains("VALORANT is running"), "{s}");
    let s = ok(&run(&env, &["doctor"]));
    assert!(s.contains("Database") && s.contains("Riot Client running"), "{s}");
    let s = ok(&run(&env, &["config", "set", "live_poll_secs", "90"]));
    assert!(s.contains("90"));
    let s = fails(&run(&env, &["config", "set", "live_poll_secs", "2"]));
    assert!(s.contains("minimum"));
    let s = ok(&run(&env, &["--help"]));
    for cmd in ["status", "accounts", "save", "load", "restore", "undo", "redo", "profiles", "backups", "compare", "export", "import", "live", "history", "doctor"] {
        assert!(s.contains(cmd), "help is missing {cmd}");
    }
    let s = fails(&run(&env, &["history"]));
    assert!(s.contains("Riot Client"), "{s}");
    ok(&run(&env, &["history", "--offline"]));
    let s = fails(&run(&env, &["live", "--once"]));
    assert!(s.contains("Riot Client"), "{s}");
}
