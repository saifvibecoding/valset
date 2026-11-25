//! Rendering for the interactive interface.

use super::*;
use crate::matches::{self, MatchView, PlayerLine};
use crate::rank;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::widgets::{Block, BorderType, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState, Wrap};

const SPIN: [&str; 4] = ["|", "/", "-", "\\"];

fn block(title: impl Into<String>) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::Rgb(70, 75, 85)))
        .title(Span::styled(format!(" {} ", title.into()), Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)))
}

fn dot(on: bool, warn: bool) -> Span<'static> {
    let c = if on && warn { Color::Yellow } else if on { Color::Green } else { MUTED };
    Span::styled("●", Style::new().fg(c))
}

fn key(k: &str) -> Span<'static> {
    Span::styled(format!(" {k} "), Style::new().fg(Color::Black).bg(Color::Rgb(160, 165, 175)))
}

fn hint(text: &str) -> Span<'static> {
    Span::styled(format!(" {text}  "), Style::new().fg(MUTED))
}

fn hints(pairs: &[(&str, &str)]) -> Line<'static> {
    let mut spans = Vec::new();
    for (k, t) in pairs {
        spans.push(key(k));
        spans.push(hint(t));
    }
    Line::from(spans)
}

fn sel_style() -> Style {
    Style::new().bg(Color::Rgb(60, 20, 28)).fg(Color::White).add_modifier(Modifier::BOLD)
}

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    let [body, status_line, footer] = Layout::vertical([Constraint::Min(5), Constraint::Length(1), Constraint::Length(1)]).areas(area);
    match &app.screen {
        Screen::Main => draw_main(f, app, body),
        Screen::Text(tv) => draw_text(f, tv, body),
        Screen::Load(st) => draw_load(f, app, st, body),
        Screen::Restore(st) => draw_restore(f, app, st, body),
        Screen::Profiles(st) => draw_profiles(f, app, st, body),
        Screen::Accounts(st) => draw_accounts(f, st, body),
        Screen::Live(st) => draw_live(f, app, st, body),
        Screen::History(st) => draw_history(f, app, st, body),
        Screen::Backups(st) => draw_backups(f, app, st, body),
        Screen::Settings { sel } => draw_settings(f, app, *sel, body),
    }
    // Status / toast line
    let status = if let Some((msg, _)) = &app.busy {
        Line::from(vec![Span::styled(format!(" {} ", SPIN[(app.tick % 4) as usize]), Style::new().fg(ACCENT)), Span::raw(format!("{msg}…"))])
    } else if app.jobs_running > 0 {
        Line::from(vec![Span::styled(format!(" {} ", SPIN[(app.tick % 4) as usize]), Style::new().fg(ACCENT)), Span::styled("Working in background…", Style::new().fg(MUTED))])
    } else if let Some((lvl, msg, _)) = &app.toast {
        let (icon, c) = match lvl {
            Level::Ok => ("✔", Color::Green),
            Level::Warn => ("!", Color::Yellow),
            Level::Error => ("✘", Color::Red),
            Level::Info => ("•", TEAL),
        };
        Line::from(vec![Span::styled(format!(" {icon} "), Style::new().fg(c).add_modifier(Modifier::BOLD)), Span::styled(msg.clone(), Style::new().fg(c))])
    } else {
        Line::from("")
    };
    f.render_widget(Paragraph::new(status), status_line);
    let foot = match &app.screen {
        Screen::Main => hints(&[("↑↓", "move"), ("Enter", "select"), ("1-9", "shortcut"), ("Esc", "exit")]),
        Screen::Text(_) => hints(&[("↑↓/PgUp/PgDn", "scroll"), ("Esc", "back")]),
        Screen::Load(st) => match st.stage {
            LoadStage::Profiles => hints(&[("↑↓", "move"), ("Enter", "choose profile"), ("Esc", "back")]),
            LoadStage::Categories => hints(&[("Space", "toggle"), ("a", "all/none"), ("Enter", "done"), ("Esc", "back")]),
            LoadStage::Preview => hints(&[("Enter", "apply"), ("c", "choose categories"), ("↑↓", "scroll"), ("Esc", "back")]),
        },
        Screen::Restore(st) if st.preview.is_some() => hints(&[("↑↓", "scroll"), ("Enter", "restore"), ("Esc", "back")]),
        Screen::Restore(_) => hints(&[("↑↓", "move"), ("Enter", "preview"), ("u", "undo"), ("r", "redo"), ("Esc", "back")]),
        Screen::Profiles(_) => hints(&[("Enter", "view"), ("r", "rename"), ("d", "dup"), ("x", "del"), ("f", "fav"), ("c", "compare"), ("e", "export"), ("i", "import"), ("p", "prefer"), ("v", "check")]),
        Screen::Accounts(_) => hints(&[("p", "preferred profile"), ("x", "forget"), ("Esc", "back")]),
        Screen::Live(_) => hints(&[("r", "refresh"), ("Esc", "back")]),
        Screen::History(_) => hints(&[("↑↓", "move"), ("Enter", "details"), ("r", "refresh"), ("Esc", "back")]),
        Screen::Backups(_) => hints(&[("Enter", "preview/restore"), ("v", "verify"), ("V", "verify all"), ("m", "manual backup"), ("x", "delete"), ("a", "all accounts")]),
        Screen::Settings { .. } => hints(&[("↑↓", "move"), ("Enter", "edit/toggle"), ("Esc", "back")]),
    };
    f.render_widget(Paragraph::new(foot), footer);
    if let Some(m) = &app.modal {
        draw_modal(f, m, area);
    }
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(2));
    let h = h.min(area.height.saturating_sub(2));
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

