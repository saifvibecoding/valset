//! Match dashboards built from VAL-MATCH-V1 data.
//!
//! Privacy and competitive-integrity rules implemented here:
//!
//! * Only *completed* matches from the official API are shown; there is no live
//!   roster for VALORANT through supported interfaces, so live rows are `N/A`.
//! * Profile-level data (rank history, peak, win rate, level) is computed only
//!   for accounts that opted in through RSO (`consent`).
//! * Other players are shown exactly as the post-match data lists them (the
//!   same information as the end-of-game scoreboard) and are never looked up
//!   further. Names can be hidden entirely with `hide_other_names`.
//! * Equipped skins / variants and RR are not exposed by any public API → `N/A`.

use crate::rank;
use crate::riot::models::{Content, Match};
use std::collections::HashSet;

#[derive(Debug, Clone, Default)]
pub struct OwnerStats {
    pub games: usize,
    pub wins: usize,
    pub kills: i64,
    pub deaths: i64,
    pub headshots: i64,
    pub shots: i64,
    pub current_tier: Option<i32>,
    pub peak_tier: Option<i32>,
    pub level: Option<i32>,
}

impl OwnerStats {
    pub fn kd(&self) -> Option<f64> {
        if self.games == 0 { None } else { Some(self.kills as f64 / (self.deaths.max(1)) as f64) }
    }
    pub fn hs_pct(&self) -> Option<f64> {
        if self.shots == 0 { None } else { Some(self.headshots as f64 * 100.0 / self.shots as f64) }
    }
    pub fn win_rate(&self) -> Option<f64> {
        if self.games == 0 { None } else { Some(self.wins as f64 * 100.0 / self.games as f64) }
    }
}

#[derive(Debug, Clone)]
pub struct PlayerLine {
    pub puuid: String,
    pub name: String,
    pub team: String,
    pub agent: String,
    pub tier: Option<i32>,
    pub level: Option<i32>,
    pub kills: i32,
    pub deaths: i32,
    pub assists: i32,
    pub score: i32,
    pub hs_pct: Option<f64>,
    pub consented: bool,
    pub is_owner: bool,
    pub is_ally: bool,
}

#[derive(Debug, Clone)]
pub struct MatchView {
    pub match_id: String,
    pub map: String,
    pub mode: String,
    pub ranked: bool,
    pub started_ms: i64,
    pub duration_ms: Option<i64>,
    pub teams: Vec<(String, bool, i32)>,
    pub players: Vec<PlayerLine>,
    pub owner_won: Option<bool>,
    pub owner_line: Option<PlayerLine>,
}

impl MatchView {
    pub fn score_line(&self) -> String {
        let ally = self.owner_line.as_ref().map(|o| o.team.clone());
        let mut t = self.teams.clone();
        if let Some(a) = ally {
            t.sort_by_key(|x| x.0 != a);
        }
        t.iter().map(|x| x.2.to_string()).collect::<Vec<_>>().join(" : ")
    }
    pub fn result_label(&self) -> &'static str {
        match self.owner_won {
            Some(true) => "WIN",
            Some(false) => "LOSS",
            None => "—",
        }
    }
}

pub fn headshot_counts(m: &Match, puuid: &str) -> (i64, i64) {
    let mut hs = 0i64;
    let mut total = 0i64;
    for r in m.round_results.iter().flatten() {
        for ps in r.player_stats.iter().filter(|p| p.puuid == puuid) {
            for d in &ps.damage {
                hs += d.headshots as i64;
                total += (d.headshots + d.bodyshots + d.legshots) as i64;
            }
        }
    }
    (hs, total)
}

fn map_label(m: &Match, content: Option<&Content>) -> String {
    if let Some(n) = content.and_then(|c| c.map_name(&m.match_info.map_id)) {
        return n;
    }
    let code = m.match_info.map_id.rsplit('/').next().unwrap_or(&m.match_info.map_id);
    crate::game::map_display_name(code)
}

