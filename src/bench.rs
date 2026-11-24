//! `valsync bench` — measure real timings and memory on this machine using a
//! synthetic VALORANT config in a temporary directory (real game files are
//! never touched).

use crate::appconfig::AppConfig;
use crate::commands::Ctx;
use crate::db::Db;
use crate::engine::{Engine, LoadOptions, Target};
use crate::game::ProcessProbe;
use crate::paths::Paths;
use crate::sample;
use crate::term;
use anyhow::Result;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub static PROCESS_START: OnceLock<Instant> = OnceLock::new();

pub fn mark_start() {
    let _ = PROCESS_START.set(Instant::now());
}

/// Peak resident memory of this process in bytes, if the OS reports it.
pub fn peak_rss_bytes() -> Option<u64> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        // SAFETY: counters struct is zeroed and sized as the API requires.
        unsafe {
            let mut c: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
            c.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
            if GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) != 0 {
                return Some(c.PeakWorkingSetSize as u64);
            }
        }
        None
    }
    #[cfg(not(windows))]
    {
        let s = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = s.lines().find(|l| l.starts_with("VmHWM:"))?;
        let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kb * 1024)
    }
}

fn time<T>(n: usize, mut f: impl FnMut() -> Result<T>) -> Result<Duration> {
    let start = Instant::now();
    for _ in 0..n {
        f()?;
    }
    Ok(start.elapsed() / n as u32)
}

fn fmt(d: Duration) -> String {
    if d.as_micros() < 1000 { format!("{} µs", d.as_micros()) } else { format!("{:.2} ms", d.as_secs_f64() * 1000.0) }
}

pub fn run(_ctx: &mut Ctx) -> Result<()> {
    let to_dispatch = PROCESS_START.get().map(|s| s.elapsed());
    let root = std::env::temp_dir().join(format!("valsync-bench-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let saved = root.join("VALORANT").join("Saved");
    let config = saved.join("Config");
    sample::write_account(&config, &sample::account_a())?;
    let dir_b = sample::write_account(&config, &sample::account_b())?;
    sample::set_last_known_user(&config, sample::PUUID_B)?;
    let paths = Paths::with_roots(&saved, &root.join("data"));
    let result = (|| -> Result<()> {
        let open = time(1, || Engine::open(paths.clone()))?;
        let db = Db::open(&paths.db_path())?;
        let mut e = Engine::with_parts(paths.clone(), db, AppConfig::default(), ProcessProbe::Fixed(vec![]));
        let ta = Target { puuid: sample::PUUID_A.into(), folder: format!("{}-eu", sample::PUUID_A), dir: config.join(format!("{}-eu", sample::PUUID_A)) };
        let tb = Target { puuid: sample::PUUID_B.into(), folder: format!("{}-eu", sample::PUUID_B), dir: dir_b.clone() };
        let status = time(20, || crate::service::status(&e).map(|_| ()))?;
        let capture = time(50, || e.capture(&ta, "bench", None).map(|_| ()))?;
        e.save_profile(&ta, "bench", true, None, None, vec![])?;
        let p = e.db.profile("bench")?.expect("saved").profile;
        let plan = time(50, || e.plan_load(&p, &tb, &LoadOptions::default()).map(|_| ()))?;
        let mut cycle = Duration::ZERO;
        let mut undo = Duration::ZERO;
        let iters = 10;
        for _ in 0..iters {
            let t0 = Instant::now();
            e.apply_load(&p, &tb, &LoadOptions::default())?;
            cycle += t0.elapsed();
            let t1 = Instant::now();
            e.undo(sample::PUUID_B, true)?;
            undo += t1.elapsed();
        }
        let verify = time(20, || e.verify_backup(1).map(|_| ()))?;
        term::heading("VALSYNC benchmark (synthetic config, this machine)");
        if let Some(d) = to_dispatch {
            println!("  Process start → command dispatch   {}", fmt(d));
        }
        println!("  Open engine (DB + recovery scan)   {}", fmt(open));
        println!("  Status dashboard refresh           {}", fmt(status));
        println!("  Capture profile                    {}", fmt(capture));
        println!("  Plan transfer (diff)               {}", fmt(plan));
        println!("  Transfer: backup+write+verify      {}", fmt(cycle / iters));
        println!("  Undo (restore + verify)            {}", fmt(undo / iters));
        println!("  Verify backup integrity            {}", fmt(verify));
        match peak_rss_bytes() {
            Some(b) => println!("  Peak memory (working set)          {:.1} MB", b as f64 / 1048576.0),
            None => println!("  Peak memory                        unavailable on this OS"),
        }
        println!("  Background processes / services    none (VALSYNC exits after each command)");
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&root);
    result
}