fn draw_modal(f: &mut Frame, m: &Modal, area: Rect) {
    match m {
        Modal::Confirm { title, body, .. } => {
            let h = body.len() as u16 + 5;
            let r = centered(area, 72, h);
            f.render_widget(Clear, r);
            let mut lines = body.clone();
            lines.push(Line::from(""));
            lines.push(Line::from(vec![key("Y"), hint("yes"), key("N"), hint("no")]));
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(block(title.clone())), r);
        }
        Modal::Input { title, prompt, value, mask, .. } => {
            let r = centered(area, 76, 7);
            f.render_widget(Clear, r);
            let shown = if *mask { "•".repeat(value.chars().count()) } else { value.clone() };
            let width = r.width.saturating_sub(6) as usize;
            let visible: String = if shown.chars().count() > width { shown.chars().skip(shown.chars().count() - width).collect() } else { shown };
            let lines = vec![
                Line::from(prompt.clone()),
                Line::from(vec![Span::styled("> ", Style::new().fg(ACCENT)), Span::raw(visible), Span::styled("█", Style::new().fg(ACCENT))]),
                Line::from(""),
                Line::from(vec![key("Enter"), hint("confirm"), key("Esc"), hint("cancel")]),
            ];
            f.render_widget(Paragraph::new(lines).block(block(title.clone())), r);
        }
    }
}

