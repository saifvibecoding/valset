//! Riot API integration against a local mock server (no real network).

mod common;

use common::{Env, a};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use valsync::riot::RiotApi;
use valsync::service;

struct Mock {
    base: String,
    hits: Arc<Mutex<HashMap<String, usize>>>,
}

fn match_json(id: &str, start: i64, owner_tier: i32, owner_won: bool) -> String {
    let me = a();
    let mut players = vec![format!(
        r#"{{"puuid":"{me}","gameName":"Owner","tagLine":"EUW","teamId":"Blue","partyId":"p","characterId":"agent-jett","stats":{{"score":5000,"roundsPlayed":24,"kills":20,"deaths":10,"assists":5,"playtimeMillis":1}},"competitiveTier":{owner_tier},"isObserver":false,"playerCard":"","playerTitle":"","accountLevel":120}}"#
    )];
    for i in 0..9 {
        let team = if i < 4 { "Blue" } else { "Red" };
        players.push(format!(
            r#"{{"puuid":"other-{i}","gameName":"P{i}","tagLine":"T{i}","teamId":"{team}","partyId":"q","characterId":"agent-sova","stats":{{"score":{},"roundsPlayed":24,"kills":10,"deaths":12,"assists":3,"playtimeMillis":1}},"competitiveTier":18,"isObserver":false,"accountLevel":55}}"#,
            3000 - i * 10
        ));
    }
    format!(
        r#"{{"matchInfo":{{"matchId":"{id}","mapId":"/Game/Maps/Ascent/Ascent","gameVersion":"release-11.08","gameLengthMillis":2100000,"region":"eu","gameStartMillis":{start},"provisioningFlowId":"Matchmaking","isCompleted":true,"customGameName":"","queueId":"competitive","gameMode":"/Game/GameModes/Bomb/BombGameMode.BombGameMode_C","isRanked":true,"seasonId":"s"}},
        "players":[{}],"coaches":[],
        "teams":[{{"teamId":"Blue","won":{owner_won},"roundsPlayed":24,"roundsWon":{},"numPoints":0}},{{"teamId":"Red","won":{},"roundsPlayed":24,"roundsWon":{},"numPoints":0}}],
        "roundResults":[{{"roundNum":0,"winningTeam":"Blue","playerStats":[{{"puuid":"{me}","kills":[],"damage":[{{"receiver":"other-5","damage":150,"legshots":1,"bodyshots":2,"headshots":1}}],"score":200}}]}}]}}"#,
        players.join(","),
        if owner_won { 13 } else { 11 },
        !owner_won,
        if owner_won { 11 } else { 13 }
    )
}

