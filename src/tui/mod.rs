//! Interactive terminal interface (Ratatui + Crossterm).
//!
//! The event loop blocks on keyboard input with a timeout, so an idle VALSYNC
//! uses effectively no CPU: it wakes about once per second, refreshes the cheap
//! status panel every few seconds and only redraws when something changed.
//! Network calls run on short-lived worker threads and never block input.

mod ui;

use crate::appconfig::AppConfig;
use crate::catalog::{Category, Scope};
use crate::db::{AccountRow, BackupKind, BackupRow, Db, OpRow, ProfileRow};
use crate::engine::{Change, Engine, LoadOptions, LoadPlan, Target};
use crate::profile::{self, Profile};
use crate::service::{self, HistoryData, LiveData, StatusInfo};
use anyhow::{Context, Result};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

pub const ACCENT: Color = Color::Rgb(255, 70, 85);
pub const TEAL: Color = Color::Rgb(0, 190, 170);
pub const MUTED: Color = Color::Rgb(120, 125, 135);

pub const MENU: [(&str, &str); 10] = [
    ("1", "Save Current Settings"),
    ("2", "Load Saved Settings"),
    ("3", "Restore Previous Settings"),
    ("4", "Manage Settings Profiles"),
    ("5", "Manage Riot Accounts"),
    ("6", "Live Match"),
    ("7", "Match History"),
    ("8", "Backup History"),
    ("9", "Application Settings"),
    ("0", "Exit"),
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Ok,
    Warn,
    Error,
}

#[derive(Clone)]
pub enum RestoreItem {
    Undo(OpRow),
    Redo(OpRow),
    Original(BackupRow),
    Backup(BackupRow),
}

impl RestoreItem {
    fn backup_id(&self) -> i64 {
        match self {
            RestoreItem::Undo(o) => o.pre_backup_id,
            RestoreItem::Redo(o) => o.post_backup_id.unwrap_or(o.pre_backup_id),
            RestoreItem::Original(b) | RestoreItem::Backup(b) => b.id,
        }
    }
}

#[derive(Clone)]
pub enum Pending {
    SaveProfile { name: String },
    ApplyLoad,
    Restore(RestoreItem),
    DeleteProfile(String),
    DeleteBackup(i64),
    ForgetAccount(String),
    Quit,
}

#[derive(Clone)]
pub enum InputKind {
    SaveName,
    Rename(String),
    Duplicate(String),
    Import,
    Export(String),
    BackupLabel,
    Preferred(String),
    Setting(String),
}

pub enum Modal {
    Confirm { title: String, body: Vec<Line<'static>>, action: Pending },
    Input { title: String, prompt: String, value: String, kind: InputKind, mask: bool },
}

pub struct TextView {
    pub title: String,
    pub lines: Vec<Line<'static>>,
    pub scroll: u16,
    pub back: Option<Box<Screen>>,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum LoadStage {
    Profiles,
    Categories,
    Preview,
}

pub struct LoadState {
    pub stage: LoadStage,
    pub profiles: Vec<ProfileRow>,
    pub sel: usize,
    /// (category, local, cloud, selected)
    pub cats: Vec<(Category, usize, usize, bool)>,
    pub cat_sel: usize,
    pub plan: Option<LoadPlan>,
    pub scroll: usize,
    pub target: Option<Target>,
    pub guessed: bool,
}

pub struct RestoreState {
    pub items: Vec<RestoreItem>,
    pub sel: usize,
    pub preview: Option<(RestoreItem, Vec<Change>)>,
    pub scroll: usize,
}

pub struct ProfilesState {
    pub rows: Vec<ProfileRow>,
    pub sel: usize,
    pub mark: Option<String>,
}

pub struct AccountsState {
    pub rows: Vec<(AccountRow, bool, bool)>,
    pub sel: usize,
}

pub struct LiveState {
    pub data: Option<LiveData>,
    pub loading: bool,
    pub last_fetch: Option<Instant>,
    pub error: Option<String>,
}

pub struct HistoryState {
    pub data: Option<HistoryData>,
    pub loading: bool,
    pub sel: usize,
    pub detail: bool,
    pub error: Option<String>,
}

pub struct BackupsState {
    pub rows: Vec<BackupRow>,
    pub sel: usize,
    pub all: bool,
}

#[allow(clippy::large_enum_variant)]
pub enum Screen {
    Main,
    Text(TextView),
    Load(LoadState),
    Restore(RestoreState),
    Profiles(ProfilesState),
    Accounts(AccountsState),
    Live(LiveState),
    History(HistoryState),
    Backups(BackupsState),
    Settings { sel: usize },
}

enum Job {
    History { limit: usize },
    Live { cached: Option<crate::matches::LiveView> },
}

enum JobResult {
    History(Box<Result<HistoryData>>),
    Live(Box<Result<LiveData>>),
    Done,
}

pub struct App {
    pub engine: Engine,
    pub screen: Screen,
    pub status: StatusInfo,
    pub menu_sel: usize,
    pub modal: Option<Modal>,
    pub toast: Option<(Level, String, Instant)>,
    pub busy: Option<(String, Instant)>,
    pub banner: Option<String>,
    last_status: Instant,
    tx: Sender<JobResult>,
    rx: Receiver<JobResult>,
    jobs_running: usize,
    last_riot_check: Instant,
    pub quit: bool,
    pub tick: u64,
}

pub fn run(engine: Engine, start_live: bool) -> Result<()> {
    let mut terminal = ratatui::init();
    let res = (|| -> Result<()> {
        let mut app = App::new(engine)?;
        if start_live {
            app.open(6);
        }
        let mut dirty = true;
        while !app.quit {
            if dirty {
                terminal.draw(|f| ui::draw(f, &app))?;
                dirty = false;
            }
            let timeout = if app.busy.is_some() || app.jobs_running > 0 { Duration::from_millis(120) } else { Duration::from_millis(1000) };
            if event::poll(timeout)? {
                match event::read()? {
                    Event::Key(k) if k.kind == KeyEventKind::Press => {
                        app.on_key(k);
                        dirty = true;
                    }
                    Event::Resize(_, _) => dirty = true,
                    _ => {}
                }
            }
            if app.on_tick() {
                dirty = true;
            }
        }
        Ok(())
    })();
    ratatui::restore();
    res
}

fn spawn_job(job: Job, logs: PathBuf, db_path: PathBuf, cfg: AppConfig, tx: Sender<JobResult>) {
    std::thread::spawn(move || {
        let rt = service::runtime();
        let result = (|| -> Result<JobResult> {
            let db = Db::open(&db_path)?;
            let session = rt.block_on(service::connect(&logs, &db, &cfg))?;
            Ok(match job {
                Job::History { limit } => JobResult::History(Box::new(rt.block_on(service::history(&session, &db, &cfg, limit)))),
                Job::Live { cached } => JobResult::Live(Box::new(rt.block_on(service::live(&session, &db, &cfg, cached.as_ref())))),
            })
        })();
        let _ = tx.send(match result {
            Ok(r) => r,
            Err(e) => JobResult::Live(Box::new(Err(e))),
        });
        let _ = tx.send(JobResult::Done);
    });
}

fn line(s: impl Into<String>) -> Line<'static> {
    Line::from(s.into())
}

fn styled(s: impl Into<String>, st: Style) -> Line<'static> {
    Line::from(Span::styled(s.into(), st))
}