fn draw_main(f: &mut Frame, app: &App, area: Rect) {
    let show_logo = area.height >= 30 && area.width >= 64;
    let logo_h = if show_logo { 8 } else { 2 };
    let banner_h = if app.banner.is_some() { 1 } else { 0 };
    let [logo_area, banner_area, status_area, menu_area] =
        Layout::vertical([Constraint::Length(logo_h), Constraint::Length(banner_h), Constraint::Length(8), Constraint::Min(4)]).areas(area);
    if show_logo {
        let w = crate::term::LOGO.lines().map(|l| l.chars().count()).max().unwrap_or(0);
        let mut lines: Vec<Line> = crate::term::LOGO
            .lines()
            .skip(1)
            .map(|l| Line::from(Span::styled(format!("{l:<w$}"), Style::new().fg(ACCENT))))
            .collect();
        lines.push(Line::from(Span::styled(format!("  v{}  ·  VALORANT settings manager", env!("CARGO_PKG_VERSION")), Style::new().fg(MUTED))));
        f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), logo_area);
    } else {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("VALSYNC", Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)),
                Span::styled(format!("  v{}", env!("CARGO_PKG_VERSION")), Style::new().fg(MUTED)),
            ]))
            .alignment(Alignment::Center),
            logo_area,
        );
    }
    if let Some(b) = &app.banner {
        f.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(" ⟳ ", Style::new().fg(Color::Black).bg(Color::Yellow)), Span::styled(format!(" {b}  "), Style::new().fg(Color::Yellow)), Span::styled("(x to dismiss)", Style::new().fg(MUTED))])),
            banner_area,
        );
    }
    let s = &app.status;
    let [left, right] = Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(status_area);
    let label = |t: &str| Span::styled(format!("{t:<16}"), Style::new().fg(MUTED));
    let (acct, method) = match &s.active {
        Some((row, _, m)) => (
            Line::from(vec![
                label("Active account"),
                Span::styled(row.display(), Style::new().add_modifier(Modifier::BOLD)),
                Span::styled(format!("  [{}]", row.shard.clone().unwrap_or_else(|| "?".into()).to_uppercase()), Style::new().fg(TEAL)),
            ]),
            Line::from(vec![label("Detection"), Span::styled(m.label().to_string(), Style::new().fg(if m.is_confident() { MUTED } else { Color::Yellow }))]),
        ),
        None => (
            Line::from(vec![label("Active account"), Span::styled("none detected", Style::new().fg(Color::Yellow))]),
            Line::from(vec![label("Detection"), Span::styled("open the Riot Client and sign in", Style::new().fg(MUTED))]),
        ),
    };
    let riot = if s.riot_signed_in {
        Line::from(vec![label("Riot Client"), dot(true, false), Span::raw(" signed in")])
    } else {
        Line::from(vec![label("Riot Client"), dot(false, false), Span::styled(" open it and sign in", Style::new().fg(Color::Yellow))])
    };
    let left_lines = vec![
        acct,
        method,
        riot,
        Line::from(vec![
            label("VALORANT"),
            dot(s.processes.valorant, true),
            Span::raw(if s.processes.valorant { " running — close it to apply settings" } else { " closed — ready" }),
        ]),
        Line::from(vec![label("Accounts known"), Span::raw(s.account_count.to_string())]),
    ];
    f.render_widget(Paragraph::new(left_lines).block(block("Status")), left);
    let right_lines = vec![
        Line::from(vec![label("Game patch"), Span::raw(s.patch.clone().unwrap_or_else(|| "unknown".into()))]),
        Line::from(vec![label("Last saved"), Span::raw(s.last_saved_profile.clone().unwrap_or_else(|| "—".into()))]),
        Line::from(vec![label("Profiles"), Span::raw(s.profile_count.to_string())]),
        Line::from(vec![label("Backups"), Span::raw(format!("{} for account · {} total", s.backup_count_active, s.backup_count_total))]),
        Line::from(vec![
            label("Undo / Redo"),
            Span::styled(if s.undo_available { "undo ✔" } else { "undo —" }, Style::new().fg(if s.undo_available { Color::Green } else { MUTED })),
            Span::raw("  "),
            Span::styled(if s.redo_available { "redo ✔" } else { "redo —" }, Style::new().fg(if s.redo_available { Color::Green } else { MUTED })),
        ]),
    ];
    f.render_widget(Paragraph::new(right_lines).block(block("Library")), right);

    let items: Vec<ListItem> = MENU
        .iter()
        .map(|(k, t)| {
            ListItem::new(Line::from(vec![
                Span::styled(format!("  [{k}] "), Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)),
                Span::raw(t.to_string()),
            ]))
        })
        .collect();
    let menu_w = 44.min(menu_area.width);
    let menu_rect = Rect { x: menu_area.x + (menu_area.width - menu_w) / 2, y: menu_area.y, width: menu_w, height: menu_area.height.min(12) };
    let mut ls = ListState::default().with_selected(Some(app.menu_sel));
    f.render_stateful_widget(List::new(items).block(block("Menu")).highlight_style(sel_style()).highlight_symbol("▶"), menu_rect, &mut ls);
}

fn draw_text(f: &mut Frame, tv: &TextView, area: Rect) {
    f.render_widget(Paragraph::new(tv.lines.clone()).scroll((tv.scroll, 0)).block(block(tv.title.clone())), area);
}

fn table_state(sel: usize) -> TableState {
    TableState::default().with_selected(Some(sel))
}

fn header(cols: &[&str]) -> Row<'static> {
    Row::new(cols.iter().map(|c| Cell::from(c.to_string())).collect::<Vec<_>>()).style(Style::new().fg(MUTED).add_modifier(Modifier::BOLD))
}

