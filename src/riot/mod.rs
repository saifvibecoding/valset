//! Riot integration through the Riot Client you are already signed into.
//!
//! This is the same approach the established community tools use (settings
//! copiers, trackers): the Riot Client writes a `lockfile` with a local port and
//! password; its local API hands out the signed-in session's tokens, which are
//! then used against Riot's game services:
//!
//! | Service                         | Used for                                  |
//! |---------------------------------|-------------------------------------------|
//! | player-preferences (`Ares.PlayerSettings`) | sensitivity, crosshair, keybinds, HUD … |
//! | `pd.{shard}.a.pvp.net`          | match history, match details, rank/RR, names, level |
//! | `glz-{region}-1.{shard}.a.pvp.net` | live match (agent select / in game), loadouts |
//! | `valorant-api.com`              | agent, map and skin names                 |
//!
//! Tokens stay in memory, are only ever sent to Riot's own hosts, and are never
//! written to disk. No password is ever requested.

pub mod content;
pub mod local;
pub mod models;
pub mod session;

pub use session::Session;

pub const REGIONS: [&str; 6] = ["na", "latam", "br", "eu", "ap", "kr"];

/// Shard (pd / glz host) for a region.
pub fn shard_for_region(region: &str) -> &'static str {
    match region.to_ascii_lowercase().as_str() {
        "eu" | "tr" => "eu",
        "ap" => "ap",
        "kr" => "kr",
        "pbe" => "pbe",
        _ => "na",
    }
}

/// Normalise region names seen in folder suffixes, logs and launch arguments.
pub fn normalize_region(s: &str) -> Option<String> {
    let s = s.trim().to_ascii_lowercase();
    let r = match s.as_str() {
        "na" | "na1" | "us" | "pbe" => "na",
        "latam" | "la1" | "la2" | "la" => "latam",
        "br" | "br1" => "br",
        "eu" | "eu1" | "euw" | "eune" | "tr" | "tr1" | "ru" => "eu",
        "ap" | "ap1" | "asia" | "sea" | "oce" | "jp" | "jp1" => "ap",
        "kr" | "kr1" => "kr",
        _ => return None,
    };
    Some(r.to_string())
}

/// Candidate player-preferences hosts, best first. Public tools disagree on
/// the "right" host per region, so VALSYNC tries them in order and remembers
/// the one that answered.
pub fn preference_hosts(region: &str) -> Vec<&'static str> {
    let regional = match region {
        "eu" => "player-preferences-euc1.pp.sgp.pvp.net",
        "ap" | "kr" => "player-preferences-apse1.pp.sgp.pvp.net",
        _ => "player-preferences-usw2.pp.sgp.pvp.net",
    };
    let mut v = vec![regional];
    for h in ["player-preferences-usw2.pp.sgp.pvp.net", "playerpreferences.riotgames.com"] {
        if !v.contains(&h) {
            v.push(h);
        }
    }
    v
}

/// Build an https URL, or route through `VALSYNC_REMOTE_BASE` (tests).
pub fn remote_url(host: &str, path: &str) -> String {
    match std::env::var("VALSYNC_REMOTE_BASE") {
        Ok(base) if !base.is_empty() => format!("{}/{host}{path}", base.trim_end_matches('/')),
        _ => format!("https://{host}{path}"),
    }
}

/// Run a future to completion on a private current-thread runtime.
pub fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime").block_on(f)
}

pub fn queue_name(queue: &str, mode: &str) -> String {
    let q = queue.to_ascii_lowercase();
    let n = match q.as_str() {
        "competitive" => "Competitive",
        "unrated" => "Unrated",
        "spikerush" => "Spike Rush",
        "deathmatch" => "Deathmatch",
        "swiftplay" => "Swiftplay",
        "premier" | "premier-seasonmatch" => "Premier",
        "ggteam" => "Escalation",
        "onefa" => "Replication",
        "hurm" => "Team Deathmatch",
        "snowball" => "Snowball Fight",
        "newmap" => "New Map",
        "" => {
            let m = mode.to_ascii_lowercase();
            if m.contains("deathmatch") {
                "Deathmatch"
            } else if m.contains("quickbomb") {
                "Spike Rush"
            } else if m.contains("onboarding") || m.contains("practice") {
                "Practice"
            } else {
                "Custom"
            }
        }
        _ => return queue.to_string(),
    };
    n.to_string()
}