fn mock() -> &'static Mock {
    static M: OnceLock<Mock> = OnceLock::new();
    M.get_or_init(|| {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let hits: Arc<Mutex<HashMap<String, usize>>> = Arc::default();
        let h2 = hits.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                let mut buf = vec![0u8; 16384];
                let n = s.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                let authed = req.to_ascii_lowercase().contains("x-riot-token: good-key");
                *h2.lock().unwrap().entry(path.clone()).or_default() += 1;
                let (code, body) = if !authed {
                    (403, r#"{"status":{"message":"Forbidden","status_code":403}}"#.to_string())
                } else if path.starts_with("/eu/val/match/v1/matchlists/by-puuid/") {
                    (200, format!(r#"{{"puuid":"{}","history":[{{"matchId":"m1","gameStartTimeMillis":1000,"queueId":"competitive"}},{{"matchId":"m2","gameStartTimeMillis":2000,"queueId":"competitive"}}]}}"#, a()))
                } else if path == "/eu/val/match/v1/matches/m1" {
                    (200, match_json("m1", 1000, 21, true))
                } else if path == "/eu/val/match/v1/matches/m2" {
                    (200, match_json("m2", 2000, 20, false))
                } else if path.starts_with("/eu/val/content/v1/contents") {
                    (200, r#"{"version":"release-11.08-shipping-5","characters":[{"name":"Jett","id":"AGENT-JETT","assetName":"x"},{"name":"Sova","id":"agent-sova","assetName":"y"}],"maps":[{"name":"Ascent","id":"m","assetName":"Ascent","assetPath":"/Game/Maps/Ascent/Ascent"}],"skins":[{"name":"huge","id":"s"}]}"#.to_string())
                } else if path.starts_with("/europe/riot/account/v1/accounts/by-puuid/")
                    || path.starts_with("/europe/riot/account/v1/accounts/by-riot-id/Owner/EUW")
                {
                    (200, format!(r#"{{"puuid":"{}","gameName":"Owner","tagLine":"EUW"}}"#, a()))
                } else if path.starts_with("/europe/riot/account/v1/active-shards/") {
                    (200, format!(r#"{{"puuid":"{}","game":"val","activeShard":"eu"}}"#, a()))
                } else if path.starts_with("/eu/val/status/v1/platform-data") {
                    (200, r#"{"id":"EU","name":"Europe","locales":["en_US"],"maintenances":[],"incidents":[{"id":1,"incident_severity":"warning","titles":[{"locale":"en_US","content":"Login issues"}]}]}"#.to_string())
                } else {
                    (404, "{}".to_string())
                };
                let resp = format!("HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                let _ = s.write_all(resp.as_bytes());
            }
        });
        // SAFETY: set once before any RiotApi is constructed in this test binary.
        unsafe { std::env::set_var("VALSYNC_RIOT_BASE", &base) };
        Mock { base, hits }
    })
}

fn hits(prefix: &str) -> usize {
    mock().hits.lock().unwrap().iter().filter(|(k, _)| k.starts_with(prefix)).map(|(_, v)| *v).sum()
}

static SERIAL: Mutex<()> = Mutex::new(());
static COUNTER: AtomicUsize = AtomicUsize::new(0);

#[test]
fn history_requires_rso_consent() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    mock();
    let env = Env::new();
    let mut e = env.engine();
    valsync::accounts::sync_with_db(&e.db, &e.detection()).unwrap();
    let api = RiotApi::new(Some("good-key".into()));
    let cfg = e.config.clone();
    let err = service::runtime().block_on(service::history(&api, &mut e.db, &cfg, a(), 5, true)).err().expect("must refuse");
    assert!(err.to_string().contains("Riot Sign On"), "{err}");
}

#[test]
fn match_sync_dashboard_and_caching() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    mock();
    COUNTER.fetch_add(1, Ordering::SeqCst);
    let env = Env::new();
    let mut e = env.engine();
    valsync::accounts::sync_with_db(&e.db, &e.detection()).unwrap();
    e.db.set_account_identity(a(), "Owner", "EUW", "rso", true, Some(a())).unwrap();
    e.db.set_account_consent(a(), true).unwrap();
    let api = RiotApi::new(Some("good-key".into()));
    let cfg = e.config.clone();
    let rt = service::runtime();
    let before_lists = hits("/eu/val/match/v1/matchlists");
    let h = rt.block_on(service::history(&api, &mut e.db, &cfg, a(), 10, true)).unwrap();
    assert!(h.warning.is_none(), "{:?}", h.warning);
    assert_eq!(h.new_matches, 2);
    assert_eq!(h.views.len(), 2);
    // Newest first.
    let v = &h.views[0];
    assert_eq!(v.match_id, "m2");
    assert_eq!(v.map, "Ascent");
    assert_eq!(v.mode, "Competitive");
    assert_eq!(v.owner_won, Some(false));
    assert_eq!(v.players.len(), 10);
    let me = v.owner_line.as_ref().unwrap();
    assert_eq!(me.agent, "Jett", "agent ids are matched case-insensitively");
    assert_eq!(me.tier, Some(20));
    assert_eq!(me.hs_pct, Some(25.0));
    // Non-consenting players: no rank/level, but scoreboard data is present.
    let other = v.players.iter().find(|p| !p.is_owner).unwrap();
    assert_eq!(other.tier, None);
    assert_eq!(other.level, None);
    assert!(other.name.starts_with('P'));
    // Aggregate stats: peak over recorded matches, current = latest.
    assert_eq!(h.stats.games, 2);
    assert_eq!(h.stats.current_tier, Some(20));
    assert_eq!(h.stats.peak_tier, Some(21));
    assert_eq!(h.stats.win_rate(), Some(50.0));
    assert_eq!(h.stats.level, Some(120));

    // Second call within the cache TTL: no new matchlist request, no re-download.
    let h2 = rt.block_on(service::history(&api, &mut e.db, &cfg, a(), 10, true)).unwrap();
    assert_eq!(h2.new_matches, 0);
    assert_eq!(hits("/eu/val/match/v1/matchlists") - before_lists, 1);
    assert_eq!(hits("/eu/val/match/v1/matches/m1"), COUNTER.load(Ordering::SeqCst));

    // Streamer-friendly mode hides other names.
    let mut cfg2 = cfg.clone();
    cfg2.hide_other_names = true;
    let h3 = rt.block_on(service::history(&api, &mut e.db, &cfg2, a(), 10, false)).unwrap();
    assert!(h3.views[0].players.iter().filter(|p| !p.is_owner).all(|p| p.name == "Player"));
}

#[test]
fn riot_id_verification_and_identity_refresh() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    mock();
    let env = Env::new();
    let mut e = env.engine();
    valsync::accounts::sync_with_db(&e.db, &e.detection()).unwrap();
    let api = RiotApi::new(Some("good-key".into()));
    let cfg = e.config.clone();
    let rt = service::runtime();
    let r = rt.block_on(service::set_riot_id(&api, &mut e.db, &cfg, a(), "Owner#EUW")).unwrap();
    assert!(matches!(r, service::IdentityResult::Verified));
    let acc = e.db.account(a()).unwrap().unwrap();
    assert_eq!(acc.riot_id().as_deref(), Some("Owner#EUW"));
    assert!(acc.verified);
    let err = rt.block_on(service::set_riot_id(&api, &mut e.db, &cfg, a(), "Nobody#000")).unwrap_err();
    assert!(err.to_string().contains("does not exist"));
    let id = rt.block_on(service::refresh_identity(&api, &mut e.db, &cfg, a())).unwrap();
    assert_eq!(id.as_deref(), Some("Owner#EUW"));
    let status = rt.block_on(api.platform_status(&mut e.db, "eu")).unwrap();
    assert_eq!(status.incidents[0].title("en-US"), "Login issues");
}

#[test]
fn bad_key_reports_production_key_requirement() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    mock();
    let env = Env::new();
    let mut e = env.engine();
    let api = RiotApi::new(Some("dev-key".into()));
    let err = service::runtime().block_on(api.platform_status(&mut e.db, "eu")).unwrap_err();
    assert!(err.to_string().contains("production key"), "{err}");
    assert!(!RiotApi::new(None).has_key());
    let _ = &mock().base;
}
