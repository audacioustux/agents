//! relay: coding-agent tasks for any harness (claude, omp), run as
//! sandboxed headless sessions on whichever account has room, or asked
//! one-shot and read-only. See README.md.

/// `println!` that returns the error instead of panicking, so `relay ... | head` ends quietly.
macro_rules! out {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        writeln!(std::io::stdout().lock(), $($arg)*)?
    }};
}

#[macro_use]
mod reply;
mod accounts;
mod bus;
mod config;
mod ctx;
mod harness;
mod host;
mod hostdir;
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
    /// One JSON value on stdout, and failures as JSON on stderr. Exit codes: 2 usage,
    /// 3 not found, 4 wrong state, 5 refused by a rule, 1 anything else.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    system: System,
}

/// Each system is one responsibility, owning the events it emits.
#[derive(Debug, Subcommand)]
enum System {
    /// Take work in, withdraw it, stop it.
    #[command(subcommand)]
    Queue(systems::queue::Cmd),
    /// Give queued jobs a seat; reap dead sessions.
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
    /// Decisions at relay's hook points: list those waiting for the orchestrator, answer one.
    #[command(subcommand)]
    Decide(systems::decide::Cmd),
    /// The job profiles: what `queue add --profile` fills in, each with its note.
    /// Outside a project, the owner's policy alone.
    Profiles,
    /// The merged policy and project config, and where each part came from.
    /// Outside a project, the owner's policy alone.
    #[command(subcommand)]
    Config(systems::profiles::Cmd),
    /// Self-test the sandbox. Starts no model session.
    Probe {
        #[arg(long)]
        account: Option<String>,
    },
    /// One-shot, read-only call to a harness, now, in your own account. Needs no relay.toml.
    Ask(systems::ask::Args),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let json = cli.json;
    let result = match cli.system {
        System::Ask(a) => systems::ask::run(&a, json),
        system => run(cli.config.as_deref(), json, system).map(|()| 0),
    };
    reply::finish(result, json, "relay")
}

fn run(config: Option<&std::path::Path>, json: bool, system: System) -> anyhow::Result<()> {
    match system {
        System::Profiles => return policy_views(config, json, None),
        System::Config(c) => return policy_views(config, json, Some(c)),
        // Outside a project the boards show every project on this machine.
        System::View(c) if c.runs_anywhere() && config::Config::find(config)?.is_none() => {
            return systems::view::outside(json, &c);
        }
        _ => {}
    }
    let (cfg, path) = config::Config::load(config)?;
    let ctx = ctx::Ctx::new(cfg, path, json)?;
    match system {
        System::Queue(c) => systems::queue::run(&ctx, c),
        System::Dispatch(c) => systems::dispatch::run(&ctx, c),
        System::Session(c) => systems::session::run(&ctx, c),
        System::Quota(c) => systems::quota::run(&ctx, c),
        System::View(c) => systems::view::run(&ctx, c),
        System::Land(c) => systems::land::run(&ctx, c),
        System::Decide(c) => systems::decide::run(&ctx, c),
        System::Probe { account } => systems::probe::run(&ctx, account.as_deref()),
        System::Ask(_) | System::Profiles | System::Config(_) => unreachable!("runs without a project context"),
    }
}

/// `profiles` (`cmd` is `None`) and `config show` read the policy and touch no project
/// state, so they need no relay.toml: outside a project they show the owner's policy alone.
fn policy_views(
    config: Option<&std::path::Path>,
    json: bool,
    cmd: Option<systems::profiles::Cmd>,
) -> anyhow::Result<()> {
    let (cfg, project) = match config::Config::find(config)? {
        Some(path) => {
            let (cfg, path) = config::Config::load_at(&path)?;
            (cfg, Some(path))
        }
        None => (config::Config::policy_only()?, None),
    };
    match cmd {
        None => systems::profiles::list(&cfg, json),
        Some(c) => systems::profiles::run(&cfg, project.as_deref(), json, c),
    }
}