impl App {
    fn new(engine: Engine) -> Result<App> {
        let (tx, rx) = channel();
        let status = service::status(&engine)?;
        let mut app = App {
            engine,
            screen: Screen::Main,
            status,
            menu_sel: 0,
            modal: None,
            toast: None,
            busy: None,
            banner: None,
            last_status: Instant::now(),
            tx,
            rx,
            jobs_running: 0,
            last_riot_check: Instant::now(),
            quit: false,
            tick: 0,
        };
        let r = app.engine.startup_recovery.clone();
        if !r.recovered_ops.is_empty() {
            app.banner = Some(format!("Recovered from an interrupted operation: previous settings restored (op {:?}).", r.recovered_ops));
        } else if r.deferred {
            app.banner = Some("An interrupted operation needs recovery. Close VALORANT and restart VALSYNC.".into());
        } else if !r.failed_ops.is_empty() {
            app.banner = Some(format!("Recovery failed for {} operation(s); run `valsync doctor`.", r.failed_ops.len()));
        }
        Ok(app)
    }

    fn start_job(&mut self, job: Job, show_busy: bool) {
        self.jobs_running += 1;
        if show_busy {
            self.busy = Some(("Talking to Riot".into(), Instant::now()));
        }
        spawn_job(job, self.engine.paths.valorant_logs.clone(), self.engine.paths.db_path(), self.engine.config.clone(), self.tx.clone());
    }

    pub fn notify(&mut self, level: Level, msg: impl Into<String>) {
        self.toast = Some((level, msg.into(), Instant::now()));
    }

    fn refresh_status(&mut self) {
        match service::status(&self.engine) {
            Ok(s) => {
                if let Some(sw) = &s.switch {
                    let from = sw.from.as_deref().map(|p| self.engine.account_label(p)).unwrap_or_else(|| "—".into());
                    self.banner = Some(format!("Account switch detected: {from} → {}", self.engine.account_label(&sw.to)));
                }
                self.status = s;
            }
            Err(e) => self.notify(Level::Error, format!("{e:#}")),
        }
        self.last_status = Instant::now();
    }

    /// Returns true when a redraw is needed.
    fn on_tick(&mut self) -> bool {
        self.tick = self.tick.wrapping_add(1);
        let mut dirty = self.busy.is_some() || self.jobs_running > 0;
        while let Ok(r) = self.rx.try_recv() {
            dirty = true;
            match r {
                JobResult::Done => {
                    self.jobs_running = self.jobs_running.saturating_sub(1);
                    if self.jobs_running == 0 {
                        self.busy = None;
                    }
                    self.refresh_status();
                    self.reload_screen_data();
                }
                JobResult::Live(result) => {
                    if let Screen::Live(st) = &mut self.screen {
                        st.loading = false;
                        st.last_fetch = Some(Instant::now());
                        match *result {
                            Ok(d) => {
                                st.error = d.warning.clone();
                                st.data = Some(d);
                            }
                            Err(e) => st.error = Some(format!("{e:#}")),
                        }
                    } else if let (Screen::History(st), Err(e)) = (&mut self.screen, *result) {
                        st.loading = false;
                        st.error = Some(format!("{e:#}"));
                    }
                }
                JobResult::History(result) => {
                    if let Screen::History(st) = &mut self.screen {
                        st.loading = false;
                        match *result {
                            Ok(d) => {
                                st.error = d.warning.clone();
                                st.data = Some(d);
                            }
                            Err(e) => st.error = Some(format!("{e:#}")),
                        }
                    }
                }
            }
        }
        if self.toast.as_ref().map(|t| t.2.elapsed() > Duration::from_secs(8)).unwrap_or(false) {
            self.toast = None;
            dirty = true;
        }
        if self.last_status.elapsed() > Duration::from_secs(4) && self.modal.is_none() {
            let before = (self.status.processes, self.status.active.as_ref().map(|a| a.0.puuid.clone()));
            self.refresh_status();
            let after = (self.status.processes, self.status.active.as_ref().map(|a| a.0.puuid.clone()));
            if before != after || self.status.switch.is_some() {
                dirty = true;
            }
        }
        // Follow Riot Client sign-ins / account switches.
        if self.last_riot_check.elapsed() > Duration::from_secs(10) && self.modal.is_none() && self.jobs_running == 0 {
            self.last_riot_check = Instant::now();
            if self.engine.refresh_riot() {
                self.refresh_status();
                dirty = true;
            }
        }
        // Live screen: refresh while in a match (configurable), slowly otherwise.
        let in_game = self.status.processes.valorant;
        let poll = Duration::from_secs(if in_game { self.engine.config.poll_secs() } else { 60 });
        let signed_in = self.status.riot_signed_in;
        let mut start = None;
        if let Screen::Live(st) = &mut self.screen
            && !st.loading
            && signed_in
            && st.last_fetch.map(|t| t.elapsed() > poll).unwrap_or(true)
        {
            st.loading = true;
            start = Some(st.data.as_ref().and_then(|d| d.view.clone()));
        }
        if let Some(cached) = start {
            self.start_job(Job::Live { cached }, false);
        }
        dirty
    }

    fn reload_screen_data(&mut self) {
        let fresh_accounts = if matches!(self.screen, Screen::Accounts(_)) { Some(self.accounts_state()) } else { None };
        let active = self.status.active.as_ref().map(|a| a.0.puuid.clone());
        match &mut self.screen {
            Screen::Accounts(st) => {
                let sel = st.sel;
                if let Some(fresh) = fresh_accounts {
                    *st = fresh;
                }
                st.sel = sel.min(st.rows.len().saturating_sub(1));
            }
            Screen::Profiles(st) => {
                let rows = self.engine.db.profiles().unwrap_or_default();
                st.sel = st.sel.min(rows.len().saturating_sub(1));
                st.rows = rows;
            }
            Screen::Backups(st) => {
                let account = if st.all { None } else { active.clone() };
                st.rows = self.engine.db.backups(account.as_deref()).unwrap_or_default();
                st.sel = st.sel.min(st.rows.len().saturating_sub(1));
            }
            _ => {}
        }
    }

