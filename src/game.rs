//! Riot Client / VALORANT process detection and patch detection.
//!
//! Process detection only enumerates process *names* through the standard
//! Windows ToolHelp snapshot API. VALSYNC never opens, reads or hooks any game
//! or anti-cheat process.

use std::path::Path;

pub const RIOT_CLIENT_EXES: [&str; 4] = ["riotclientservices.exe", "riot client.exe", "riotclientux.exe", "riotclientuxrender.exe"];
pub const VALORANT_EXES: [&str; 2] = ["valorant-win64-shipping.exe", "valorant.exe"];
pub const VANGUARD_EXES: [&str; 2] = ["vgc.exe", "vgtray.exe"];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ProcessProbe {
    /// Query the operating system.
    #[default]
    System,
    /// Fixed list (tests / `VALSYNC_FAKE_PROCESSES`).
    Fixed(Vec<String>),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProcessStatus {
    pub riot_client: bool,
    pub valorant: bool,
    pub vanguard: bool,
}

impl ProcessProbe {
    pub fn from_env() -> ProcessProbe {
        match std::env::var("VALSYNC_FAKE_PROCESSES") {
            Ok(v) => ProcessProbe::Fixed(v.split(',').map(|s| s.trim().to_ascii_lowercase()).filter(|s| !s.is_empty()).collect()),
            Err(_) => ProcessProbe::System,
        }
    }

    pub fn names(&self) -> Vec<String> {
        match self {
            ProcessProbe::System => system_process_names(),
            ProcessProbe::Fixed(v) => v.clone(),
        }
    }

    pub fn status(&self) -> ProcessStatus {
        let names = self.names();
        let has = |list: &[&str]| names.iter().any(|n| list.contains(&n.as_str()));
        ProcessStatus { riot_client: has(&RIOT_CLIENT_EXES), valorant: has(&VALORANT_EXES), vanguard: has(&VANGUARD_EXES) }
    }
}

#[cfg(windows)]
fn system_process_names() -> Vec<String> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    };
    let mut out = Vec::new();
    // SAFETY: standard ToolHelp enumeration; the entry struct is zero-initialised
    // with dwSize set as the API requires, and the snapshot handle is closed.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE || snap.is_null() {
            return out;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snap, &mut entry) != 0 {
            loop {
                let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
                out.push(String::from_utf16_lossy(&entry.szExeFile[..len]).to_ascii_lowercase());
                if Process32NextW(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
    }
    out
}

#[cfg(not(windows))]
fn system_process_names() -> Vec<String> {
    // Development/testing on Linux (e.g. under Wine): read /proc/*/comm and cmdline.
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir("/proc") {
        for e in rd.flatten() {
            let p = e.path();
            if let Ok(cmd) = std::fs::read(p.join("cmdline")) {
                if let Some(first) = cmd.split(|b| *b == 0).next() {
                    let s = String::from_utf8_lossy(first).replace('\\', "/");
                    if let Some(name) = s.rsplit('/').next()
                        && !name.is_empty()
                    {
                        out.push(name.to_ascii_lowercase());
                    }
                }
            } else if let Ok(c) = std::fs::read_to_string(p.join("comm")) {
                out.push(c.trim().to_ascii_lowercase());
            }
        }
    }
    out
}

/// Read up to `max` bytes from the start of a file.
fn read_head(path: &Path, max: usize) -> Option<String> {
    use std::io::Read;
    let f = std::fs::File::open(path).ok()?;
    let mut buf = Vec::with_capacity(max.min(1 << 20));
    f.take(max as u64).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).to_string())
}

fn read_tail(path: &Path, max: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(max))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).to_string())
}

/// Extract `release-XX.YY` from a text blob.
pub fn find_release(text: &str) -> Option<String> {
    let idx = text.find("release-")?;
    let rest = &text[idx..];
    let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '.')).unwrap_or(rest.len());
    let tok = rest[..end].trim_end_matches(['.', '-']);
    let ver = tok.strip_prefix("release-")?;
    let mut parts = ver.split(['.', '-']);
    let major = parts.next()?;
    let minor = parts.next()?;
    if major.chars().all(|c| c.is_ascii_digit()) && minor.chars().all(|c| c.is_ascii_digit()) && !major.is_empty() && !minor.is_empty() {
        Some(format!("release-{major}.{minor}"))
    } else {
        None
    }
}

