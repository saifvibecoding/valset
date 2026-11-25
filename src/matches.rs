//! Match history and live match dashboards.
//!
//! Fair-play rules: only information the game itself shows you is displayed.
//! Your own rank, RR and peak are shown; other players' ranks are never looked
//! up. Players who hid their name (incognito / streamer mode) stay hidden, and
//! hidden account levels stay hidden.

use crate::rank;
use crate::riot::content::Content;
use crate::riot::models::{self, CoreGameMatch, Loadouts, Match, PregameMatch};
use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct OwnerStats {
    pub games: usize,
    pub wins: usize,
    pub kills: i64,
    pub deaths: i64,
    pub headshots: i64,
    pub shots: i64,
    pub current_tier: Option<i32>,
    pub rr: Option<i32>,
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
    pub fn apply_mmr(&mut self, mmr: &models::Mmr) {
        if let Some((t, rr)) = mmr.current() {
            self.current_tier = Some(t);
            self.rr = Some(rr);
        }
        if let Some(p) = mmr.peak_tier() {
            self.peak_tier = Some(self.peak_tier.map_or(p, |x| x.max(p)));
        }
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
    pub is_owner: bool,
    pub is_ally: bool,
    pub vandal: Option<String>,
    pub phantom: Option<String>,
    pub variant: Option<String>,
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
        for ps in r.player_stats.iter().filter(|p| p.subject == puuid) {
            for d in &ps.damage {
                hs += d.headshots as i64;
                total += (d.headshots + d.bodyshots + d.legshots) as i64;
            }
        }
    }
    (hs, total)
}

pub fn map_label(map_id: &str, content: Option<&Content>) -> String {
    if let Some(n) = content.and_then(|c| c.map_name(map_id)) {
        return n;
    }
    let code = map_id.rsplit('/').next().unwrap_or(map_id);
    if code.is_empty() { "Unknown map".into() } else { crate::game::map_display_name(code) }
}

fn agent_label(id: &str, content: Option<&Content>) -> String {
    if id.is_empty() {
        return "—".into();
    }
    content.and_then(|c| c.agent_name(id)).unwrap_or_else(|| "Agent".into())
}

fn display_name(game_name: &str, tag: &str, agent: &str, is_owner: bool, hide: bool) -> String {
    if !is_owner && hide {
        return agent.to_string();
    }
    if game_name.is_empty() { format!("Hidden ({agent})") } else { format!("{game_name}#{tag}") }
}

