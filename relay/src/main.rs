//! relay: queued coding-agent tasks, run as sandboxed headless Claude Code
//! sessions on whichever account has room. See README.md.

/// `println!` that returns the error instead of panicking, so `relay ... | head` ends quietly.
macro_rules! out {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        writeln!(std::io::stdout().lock(), $($arg)*)?
    }};
}

mod accounts;
mod bus;
mod config;
mod ctx;
mod os;
mod sandbox;
mod stream;
mod systems;
mod usage;
mod world;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Run queued tasks as sandboxed headless Claude Code sessions, spread over accounts by quota"
)]
struct Cli {
    /// The project's `relay.toml`. Default: `$RELAY_CONFIG`, else the nearest `relay.toml` upwards.
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    system: System,
}

/// Each system is one responsibility, owning the events it emits.
#[derive(Debug, Subcommand)]
enum System {
    /// Take work in, withdraw it, stop it.
    #[command(subcommand)]
    Queue(systems::queue::Cmd),
    /// Give queued jobs a slot; reap dead sessions.
    #[command(subcommand)]
    Dispatch(systems::dispatch::Cmd),
    /// Run one job (started by dispatch).
    #[command(subcommand)]
    Session(systems::session::Cmd),
    /// Account room and switches.
    #[command(subcommand)]
    Quota(systems::quota::Cmd),
    /// Read-only views.
    #[command(subcommand)]
    View(systems::view::Cmd),
    /// Bring branches back; clean up.
    #[command(subcommand)]
    Land(systems::land::Cmd),
    /// Self-test the sandbox. Starts no model session.
    Probe {
        #[arg(long)]
        account: Option<String>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result =
        config::Config::load(cli.config.as_deref()).and_then(|(cfg, path)| ctx::Ctx::new(cfg, path)).and_then(|ctx| {
            match cli.system {
                System::Queue(c) => systems::queue::run(&ctx, c),
                System::Dispatch(c) => systems::dispatch::run(&ctx, c),
                System::Session(c) => systems::session::run(&ctx, c),
                System::Quota(c) => systems::quota::run(&ctx, c),
                System::View(c) => systems::view::run(&ctx, c),
                System::Land(c) => systems::land::run(&ctx, c),
                System::Probe { account } => systems::probe::run(&ctx, account.as_deref()),
            }
        });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.downcast_ref::<std::io::Error>().is_some_and(|io| io.kind() == std::io::ErrorKind::BrokenPipe) => {
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("relay: {e:#}");
            ExitCode::FAILURE
        }
    }
}