    fn accounts_state(&self) -> AccountsState {
        let det = self.engine.detection();
        let active = det.active.as_ref().map(|a| a.0.puuid.clone());
        let rows = self
            .engine
            .db
            .accounts()
            .unwrap_or_default()
            .into_iter()
            .map(|a| {
                let local = det.accounts.iter().any(|l| l.puuid == a.puuid);
                let is_active = active.as_deref() == Some(a.puuid.as_str());
                (a, local, is_active)
            })
            .collect();
        AccountsState { rows, sel: 0 }
    }

    fn active_puuid(&self) -> Option<String> {
        self.status.active.as_ref().map(|a| a.0.puuid.clone())
    }

    fn require_active(&mut self) -> Option<(Target, bool)> {
        match self.engine.active_target() {
            Ok((t, m)) => Some((t, m.is_confident())),
            Err(e) => {
                self.notify(Level::Error, format!("{e:#}"));
                None
            }
        }
    }

    // ------------------------------------------------------------ navigation
    fn open(&mut self, item: usize) {
        self.toast = None;
        match item {
            1 => {
                if self.require_active().is_some() {
                    let default = self.status.last_saved_profile.clone().unwrap_or_else(|| "Main".into());
                    self.modal = Some(Modal::Input {
                        title: "Save Current Settings".into(),
                        prompt: format!("Profile name for {}:", self.active_label()),
                        value: default,
                        kind: InputKind::SaveName,
                        mask: false,
                    });
                }
            }
            2 => {
                let profiles = self.engine.db.profiles().unwrap_or_default();
                if profiles.is_empty() {
                    self.notify(Level::Warn, "No profiles yet. Save your current settings first ([1]).");
                    return;
                }
                let Some((t, confident)) = self.require_active() else { return };
                // Pre-select the account's preferred profile.
                let pref = self.status.active.as_ref().and_then(|a| a.0.preferred_profile.clone());
                let sel = pref.and_then(|p| profiles.iter().position(|r| r.name.eq_ignore_ascii_case(&p))).unwrap_or(0);
                self.screen = Screen::Load(LoadState {
                    stage: LoadStage::Profiles,
                    profiles,
                    sel,
                    cats: Vec::new(),
                    cat_sel: 0,
                    plan: None,
                    scroll: 0,
                    target: Some(t),
                    guessed: !confident,
                });
            }
            3 => {
                let Some(p) = self.active_puuid() else {
                    self.notify(Level::Error, "No active account detected");
                    return;
                };
                self.screen = Screen::Restore(self.restore_state(&p));
            }
            4 => self.screen = Screen::Profiles(ProfilesState { rows: self.engine.db.profiles().unwrap_or_default(), sel: 0, mark: None }),
            5 => self.screen = Screen::Accounts(self.accounts_state()),
            6 => {
                let mut st = LiveState { data: None, loading: false, last_fetch: None, error: None };
                if !self.status.riot_signed_in {
                    st.error = Some("Open the Riot Client and sign in to see your live match.".into());
                }
                self.screen = Screen::Live(st);
            }
            7 => {
                let Some(p) = self.active_puuid() else {
                    self.notify(Level::Error, "Open the Riot Client and sign in first");
                    return;
                };
                let mut st = HistoryState { data: None, loading: false, sel: 0, detail: false, error: None };
                match service::history_offline(&self.engine.db, &self.engine.config, &p, 20) {
                    Ok(d) => st.data = Some(d),
                    Err(e) => st.error = Some(format!("{e:#}")),
                }
                if self.status.riot_signed_in {
                    st.loading = true;
                    self.start_job(Job::History { limit: 20 }, false);
                } else {
                    st.error = Some("Showing saved matches. Open the Riot Client and sign in to load new ones.".into());
                }
                self.screen = Screen::History(st);
            }
            8 => {
                let account = self.active_puuid();
                let rows = self.engine.db.backups(account.as_deref()).unwrap_or_default();
                self.screen = Screen::Backups(BackupsState { rows, sel: 0, all: account.is_none() });
            }
            9 => self.screen = Screen::Settings { sel: 0 },
            0 => self.quit = true,
            _ => {}
        }
    }

    fn active_label(&self) -> String {
        self.status.active.as_ref().map(|a| a.0.display()).unwrap_or_else(|| "—".into())
    }

    fn restore_state(&self, puuid: &str) -> RestoreState {
        let mut items = Vec::new();
        if let Ok(Some(op)) = self.engine.undo_candidate(puuid) {
            items.push(RestoreItem::Undo(op));
        }
        if let Ok(Some(op)) = self.engine.redo_candidate(puuid) {
            items.push(RestoreItem::Redo(op));
        }
        if let Ok(Some(b)) = self.engine.db.original_backup(puuid) {
            items.push(RestoreItem::Original(b));
        }
        for b in self.engine.db.backups(Some(puuid)).unwrap_or_default() {
            if b.kind != BackupKind::Original {
                items.push(RestoreItem::Backup(b));
            }
        }
        RestoreState { items, sel: 0, preview: None, scroll: 0 }
    }

    fn back(&mut self) {
        let screen = std::mem::replace(&mut self.screen, Screen::Main);
        self.screen = match screen {
            Screen::Text(tv) => tv.back.map(|b| *b).unwrap_or(Screen::Main),
            Screen::Load(mut st) => match st.stage {
                LoadStage::Preview => {
                    st.stage = LoadStage::Profiles;
                    st.plan = None;
                    Screen::Load(st)
                }
                LoadStage::Categories => {
                    st.stage = LoadStage::Preview;
                    Screen::Load(st)
                }
                LoadStage::Profiles => Screen::Main,
            },
            Screen::Restore(mut st) if st.preview.is_some() => {
                st.preview = None;
                Screen::Restore(st)
            }
            Screen::History(mut st) if st.detail => {
                st.detail = false;
                Screen::History(st)
            }
            _ => Screen::Main,
        };
        if matches!(self.screen, Screen::Main) {
            self.refresh_status();
        }
    }