fn draw_load(f: &mut Frame, app: &App, st: &LoadState, area: Rect) {
    let target = st.target.as_ref().map(|t| app.engine.account_label(&t.puuid)).unwrap_or_default();
    match st.stage {
        LoadStage::Profiles => {
            let pref = app.status.active.as_ref().and_then(|a| a.0.preferred_profile.clone());
            let rows: Vec<Row> = st
                .profiles
                .iter()
                .map(|p| {
                    let is_pref = pref.as_deref().map(|x| x.eq_ignore_ascii_case(&p.name)).unwrap_or(false);
                    Row::new(vec![
                        Cell::from(if p.favorite { "★" } else { " " }).style(Style::new().fg(Color::Yellow)),
                        Cell::from(p.name.clone()),
                        Cell::from(p.profile.local_count().to_string()),
                        Cell::from((p.profile.settings.len() - p.profile.local_count()).to_string()),
                        Cell::from(p.profile.patch.clone().unwrap_or_else(|| "?".into())),
                        Cell::from(crate::term::local_time(&p.updated_at)),
                        Cell::from(if is_pref { "preferred" } else { "" }).style(Style::new().fg(TEAL)),
                    ])
                })
                .collect();
            let t = Table::new(rows, [Constraint::Length(2), Constraint::Min(16), Constraint::Length(6), Constraint::Length(6), Constraint::Length(14), Constraint::Length(20), Constraint::Length(10)])
                .header(header(&["", "Profile", "Local", "Cloud", "Patch", "Updated", ""]))
                .row_highlight_style(sel_style())
                .block(block(format!("Load Saved Settings → {target}{}", if st.guessed { "  (account guessed!)" } else { "" })));
            f.render_stateful_widget(t, area, &mut table_state(st.sel));
        }
        LoadStage::Categories => {
            let [info, list] = Layout::vertical([Constraint::Length(3), Constraint::Min(3)]).areas(area);
            f.render_widget(
                Paragraph::new(vec![
                    Line::from(format!("Profile '{}' → {target}", st.profiles[st.sel].name)),
                    Line::from(Span::styled("Pick what to copy. Everything is selected by default.", Style::new().fg(MUTED))),
                ])
                .block(Block::default()),
                info,
            );
            let items: Vec<ListItem> = st
                .cats
                .iter()
                .map(|(c, l, cl, on)| {
                    ListItem::new(Line::from(vec![
                        Span::styled(if *on { " [x] " } else { " [ ] " }, Style::new().fg(ACCENT)),
                        Span::raw(format!("{:<26}", c.label())),
                        Span::styled(format!("{} setting(s)", l + cl), Style::new().fg(MUTED)),
                    ]))
                })
                .collect();
            let mut ls = ListState::default().with_selected(Some(st.cat_sel));
            f.render_stateful_widget(List::new(items).block(block("Select categories")).highlight_style(sel_style()), list, &mut ls);
        }
        LoadStage::Preview => {
            let Some(plan) = &st.plan else { return };
            let mut lines = Vec::new();
            for w in plan.warnings.iter().chain(plan.patch_warning.iter()) {
                lines.push(styled(format!("! {w}"), Style::new().fg(Color::Yellow)));
            }
            if app.status.processes.valorant {
                lines.push(styled("! VALORANT is running — close the game before applying.", Style::new().fg(Color::Yellow)));
            }
            lines.push(Line::from(vec![
                Span::styled(format!("{} change(s)", plan.changes.len()), Style::new().fg(Color::Green).add_modifier(Modifier::BOLD)),
                Span::raw(format!(" will be applied · {} already match", plan.unchanged)),
            ]));
            lines.push(Line::from(""));
            if plan.changes.is_empty() {
                lines.push(Line::from("Nothing to change — the account already matches the selected categories."));
            }
            for c in &plan.changes {
                lines.push(change_line(c));
            }
            lines.extend(manual_lines(&plan.manual, &st.profiles[st.sel].profile.manual));
            f.render_widget(Paragraph::new(lines).scroll((st.scroll as u16, 0)).block(block(format!("Preview: '{}' → {target}", st.profiles[st.sel].name))), area);
        }
    }
}

fn draw_restore(f: &mut Frame, app: &App, st: &RestoreState, area: Rect) {
    if let Some((item, changes)) = &st.preview {
        let mut lines = vec![
            Line::from(Span::styled(restore_title(item), Style::new().add_modifier(Modifier::BOLD))),
            Line::from(format!("{} setting(s) will change on {}", changes.len(), app.active_label())),
            Line::from(""),
        ];
        if changes.is_empty() {
            lines.push(Line::from("Current settings already match this snapshot."));
        }
        for c in changes {
            lines.push(change_line(c));
        }
        f.render_widget(Paragraph::new(lines).scroll((st.scroll as u16, 0)).block(block("Preview restore")), area);
        return;
    }
    let items: Vec<ListItem> = st
        .items
        .iter()
        .map(|i| {
            let (icon, c) = match i {
                RestoreItem::Undo(_) => ("↶ ", Color::Green),
                RestoreItem::Redo(_) => ("↷ ", TEAL),
                RestoreItem::Original(_) => ("◆ ", Color::Cyan),
                RestoreItem::Backup(b) if b.corrupted => ("✘ ", Color::Red),
                RestoreItem::Backup(_) => ("• ", MUTED),
            };
            ListItem::new(Line::from(vec![Span::styled(icon, Style::new().fg(c)), Span::raw(restore_title(i))]))
        })
        .collect();
    let title = format!("Restore Previous Settings — {}", app.active_label());
    if items.is_empty() {
        f.render_widget(Paragraph::new("No backups for this account yet. They are created automatically before every transfer.").block(block(title)), area);
        return;
    }
    let mut ls = ListState::default().with_selected(Some(st.sel));
    f.render_stateful_widget(List::new(items).block(block(title)).highlight_style(sel_style()), area, &mut ls);
}

