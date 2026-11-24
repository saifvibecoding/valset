#![allow(dead_code)]

use std::path::PathBuf;
use valsync::appconfig::AppConfig;
use valsync::db::Db;
use valsync::engine::{Engine, Target};
use valsync::game::ProcessProbe;
use valsync::paths::Paths;
use valsync::sample::{self, PUUID_A, PUUID_B};

pub struct Env {
    pub dir: tempfile::TempDir,
    pub paths: Paths,
    pub config: PathBuf,
}

impl Env {
    pub fn new() -> Env {
        let dir = tempfile::tempdir().unwrap();
        let saved = dir.path().join("VALORANT").join("Saved");
        let config = saved.join("Config");
        sample::write_account(&config, &sample::account_a()).unwrap();
        sample::write_account(&config, &sample::account_b()).unwrap();
        sample::set_last_known_user(&config, PUUID_A).unwrap();
        let paths = Paths::with_roots(&saved, &dir.path().join("data"));
        Env { dir, paths, config }
    }

    pub fn engine(&self) -> Engine {
        let db = Db::open(&self.paths.db_path()).unwrap();
        Engine::with_parts(self.paths.clone(), db, AppConfig::default(), ProcessProbe::Fixed(vec!["riotclientservices.exe".into()]))
    }

    /// Full startup path (runs crash recovery).
    pub fn reopen(&self) -> Engine {
        let mut e = self.engine();
        e.startup_recovery = e.recover().unwrap();
        e
    }

    pub fn switch_to(&self, puuid: &str) {
        sample::set_last_known_user(&self.config, puuid).unwrap();
    }

    pub fn folder(&self, puuid: &str) -> PathBuf {
        self.config.join(format!("{puuid}-eu"))
    }

    pub fn gus(&self, puuid: &str) -> String {
        std::fs::read_to_string(self.folder(puuid).join("Windows").join("GameUserSettings.ini")).unwrap()
    }

    pub fn rus(&self, puuid: &str) -> String {
        std::fs::read_to_string(self.folder(puuid).join("Windows").join("RiotUserSettings.ini")).unwrap()
    }

    pub fn snapshot_bytes(&self, puuid: &str) -> Vec<(String, Vec<u8>)> {
        Engine::read_account_files(&self.folder(puuid)).unwrap().into_iter().collect()
    }

    pub fn target(&self, puuid: &str) -> Target {
        Target { puuid: puuid.into(), folder: format!("{puuid}-eu"), dir: self.folder(puuid) }
    }
}

pub fn a() -> &'static str {
    PUUID_A
}
pub fn b() -> &'static str {
    PUUID_B
}
