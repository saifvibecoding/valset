//! Riot API DTOs (only the fields VALSYNC uses; unknown fields are ignored and
//! missing ones default, so API additions never break parsing).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AccountDto {
    pub puuid: String,
    pub game_name: Option<String>,
    pub tag_line: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ActiveShardDto {
    pub puuid: String,
    pub game: String,
    pub active_shard: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Matchlist {
    pub puuid: String,
    pub history: Vec<MatchlistEntry>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MatchlistEntry {
    pub match_id: String,
    pub game_start_time_millis: i64,
    pub queue_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Match {
    pub match_info: MatchInfo,
    pub players: Vec<Player>,
    pub teams: Option<Vec<Team>>,
    pub round_results: Option<Vec<RoundResult>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MatchInfo {
    pub match_id: String,
    pub map_id: String,
    pub game_version: String,
    pub game_length_millis: Option<i64>,
    pub game_start_millis: i64,
    pub provisioning_flow_id: String,
    pub is_completed: bool,
    pub custom_game_name: String,
    pub queue_id: String,
    pub game_mode: String,
    pub is_ranked: bool,
    pub season_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Player {
    pub puuid: String,
    pub game_name: String,
    pub tag_line: String,
    pub team_id: String,
    pub party_id: String,
    pub character_id: Option<String>,
    pub stats: Option<PlayerStats>,
    pub competitive_tier: i32,
    pub is_observer: bool,
    pub account_level: i32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PlayerStats {
    pub score: i32,
    pub rounds_played: i32,
    pub kills: i32,
    pub deaths: i32,
    pub assists: i32,
    pub playtime_millis: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Team {
    pub team_id: String,
    pub won: bool,
    pub rounds_played: i32,
    pub rounds_won: i32,
    pub num_points: i32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RoundResult {
    pub round_num: i32,
    pub winning_team: String,
    pub player_stats: Vec<PlayerRoundStats>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PlayerRoundStats {
    pub puuid: String,
    pub damage: Vec<Damage>,
    pub score: i32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Damage {
    pub receiver: String,
    pub damage: i32,
    pub legshots: i32,
    pub bodyshots: i32,
    pub headshots: i32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ContentItem {
    pub name: String,
    pub id: String,
    pub asset_name: String,
    pub asset_path: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Act {
    pub name: String,
    pub id: String,
    pub is_active: bool,
    #[serde(rename = "type")]
    pub kind: Option<String>,
}

/// VAL-CONTENT-V1, reduced to what VALSYNC needs (the full payload is large).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Content {
    pub version: String,
    pub characters: Vec<ContentItem>,
    pub maps: Vec<ContentItem>,
    pub game_modes: Vec<ContentItem>,
    pub acts: Vec<Act>,
}

impl Content {
    pub fn agent_name(&self, id: &str) -> Option<String> {
        self.characters.iter().find(|c| c.id.eq_ignore_ascii_case(id)).map(|c| c.name.clone())
    }
    pub fn map_name(&self, map_id: &str) -> Option<String> {
        self.maps
            .iter()
            .find(|m| m.asset_path.as_deref().map(|p| p.eq_ignore_ascii_case(map_id)).unwrap_or(false) || m.id.eq_ignore_ascii_case(map_id))
            .map(|m| m.name.clone())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PlatformData {
    pub id: String,
    pub name: String,
    pub maintenances: Vec<StatusEntry>,
    pub incidents: Vec<StatusEntry>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StatusEntry {
    pub id: serde_json::Value,
    pub maintenance_status: Option<String>,
    pub incident_severity: Option<String>,
    pub titles: Vec<LocalizedText>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalizedText {
    pub locale: String,
    pub content: String,
}

impl StatusEntry {
    pub fn title(&self, locale: &str) -> String {
        let want = locale.replace('-', "_");
        self.titles
            .iter()
            .find(|t| t.locale.eq_ignore_ascii_case(&want) || t.locale.eq_ignore_ascii_case(locale))
            .or_else(|| self.titles.first())
            .map(|t| t.content.clone())
            .unwrap_or_else(|| "(untitled)".into())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TokenResponse {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: i64,
}