fn draw_profiles(f: &mut Frame, app: &App, st: &ProfilesState, area: Rect) {
    let accounts = app.engine.db.accounts().unwrap_or_default();
    let rows: Vec<Row> = st
        .rows
        .iter()
        .map(|p| {
            let prefs: Vec<String> = accounts.iter().filter(|a| a.preferred_profile.as_deref().map(|x| x.eq_ignore_ascii_case(&p.name)).unwrap_or(false)).map(|a| a.display()).collect();
            let marked = st.mark.as_deref() == Some(p.name.as_str());
            Row::new(vec![
                Cell::from(if p.favorite { "★" } else { " " }).style(Style::new().fg(Color::Yellow)),
                Cell::from(format!("{}{}", p.name, if marked { "  ◀ compare" } else { "" })),
                Cell::from(p.profile.local_count().to_string()),
                Cell::from((p.profile.settings.len() - p.profile.local_count()).to_string()),
                Cell::from(p.profile.patch.clone().unwrap_or_else(|| "?".into())),
                Cell::from(crate::term::local_time(&p.updated_at)),
                Cell::from(p.source.clone()),
                Cell::from(prefs.join(", ")).style(Style::new().fg(TEAL)),
            ])
        })
        .collect();
    if rows.is_empty() {
        f.render_widget(Paragraph::new("No profiles yet. Use [1] Save Current Settings, or press i to import a .valsync file.").block(block("Manage Settings Profiles")), area);
        return;
    }
    let t = Table::new(
        rows,
        [Constraint::Length(2), Constraint::Min(18), Constraint::Length(6), Constraint::Length(6), Constraint::Length(14), Constraint::Length(20), Constraint::Length(10), Constraint::Min(10)],
    )
    .header(header(&["", "Profile", "Local", "Cloud", "Patch", "Updated", "Source", "Preferred by"]))
    .row_highlight_style(sel_style())
    .block(block("Manage Settings Profiles"));
    f.render_stateful_widget(t, area, &mut table_state(st.sel));
}

fn draw_accounts(f: &mut Frame, st: &AccountsState, area: Rect) {
    let rows: Vec<Row> = st
        .rows
        .iter()
        .map(|(a, _, active)| {
            Row::new(vec![
                Cell::from(if *active { "▶" } else { " " }).style(Style::new().fg(Color::Green)),
                Cell::from(a.riot_id().unwrap_or_else(|| "(name appears after you sign in)".into())),
                Cell::from(a.shard.clone().unwrap_or_else(|| "?".into()).to_uppercase()),
                Cell::from(a.preferred_profile.clone().unwrap_or_else(|| "—".into())),
                Cell::from(crate::term::local_time(&a.last_seen)).style(Style::new().fg(MUTED)),
            ])
        })
        .collect();
    let [tarea, note] = Layout::vertical([Constraint::Min(4), Constraint::Length(2)]).areas(area);
    let t = Table::new(rows, [Constraint::Length(2), Constraint::Min(24), Constraint::Length(7), Constraint::Min(16), Constraint::Length(20)])
        .header(header(&["", "Riot ID", "Region", "Preferred profile", "Last seen"]))
        .row_highlight_style(sel_style())
        .block(block("Riot Accounts"));
    f.render_stateful_widget(t, tarea, &mut table_state(st.sel));
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Switch accounts in the Riot Client as usual — VALSYNC follows automatically. It never asks for your password.",
            Style::new().fg(MUTED),
        )))
        .wrap(Wrap { trim: true }),
        note,
    );
}

fn tier_cell(t: Option<i32>) -> Cell<'static> {
    match t {
        Some(t) => {
            let st = Style::new().fg(rank::color(t));
            let st = if rank::family(t) == rank::RankFamily::Radiant { st.add_modifier(Modifier::BOLD) } else { st };
            Cell::from(rank::tier_name(t)).style(st)
        }
        None => Cell::from("N/A").style(Style::new().fg(MUTED)),
    }
}

fn na() -> Cell<'static> {
    Cell::from("N/A").style(Style::new().fg(MUTED))
}

fn opt_cell(v: Option<String>) -> Cell<'static> {
    v.map(Cell::from).unwrap_or_else(na)
}

