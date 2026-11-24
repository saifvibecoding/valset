//! VALSYNC entry point.

use clap::Parser;
use valsync::cli::Cli;
use valsync::commands::{self, Ctx};
use valsync::engine::Engine;
use valsync::paths::Paths;
use valsync::term;

fn main() {
    valsync::bench::mark_start();
    let cli = Cli::parse();
    let code = match real_main(cli) {
        Ok(()) => 0,
        Err(e) => {
            term::error(&format!("{e:#}"));
            1
        }
    };
    std::process::exit(code);
}

fn real_main(cli: Cli) -> anyhow::Result<()> {
    let engine = Engine::open(Paths::detect())?;
    term::init_color(!cli.no_color && engine.config.color);
    match cli.command {
        None => valsync::tui::run(engine, false),
        Some(cmd) => {
            let mut ctx = Ctx { engine, yes: cli.yes };
            match commands::run(cmd, &mut ctx) {
                Err(e) if e.to_string() == "__tui_live__" => valsync::tui::run(ctx.engine, true),
                other => other,
            }
        }
    }
}
