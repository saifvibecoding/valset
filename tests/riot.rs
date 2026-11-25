//! End-to-end tests against a mock Riot Client + Riot services (no real network).
//! Exercises the exact HTTP paths, headers and encodings VALSYNC uses.

mod common;

use common::{Env, a, b};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Mutex, OnceLock};
use valsync::cloud::{CloudSettings, RiotCloud};
use valsync::engine::LoadOptions;
use valsync::riot::session::{decode_settings, encode_settings};
use valsync::service;

#[derive(Default)]
struct MockState {
    signed_in: String,
    settings: HashMap<String, Value>,
    in_game: bool,
    hits: Vec<String>,
    bad_headers: usize,
}

fn state() -> &'static Mutex<MockState> {
    static S: OnceLock<Mutex<MockState>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(MockState::default()))
}

static SERIAL: Mutex<()> = Mutex::new(());

fn match_json(id: &str, start: i64, tier: i32, won: bool) -> String {
    let me = a();
    let mut players = vec![format!(
        r#"{{"subject":"{me}","gameName":"Owner","tagLine":"EUW","teamId":"Blue","partyId":"p","characterId":"agent-jett","stats":{{"score":5000,"roundsPlayed":24,"kills":20,"deaths":10,"assists":5}},"competitiveTier":{tier},"isObserver":false,"accountLevel":120}}"#
    )];
    for i in 0..9 {
        let team = if i < 4 { "Blue" } else { "Red" };
        players.push(format!(
            r#"{{"subject":"other-{i}","gameName":"P{i}","tagLine":"T{i}","teamId":"{team}","characterId":"agent-sova","stats":{{"score":{},"kills":10,"deaths":12,"assists":3}},"competitiveTier":18,"accountLevel":55}}"#,
            3000 - i * 10
        ));
    }
    format!(
        r#"{{"matchInfo":{{"matchId":"{id}","mapId":"/Game/Maps/Ascent/Ascent","gameLengthMillis":2100000,"gameStartMillis":{start},"queueId":"competitive","gameMode":"/Game/GameModes/Bomb/BombGameMode.BombGameMode_C","isRanked":true}},
        "players":[{}],
        "teams":[{{"teamId":"Blue","won":{won},"roundsWon":{}}},{{"teamId":"Red","won":{},"roundsWon":{}}}],
        "roundResults":[{{"roundNum":0,"playerStats":[{{"subject":"{me}","damage":[{{"receiver":"other-5","damage":150,"legshots":1,"bodyshots":2,"headshots":1}}]}}]}}]}}"#,
        players.join(","),
        if won { 13 } else { 11 },
        !won,
        if won { 11 } else { 13 }
    )
}

