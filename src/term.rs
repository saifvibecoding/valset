//! Plain (non-TUI) terminal output helpers for direct commands.

use crossterm::style::{Color, Stylize};
use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};

static COLOR: AtomicBool = AtomicBool::new(true);

pub fn init_color(enabled: bool) {
    let env_off = std::env::var_os("NO_COLOR").is_some();
    COLOR.store(enabled && !env_off && std::io::stdout().is_terminal(), Ordering::Relaxed);
    #[cfg(windows)]
    {
        // Enable ANSI escape processing on legacy consoles (Windows 10+ conhost).
        let _ = crossterm::ansi_support::supports_ansi();
    }
}

pub fn color_on() -> bool {
    COLOR.load(Ordering::Relaxed)
}

pub fn paint(s: &str, c: Color) -> String {
    if color_on() { s.with(c).to_string() } else { s.to_string() }
}

pub fn bold(s: &str) -> String {
    if color_on() { s.bold().to_string() } else { s.to_string() }
}

pub fn dim(s: &str) -> String {
    if color_on() { s.dark_grey().to_string() } else { s.to_string() }
}

pub fn ok(msg: &str) {
    println!("{} {msg}", paint("✔", Color::Green));
}

pub fn warn(msg: &str) {
    println!("{} {msg}", paint("!", Color::Yellow));
}

pub fn info(msg: &str) {
    println!("{} {msg}", paint("•", Color::Cyan));
}

pub fn error(msg: &str) {
    eprintln!("{} {msg}", paint("error:", Color::Red));
}

pub fn heading(s: &str) {
    println!("\n{}", bold(&paint(s, Color::Rgb { r: 255, g: 70, b: 85 })));
}

pub fn dot(on: bool) -> String {
    if on { paint("●", Color::Green) } else { paint("●", Color::DarkGrey) }
}

/// Ask for confirmation. `--yes` answers automatically; non-interactive stdin
/// without `--yes` refuses rather than guessing.
pub fn confirm(question: &str, yes: bool) -> anyhow::Result<bool> {
    if yes {
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        anyhow::bail!("confirmation required for: {question}\nRe-run with --yes to confirm non-interactively.");
    }
    print!("{} {question} [y/N] ", paint("?", Color::Yellow));
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}

/// Simple left-aligned table printer that measures display width by chars.
pub fn table(headers: &[&str], rows: &[Vec<String>]) {
    let strip = |s: &str| -> usize {
        // Width ignoring ANSI escape sequences.
        let mut w = 0;
        let mut in_esc = false;
        for c in s.chars() {
            if in_esc {
                if c.is_ascii_alphabetic() {
                    in_esc = false;
                }
            } else if c == '\x1b' {
                in_esc = true;
            } else {
                w += 1;
            }
        }
        w
    };
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for r in rows {
        for (i, c) in r.iter().enumerate() {
            if i < widths.len() {
                widths[i] = widths[i].max(strip(c)).min(60);
            }
        }
    }
    let line: String = headers.iter().enumerate().map(|(i, h)| format!("{:<w$}", h, w = widths[i])).collect::<Vec<_>>().join("  ");
    println!("{}", bold(&line));
    for r in rows {
        let mut out = String::new();
        for (i, c) in r.iter().enumerate() {
            if i >= widths.len() {
                break;
            }
            let w = strip(c);
            out.push_str(c);
            if i + 1 < r.len() {
                out.push_str(&" ".repeat(widths[i].saturating_sub(w) + 2));
            }
        }
        println!("{out}");
    }
}

pub fn human_bytes(n: i64) -> String {
    if n < 1024 { format!("{n} B") } else if n < 1024 * 1024 { format!("{:.1} KB", n as f64 / 1024.0) } else { format!("{:.1} MB", n as f64 / 1048576.0) }
}

pub fn local_time(rfc3339: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|_| rfc3339.to_string())
}

pub const LOGO: &str = r#"
 ██╗   ██╗ █████╗ ██╗     ███████╗██╗   ██╗███╗   ██╗ ██████╗
 ██║   ██║██╔══██╗██║     ██╔════╝╚██╗ ██╔╝████╗  ██║██╔════╝
 ██║   ██║███████║██║     ███████╗ ╚████╔╝ ██╔██╗ ██║██║
 ╚██╗ ██╔╝██╔══██║██║     ╚════██║  ╚██╔╝  ██║╚██╗██║██║
  ╚████╔╝ ██║  ██║███████╗███████║   ██║   ██║ ╚████║╚██████╗
   ╚═══╝  ╚═╝  ╚═╝╚══════╝╚══════╝   ╚═╝   ╚═╝  ╚═══╝ ╚═════╝"#;
