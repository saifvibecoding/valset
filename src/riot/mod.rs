//! Riot Games API integration (official, documented endpoints only).
//!
//! | API            | Used for                              | Host            |
//! |----------------|---------------------------------------|-----------------|
//! | ACCOUNT-V1     | Riot ID lookup / verification, shard  | americas/europe/asia |
//! | VAL-MATCH-V1   | Post-match dashboards, match history  | shard (na, eu…) |
//! | VAL-CONTENT-V1 | Agent / map / mode names, patch       | shard           |
//! | VAL-STATUS-V1  | Server status                         | shard           |
//!
//! Riot only grants VALORANT API access to **production** keys, and match data
//! may only be shown for players who opted in through **Riot Sign On (RSO)**.
//! There is no supported live-match endpoint for VALORANT; VAL-MATCH-V1 returns
//! completed matches only.

pub mod api;
pub mod models;
pub mod rso;

pub use api::{ApiError, RiotApi};

pub const SHARDS: [&str; 6] = ["na", "latam", "br", "eu", "ap", "kr"];

/// Regional routing value for ACCOUNT-V1 / RSO calls for a VALORANT shard.
pub fn region_for_shard(shard: &str) -> &'static str {
    match shard.to_ascii_lowercase().as_str() {
        "eu" => "europe",
        "ap" | "kr" => "asia",
        _ => "americas",
    }
}

/// Normalise shard names seen in folder suffixes / API responses.
pub fn normalize_shard(s: &str) -> Option<String> {
    let s = s.trim().to_ascii_lowercase();
    let mapped = match s.as_str() {
        "na" | "pbe" => "na",
        "latam" | "la" => "latam",
        "br" => "br",
        "eu" => "eu",
        "ap" | "asia" => "ap",
        "kr" => "kr",
        _ => return None,
    };
    Some(mapped.to_string())
}

pub fn queue_name(queue: &str, mode: &str) -> String {
    let q = queue.to_ascii_lowercase();
    let n = match q.as_str() {
        "competitive" => "Competitive",
        "unrated" => "Unrated",
        "spikerush" => "Spike Rush",
        "deathmatch" => "Deathmatch",
        "swiftplay" => "Swiftplay",
        "premier" => "Premier",
        "ggteam" => "Escalation",
        "onefa" => "Replication",
        "hurm" => "Team Deathmatch",
        "snowball" => "Snowball Fight",
        "newmap" => "New Map",
        "" => {
            if mode.to_ascii_lowercase().contains("bomb") || mode.is_empty() {
                "Custom"
            } else {
                return mode_name(mode);
            }
        }
        _ => return queue.to_string(),
    };
    n.to_string()
}

fn mode_name(mode: &str) -> String {
    // "/Game/GameModes/Bomb/BombGameMode.BombGameMode_C" → "Bomb"
    mode.split('/').filter(|s| !s.is_empty()).nth(2).unwrap_or(mode).to_string()
}
