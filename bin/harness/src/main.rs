//! `harness` — the loop binary (docs/phase-2-plan.md).
//!
//! Subcommands:
//! - `harness run SESSION` — the full turn loop, replaces `turn.sh`.
//! - `harness step SESSION` — one step, replaces `step.sh`.
//!
//! Still spawns the stage binaries: `claim`, `assemble`, `model`,
//! `parse`, `route`, `compact`. Appends in-process through the shared
//! `LogLine` and validator.

mod classifier;
mod config;
mod signals;
mod step;
mod run_loop;
mod stage_runner;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "harness", about = "The loop binary: run the full turn loop or a single step")]
struct Args {
    /// Path to config file
    #[arg(long, default_value = "config.toml", global = true)]
    config: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the full turn loop (replaces `turn.sh`)
    Run {
        /// Session name or directory
        session: String,
    },
    /// Run a single step (replaces `step.sh`)
    Step {
        /// Session name or directory
        session: String,
    },
}

fn main() {
    let args = Args::parse();

    // The CONFIG env var sets the config path, as the old scripts did.
    let config_path = std::env::var("CONFIG").unwrap_or_else(|_| {
        args.config.to_string_lossy().into_owned()
    });
    let cfg = config::HarnessConfig::load(&PathBuf::from(&config_path));
    let session_dir = cfg.resolve_session(&args.command_session());

    match &args.command {
        Command::Run { .. } => {
            signals::install();
            run_loop::run(&cfg, &session_dir);
        }
        Command::Step { .. } => {
            signals::install();
            step::do_step(&cfg, &session_dir, step::StepMode::Step);
        }
    }
}

impl Args {
    fn command_session(&self) -> &str {
        match &self.command {
            Command::Run { session } | Command::Step { session } => session,
        }
    }
}