fn respond(s: &mut TcpStream, code: u16, body: &str) {
    let _ = s.write_all(format!("HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes());
}

fn read_request(s: &mut TcpStream) -> (String, String, HashMap<String, String>, String) {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    let header_end;
    loop {
        let n = s.read(&mut tmp).unwrap_or(0);
        if n == 0 {
            return Default::default();
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            header_end = i + 4;
            break;
        }
    }
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.lines();
    let first = lines.next().unwrap_or_default().to_string();
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();
    let headers: HashMap<String, String> =
        lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string())).collect();
    let len: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    while buf.len() < header_end + len {
        let n = s.read(&mut tmp).unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let body = String::from_utf8_lossy(&buf[header_end..]).to_string();
    (method, path, headers, body)
}

fn handle(mut s: TcpStream) {
    let (method, path, headers, body) = read_request(&mut s);
    let mut st = state().lock().unwrap();
    st.hits.push(format!("{method} {path}"));
    let me = st.signed_in.clone();
    let token_owner = headers.get("authorization").and_then(|v| v.strip_prefix("Bearer tok-")).map(str::to_string);
    let remote = !path.starts_with("/entitlements") && !path.starts_with("/chat") && !path.starts_with("/product-session") && !path.starts_with("/v1/");
    if remote
        && (headers.get("x-riot-entitlements-jwt").map(String::as_str) != Some("ent-jwt")
            || headers.get("x-riot-clientversion").map(|v| v.starts_with("release-")) != Some(true)
            || !headers.contains_key("x-riot-clientplatform"))
    {
        st.bad_headers += 1;
    }
    let (code, out): (u16, String) = match (method.as_str(), path.as_str()) {
        ("GET", "/entitlements/v1/token") => {
            if headers.get("authorization").map(String::as_str) != Some("Basic cmlvdDpwYXNz") {
                (401, "{}".into())
            } else if me.is_empty() {
                (404, "{}".into())
            } else {
                (200, json!({"accessToken": format!("tok-{me}"), "token": "ent-jwt", "subject": me, "entitlements": [], "issuer": "x"}).to_string())
            }
        }
        ("GET", "/chat/v1/session") => {
            let name = if me == a() { "Owner" } else { "Smurf" };
            (200, json!({"game_name": name, "game_tag": "EUW", "puuid": me, "loaded": true, "region": "eu1"}).to_string())
        }
        ("GET", "/product-session/v1/external-sessions") => (
            200,
            json!({"s1": {"productId": "valorant", "launchConfiguration": {"arguments": ["-ares-deployment=eu", "-config-endpoint=https://shared.eu.a.pvp.net"]}}}).to_string(),
        ),
        ("GET", p) if p.starts_with("/player-preferences-euc1.pp.sgp.pvp.net/playerPref/v3/getPreference/Ares.PlayerSettings") => {
            let who = token_owner.unwrap_or_default();
            match st.settings.get(&who) {
                Some(v) => (200, json!({"type": "Ares.PlayerSettings", "data": encode_settings(v).unwrap(), "modified": 1}).to_string()),
                None => (200, json!({"type": "Ares.PlayerSettings", "data": ""}).to_string()),
            }
        }
        ("PUT", "/player-preferences-euc1.pp.sgp.pvp.net/playerPref/v3/savePreference") => {
            let who = token_owner.unwrap_or_default();
            let v: Value = serde_json::from_str(&body).unwrap_or_default();
            assert_eq!(v["type"], "Ares.PlayerSettings");
            let doc = decode_settings(v["data"].as_str().unwrap_or_default()).expect("valid upload");
            st.settings.insert(who, doc);
            (200, "{}".into())
        }
        ("GET", p) if p.starts_with("/pd.eu.a.pvp.net/match-history/v1/history/") => (
            200,
            json!({"Subject": me, "History": [{"MatchID": "m2", "GameStartTime": 2000, "QueueID": "competitive"}, {"MatchID": "m1", "GameStartTime": 1000, "QueueID": "competitive"}]}).to_string(),
        ),
        ("GET", "/pd.eu.a.pvp.net/match-details/v1/matches/m1") => (200, match_json("m1", 1000, 21, true)),
        ("GET", "/pd.eu.a.pvp.net/match-details/v1/matches/m2") => (200, match_json("m2", 2000, 20, false)),
        ("GET", p) if p.starts_with("/pd.eu.a.pvp.net/mmr/v1/players/") => (
            200,
            json!({"QueueSkills": {"competitive": {"SeasonalInfoBySeasonID": {
                "s-new": {"SeasonID": "s-new", "CompetitiveTier": 20, "WinsByTier": {"19": 3, "20": 2}},
                "a16955a5-4ad0-f761-5e9e-389df1c892fb": {"SeasonID": "old", "CompetitiveTier": 21, "WinsByTier": {"21": 1}}
            }}}, "LatestCompetitiveUpdate": {"MatchID": "m2", "TierAfterUpdate": 20, "RankedRatingAfterUpdate": 57}})
            .to_string(),
        ),
        ("GET", p) if p.starts_with("/pd.eu.a.pvp.net/account-xp/v1/players/") => (200, json!({"Progress": {"Level": 121, "XP": 5}}).to_string()),
        ("PUT", "/pd.eu.a.pvp.net/name-service/v2/players") => {
            let ids: Vec<String> = serde_json::from_str(&body).unwrap_or_default();
            assert!(!ids.contains(&"enemy-hidden".to_string()), "VALSYNC must not look up names of hidden players");
            let v: Vec<Value> = ids.iter().map(|i| json!({"Subject": i, "GameName": format!("N-{i}"), "TagLine": "T"})).collect();
            (200, Value::Array(v).to_string())
        }
        ("GET", p) if p.starts_with("/glz-eu-1.eu.a.pvp.net/core-game/v1/players/") => {
            if st.in_game { (200, json!({"Subject": me, "MatchID": "live1"}).to_string()) } else { (404, "{}".into()) }
        }
        ("GET", "/glz-eu-1.eu.a.pvp.net/core-game/v1/matches/live1") => (
            200,
            json!({"MatchID": "live1", "State": "IN_PROGRESS", "MapID": "/Game/Maps/Ascent/Ascent", "ModeID": "/Game/GameModes/Bomb/BombGameMode.BombGameMode_C",
                "MatchmakingData": {"QueueID": "competitive"},
                "Players": [
                    {"Subject": me, "TeamID": "Blue", "CharacterID": "AGENT-JETT", "PlayerIdentity": {"Subject": me, "AccountLevel": 121, "Incognito": false, "HideAccountLevel": false}, "SeasonalBadgeInfo": {"Rank": 20}},
                    {"Subject": "ally-1", "TeamID": "Blue", "CharacterID": "agent-sova", "PlayerIdentity": {"Subject": "ally-1", "AccountLevel": 40, "Incognito": false, "HideAccountLevel": true}, "SeasonalBadgeInfo": {"Rank": 24}},
                    {"Subject": "enemy-hidden", "TeamID": "Red", "CharacterID": "agent-sova", "PlayerIdentity": {"Subject": "enemy-hidden", "AccountLevel": 300, "Incognito": true, "HideAccountLevel": false}, "SeasonalBadgeInfo": {"Rank": 27}}
                ]})
            .to_string(),
        ),
        ("GET", "/glz-eu-1.eu.a.pvp.net/core-game/v1/matches/live1/loadouts") => (
            200,
            json!({"Loadouts": [{"CharacterID": "agent-jett", "Loadout": {"Subject": me, "Items": {
                "9c82e19d-4575-0200-1a81-3eacf00cf872": {"ID": "9c82e19d-4575-0200-1a81-3eacf00cf872", "Sockets": {
                    "bcef87d6-209b-46c6-8b19-fbe40bd95abc": {"ID": "x", "Item": {"ID": "skin-prime-vandal"}},
                    "3ad1b2b2-acdb-4524-852f-954a76ddae0a": {"ID": "y", "Item": {"ID": "chroma-prime-red"}}}}}}}]})
            .to_string(),
        ),
        ("GET", p) if p.starts_with("/glz-eu-1.eu.a.pvp.net/pregame/v1/players/") => (404, "{}".into()),
        ("GET", p) if p.starts_with("/v1/agents") => {
            (200, json!({"data": [{"uuid": "agent-jett", "displayName": "Jett"}, {"uuid": "agent-sova", "displayName": "Sova"}]}).to_string())
        }
        ("GET", "/v1/maps") => (200, json!({"data": [{"uuid": "m", "displayName": "Ascent", "mapUrl": "/Game/Maps/Ascent/Ascent"}]}).to_string()),
        ("GET", "/v1/weapons/skins") => (
            200,
            json!({"data": [{"uuid": "skin-prime-vandal", "displayName": "Prime Vandal", "chromas": [{"uuid": "chroma-prime-red", "displayName": "Prime Vandal Level 4 (Variant 1 Red)"}]}]}).to_string(),
        ),
        ("GET", "/v1/version") => (200, json!({"data": {"riotClientVersion": "release-99.01-shipping-1-1"}}).to_string()),
        _ => (404, "{}".into()),
    };
    drop(st);
    respond(&mut s, code, &out);
}

/// Start the mock once; point VALSYNC at it via env vars.
fn mock(env: &Env) -> String {
    static BASE: OnceLock<String> = OnceLock::new();
    let base = BASE
        .get_or_init(|| {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            let base = format!("http://{}", l.local_addr().unwrap());
            std::thread::spawn(move || {
                for s in l.incoming().flatten() {
                    std::thread::spawn(move || handle(s));
                }
            });
            // SAFETY: tests in this binary run serially (SERIAL) and set these once.
            unsafe {
                std::env::set_var("VALSYNC_REMOTE_BASE", &base);
                std::env::set_var("VALSYNC_CONTENT_BASE", &base);
            }
            base
        })
        .clone();
    let port = base.rsplit(':').next().unwrap();
    let lock = env.dir.path().join("lockfile");
    std::fs::write(&lock, format!("Riot Client:4242:{port}:pass:http")).unwrap();
    unsafe { std::env::set_var("VALSYNC_LOCKFILE", &lock) };
    let mut st = state().lock().unwrap();
    *st = MockState::default();
    st.signed_in = a().into();
    st.settings.insert(a().into(), valsync::cloud::sample_doc(0.176, "0;P;c;5;o;1"));
    let mut doc_b = valsync::cloud::sample_doc(0.25, "0;P;h;0");
    doc_b["stringSettings"][1]["value"] = json!("device-of-B");
    st.settings.insert(b().into(), doc_b);
    base
}

fn sign_in(puuid: &str) {
    state().lock().unwrap().signed_in = puuid.into();
}

fn cloud_of(puuid: &str) -> Value {
    state().lock().unwrap().settings.get(puuid).cloned().unwrap()
}

fn sens(doc: &Value) -> f64 {
    doc["floatSettings"].as_array().unwrap().iter().find(|e| e["settingEnum"] == "EAresFloatSettingName::MouseSensitivity").unwrap()["value"].as_f64().unwrap()
}

#[test]
fn full_transfer_through_the_real_riot_client_code_path() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let env = Env::new();
    mock(&env);
    let mut e = env.engine();
    e.connect_riot();
    assert!(e.cloud_error.is_none(), "{:?}", e.cloud_error);
    assert_eq!(e.cloud_account().as_deref(), Some(a()));
    // Riot ID and region learned automatically.
    let acc = e.db.account(a()).unwrap().unwrap();
    assert_eq!(acc.riot_id().as_deref(), Some("Owner#EUW"));
    assert_eq!(acc.shard.as_deref(), Some("eu"));
    let (ta, m) = e.active_target().unwrap();
    assert_eq!(m, valsync::accounts::DetectMethod::RiotClient);

    // Save on A.
    let out = e.save_profile(&ta, "Main", false, None, None, vec![]).unwrap();
    assert!(out.cloud);
    let json = out.profile.to_file_json().unwrap();
    assert!(!json.contains("device-of-this-pc"), "machine-specific audio device must not be saved");
    assert!(!json.contains("HasSeenTutorial"));

    // Switch to B in the Riot Client; VALSYNC follows.
    sign_in(b());
    assert!(e.refresh_riot());
    let (tb, _) = e.active_target().unwrap();
    assert_eq!(tb.puuid, b());
    let before_b = cloud_of(b());

    let p = e.db.profile("Main").unwrap().unwrap().profile;
    let res = e.apply_load(&p, &tb, &LoadOptions::default()).unwrap();
    assert!(res.txn.as_ref().unwrap().cloud_written);
    let after = cloud_of(b());
    assert_eq!(sens(&after), 0.176);
    assert!(after.to_string().contains("device-of-B"), "B keeps its own audio device");
    assert!(after.to_string().contains("0;P;c;5;o;1"), "crosshair transferred");
    assert!(env.gus(b()).contains("ResolutionSizeX=1920"), "video settings transferred");

    // Restore Previous Settings → B's 0.25 is back, exactly.
    e.undo(b(), false).unwrap();
    assert_eq!(cloud_of(b()), before_b);
    assert!(env.gus(b()).contains("ResolutionSizeX=1280"));
    // Redo.
    e.redo(b(), false).unwrap();
    assert_eq!(sens(&cloud_of(b())), 0.176);
    // A was never modified.
    assert_eq!(sens(&cloud_of(a())), 0.176);
    assert_eq!(state().lock().unwrap().bad_headers, 0, "every Riot request must carry token, entitlement, version and platform headers");
}