pub fn build_view(m: &Match, content: Option<&Content>, owner_api_puuid: &str, consented: &HashSet<String>, hide_other_names: bool) -> MatchView {
    let owner_team = m.players.iter().find(|p| p.puuid == owner_api_puuid).map(|p| p.team_id.clone());
    let mut players: Vec<PlayerLine> = m
        .players
        .iter()
        .filter(|p| !p.is_observer)
        .map(|p| {
            let is_owner = p.puuid == owner_api_puuid;
            let consented_p = is_owner || consented.contains(&p.puuid);
            let st = p.stats.clone().unwrap_or_default();
            let (hs, total) = headshot_counts(m, &p.puuid);
            let name = if consented_p || !hide_other_names {
                if p.game_name.is_empty() { "Hidden player".to_string() } else { format!("{}#{}", p.game_name, p.tag_line) }
            } else {
                "Player".to_string()
            };
            let agent = p
                .character_id
                .as_deref()
                .map(|id| content.and_then(|c| c.agent_name(id)).unwrap_or_else(|| "Unknown agent".into()))
                .unwrap_or_else(|| "N/A".into());
            PlayerLine {
                puuid: p.puuid.clone(),
                name,
                team: p.team_id.clone(),
                agent,
                tier: if consented_p && p.competitive_tier > 0 { Some(p.competitive_tier) } else { None },
                level: if consented_p && p.account_level > 0 { Some(p.account_level) } else { None },
                kills: st.kills,
                deaths: st.deaths,
                assists: st.assists,
                score: st.score,
                hs_pct: if total > 0 { Some(hs as f64 * 100.0 / total as f64) } else { None },
                consented: consented_p,
                is_owner,
                is_ally: owner_team.as_deref() == Some(p.team_id.as_str()),
            }
        })
        .collect();
    players.sort_by(|a, b| b.is_ally.cmp(&a.is_ally).then(b.score.cmp(&a.score)));
    let teams: Vec<(String, bool, i32)> = m.teams.iter().flatten().map(|t| (t.team_id.clone(), t.won, t.rounds_won)).collect();
    let owner_won = owner_team.as_ref().and_then(|ot| teams.iter().find(|t| &t.0 == ot).map(|t| t.1));
    let owner_line = players.iter().find(|p| p.is_owner).cloned();
    MatchView {
        match_id: m.match_info.match_id.clone(),
        map: map_label(m, content),
        mode: crate::riot::queue_name(&m.match_info.queue_id, &m.match_info.game_mode),
        ranked: m.match_info.is_ranked,
        started_ms: m.match_info.game_start_millis,
        duration_ms: m.match_info.game_length_millis,
        teams,
        players,
        owner_won,
        owner_line,
    }
}

/// Aggregate stats for a consenting player over stored matches (newest first).
pub fn owner_stats(matches: &[Match], owner_api_puuid: &str) -> OwnerStats {
    let mut s = OwnerStats::default();
    for m in matches {
        let Some(p) = m.players.iter().find(|p| p.puuid == owner_api_puuid) else { continue };
        s.games += 1;
        if m.teams.iter().flatten().any(|t| t.team_id == p.team_id && t.won) {
            s.wins += 1;
        }
        if let Some(st) = &p.stats {
            s.kills += st.kills as i64;
            s.deaths += st.deaths as i64;
        }
        let (hs, total) = headshot_counts(m, owner_api_puuid);
        s.headshots += hs;
        s.shots += total;
        if s.level.is_none() && p.account_level > 0 {
            s.level = Some(p.account_level);
        }
        if m.match_info.is_ranked || m.match_info.queue_id == "competitive" {
            if s.current_tier.is_none() && p.competitive_tier > 0 {
                s.current_tier = Some(p.competitive_tier);
            }
            if p.competitive_tier > 0 {
                s.peak_tier = Some(s.peak_tier.map_or(p.competitive_tier, |x| x.max(p.competitive_tier)));
            }
        }
    }
    s
}

pub fn fmt_opt_pct(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.1}%")).unwrap_or_else(|| "N/A".into())
}

pub fn fmt_tier(t: Option<i32>) -> String {
    t.map(rank::tier_name).unwrap_or_else(|| "N/A".into())
}

pub fn fmt_duration(ms: Option<i64>) -> String {
    match ms {
        Some(ms) if ms > 0 => format!("{}:{:02}", ms / 60_000, (ms / 1000) % 60),
        _ => "N/A".into(),
    }
}

pub fn fmt_time(ms: i64) -> String {
    use chrono::TimeZone;
    chrono::Local.timestamp_millis_opt(ms).single().map(|d| d.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_else(|| "N/A".into())
}