    // ------------------------------------------------------------ keys
    fn on_key(&mut self, k: KeyEvent) {
        if k.modifiers.contains(KeyModifiers::CONTROL) && matches!(k.code, KeyCode::Char('c')) {
            self.quit = true;
            return;
        }
        if self.modal.is_some() {
            self.on_modal_key(k);
            return;
        }
        if self.busy.is_some() && self.jobs_running == 0 {
            self.busy = None;
        }
        let code = k.code;
        match &mut self.screen {
            Screen::Main => match code {
                KeyCode::Up | KeyCode::Char('k') => self.menu_sel = (self.menu_sel + MENU.len() - 1) % MENU.len(),
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => self.menu_sel = (self.menu_sel + 1) % MENU.len(),
                KeyCode::Enter => {
                    let n: usize = MENU[self.menu_sel].0.parse().unwrap_or(0);
                    self.open(n);
                }
                KeyCode::Char(c) if c.is_ascii_digit() => {
                    let n = c.to_digit(10).unwrap_or(0) as usize;
                    self.menu_sel = MENU.iter().position(|m| m.0 == c.to_string()).unwrap_or(0);
                    self.open(n);
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.modal = Some(Modal::Confirm { title: "Exit VALSYNC".into(), body: vec![line("Exit VALSYNC?")], action: Pending::Quit })
                }
                KeyCode::Char('x') => self.banner = None,
                _ => {}
            },
            Screen::Text(tv) => match code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Backspace => self.back(),
                KeyCode::Down | KeyCode::Char('j') => tv.scroll = tv.scroll.saturating_add(1),
                KeyCode::Up | KeyCode::Char('k') => tv.scroll = tv.scroll.saturating_sub(1),
                KeyCode::PageDown => tv.scroll = tv.scroll.saturating_add(15),
                KeyCode::PageUp => tv.scroll = tv.scroll.saturating_sub(15),
                _ => {}
            },
            Screen::Load(_) => self.on_load_key(code),
            Screen::Restore(_) => self.on_restore_key(code),
            Screen::Profiles(_) => self.on_profiles_key(code),
            Screen::Accounts(_) => self.on_accounts_key(code),
            Screen::Live(st) => match code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Backspace => self.back(),
                KeyCode::Char('r') => st.last_fetch = None,
                _ => {}
            },
            Screen::History(st) => {
                let n = st.data.as_ref().map(|d| d.views.len()).unwrap_or(0);
                match code {
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Backspace => self.back(),
                    KeyCode::Up | KeyCode::Char('k') if !st.detail => st.sel = st.sel.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') if !st.detail => st.sel = (st.sel + 1).min(n.saturating_sub(1)),
                    KeyCode::Enter if n > 0 => st.detail = !st.detail,
                    KeyCode::Char('r') => {
                        if self.status.riot_signed_in {
                            if let Screen::History(st) = &mut self.screen {
                                st.loading = true;
                            }
                            self.start_job(Job::History { limit: 20 }, true);
                        } else {
                            self.notify(Level::Warn, "Open the Riot Client and sign in to load new matches");
                        }
                    }
                    _ => {}
                }
            }
            Screen::Backups(_) => self.on_backups_key(code),
            Screen::Settings { .. } => self.on_settings_key(code),
        }
    }

    fn on_modal_key(&mut self, k: KeyEvent) {
        let Some(modal) = self.modal.take() else { return };
        match modal {
            Modal::Confirm { title, body, action } => match k.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => self.perform(action),
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => self.notify(Level::Info, "Cancelled"),
                _ => self.modal = Some(Modal::Confirm { title, body, action }),
            },
            Modal::Input { title, prompt, mut value, kind, mask } => match k.code {
                KeyCode::Esc => self.notify(Level::Info, "Cancelled"),
                KeyCode::Enter => self.submit_input(kind, value),
                KeyCode::Backspace => {
                    value.pop();
                    self.modal = Some(Modal::Input { title, prompt, value, kind, mask });
                }
                KeyCode::Char(c) => {
                    if value.chars().count() < 512 && !c.is_control() {
                        value.push(c);
                    }
                    self.modal = Some(Modal::Input { title, prompt, value, kind, mask });
                }
                _ => self.modal = Some(Modal::Input { title, prompt, value, kind, mask }),
            },
        }
    }

    fn show_text(&mut self, title: impl Into<String>, lines: Vec<Line<'static>>, back_to_current: bool) {
        let back = if back_to_current { Some(Box::new(std::mem::replace(&mut self.screen, Screen::Main))) } else { None };
        self.screen = Screen::Text(TextView { title: title.into(), lines, scroll: 0, back });
    }

    fn submit_input(&mut self, kind: InputKind, value: String) {
        let value = value.trim().to_string();
        let res: Result<()> = (|| {
            match kind {
                InputKind::SaveName => {
                    let name = profile::validate_name(&value)?;
                    if self.engine.db.profile(&name)?.is_some() {
                        self.modal = Some(Modal::Confirm {
                            title: "Overwrite profile".into(),
                            body: vec![line(format!("A profile named '{name}' already exists.")), line("Overwrite it with the current settings?")],
                            action: Pending::SaveProfile { name },
                        });
                    } else {
                        self.perform(Pending::SaveProfile { name });
                    }
                }
                InputKind::Rename(old) => {
                    self.engine.db.rename_profile(&old, &value)?;
                    self.notify(Level::Ok, format!("Renamed '{old}' → '{value}'"));
                }
                InputKind::Duplicate(src) => {
                    let new = profile::validate_name(&value)?;
                    let mut p = self.engine.db.profile(&src)?.context("profile not found")?.profile;
                    p.name = new.clone();
                    p.created_at = crate::db::now();
                    self.engine.db.save_profile(&p, "duplicate", false)?;
                    self.notify(Level::Ok, format!("Duplicated as '{new}'"));
                }
                InputKind::Import => {
                    let path = PathBuf::from(value.trim_matches('"'));
                    let text = std::fs::read_to_string(&path).with_context(|| format!("cannot read {}", path.display()))?;
                    let (mut p, removed) = Profile::from_file_json(&text)?;
                    p.name = profile::validate_name(&p.name)?;
                    let mut name = p.name.clone();
                    let mut n = 2;
                    while self.engine.db.profile(&name)?.is_some() {
                        name = format!("{} ({n})", p.name);
                        n += 1;
                    }
                    p.name = name.clone();
                    self.engine.db.save_profile(&p, "imported", false)?;
                    let extra = if removed > 0 { format!(" ({removed} unsupported entries dropped)") } else { String::new() };
                    self.notify(Level::Ok, format!("Imported '{name}' with {} settings{extra}", p.settings.len()));
                }
                InputKind::Export(name) => {
                    let p = self.engine.db.profile(&name)?.context("profile not found")?;
                    let path = PathBuf::from(value.trim_matches('"'));
                    crate::txn::atomic_write(&path, p.profile.to_file_json()?.as_bytes())?;
                    self.notify(Level::Ok, format!("Exported to {}", path.display()));
                }
                InputKind::BackupLabel => {
                    let (t, _) = self.engine.active_target()?;
                    let id = self.engine.create_manual_backup(&t, &value)?;
                    self.notify(Level::Ok, format!("Created manual backup #{id}"));
                }
                InputKind::Preferred(puuid) => {
                    if value.is_empty() {
                        self.engine.db.set_preferred_profile(&puuid, None)?;
                        self.notify(Level::Ok, "Preferred profile cleared");
                    } else {
                        let p = self.engine.db.profile(&value)?.with_context(|| format!("profile '{value}' not found"))?;
                        self.engine.db.set_preferred_profile(&puuid, Some(&p.name))?;
                        self.notify(Level::Ok, format!("Preferred profile set to '{}'", p.name));
                    }
                }
                InputKind::Setting(key) => {
                    self.engine.config.set_field(&key, &value)?;
                    self.engine.save_config()?;
                    self.notify(Level::Ok, format!("{key} updated"));
                }
            }
            Ok(())
        })();
        if let Err(e) = res {
            self.notify(Level::Error, format!("{e:#}"));
        }
        self.reload_screen_data();
    }

    fn perform(&mut self, action: Pending) {
        let res: Result<()> = (|| {
            match action {
                Pending::Quit => self.quit = true,
                Pending::SaveProfile { name } => {
                    let (t, _) = self.engine.active_target()?;
                    let out = self.engine.save_profile(&t, &name, true, None, None, Vec::new())?;
                    let mut lines = vec![
                        styled(format!("✔ Saved '{}' from {}", out.profile.name, self.engine.account_label(&t.puuid)), Style::new().fg(Color::Green).add_modifier(Modifier::BOLD)),
                        line(""),
                    ];
                    for (k, v) in out.profile.cloud.as_ref().map(crate::cloud::highlights).unwrap_or_default() {
                        lines.push(Line::from(vec![Span::styled(format!("  {k:<20}"), Style::new().fg(MUTED)), Span::styled(v, Style::new().add_modifier(Modifier::BOLD))]));
                    }
                    if !out.cloud {
                        lines.push(styled(
                            "  Only video settings were saved — the Riot Client isn't signed in to this account. Sign in and save again to include sensitivity, crosshair and keybinds.",
                            Style::new().fg(Color::Yellow),
                        ));
                    }
                    lines.push(line(""));
                    lines.extend(category_lines(&out.profile));
                    self.status.last_saved_profile = Some(name);
                    self.show_text("Save Current Settings", lines, false);
                }
                Pending::ApplyLoad => self.apply_load()?,
                Pending::Restore(item) => self.apply_restore(item)?,
                Pending::DeleteProfile(n) => {
                    self.engine.db.delete_profile(&n)?;
                    self.notify(Level::Ok, format!("Deleted profile '{n}'"));
                }
                Pending::DeleteBackup(id) => {
                    self.engine.db.delete_backup(id)?;
                    self.notify(Level::Ok, format!("Deleted backup #{id}"));
                }
                Pending::ForgetAccount(p) => {
                    self.engine.db.forget_account(&p)?;
                    self.notify(Level::Ok, "Account forgotten (game files and backups kept)");
                }
            }
            Ok(())
        })();
        if let Err(e) = res {
            self.notify(Level::Error, format!("{e:#}"));
        }
        self.refresh_status();
        self.reload_screen_data();
    }

    // ------------------------------------------------------------ load flow
    fn on_load_key(&mut self, code: KeyCode) {
        let Screen::Load(st) = &mut self.screen else { return };
        match st.stage {
            LoadStage::Profiles => match code {
                KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('q') => self.back(),
                KeyCode::Up | KeyCode::Char('k') => st.sel = st.sel.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => st.sel = (st.sel + 1).min(st.profiles.len().saturating_sub(1)),
                KeyCode::Enter => {
                    let p = &st.profiles[st.sel].profile;
                    st.cats = p.categories().iter().map(|(c, (l, cl))| (*c, *l, *cl, true)).collect();
                    st.cat_sel = 0;
                    self.replan();
                }
                _ => {}
            },
            LoadStage::Categories => match code {
                KeyCode::Esc | KeyCode::Backspace => self.back(),
                KeyCode::Up | KeyCode::Char('k') => st.cat_sel = st.cat_sel.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => st.cat_sel = (st.cat_sel + 1).min(st.cats.len().saturating_sub(1)),
                KeyCode::Char(' ') => {
                    if let Some(c) = st.cats.get_mut(st.cat_sel) {
                        c.3 = !c.3;
                    }
                }
                KeyCode::Char('a') => {
                    let all = st.cats.iter().all(|c| c.3);
                    for c in &mut st.cats {
                        c.3 = !all;
                    }
                }
                KeyCode::Enter => {
                    if !st.cats.iter().any(|c| c.3) {
                        self.notify(Level::Warn, "Select at least one category (Space)");
                        return;
                    }
                    self.replan();
                }
                _ => {}
            },
            LoadStage::Preview => match code {
                KeyCode::Esc | KeyCode::Backspace => self.back(),
                KeyCode::Char('c') => st.stage = LoadStage::Categories,
                KeyCode::Down | KeyCode::Char('j') => st.scroll = st.scroll.saturating_add(1),
                KeyCode::Up | KeyCode::Char('k') => st.scroll = st.scroll.saturating_sub(1),
                KeyCode::Enter | KeyCode::Char('y') => {
                    let Some(plan) = &st.plan else { return };
                    if plan.changes.is_empty() {
                        self.notify(Level::Ok, "Nothing to change: the account already matches this profile");
                        return;
                    }
                    let name = st.profiles[st.sel].name.clone();
                    let n = plan.changes.len();
                    let guessed = st.guessed;
                    let target = st.target.as_ref().map(|t| self.engine.account_label(&t.puuid)).unwrap_or_default();
                    let mut body = vec![
                        line(format!("Apply {n} change(s) from '{name}' to {target}?")),
                        line("Current settings are backed up automatically first."),
                    ];
                    if guessed {
                        body.push(styled("Warning: the active account was guessed — make sure this is the right account.", Style::new().fg(Color::Yellow)));
                    }
                    if self.status.processes.valorant {
                        body.push(styled("VALORANT is running: close it first, or the transfer will be refused.", Style::new().fg(Color::Yellow)));
                    }
                    self.modal = Some(Modal::Confirm { title: "Load Saved Settings".into(), body, action: Pending::ApplyLoad });
                }
                _ => {}
            },
        }
    }

    fn load_options(st: &LoadState) -> LoadOptions {
        let all = st.cats.iter().all(|c| c.3);
        let categories = if all { None } else { Some(st.cats.iter().filter(|c| c.3).map(|c| c.0).collect::<BTreeSet<Category>>()) };
        LoadOptions { categories, include_cloud: false }
    }

    fn replan(&mut self) {
        let Screen::Load(st) = &mut self.screen else { return };
        let Some(t) = st.target.clone() else { return };
        let opts = Self::load_options(st);
        match self.engine.plan_load(&st.profiles[st.sel].profile, &t, &opts) {
            Ok(plan) => {
                st.plan = Some(plan);
                st.stage = LoadStage::Preview;
                st.scroll = 0;
            }
            Err(e) => self.notify(Level::Error, format!("{e:#}")),
        }
    }

    fn apply_load(&mut self) -> Result<()> {
        let Screen::Load(st) = &self.screen else { return Ok(()) };
        let profile = st.profiles[st.sel].profile.clone();
        let t = st.target.clone().context("no target")?;
        let opts = Self::load_options(st);
        self.busy = Some(("Applying settings".into(), Instant::now()));
        let out = self.engine.apply_load(&profile, &t, &opts);
        self.busy = None;
        let out = out?;
        let mut lines = Vec::new();
        match &out.txn {
            Some(txn) => {
                lines.push(styled(
                    format!("✔ Applied and verified {} change(s) to {}", out.plan.changes.len(), self.engine.account_label(&t.puuid)),
                    Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
                ));
                lines.push(line(format!("  Previous settings saved as backup #{} — use Restore Previous Settings or `valsync undo` to revert.", txn.pre_backup)));
            }
            None => lines.push(line("Nothing needed to change.")),
        }
        lines.push(line(""));
        for c in &out.plan.changes {
            lines.push(change_line(c));
        }
        lines.extend(manual_lines(&out.plan.manual, &profile.manual));
        self.show_text("Load Saved Settings — result", lines, false);
        Ok(())
    }

    // ------------------------------------------------------------ restore flow
    fn on_restore_key(&mut self, code: KeyCode) {
        let Screen::Restore(st) = &mut self.screen else { return };
        if let Some((item, changes)) = &st.preview {
            match code {
                KeyCode::Esc | KeyCode::Backspace => self.back(),
                KeyCode::Down | KeyCode::Char('j') => st.scroll = st.scroll.saturating_add(1),
                KeyCode::Up | KeyCode::Char('k') => st.scroll = st.scroll.saturating_sub(1),
                KeyCode::Enter | KeyCode::Char('y') => {
                    let what = restore_title(item);
                    let n = changes.len();
                    let item = item.clone();
                    let mut body = vec![line(format!("{what}: {n} setting(s) will change.")), line("A safety backup of the current state is taken first.")];
                    if self.status.processes.valorant {
                        body.push(styled("VALORANT is running: close it first.", Style::new().fg(Color::Yellow)));
                    }
                    self.modal = Some(Modal::Confirm { title: "Restore Previous Settings".into(), body, action: Pending::Restore(item) });
                }
                _ => {}
            }
            return;
        }
        match code {
            KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('q') => self.back(),
            KeyCode::Up | KeyCode::Char('k') => st.sel = st.sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => st.sel = (st.sel + 1).min(st.items.len().saturating_sub(1)),
            KeyCode::Char('u') => {
                if let Some(i) = st.items.iter().position(|i| matches!(i, RestoreItem::Undo(_))) {
                    st.sel = i;
                    self.preview_restore();
                } else {
                    self.notify(Level::Warn, "Nothing to undo");
                }
            }
            KeyCode::Char('r') => {
                if let Some(i) = st.items.iter().position(|i| matches!(i, RestoreItem::Redo(_))) {
                    st.sel = i;
                    self.preview_restore();
                } else {
                    self.notify(Level::Warn, "Nothing to redo");
                }
            }
            KeyCode::Enter => self.preview_restore(),
            _ => {}
        }
    }

    fn preview_restore(&mut self) {
        let Screen::Restore(st) = &mut self.screen else { return };
        let Some(item) = st.items.get(st.sel).cloned() else { return };
        let id = item.backup_id();
        match self.engine.verify_backup(id) {
            Ok(r) if !r.ok => {
                self.notify(Level::Error, format!("Backup #{id} is corrupted: {}", r.problems.join("; ")));
                return;
            }
            Err(e) => {
                self.notify(Level::Error, format!("{e:#}"));
                return;
            }
            _ => {}
        }
        match self.engine.preview_backup(id) {
            Ok(changes) => {
                if let Screen::Restore(st) = &mut self.screen {
                    st.preview = Some((item, changes));
                    st.scroll = 0;
                }
            }
            Err(e) => self.notify(Level::Error, format!("{e:#}")),
        }
    }

    fn apply_restore(&mut self, item: RestoreItem) -> Result<()> {
        let puuid = self.active_puuid().context("no active account")?;
        let (title, out) = match &item {
            RestoreItem::Undo(_) => ("Undo last transfer", self.engine.undo(&puuid, false)?.1),
            RestoreItem::Redo(_) => ("Redo last transfer", self.engine.redo(&puuid, false)?.1),
            RestoreItem::Original(b) | RestoreItem::Backup(b) => ("Restore backup", self.engine.restore_backup(b.id, false)?),
        };
        let mut lines = vec![
            styled(format!("✔ {title}: {} setting(s) restored and verified", out.changes.len()), Style::new().fg(Color::Green).add_modifier(Modifier::BOLD)),
            line(""),
        ];
        for c in &out.changes {
            lines.push(change_line(c));
        }
        self.show_text("Restore Previous Settings — result", lines, false);
        Ok(())
    }

    // ------------------------------------------------------------ profiles
    fn on_profiles_key(&mut self, code: KeyCode) {
        let Screen::Profiles(st) = &mut self.screen else { return };
        let cur = st.rows.get(st.sel).map(|r| r.name.clone());
        match code {
            KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('q') => self.back(),
            KeyCode::Up | KeyCode::Char('k') => st.sel = st.sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => st.sel = (st.sel + 1).min(st.rows.len().saturating_sub(1)),
            KeyCode::Char('i') => {
                self.modal = Some(Modal::Input { title: "Import profile".into(), prompt: "Path to .valsync file:".into(), value: String::new(), kind: InputKind::Import, mask: false })
            }
            _ => {
                let Some(name) = cur else { return };
                match code {
                    KeyCode::Enter => {
                        let row = st.rows[st.sel].clone();
                        let mut lines = vec![
                            line(format!("Created {} · updated {} · patch {}", crate::term::local_time(&row.created_at), crate::term::local_time(&row.updated_at), row.profile.patch.clone().unwrap_or_else(|| "?".into()))),
                            line(format!("Source: {}{}", row.source, if row.favorite { " · ★ favorite" } else { "" })),
                            line(""),
                        ];
                        lines.extend(category_lines(&row.profile));
                        lines.push(line(""));
                        for s in &row.profile.settings {
                            lines.push(Line::from(vec![
                                Span::styled(format!("{:<24}", s.category.label()), Style::new().fg(MUTED)),
                                Span::raw(format!("{} = {}", s.key, s.display_value())),
                                Span::styled(format!("  [{}]", s.scope.label()), Style::new().fg(if s.scope == Scope::Cloud { Color::Yellow } else { TEAL })),
                            ]));
                        }
                        for m in &row.profile.manual {
                            lines.push(line(format!("☐ {}: {}", m.label, m.value)));
                        }
                        self.show_text(format!("Profile '{}'", row.name), lines, true);
                    }
                    KeyCode::Char('r') => {
                        self.modal = Some(Modal::Input { title: "Rename profile".into(), prompt: format!("New name for '{name}':"), value: name.clone(), kind: InputKind::Rename(name), mask: false })
                    }
                    KeyCode::Char('d') => {
                        self.modal = Some(Modal::Input {
                            title: "Duplicate profile".into(),
                            prompt: format!("Name for the copy of '{name}':"),
                            value: format!("{name} copy"),
                            kind: InputKind::Duplicate(name),
                            mask: false,
                        })
                    }
                    KeyCode::Char('x') | KeyCode::Delete => {
                        self.modal = Some(Modal::Confirm {
                            title: "Delete profile".into(),
                            body: vec![line(format!("Delete profile '{name}'?")), line("Backups are not affected.")],
                            action: Pending::DeleteProfile(name),
                        })
                    }
                    KeyCode::Char('f') => {
                        let fav = st.rows[st.sel].favorite;
                        if let Err(e) = self.engine.db.set_favorite(&name, !fav) {
                            self.notify(Level::Error, format!("{e:#}"));
                        }
                        self.reload_screen_data();
                    }
                    KeyCode::Char('e') => {
                        let default = std::env::current_dir().unwrap_or_default().join(format!("{}.valsync", profile::file_stem(&name)));
                        self.modal = Some(Modal::Input {
                            title: "Export profile".into(),
                            prompt: "Export to file:".into(),
                            value: default.display().to_string(),
                            kind: InputKind::Export(name),
                            mask: false,
                        })
                    }
                    KeyCode::Char('p') => match self.active_puuid() {
                        Some(p) => {
                            let _ = self.engine.db.set_preferred_profile(&p, Some(&name));
                            self.notify(Level::Ok, format!("'{name}' is now the preferred profile for {}", self.active_label()));
                        }
                        None => self.notify(Level::Warn, "No active account"),
                    },
                    KeyCode::Char('c') => match st.mark.take() {
                        None => {
                            st.mark = Some(name.clone());
                            self.notify(Level::Info, format!("Marked '{name}'. Select another profile and press c to compare."));
                        }
                        Some(a) if a == name => self.notify(Level::Info, "Compare cancelled"),
                        Some(a) => self.compare(&a, &name),
                    },
                    KeyCode::Char('v') => self.check_profile(&name),
                    _ => {}
                }
            }
        }
    }

    fn compare(&mut self, a: &str, b: &str) {
        let (Ok(Some(pa)), Ok(Some(pb))) = (self.engine.db.profile(a), self.engine.db.profile(b)) else { return };
        let d = profile::compare(&pa.profile, &pb.profile);
        let mut lines = vec![
            line(format!("{} identical · {} different · {} only in '{a}' · {} only in '{b}'", d.same, d.changed.len(), d.only_a.len(), d.only_b.len())),
            line(""),
        ];
        for (x, y) in &d.changed {
            lines.push(Line::from(vec![
                Span::styled(format!("{:<24}", x.category.label()), Style::new().fg(MUTED)),
                Span::raw(format!("{:<40} ", x.key)),
                Span::styled(x.display_value(), Style::new().fg(Color::Cyan)),
                Span::raw("  vs  "),
                Span::styled(y.display_value(), Style::new().fg(Color::Magenta)),
            ]));
        }
        for s in &d.only_a {
            lines.push(line(format!("only in '{a}': {} = {}", s.key, s.display_value())));
        }
        for s in &d.only_b {
            lines.push(line(format!("only in '{b}': {} = {}", s.key, s.display_value())));
        }
        self.show_text(format!("Compare '{a}' vs '{b}'"), lines, true);
    }

    fn check_profile(&mut self, name: &str) {
        let Ok(Some(p)) = self.engine.db.profile(name) else { return };
        let Ok((t, _)) = self.engine.active_target() else {
            self.notify(Level::Warn, "No active account to check against");
            return;
        };
        match self.engine.plan_load(&p.profile, &t, &LoadOptions::default()) {
            Ok(plan) => {
                let mut lines = vec![match &plan.patch_warning {
                    Some(w) => styled(format!("! {w}"), Style::new().fg(Color::Yellow)),
                    None => styled(format!("✔ Patch compatible ({})", p.profile.patch.clone().unwrap_or_else(|| "unknown".into())), Style::new().fg(Color::Green)),
                }];
                lines.push(line(format!("{} local change(s) would be applied; {} already match", plan.changes.len(), plan.unchanged)));
                if plan.new_keys > 0 {
                    lines.push(styled(format!("! {} key(s) are new to this account (other patch/version); they will be added", plan.new_keys), Style::new().fg(Color::Yellow)));
                }
                if !plan.new_files.is_empty() {
                    lines.push(line(format!("Files to create: {}", plan.new_files.join(", "))));
                }
                lines.push(line(format!("{} cloud-synced setting(s) will appear in the manual checklist", plan.manual.len())));
                self.show_text(format!("Compatibility: '{name}' → {}", self.engine.account_label(&t.puuid)), lines, true);
            }
            Err(e) => self.notify(Level::Error, format!("{e:#}")),
        }
    }

    // ------------------------------------------------------------ accounts
    fn on_accounts_key(&mut self, code: KeyCode) {
        let Screen::Accounts(st) = &mut self.screen else { return };
        let cur = st.rows.get(st.sel).cloned();
        match code {
            KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('q') => self.back(),
            KeyCode::Up | KeyCode::Char('k') => st.sel = st.sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => st.sel = (st.sel + 1).min(st.rows.len().saturating_sub(1)),
            _ => {
                let Some((acc, _, _)) = cur else { return };
                match code {
                    KeyCode::Char('p') => {
                        self.modal = Some(Modal::Input {
                            title: "Preferred profile".into(),
                            prompt: format!("Preferred profile for {} (empty clears):", acc.display()),
                            value: acc.preferred_profile.clone().unwrap_or_default(),
                            kind: InputKind::Preferred(acc.puuid.clone()),
                            mask: false,
                        })
                    }
                    KeyCode::Char('x') | KeyCode::Delete => {
                        self.modal = Some(Modal::Confirm {
                            title: "Forget account".into(),
                            body: vec![line(format!("Forget {}?", acc.display())), line("Game files and backups are kept.")],
                            action: Pending::ForgetAccount(acc.puuid.clone()),
                        })
                    }
                    _ => {}
                }
            }
        }
    }

    // ------------------------------------------------------------ backups
    fn on_backups_key(&mut self, code: KeyCode) {
        let Screen::Backups(st) = &mut self.screen else { return };
        let cur = st.rows.get(st.sel).cloned();
        match code {
            KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('q') => self.back(),
            KeyCode::Up | KeyCode::Char('k') => st.sel = st.sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => st.sel = (st.sel + 1).min(st.rows.len().saturating_sub(1)),
            KeyCode::Char('a') => {
                st.all = !st.all;
                self.reload_screen_data();
            }
            KeyCode::Char('m') => {
                if self.require_active().is_some() {
                    self.modal = Some(Modal::Input { title: "Manual backup".into(), prompt: "Label:".into(), value: "Manual backup".into(), kind: InputKind::BackupLabel, mask: false })
                }
            }
            KeyCode::Char('V') => {
                let ids: Vec<i64> = st.rows.iter().map(|b| b.id).collect();
                let bad = ids.iter().filter(|id| !self.engine.verify_backup(**id).map(|r| r.ok).unwrap_or(false)).count();
                if bad == 0 {
                    self.notify(Level::Ok, format!("All {} backup(s) verified", ids.len()));
                } else {
                    self.notify(Level::Error, format!("{bad} backup(s) failed verification"));
                }
                self.reload_screen_data();
            }
            _ => {
                let Some(b) = cur else { return };
                match code {
                    KeyCode::Char('v') => {
                        match self.engine.verify_backup(b.id) {
                            Ok(r) if r.ok => self.notify(Level::Ok, format!("Backup #{} is intact", b.id)),
                            Ok(r) => self.notify(Level::Error, format!("Backup #{}: {}", b.id, r.problems.join("; "))),
                            Err(e) => self.notify(Level::Error, format!("{e:#}")),
                        }
                        self.reload_screen_data();
                    }
                    KeyCode::Enter => {
                        if Some(b.account_puuid.clone()) != self.active_puuid() {
                            self.notify(Level::Warn, "This backup belongs to another account. Switch to it in the Riot Client to restore it.");
                            return;
                        }
                        let item = if b.kind == BackupKind::Original { RestoreItem::Original(b) } else { RestoreItem::Backup(b) };
                        let mut rs = RestoreState { items: vec![item], sel: 0, preview: None, scroll: 0 };
                        rs.sel = 0;
                        self.screen = Screen::Restore(rs);
                        self.preview_restore();
                    }
                    KeyCode::Char('x') | KeyCode::Delete => {
                        let mut body = vec![line(format!("Delete backup #{} ({})?", b.id, b.label))];
                        if b.kind == BackupKind::Original {
                            body.push(styled("This is the ORIGINAL settings snapshot of the account!", Style::new().fg(Color::Red)));
                        } else if self.engine.db.backup_referenced(b.id).unwrap_or(false) {
                            body.push(line("Undo/redo history that depends on it will be removed."));
                        }
                        self.modal = Some(Modal::Confirm { title: "Delete backup".into(), body, action: Pending::DeleteBackup(b.id) });
                    }
                    _ => {}
                }
            }
        }
    }

    // ------------------------------------------------------------ settings
    fn on_settings_key(&mut self, code: KeyCode) {
        let Screen::Settings { sel } = &mut self.screen else { return };
        let keys = AppConfig::KEYS;
        match code {
            KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('q') => self.back(),
            KeyCode::Up | KeyCode::Char('k') => *sel = sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => *sel = (*sel + 1).min(keys.len() - 1),
            KeyCode::Enter | KeyCode::Char(' ') => {
                let key = keys[*sel];
                if AppConfig::is_bool(key) {
                    let cur = self.engine.config.get_display(key) == "true";
                    let r = self.engine.config.set_field(key, if cur { "false" } else { "true" }).and_then(|_| self.engine.save_config());
                    match r {
                        Ok(()) => self.notify(Level::Ok, format!("{key} = {}", !cur)),
                        Err(e) => self.notify(Level::Error, format!("{e:#}")),
                    }
                } else {
                    let secret = key.contains("key") || key.contains("secret");
                    let value = if secret { String::new() } else { self.engine.config.get_display(key).replace("(not set)", "").replace(" (unlimited)", "") };
                    self.modal = Some(Modal::Input {
                        title: "Application Settings".into(),
                        prompt: format!("{} ({}):", key, AppConfig::describe(key)),
                        value,
                        kind: InputKind::Setting(key.to_string()),
                        mask: secret,
                    });
                }
            }
            _ => {}
        }
    }
}