#[test]
fn backends_respect_the_signed_in_account() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let env = Env::new();
    mock(&env);
    let mut e = env.engine();
    e.connect_riot();
    let (ta, _) = e.active_target().unwrap();
    let id = e.create_manual_backup(&ta, "A").unwrap();
    sign_in(b());
    e.refresh_riot();
    let before = cloud_of(b());
    // A's backup must never be written while B is signed in.
    assert!(e.restore_backup(id, false).is_err());
    assert_eq!(cloud_of(b()), before);
    // Signed out: cloud part of loads is skipped with a clear warning.
    sign_in("");
    e.refresh_riot();
    assert!(e.cloud_account().is_none());
    assert!(e.cloud_error.as_deref().unwrap().contains("nobody is signed in"));
}

#[test]
fn riot_cloud_backend_roundtrip() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let env = Env::new();
    mock(&env);
    let c = RiotCloud::connect(&env.paths.valorant_logs, None).unwrap();
    assert_eq!(c.account().as_deref(), Some(a()));
    let mut doc = c.fetch().unwrap();
    doc["floatSettings"][0]["value"] = json!(0.5);
    c.store(&doc).unwrap();
    assert_eq!(sens(&c.fetch().unwrap()), 0.5);
}

#[test]
fn history_rank_and_live_match() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let env = Env::new();
    mock(&env);
    let e = env.engine();
    let rt = service::runtime();
    let session = rt.block_on(service::connect(&env.paths.valorant_logs, &e.db, &e.config)).unwrap();
    assert_eq!((session.region.as_str(), session.shard.as_str()), ("eu", "eu"));

    let h = rt.block_on(service::history(&session, &e.db, &e.config, 10)).unwrap();
    assert!(h.warning.is_none(), "{:?}", h.warning);
    assert_eq!(h.new_matches, 2);
    assert_eq!(h.views[0].match_id, "m2");
    assert_eq!(h.views[0].map, "Ascent");
    let me = h.views[0].owner_line.as_ref().unwrap();
    assert_eq!(me.agent, "Jett");
    assert_eq!(me.hs_pct, Some(25.0));
    assert!(h.views[0].players.iter().filter(|p| !p.is_owner).all(|p| p.tier.is_none()), "other players' ranks are not shown");
    assert_eq!(h.stats.current_tier, Some(20));
    assert_eq!(h.stats.rr, Some(57));
    assert_eq!(h.stats.peak_tier, Some(24), "pre-Ascendant Immortal 1 (21) maps to tier 24");
    assert_eq!(h.stats.level, Some(121));
    assert_eq!(h.stats.win_rate(), Some(50.0));
    // Matches are cached: a second sync downloads nothing new.
    let h2 = rt.block_on(service::history(&session, &e.db, &e.config, 10)).unwrap();
    assert_eq!(h2.new_matches, 0);

    // Not in a match.
    let d = rt.block_on(service::live(&session, &e.db, &e.config, None)).unwrap();
    assert!(d.view.is_none());
    assert!(d.last_match.is_some());

    // In a match.
    state().lock().unwrap().in_game = true;
    let d = rt.block_on(service::live(&session, &e.db, &e.config, None)).unwrap();
    let v = d.view.expect("live view");
    assert_eq!(v.map, "Ascent");
    assert_eq!(v.mode, "Competitive");
    let me = v.players.iter().find(|p| p.is_owner).unwrap();
    assert_eq!(me.agent, "Jett");
    assert_eq!(me.vandal.as_deref(), Some("Prime Vandal"));
    assert_eq!(me.variant.as_deref(), Some("Prime Vandal Level 4 (Variant 1 Red)"));
    let ally = v.players.iter().find(|p| p.puuid == "ally-1").unwrap();
    assert_eq!(ally.name, "N-ally-1#T");
    assert_eq!(ally.level, None, "hidden account level stays hidden");
    let hidden = v.players.iter().find(|p| p.puuid == "enemy-hidden").unwrap();
    assert!(hidden.name.starts_with("Hidden"), "incognito name stays hidden: {}", hidden.name);
    assert!(v.players.iter().all(|p| p.tier.is_none()), "no rank scouting");
    // Re-polling the same match reuses loadouts.
    let loads_before = state().lock().unwrap().hits.iter().filter(|h| h.ends_with("/loadouts")).count();
    let _ = rt.block_on(service::live(&session, &e.db, &e.config, Some(&v))).unwrap();
    let loads_after = state().lock().unwrap().hits.iter().filter(|h| h.ends_with("/loadouts")).count();
    assert_eq!(loads_before, loads_after);
}

#[test]
fn riot_client_not_running() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let env = Env::new();
    mock(&env);
    unsafe { std::env::set_var("VALSYNC_LOCKFILE", env.dir.path().join("missing")) };
    let mut e = env.engine();
    e.connect_riot();
    assert!(e.cloud_account().is_none());
    assert!(e.cloud_error.as_deref().unwrap().contains("not running"));
    // Falls back to VALORANT's own files for the account.
    let (t, m) = e.active_target().unwrap();
    assert_eq!(t.puuid, a());
    assert_eq!(m, valsync::accounts::DetectMethod::LastKnownUser);
}
