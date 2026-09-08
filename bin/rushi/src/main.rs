//! `rushi` — the distribution entry point.
//!
//! Subcommands:
//! - `rushi` (default) or `rushi tui [SESSION]` — open the TUI
//! - `rushi setup [--locked]` — initialize a project from `rushi.toml`
//! - `rushi run SESSION` — the full turn loop (internal, TUI-supervised)
//! - `rushi step SESSION` — one step (internal, TUI-supervised)
//!
//! The loop stages (`claim`, `assemble`, `model`, `parse`, `route`,
//! `compact`) are spawned as separate binaries. The TUI is a separate
//! binary (`tui`) that `rushi` launches via the `tui` subcommand.

mod classifier;
mod config;
mod run_loop;
mod signals;
mod stage_runner;
mod step;
mod stream_channel;
mod setup;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "rushi", about = "The rushi distribution: TUI entry point and project setup")]
struct Args {
    /// Path to config file (for loop subcommands)
    #[arg(long, default_value = "config.toml", global = true)]
    config: PathBuf,

    /// Subcommand. Omit to open the TUI.
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Open the TUI (default when no subcommand is given)
    Tui {
        /// Session to open. If omitted, the TUI asks for a new session
        /// name on its first event.
        session: Option<String>,
    },
    /// Initialize a project from `rushi.toml`
    Setup {
        /// Verify against an existing `rushi.lock` instead of
        /// regenerating it.
        #[arg(long)]
        locked: bool,
    },
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
    let config_path = resolve_config_path(&args);

    // Default to TUI when no subcommand is given.
    let cmd = args.command.unwrap_or(Command::Tui { session: None });

    match cmd {
        Command::Tui { session } => {
            let tui_bin = resolve_tui_binary(&config_path);
            let mut child = std::process::Command::new(&tui_bin);
            if let Some(s) = &session {
                child.arg(s);
            }
            child.arg("--config").arg(config_path);
            let status = child
                .status()
                .unwrap_or_else(|e| {
                    eprintln!("rushi: failed to spawn tui ({tui_bin:?}): {e}");
                    std::process::exit(1);
                });
            std::process::exit(status.code().unwrap_or(1));
        }

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

        Command::Run { session } => {
            let cfg = config::HarnessConfig::load(&PathBuf::from(config_path));
            let session_dir = cfg.resolve_session(&session);
            signals::install();
            run_loop::run(&cfg, &session_dir);
        }

        Command::Step { session } => {
            let cfg = config::HarnessConfig::load(&PathBuf::from(config_path));
            let session_dir = cfg.resolve_session(&session);
            signals::install();
            step::do_step(&cfg, &session_dir, step::StepMode::Step);
        }
    }
}

/// Resolve the config path: `$CONFIG` env var overrides the CLI flag.
fn resolve_config_path(args: &Args) -> String {
    std::env::var("CONFIG").unwrap_or_else(|_| args.config.to_string_lossy().into_owned())
}

/// Find the `tui` binary. Resolution order:
/// 1. `[tui].binary` in the config file (resolved relative to the config
///    directory). This is the recommended way to point `rushi` at a
///    locally-built TUI without editing the launcher.
/// 2. Next to the `rushi` executable (side-by-side install).
/// 3. `tui` on `PATH`.
fn resolve_tui_binary(config_path: &str) -> String {
    if let Some(p) = config_tui_binary(config_path) {
        return p;
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("tui");
            if candidate.exists() {
                return candidate.to_string_lossy().into_owned();
            }
        }
    }
    "tui".into()
}

/// Read `[tui].binary` from the config file. The value is resolved
/// relative to the config directory; `None` when the key is absent
/// or the resolved path does not exist.
fn config_tui_binary(config_path: &str) -> Option<String> {
    let raw = std::fs::read_to_string(config_path).ok()?;
    let cfg: toml::Value = raw.parse().ok()?;
    let rel = cfg.get("tui")?.get("binary")?.as_str()?;
    let config_dir = std::path::Path::new(config_path)
        .canonicalize()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let p = config_dir.join(rel);
    if p.exists() {
        Some(p.to_string_lossy().into_owned())
    } else {
        None
    }
}