/// 5-row team table. Rank/RR/peak/K/D/HS/WR are shown for you only.
fn team_table(players: &[PlayerLine], title: String, owner: Option<&crate::matches::OwnerStats>, width: u16, live: bool) -> Table<'static> {
    let wide = width >= 150;
    let mut cols = vec!["Player", "Agent", "Rank", "RR", "Peak", "Lvl", if live { "K/D" } else { "K/D/A" }, "HS%", "WR", "Vandal", "Phantom"];
    if wide {
        cols.insert(5, "Peak RR");
        cols.push("Variant");
    }
    let rows: Vec<Row> = (0..5)
        .map(|i| match players.get(i) {
            Some(p) => {
                let me = if p.is_owner { owner } else { None };
                let mut cells = vec![
                    Cell::from(p.name.clone()).style(if p.is_owner { Style::new().add_modifier(Modifier::BOLD) } else { Style::new() }),
                    Cell::from(p.agent.clone()),
                    tier_cell(me.and_then(|o| o.current_tier).or(p.tier)),
                    opt_cell(me.and_then(|o| o.rr).map(|r| r.to_string())),
                    tier_cell(me.and_then(|o| o.peak_tier)),
                ];
                if wide {
                    cells.push(na());
                }
                cells.push(opt_cell(p.level.or(me.and_then(|o| o.level)).map(|l| l.to_string())));
                if live {
                    cells.push(opt_cell(me.and_then(|o| o.kd()).map(|k| format!("{k:.2}"))));
                    cells.push(opt_cell(me.and_then(|o| o.hs_pct()).map(|h| format!("{h:.1}%"))));
                } else {
                    cells.push(Cell::from(format!("{}/{}/{}", p.kills, p.deaths, p.assists)));
                    cells.push(opt_cell(p.hs_pct.map(|h| format!("{h:.1}%"))));
                }
                cells.push(opt_cell(me.and_then(|o| o.win_rate()).map(|w| format!("{w:.0}%"))));
                cells.push(opt_cell(p.vandal.clone()));
                cells.push(opt_cell(p.phantom.clone()));
                if wide {
                    cells.push(opt_cell(p.variant.clone()));
                }
                Row::new(cells)
            }
            None => Row::new(cols.iter().map(|_| Cell::from("")).collect::<Vec<_>>()),
        })
        .collect();
    let mut widths = vec![
        Constraint::Min(18),
        Constraint::Length(10),
        Constraint::Length(12),
        Constraint::Length(4),
        Constraint::Length(12),
        Constraint::Length(4),
        Constraint::Length(if live { 5 } else { 9 }),
        Constraint::Length(6),
        Constraint::Length(4),
        Constraint::Length(16),
        Constraint::Length(16),
    ];
    if wide {
        widths.insert(5, Constraint::Length(8));
        widths.push(Constraint::Length(18));
    }
    Table::new(rows, widths).header(header(&cols)).block(block(title))
}

fn draw_match(f: &mut Frame, v: &MatchView, owner: Option<&crate::matches::OwnerStats>, area: Rect, heading: &str) {
    let [info, a, b] = Layout::vertical([Constraint::Length(3), Constraint::Length(8), Constraint::Length(8)]).areas(area);
    let res_style = match v.owner_won {
        Some(true) => Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
        Some(false) => Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
        None => Style::new(),
    };
    let comp: Vec<String> = v.players.iter().filter(|p| p.is_ally).map(|p| p.agent.clone()).collect();
    f.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(format!("{heading}  "), Style::new().fg(MUTED)),
                Span::styled(format!("{} · {} ", v.map, v.mode), Style::new().add_modifier(Modifier::BOLD)),
                Span::styled(format!("{} {}", v.result_label(), v.score_line()), res_style),
                Span::styled(format!("   {} · {}", matches::fmt_time(v.started_ms), matches::fmt_duration(v.duration_ms)), Style::new().fg(MUTED)),
            ]),
            Line::from(Span::styled(format!("Your team: {}", comp.join(", ")), Style::new().fg(MUTED))),
        ]),
        info,
    );
    let ally: Vec<PlayerLine> = v.players.iter().filter(|p| p.is_ally).cloned().collect();
    let enemy: Vec<PlayerLine> = v.players.iter().filter(|p| !p.is_ally).cloned().collect();
    f.render_widget(team_table(&ally, "Your team".into(), owner, area.width, false), a);
    f.render_widget(team_table(&enemy, "Opponents".into(), owner, area.width, false), b);
}

