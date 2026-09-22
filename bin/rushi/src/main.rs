//! `rushi` — the distribution entry point.
//!
//! Subcommands:
//! - `rushi` (no subcommand) — print the top-level help. The TUI is a
//!   Tier-2 front-end (`rushi-tui`, rushi-tui repo); the kernel no
//!   longer launches front-ends (docs/itches.md, 2026-09-20 user
//!   decision; the `rushi tui` arm was removed in the issue #31
//!   follow-up).
//! - `rushi setup [--locked]` — initialize a project from `rushi.toml`
//! - `rushi run SESSION [TASK]` — the full turn loop. With `TASK`, it
//!   is first logged as the session's initial `user_message` (steer
//!   queue) so the loop starts by running a model turn on it — the
//!   subagent spawn contract (docs/subagent-design.md section 4).
//!   `--no-run` logs the task without running the loop.
//! - `rushi step SESSION` — one step (internal, TUI-supervised)
//! - `rushi docs [SECTION|DOC]` — print the embedded harness reference;
//!   a section of the default reference, or a bundled sub-document by
//!   name (e.g. `rushi docs nix-flake-module`)
//! - `rushi config` — print the resolved config file to stdout, so it
//!   can be dumped to disk for tweaking (e.g. `sessions_root`)
//!
//! The loop stages (`claim`, `assemble`, `model`, `parse`, `route`,
//! `compact`) are spawned as separate binaries. The TUI is a separate
//! front-end binary in the rushi-tui repo; invoke `rushi-tui`
//! directly, the kernel no longer launches it.

mod classifier;
mod config;
mod docs;
mod run_loop;
mod signals;
mod stage_runner;
mod step;
mod stream_channel;
mod setup;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "rushi", about = "The rushi distribution: loop engine, tools, and project setup")]
struct Args {
    /// Path to config file (for the loop and `config` subcommands).
    /// When omitted, falls back to the Nix side-by-side config or CWD.
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Subcommand. Omit to print the top-level help.
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Initialize a project from `rushi.toml`
    Setup {
        /// Verify against an existing `rushi.lock` instead of
        /// regenerating it.
        #[arg(long)]
        locked: bool,
    },
    /// Run the full turn loop (replaces `turn.sh`).
    ///
    /// With an optional `TASK`, the prompt is logged as the session's
    /// initial `user_message` (steer queue) before the loop starts, so
    /// the loop's first step runs a model turn on it. This makes `rushi`
    /// self-contained for seeding a session (the subagent spawn
    /// contract, docs/subagent-design.md section 4) without the
    /// separate `user` binary, which plain installs do not ship.
    Run {
        /// Session name or directory
        session: String,

        /// Initial prompt, logged as a `user_message` before the loop
        /// starts. Omit to continue from the log's current state.
        task: Option<String>,

        /// Log the task without running the loop (append-only mode,
        /// mirrors `user --no-run`). The event line is printed to
        /// stdout.
        #[arg(long)]
        no_run: bool,
    },
    /// Run a single step (replaces `step.sh`)
    Step {
        /// Session name or directory
        session: String,
    },
    /// Print the embedded harness reference.
    ///
    /// With no argument, prints the default reference. With a section
    /// number or title substring, prints only that section. With a
    /// bundled-document name (e.g. `nix-flake-module`), prints that
    /// whole sub-document. Run `rushi docs --list` to list sections and
    /// bundled docs.
    Docs {
        /// Section number / title substring, or a bundled-document name
        /// (case-insensitive). Omit to print the default reference.
        section: Option<String>,

        /// List all section headings and bundled docs without printing
        /// content.
        #[arg(long)]
        list: bool,
    },
    /// Print the config file that would be used to stdout.
    ///
    /// Resolves the config like the loop subcommands (`$CONFIG`, then
    /// `--config`, then the Nix side-by-side layout, then CWD) and
    /// prints its raw contents. Useful for dumping the active config to
    /// disk for tweaking, e.g. `rushi config > my-config.toml`.
    Config,
}

fn main() {
    let args = Args::parse();

    // The CONFIG env var sets the config path, as the old scripts did.
    let config_path = rushi_common::paths::resolve_config_path(args.config.as_deref());

    // A bare `rushi` prints the top-level help and exits. The TUI is a
    // Tier-2 front-end; the kernel no longer launches it (docs/itches.md,
    // 2026-09-20 user decision; issue #31 follow-up).
    let cmd = match args.command {
        Some(c) => c,
        None => {
            use clap::CommandFactory;
            let mut cmd = Args::command();
            cmd.print_help().ok();
            std::process::exit(0);
        }
    };

    match cmd {
        Command::Setup { locked } => {
            let project_dir = std::env::current_dir().unwrap_or_else(|e| {
                eprintln!("rushi setup: cannot resolve CWD: {e}");
                std::process::exit(1);
            });
            let kernel_dir = setup::resolve_kernel_tools_dir(&project_dir);
            if let Err(e) = setup::do_setup(locked, &project_dir, &kernel_dir) {
                eprintln!("rushi setup: {e}");
                std::process::exit(1);
            }
        }

        Command::Run { session, task, no_run } => {
            let cfg = config::HarnessConfig::load(&PathBuf::from(config_path));
            let session_dir = cfg.resolve_session(&session);
            signals::install();
            run_loop::run(&cfg, &session_dir, task.as_deref(), no_run);
        }

        Command::Step { session } => {
            let cfg = config::HarnessConfig::load(&PathBuf::from(config_path));
            let session_dir = cfg.resolve_session(&session);
            signals::install();
            step::do_step(&cfg, &session_dir, step::StepMode::Step);
        }

        Command::Docs { section, list } => {
            if list {
                docs::list_sections();
            } else {
                docs::print_docs(section.as_deref());
            }
        }

        Command::Config => {
            match config::config_dump(&config_path) {
                Ok(text) => print!("{text}"),
                Err(e) => {
                    eprintln!("rushi config: cannot read config {config_path:?}: {e}");
                    eprintln!("hint: pass an explicit path with --config <file> or set CONFIG=<file>");
                    std::process::exit(1);
                }
            }
        }
    }
}