/// Detect the installed VALORANT patch from the game's own log
/// (`Saved\Logs\ShooterGame.log`, line `Branch: release-XX.YY`).
pub fn detect_patch(logs_dir: &Path) -> Option<String> {
    for name in ["ShooterGame.log", "ShooterGame-backup.log"] {
        if let Some(head) = read_head(&logs_dir.join(name), 256 * 1024) {
            for line in head.lines() {
                if (line.contains("Branch:") || line.contains("branch")) && let Some(r) = find_release(line) {
                    return Some(r);
                }
            }
            if let Some(r) = find_release(&head) {
                return Some(r);
            }
        }
    }
    None
}

/// Compare two `release-XX.YY` patch strings. Returns a warning if they differ.
pub fn patch_warning(profile_patch: Option<&str>, current: Option<&str>) -> Option<String> {
    match (profile_patch, current) {
        (Some(p), Some(c)) if p != c => {
            let major = |s: &str| s.trim_start_matches("release-").split('.').next().map(str::to_string);
            if major(p) != major(c) {
                Some(format!("Profile was saved on {p}; this PC runs {c} (different major version). Keys that no longer exist are skipped by the game."))
            } else {
                Some(format!("Profile was saved on {p}; this PC runs {c}."))
            }
        }
        _ => None,
    }
}

/// Best effort: the map currently loaded by *your own* game client, from the
/// tail of the local log. Only used to label the Live Match screen.
pub fn current_map_from_log(logs_dir: &Path) -> Option<String> {
    let tail = read_tail(&logs_dir.join("ShooterGame.log"), 512 * 1024)?;
    let mut last: Option<String> = None;
    for line in tail.lines() {
        if let Some(i) = line.find("/Game/Maps/") {
            let rest = &line[i + "/Game/Maps/".len()..];
            let name: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
            if !name.is_empty() {
                last = Some(name);
            }
        }
    }
    let m = last?;
    let lower = m.to_ascii_lowercase();
    if ["menu", "init", "entry", "mainmenu", "lobby", "loading", "poveggiontitle"].iter().any(|x| lower.contains(x)) {
        return None;
    }
    Some(m)
}

/// Codename → public map name (VALORANT internal map names are codenames).
pub fn map_display_name(codename: &str) -> String {
    let n = match codename.to_ascii_lowercase().as_str() {
        "ascent" => "Ascent",
        "bonsai" => "Split",
        "canyon" => "Fracture",
        "duality" => "Bind",
        "foxtrot" => "Breeze",
        "jam" => "Lotus",
        "juliett" => "Sunset",
        "pitt" => "Pearl",
        "port" => "Icebox",
        "triad" => "Haven",
        "infinity" => "Abyss",
        "rook" => "Corrode",
        "range" => "The Range",
        "hurm_alley" => "District",
        "hurm_bowl" => "Kasbah",
        "hurm_yard" => "Piazza",
        "hurm_helix" => "Drift",
        "hurm_highrise" => "Glitch",
        _ => return codename.to_string(),
    };
    n.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_release() {
        assert_eq!(find_release("LogShooter: Display: Branch: release-09.10"), Some("release-09.10".into()));
        assert_eq!(find_release("x release-11.02-shipping-12-345 y"), Some("release-11.02".into()));
        assert_eq!(find_release("nothing"), None);
    }

    #[test]
    fn patch_warnings() {
        assert!(patch_warning(Some("release-08.11"), Some("release-09.10")).unwrap().contains("major"));
        assert!(patch_warning(Some("release-09.10"), Some("release-09.10")).is_none());
        assert!(patch_warning(None, Some("release-09.10")).is_none());
    }

    #[test]
    fn fixed_probe() {
        let p = ProcessProbe::Fixed(vec!["riotclientservices.exe".into()]);
        let s = p.status();
        assert!(s.riot_client && !s.valorant);
    }
}