fn draw_live(f: &mut Frame, app: &App, st: &LiveState, area: Rect) {
    let [head, main] = Layout::vertical([Constraint::Length(4), Constraint::Min(10)]).areas(area);
    let next = st.last_fetch.map(|t| {
        let every = if app.status.processes.valorant { app.engine.config.poll_secs() } else { 60 };
        every.saturating_sub(t.elapsed().as_secs())
    });
    let view = st.data.as_ref().and_then(|d| d.view.as_ref());
    let mut lines = vec![Line::from(vec![
        Span::styled(st.data.as_ref().map(|d| d.account.clone()).unwrap_or_else(|| app.active_label()), Style::new().add_modifier(Modifier::BOLD)),
        Span::raw("   "),
        match view {
            Some(v) => Span::styled(
                format!("{} · {} · {}", if v.phase == matches::LivePhase::AgentSelect { "AGENT SELECT" } else { "IN GAME" }, v.map, v.mode),
                Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
            ),
            None => Span::styled("not in a match", Style::new().fg(MUTED)),
        },
        Span::styled(
            if st.loading { "   refreshing…".to_string() } else { next.map(|n| format!("   refresh in {n}s")).unwrap_or_default() },
            Style::new().fg(MUTED),
        ),
    ])];
    if let Some(e) = &st.error {
        lines.push(Line::from(Span::styled(e.clone(), Style::new().fg(Color::Yellow))));
    } else {
        lines.push(Line::from(Span::styled(
            "Shows what the game shows you. Other players' ranks are never looked up; hidden names and levels stay hidden.",
            Style::new().fg(MUTED),
        )));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }).block(block("Live Match")), head);
    let stats = st.data.as_ref().map(|d| &d.stats);
    match (view, st.data.as_ref().and_then(|d| d.last_match.as_ref())) {
        (Some(v), _) => {
            let [a, b] = Layout::vertical([Constraint::Length(8), Constraint::Length(8)]).areas(main);
            let ally: Vec<PlayerLine> = v.players.iter().filter(|p| p.is_ally).cloned().collect();
            let enemy: Vec<PlayerLine> = v.players.iter().filter(|p| !p.is_ally).cloned().collect();
            f.render_widget(team_table(&ally, "Your team".into(), stats, main.width, true), a);
            if v.phase == matches::LivePhase::InGame {
                f.render_widget(team_table(&enemy, "Opponents".into(), stats, main.width, true), b);
            } else {
                f.render_widget(Paragraph::new("Opponents appear once the match loads.").block(block("Opponents")), b);
            }
        }
        (None, Some(last)) => draw_match(f, last, stats, main, "Last match"),
        (None, None) => {
            let me: Vec<PlayerLine> = app
                .status
                .active
                .as_ref()
                .map(|a| PlayerLine {
                    puuid: a.0.puuid.clone(),
                    name: a.0.display(),
                    team: String::new(),
                    agent: "—".into(),
                    tier: None,
                    level: None,
                    kills: 0,
                    deaths: 0,
                    assists: 0,
                    score: 0,
                    hs_pct: None,
                    is_owner: true,
                    is_ally: true,
                    vandal: None,
                    phantom: None,
                    variant: None,
                })
                .into_iter()
                .collect();
            let [a, _] = Layout::vertical([Constraint::Length(8), Constraint::Min(0)]).areas(main);
            f.render_widget(team_table(&me, "You".into(), stats, main.width, true), a);
        }
    }
}

fn draw_history(f: &mut Frame, app: &App, st: &HistoryState, area: Rect) {
    let title = format!("Match History — {}{}", st.data.as_ref().map(|d| d.account.clone()).unwrap_or_else(|| app.active_label()), if st.loading { "  (loading new matches…)" } else { "" });
    let Some(d) = &st.data else {
        let msg = st.error.clone().unwrap_or_else(|| "Loading…".into());
        f.render_widget(Paragraph::new(msg).wrap(Wrap { trim: true }).block(block(title)), area);
        return;
    };
    if st.detail
        && let Some(v) = d.views.get(st.sel)
    {
        draw_match(f, v, Some(&d.stats), area, "Match");
        return;
    }
    let [summary, list] = Layout::vertical([Constraint::Length(4), Constraint::Min(4)]).areas(area);
    let s = &d.stats;
    let mut lines = vec![Line::from(vec![
        Span::raw(format!("Games {} · Win rate {} · K/D {} · HS {} · ", s.games, matches::fmt_opt_pct(s.win_rate()), s.kd().map(|k| format!("{k:.2}")).unwrap_or_else(|| "N/A".into()), matches::fmt_opt_pct(s.hs_pct()))),
        Span::raw("Rank "),
        Span::styled(matches::fmt_tier(s.current_tier), Style::new().fg(s.current_tier.map(rank::color).unwrap_or(MUTED))),
        Span::raw(" · Peak recorded "),
        Span::styled(matches::fmt_tier(s.peak_tier), Style::new().fg(s.peak_tier.map(rank::color).unwrap_or(MUTED))),
        Span::raw(format!(" · Level {}", s.level.map(|l| l.to_string()).unwrap_or_else(|| "N/A".into()))),
    ])];
    lines.push(Line::from(Span::styled("RR and skins are not exposed by supported Riot APIs (N/A). Peak = highest rank in matches stored by VALSYNC.", Style::new().fg(MUTED))));
    if let Some(e) = &st.error {
        lines.push(Line::from(Span::styled(e.clone(), Style::new().fg(Color::Yellow))));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }).block(Block::default()), summary);
    let rows: Vec<Row> = d
        .views
        .iter()
        .map(|v| {
            let me = v.owner_line.as_ref();
            Row::new(vec![
                Cell::from(matches::fmt_time(v.started_ms)),
                Cell::from(v.map.clone()),
                Cell::from(v.mode.clone()),
                Cell::from(v.result_label()).style(match v.owner_won {
                    Some(true) => Style::new().fg(Color::Green),
                    Some(false) => Style::new().fg(Color::Red),
                    None => Style::new(),
                }),
                Cell::from(v.score_line()),
                Cell::from(me.map(|m| m.agent.clone()).unwrap_or_default()),
                Cell::from(me.map(|m| format!("{}/{}/{}", m.kills, m.deaths, m.assists)).unwrap_or_default()),
                Cell::from(matches::fmt_opt_pct(me.and_then(|m| m.hs_pct))),
                tier_cell(me.and_then(|m| m.tier)),
            ])
        })
        .collect();
    if rows.is_empty() {
        f.render_widget(Paragraph::new("No matches stored yet. Press r to fetch (requires API key + RSO consent).").block(block(title)), list);
        return;
    }
    let t = Table::new(
        rows,
        [Constraint::Length(17), Constraint::Length(10), Constraint::Length(16), Constraint::Length(5), Constraint::Length(8), Constraint::Length(10), Constraint::Length(9), Constraint::Length(6), Constraint::Length(12)],
    )
    .header(header(&["Date", "Map", "Mode", "", "Score", "Agent", "K/D/A", "HS%", "Rank"]))
    .row_highlight_style(sel_style())
    .block(block(title));
    f.render_stateful_widget(t, list, &mut table_state(st.sel));
}

