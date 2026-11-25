//! Data shapes returned by the Riot Client and Riot's game services. Every
//! field is optional/defaulted so a format change never breaks parsing.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ----------------------------------------------------------------- local client

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Entitlements {
    pub access_token: String,
    /// Entitlements JWT.
    pub token: String,
    /// PUUID of the signed-in account.
    pub subject: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ChatSession {
    pub game_name: String,
    pub game_tag: String,
    pub puuid: String,
    pub region: String,
    pub loaded: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ExternalSession {
    pub product_id: String,
    pub launch_configuration: Option<LaunchConfiguration>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LaunchConfiguration {
    pub arguments: Vec<String>,
}

// ----------------------------------------------------------------- pd: match details (same shape as VAL-MATCH-V1)

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
    #[serde(alias = "puuid")]
    pub subject: String,
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
    #[serde(alias = "puuid")]
    pub subject: String,
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

// ----------------------------------------------------------------- pd: other

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct MatchHistory {
    pub subject: String,
    pub history: Vec<MatchHistoryEntry>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct MatchHistoryEntry {
    #[serde(rename = "MatchID")]
    pub match_id: String,
    #[serde(rename = "GameStartTime")]
    pub game_start_time: i64,
    #[serde(rename = "QueueID")]
    pub queue_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct Mmr {
    pub queue_skills: HashMap<String, QueueSkill>,
    pub latest_competitive_update: Option<CompetitiveUpdate>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct QueueSkill {
    #[serde(rename = "SeasonalInfoBySeasonID")]
    pub seasonal_info_by_season_id: Option<HashMap<String, SeasonalInfo>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct SeasonalInfo {
    #[serde(rename = "SeasonID")]
    pub season_id: String,
    pub number_of_wins: i32,
    pub number_of_games: i32,
    pub rank: i32,
    pub competitive_tier: i32,
    pub ranked_rating: i32,
    pub wins_by_tier: Option<HashMap<String, i32>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct CompetitiveUpdate {
    #[serde(rename = "MatchID")]
    pub match_id: String,
    #[serde(rename = "SeasonID")]
    pub season_id: String,
    pub tier_after_update: i32,
    pub ranked_rating_after_update: i32,
    pub ranked_rating_earned: i32,
    pub match_start_time: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct CompetitiveUpdates {
    pub matches: Vec<CompetitiveUpdate>,
}

/// Tier numbers changed when Ascendant was added; normalise old acts.
pub fn normalize_tier(season_id: &str, tier: i32) -> i32 {
    if PRE_ASCENDANT_SEASONS.contains(&season_id) && tier > 20 { tier + 3 } else { tier }
}

/// Highest rank ever reached, with the best RR seen at that rank.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Peak {
    pub tier: i32,
    pub rr: Option<i32>,
}

impl Peak {
    pub fn merge_tier(&mut self, tier: i32) {
        if tier > self.tier {
            self.tier = tier;
            self.rr = None;
        }
    }
    pub fn merge(&mut self, tier: i32, rr: i32) {
        if tier > self.tier {
            *self = Peak { tier, rr: Some(rr) };
        } else if tier == self.tier && tier > 0 {
            self.rr = Some(self.rr.map_or(rr, |x| x.max(rr)));
        }
    }
    pub fn merge_peak(&mut self, other: Peak) {
        match other.rr {
            Some(rr) => self.merge(other.tier, rr),
            None => self.merge_tier(other.tier),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct NameEntry {
    pub subject: String,
    pub game_name: String,
    pub tag_line: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct AccountXp {
    pub progress: XpProgress,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct XpProgress {
    pub level: i32,
}

// ----------------------------------------------------------------- glz: live match

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CurrentMatchRef {
    #[serde(rename = "MatchID")]
    pub match_id: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PlayerIdentity {
    #[serde(rename = "Subject")]
    pub subject: String,
    #[serde(rename = "AccountLevel")]
    pub account_level: i32,
    #[serde(rename = "Incognito")]
    pub incognito: bool,
    #[serde(rename = "HideAccountLevel")]
    pub hide_account_level: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CoreGameMatch {
    #[serde(rename = "MatchID")]
    pub match_id: String,
    #[serde(rename = "State")]
    pub state: String,
    #[serde(rename = "MapID")]
    pub map_id: String,
    #[serde(rename = "ModeID")]
    pub mode_id: String,
    #[serde(rename = "MatchmakingData")]
    pub matchmaking_data: Option<MatchmakingData>,
    #[serde(rename = "Players")]
    pub players: Vec<CoreGamePlayer>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct MatchmakingData {
    #[serde(rename = "QueueID")]
    pub queue_id: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CoreGamePlayer {
    #[serde(rename = "Subject")]
    pub subject: String,
    #[serde(rename = "TeamID")]
    pub team_id: String,
    #[serde(rename = "CharacterID")]
    pub character_id: String,
    #[serde(rename = "PlayerIdentity")]
    pub player_identity: PlayerIdentity,
    #[serde(rename = "IsCoach")]
    pub is_coach: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PregameMatch {
    #[serde(rename = "ID")]
    pub id: String,
    #[serde(rename = "MapID")]
    pub map_id: String,
    #[serde(rename = "QueueID")]
    pub queue_id: String,
    #[serde(rename = "Mode")]
    pub mode: String,
    #[serde(rename = "AllyTeam")]
    pub ally_team: Option<PregameTeam>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PregameTeam {
    #[serde(rename = "TeamID")]
    pub team_id: String,
    #[serde(rename = "Players")]
    pub players: Vec<PregamePlayer>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PregamePlayer {
    #[serde(rename = "Subject")]
    pub subject: String,
    #[serde(rename = "CharacterID")]
    pub character_id: String,
    #[serde(rename = "CharacterSelectionState")]
    pub character_selection_state: String,
    #[serde(rename = "PlayerIdentity")]
    pub player_identity: PlayerIdentity,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Loadouts {
    #[serde(rename = "Loadouts")]
    pub loadouts: Vec<PlayerLoadout>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PlayerLoadout {
    #[serde(rename = "CharacterID")]
    pub character_id: String,
    #[serde(rename = "Loadout")]
    pub loadout: Loadout,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Loadout {
    #[serde(rename = "Subject")]
    pub subject: String,
    #[serde(rename = "Items")]
    pub items: HashMap<String, LoadoutItem>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LoadoutItem {
    #[serde(rename = "ID")]
    pub id: String,
    #[serde(rename = "Sockets")]
    pub sockets: HashMap<String, Socket>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Socket {
    #[serde(rename = "Item")]
    pub item: SocketItem,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SocketItem {
    #[serde(rename = "ID")]
    pub id: String,
}

pub const WEAPON_VANDAL: &str = "9c82e19d-4575-0200-1a81-3eacf00cf872";
pub const WEAPON_PHANTOM: &str = "ee8e8d15-496b-07ac-e5f6-8fae5d4c7b1a";
pub const SOCKET_SKIN: &str = "bcef87d6-209b-46c6-8b19-fbe40bd95abc";
pub const SOCKET_CHROMA: &str = "3ad1b2b2-acdb-4524-852f-954a76ddae0a";

/// Seasons before Ascendant was added: tiers above 20 must be shifted by 3.
pub const PRE_ASCENDANT_SEASONS: [&str; 12] = [
    "0df5adb9-4dcb-6899-1306-3e9860661dd3",
    "3f61c772-4560-cd3f-5d3f-a7ab5abda6b3",
    "0530b9c4-4980-f2ee-df5d-09864cd00542",
    "46ea6166-4573-1128-9cea-60a15640059b",
    "fcf2c8f4-4324-e50b-2e23-718e4a3ab046",
    "97b6e739-44cc-ffa7-49ad-398ba502ceb0",
    "ab57ef51-4e59-da91-cc7d-51a5a2b9b8ff",
    "52e9749a-429b-7060-99fe-4595426a0cf7",
    "71c81c67-4fae-ceb1-844c-aab2bb8710fa",
    "2a27e5d2-4d30-c9e2-b15a-93b8909a442c",
    "4cb622e1-4244-6f41-a05f-2c7b300ee8fc",
    "a16955a5-4ad0-f761-5e9e-389df1c892fb",
];

impl Mmr {
    /// (current tier, current RR) from the latest competitive update.
    pub fn current(&self) -> Option<(i32, i32)> {
        if let Some(u) = &self.latest_competitive_update
            && u.tier_after_update > 0
        {
            return Some((u.tier_after_update, u.ranked_rating_after_update));
        }
        // No recent update: the act with the most games.
        let seasons = self.queue_skills.get("competitive")?.seasonal_info_by_season_id.as_ref()?;
        let s = seasons.values().filter(|s| s.competitive_tier.max(s.rank) > 2).max_by_key(|s| s.number_of_games)?;
        Some((normalize_tier(&s.season_id, s.competitive_tier.max(s.rank)), s.ranked_rating))
    }

    /// Highest tier with at least one recorded win across all acts.
    pub fn peak_tier(&self) -> Option<i32> {
        let seasons = self.queue_skills.get("competitive")?.seasonal_info_by_season_id.as_ref()?;
        let mut best = 0;
        for (id, s) in seasons {
            let shift = |t: i32| normalize_tier(id, t);
            for (tier, wins) in s.wins_by_tier.iter().flatten() {
                if let Ok(t) = tier.parse::<i32>()
                    && *wins > 0
                {
                    best = best.max(shift(t));
                }
            }
            best = best.max(shift(s.competitive_tier.max(s.rank)));
        }
        if best > 2 { Some(best) } else { None }
    }
}

impl Match {
    /// pd match details use `subject`; normalise access.
    pub fn player(&self, puuid: &str) -> Option<&Player> {
        self.players.iter().find(|p| p.subject == puuid)
    }
}
