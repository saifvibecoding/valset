//! Headless rendering of every TUI screen (no panics, expected content).

mod common;

use common::{Env, b};
use ratatui::crossterm::event::KeyCode;

fn render(env: &Env, keys: &[KeyCode], w: u16, h: u16) -> String {
    let e = env.engine();
    valsync::tui::render_headless(e, keys, w, h).unwrap().0
}

#[test]
fn every_screen_renders() {
    let env = Env::new();
    {
        let mut e = env.engine();
        let (t, _) = e.active_target().unwrap();
        e.save_profile(&t, "Main", false, None, None, vec![]).unwrap();
    }
    env.switch_to(b());
    let main = render(&env, &[], 120, 40);
    assert!(main.contains("Save Current Settings") && main.contains("Active account") && main.contains("Riot Client"));
    let cases: Vec<(Vec<KeyCode>, &str)> = vec![
        (vec![KeyCode::Char('1')], "Profile name"),
        (vec![KeyCode::Char('2')], "Load Saved Settings"),
        (vec![KeyCode::Char('2'), KeyCode::Enter], "Select categories"),
        (vec![KeyCode::Char('2'), KeyCode::Enter, KeyCode::Enter], "change(s)"),
        (vec![KeyCode::Char('2'), KeyCode::Enter, KeyCode::Enter, KeyCode::Enter], "Apply"),
        (vec![KeyCode::Char('2'), KeyCode::Enter, KeyCode::Enter, KeyCode::Enter, KeyCode::Char('y')], "Applied and verified"),
        (vec![KeyCode::Char('3')], "Restore Previous Settings"),
        (vec![KeyCode::Char('3'), KeyCode::Char('u')], "Preview restore"),
        (vec![KeyCode::Char('4')], "Manage Settings Profiles"),
        (vec![KeyCode::Char('4'), KeyCode::Enter], "Profile 'Main'"),
        (vec![KeyCode::Char('5')], "Manage Riot Accounts"),
        (vec![KeyCode::Char('6')], "Live Match"),
        (vec![KeyCode::Char('7')], "Match History"),
        (vec![KeyCode::Char('8')], "Backup History"),
        (vec![KeyCode::Char('9')], "Application Settings"),
        (vec![KeyCode::Esc], "Exit VALSYNC"),
    ];
    for (keys, expect) in cases {
        let s = render(&env, &keys, 120, 40);
        assert!(s.contains(expect), "after {keys:?} expected '{expect}' in:\n{s}");
    }
    // Small terminals must not panic either.
    for (w, h) in [(40, 12), (80, 24), (200, 60)] {
        for k in ['1', '2', '3', '4', '5', '6', '7', '8', '9'] {
            render(&env, &[KeyCode::Char(k)], w, h);
        }
    }
}

#[test]
fn undo_through_tui_restores_files() {
    let env = Env::new();
    {
        let mut e = env.engine();
        let (t, _) = e.active_target().unwrap();
        e.save_profile(&t, "Main", false, None, None, vec![]).unwrap();
    }
    env.switch_to(b());
    render(&env, &[KeyCode::Char('2'), KeyCode::Enter, KeyCode::Enter, KeyCode::Enter, KeyCode::Char('y')], 120, 40);
    assert!(env.gus(b()).contains("ResolutionSizeX=1920"));
    let s = render(&env, &[KeyCode::Char('3'), KeyCode::Char('u'), KeyCode::Enter, KeyCode::Char('y')], 120, 40);
    assert!(s.contains("restored and verified"), "{s}");
    assert!(env.gus(b()).contains("ResolutionSizeX=1280"));
}