pub fn change_line(c: &Change) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{:<24}", c.category.label()), Style::new().fg(MUTED)),
        Span::raw(format!("{:<38} ", truncate(&c.key, 38))),
        Span::styled(format!("{:>14}", c.old_display()), Style::new().fg(Color::Red)),
        Span::raw(" → "),
        Span::styled(c.new_display(), Style::new().fg(Color::Green)),
        Span::styled(if c.file == crate::cloud::CLOUD_REL { String::new() } else { "  [video]".to_string() }, Style::new().fg(MUTED)),
    ])
}

pub fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() > n { format!("{}…", s.chars().take(n.saturating_sub(1)).collect::<String>()) } else { s.to_string() }
}

pub fn category_lines(p: &Profile) -> Vec<Line<'static>> {
    let counts = p.categories();
    let mut out = vec![styled(format!("  {:<26}{:>9}", "What's included", "Settings"), Style::new().add_modifier(Modifier::BOLD))];
    for c in Category::ALL {
        let (l, cl) = counts.get(&c).copied().unwrap_or((0, 0));
        if l + cl == 0 {
            continue;
        }
        out.push(Line::from(vec![Span::raw(format!("  {:<26}{:>9}", c.label(), l + cl))]));
    }
    out
}

pub fn manual_lines(manual: &[profile::SettingEntry], extra: &[profile::ManualValue]) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    if manual.is_empty() && extra.is_empty() {
        return out;
    }
    out.push(line(""));
    out.push(styled("Manual checklist — account-synced settings to set in-game", Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD)));
    out.push(styled("These are stored on Riot's servers; no supported API can write them.", Style::new().fg(MUTED)));
    for m in manual {
        out.push(line(format!("  ☐ {:<22} {} = {}", m.category.label(), m.key.rsplit("::").next().unwrap_or(&m.key), m.display_value())));
    }
    for e in extra {
        out.push(line(format!("  ☐ {}: {}", e.label, e.value)));
    }
    out
}