pub fn build_view(m: &Match, content: Option<&Content>, owner: &str, hide_other_names: bool) -> MatchView {
    let owner_team = m.player(owner).map(|p| p.team_id.clone());
    let mut players: Vec<PlayerLine> = m
        .players
        .iter()
        .filter(|p| !p.is_observer)
        .map(|p| {
            let is_owner = p.subject == owner;
            let st = p.stats.clone().unwrap_or_default();
            let (hs, total) = headshot_counts(m, &p.subject);
            let agent = agent_label(p.character_id.as_deref().unwrap_or(""), content);
            PlayerLine {
                puuid: p.subject.clone(),
                name: display_name(&p.game_name, &p.tag_line, &agent, is_owner, hide_other_names),
                team: p.team_id.clone(),
                agent,
                tier: if is_owner && p.competitive_tier > 0 { Some(p.competitive_tier) } else { None },
                level: if is_owner && p.account_level > 0 { Some(p.account_level) } else { None },
                kills: st.kills,
                deaths: st.deaths,
                assists: st.assists,
                score: st.score,
                hs_pct: if total > 0 { Some(hs as f64 * 100.0 / total as f64) } else { None },
                is_owner,
                is_ally: owner_team.as_deref() == Some(p.team_id.as_str()),
                vandal: None,
                phantom: None,
                variant: None,
            }
        })
        .collect();
    players.sort_by(|a, b| b.is_ally.cmp(&a.is_ally).then(b.score.cmp(&a.score)));
    let teams: Vec<(String, bool, i32)> = m.teams.iter().flatten().map(|t| (t.team_id.clone(), t.won, t.rounds_won)).collect();
    let owner_won = owner_team.as_ref().and_then(|ot| teams.iter().find(|t| &t.0 == ot).map(|t| t.1));
    let owner_line = players.iter().find(|p| p.is_owner).cloned();
    MatchView {
        match_id: m.match_info.match_id.clone(),
        map: map_label(&m.match_info.map_id, content),
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

/// Aggregate stats over stored matches (newest first).
pub fn owner_stats(matches: &[Match], owner: &str) -> OwnerStats {
    let mut s = OwnerStats::default();
    for m in matches {
        let Some(p) = m.player(owner) else { continue };
        s.games += 1;
        if m.teams.iter().flatten().any(|t| t.team_id == p.team_id && t.won) {
            s.wins += 1;
        }
        if let Some(st) = &p.stats {
            s.kills += st.kills as i64;
            s.deaths += st.deaths as i64;
        }
        let (hs, total) = headshot_counts(m, owner);
        s.headshots += hs;
        s.shots += total;
        if s.level.is_none() && p.account_level > 0 {
            s.level = Some(p.account_level);
        }
        if (m.match_info.is_ranked || m.match_info.queue_id == "competitive") && p.competitive_tier > 0 {
            if s.current_tier.is_none() {
                s.current_tier = Some(p.competitive_tier);
            }
            s.peak_tier = Some(s.peak_tier.map_or(p.competitive_tier, |x| x.max(p.competitive_tier)));
        }
    }
    s
}

// ------------------------------------------------------------------ live

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LivePhase {
    AgentSelect,
    InGame,
}

#[derive(Debug, Clone)]
pub struct LiveView {
    pub phase: LivePhase,
    pub match_id: String,
    pub map: String,
    pub mode: String,
    pub players: Vec<PlayerLine>,
}

fn skins_for(loadouts: Option<&Loadouts>, puuid: &str, content: Option<&Content>) -> (Option<String>, Option<String>, Option<String>) {
    let Some(l) = loadouts.and_then(|l| l.loadouts.iter().find(|x| x.loadout.subject == puuid)) else { return (None, None, None) };
    let skin = |weapon: &str| -> (Option<String>, Option<String>) {
        let Some(item) = l.loadout.items.get(weapon) else { return (None, None) };
        let skin_id = item.sockets.get(models::SOCKET_SKIN).map(|s| s.item.id.clone()).unwrap_or_default();
        let chroma_id = item.sockets.get(models::SOCKET_CHROMA).map(|s| s.item.id.clone()).unwrap_or_default();
        let name = content.and_then(|c| c.skin_name(&skin_id));
        let chroma = content.and_then(|c| c.chroma_name(&chroma_id)).filter(|ch| Some(ch) != name.as_ref());
        (name, chroma)
    };
    let (vandal, variant) = skin(models::WEAPON_VANDAL);
    let (phantom, _) = skin(models::WEAPON_PHANTOM);
    (vandal, phantom, variant)
}

pub fn build_live(m: &CoreGameMatch, loadouts: Option<&Loadouts>, names: &HashMap<String, (String, String)>, content: Option<&Content>, owner: &str, hide_other_names: bool) -> LiveView {
    let owner_team = m.players.iter().find(|p| p.subject == owner).map(|p| p.team_id.clone());
    let mut players: Vec<PlayerLine> = m
        .players
        .iter()
        .filter(|p| !p.is_coach)
        .map(|p| {
            let is_owner = p.subject == owner;
            let agent = agent_label(&p.character_id, content);
            let hidden = p.player_identity.incognito && !is_owner;
            let (gn, tag) = if hidden { (String::new(), String::new()) } else { names.get(&p.subject).cloned().unwrap_or_default() };
            let (vandal, phantom, variant) = skins_for(loadouts, &p.subject, content);
            PlayerLine {
                puuid: p.subject.clone(),
                name: if !is_owner && gn.is_empty() && !hidden { agent.clone() } else { display_name(&gn, &tag, &agent, is_owner, hide_other_names) },
                team: p.team_id.clone(),
                agent,
                tier: None,
                level: if p.player_identity.hide_account_level && !is_owner { None } else { Some(p.player_identity.account_level).filter(|l| *l > 0) },
                kills: 0,
                deaths: 0,
                assists: 0,
                score: 0,
                hs_pct: None,
                is_owner,
                is_ally: owner_team.as_deref() == Some(p.team_id.as_str()),
                vandal,
                phantom,
                variant,
            }
        })
        .collect();
    players.sort_by_key(|p| !p.is_owner);
    let queue = m.matchmaking_data.as_ref().map(|d| d.queue_id.clone()).unwrap_or_default();
    LiveView {
        phase: LivePhase::InGame,
        match_id: m.match_id.clone(),
        map: map_label(&m.map_id, content),
        mode: crate::riot::queue_name(&queue, &m.mode_id),
        players,
    }
}

pub fn build_pregame(m: &PregameMatch, names: &HashMap<String, (String, String)>, content: Option<&Content>, owner: &str, hide_other_names: bool) -> LiveView {
    let team = m.ally_team.clone().unwrap_or_default();
    let mut players: Vec<PlayerLine> = team
        .players
        .iter()
        .map(|p| {
            let is_owner = p.subject == owner;
            let agent = if p.character_id.is_empty() { "Selecting…".to_string() } else { agent_label(&p.character_id, content) };
            let hidden = p.player_identity.incognito && !is_owner;
            let (gn, tag) = if hidden { (String::new(), String::new()) } else { names.get(&p.subject).cloned().unwrap_or_default() };
            PlayerLine {
                puuid: p.subject.clone(),
                name: display_name(&gn, &tag, &agent, is_owner, hide_other_names),
                team: team.team_id.clone(),
                agent,
                tier: None,
                level: if p.player_identity.hide_account_level && !is_owner { None } else { Some(p.player_identity.account_level).filter(|l| *l > 0) },
                kills: 0,
                deaths: 0,
                assists: 0,
                score: 0,
                hs_pct: None,
                is_owner,
                is_ally: true,
                vandal: None,
                phantom: None,
                variant: None,
            }
        })
        .collect();
    players.sort_by_key(|p| !p.is_owner);
    LiveView { phase: LivePhase::AgentSelect, match_id: m.id.clone(), map: map_label(&m.map_id, content), mode: crate::riot::queue_name(&m.queue_id, &m.mode), players }
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