fn draw_backups(f: &mut Frame, app: &App, st: &BackupsState, area: Rect) {
    let title = if st.all { "Backup History — all accounts".to_string() } else { format!("Backup History — {}", app.active_label()) };
    if st.rows.is_empty() {
        f.render_widget(Paragraph::new("No backups yet. Press m to create a manual backup; automatic backups are taken before every transfer.").block(block(title)), area);
        return;
    }
    let rows: Vec<Row> = st
        .rows
        .iter()
        .map(|b| {
            let (kind, c) = match b.kind {
                BackupKind::Original => ("original", Color::Cyan),
                BackupKind::Manual => ("manual", Color::Green),
                BackupKind::PreChange => ("pre-change", Color::White),
                BackupKind::PostChange => ("post-change", MUTED),
            };
            Row::new(vec![
                Cell::from(format!("#{}", b.id)),
                Cell::from(kind).style(Style::new().fg(c)),
                Cell::from(crate::term::local_time(&b.created_at)),
                Cell::from(b.label.clone()),
                Cell::from(format!("{} · {}", b.file_count, crate::term::human_bytes(b.total_bytes))),
                Cell::from(if b.corrupted { "CORRUPTED" } else { "ok" }).style(Style::new().fg(if b.corrupted { Color::Red } else { Color::Green })),
                Cell::from(if st.all { app.engine.account_label(&b.account_puuid) } else { String::new() }),
            ])
        })
        .collect();
    let t = Table::new(rows, [Constraint::Length(6), Constraint::Length(12), Constraint::Length(20), Constraint::Min(24), Constraint::Length(14), Constraint::Length(10), Constraint::Length(18)])
        .header(header(&["ID", "Kind", "Created", "Label", "Files", "Integrity", if st.all { "Account" } else { "" }]))
        .row_highlight_style(sel_style())
        .block(block(title));
    f.render_stateful_widget(t, area, &mut table_state(st.sel));
}

fn draw_settings(f: &mut Frame, app: &App, sel: usize, area: Rect) {
    let rows: Vec<Row> = AppConfig::KEYS
        .iter()
        .map(|k| Row::new(vec![Cell::from(k.to_string()), Cell::from(app.engine.config.get_display(k)), Cell::from(AppConfig::describe(k)).style(Style::new().fg(MUTED))]))
        .collect();
    let [tarea, note] = Layout::vertical([Constraint::Min(5), Constraint::Length(3)]).areas(area);
    let t = Table::new(rows, [Constraint::Length(24), Constraint::Length(28), Constraint::Min(20)])
        .header(header(&["Setting", "Value", "Description"]))
        .row_highlight_style(sel_style())
        .block(block("Application Settings"));
    f.render_stateful_widget(t, tarea, &mut table_state(sel));
    f.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(format!("Config file: {}", app.engine.paths.config_path().display()), Style::new().fg(MUTED))),
            Line::from(Span::styled("VALORANT data requires a Riot production API key; match data also requires RSO consent.", Style::new().fg(MUTED))),
        ]),
        note,
    );
}