pub fn restore_title(item: &RestoreItem) -> String {
    match item {
        RestoreItem::Undo(o) => format!("Undo last transfer ({} · {})", o.profile_name.clone().unwrap_or_else(|| o.kind.clone()), crate::term::local_time(&o.created_at)),
        RestoreItem::Redo(o) => format!("Redo transfer ({} · {})", o.profile_name.clone().unwrap_or_else(|| o.kind.clone()), crate::term::local_time(&o.created_at)),
        RestoreItem::Original(b) => format!("Restore ORIGINAL settings (#{} · {})", b.id, crate::term::local_time(&b.created_at)),
        RestoreItem::Backup(b) => format!("Backup #{} · {} · {}", b.id, b.label, crate::term::local_time(&b.created_at)),
    }
}

/// Drive the interface headlessly: feed keys and return the rendered screen
/// as text. Used by automated tests to exercise every screen.
pub fn render_headless(engine: Engine, keys: &[KeyCode], width: u16, height: u16) -> Result<(String, Engine)> {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend)?;
    let mut app = App::new(engine)?;
    for k in keys {
        app.on_key(KeyEvent::new(*k, KeyModifiers::NONE));
        app.on_tick();
    }
    terminal.draw(|f| ui::draw(f, &app))?;
    let buf = terminal.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    Ok((out, app.engine))
}
