//! relay: coding-agent tasks for any harness (claude, omp, puku-cli), run as
//! sandboxed headless sessions on whichever account has room, or asked
//! one-shot and read-only. See README.md.

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
mod harness;
mod os;
mod sandbox;
mod systems;
mod world;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Coding-agent tasks for any harness: queued as sandboxed headless sessions on the account with most room, or asked one-shot"
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
    /// One-shot, read-only call to a harness, now: relay ask AGENT MODE ... (relay ask --help)
    Ask {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

fn finish(result: anyhow::Result<()>) -> ExitCode {
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

fn main() -> ExitCode {
    // `ask` takes its arguments raw: its contract treats `--continue` as an
    // error before `--` and as text after it, which a clap parse would blur.
    let raw: Vec<String> = std::env::args().collect();
    if raw.get(1).map(String::as_str) == Some("ask") {
        return match systems::ask::main(&raw[2..]) {
            Ok(code) => ExitCode::from(code),
            Err(e) => {
                eprintln!("relay ask: {e:#}");
                ExitCode::FAILURE
            }
        };
    }
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
                System::Ask { .. } => unreachable!("handled before parsing"),
            }
        });
    finish(result)
}
