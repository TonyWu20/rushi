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
//!   is first logged as the session's `user_message` (steer queue) so
//!   the loop starts by running a model turn on it — the subagent
//!   spawn contract (docs/subagent-design.md section 4). The call
//!   branches on the session lock: a live loop holds it, so the task
//!   is appended with the log-line lock only and the call exits 0
//!   (the live loop drains the message at its next step). No live
//!   loop: the call starts the loop. This is the single canonical poke
//!   of a session; the caller never branches on loop state.
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
#[command(name = "rushi", about = "The rushi distribution: loop engine, tools, and project setup", version)]
struct Args {
    /// Path to config file (for the loop and `config` subcommands).
    /// Outranks the `$CONFIG` env var; when omitted, falls back to
    /// `$CONFIG`, then the Nix side-by-side config, then CWD.
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
    /// `user_message` (steer queue). The call branches on the session
    /// lock: a live loop holds it, so the task is appended with the
    /// log-line lock only and the call exits 0 — the live loop drains
    /// the message at its next step. No live loop: the call starts the
    /// loop, which logs the task and runs it. This is the single
    /// canonical poke of a session; the caller never branches on loop
    /// state. The call keeps `rushi` self-contained for seeding a
    /// session without the separate `user` binary, which plain
    /// installs do not ship (the subagent spawn contract,
    /// docs/subagent-design.md section 4).
    Run {
        /// Session name or directory
        session: String,

        /// Prompt, logged as a `user_message` (steer queue). Omit to
        /// continue from the log's current state. Against a live loop
        /// the prompt is appended and the call exits 0 without
        /// starting a loop; the live loop drains it at its next step.
        task: Option<String>,

        /// Override `[paths] sessions_root` from the config for this
        /// call. Absolute paths are used as-is; relative paths resolve
        /// against the current directory. This lets `rushi run` target
        /// a session tree from anywhere without switching to the
        /// workspace first.
        #[arg(long)]
        sessions_root: Option<PathBuf>,

        /// Set the working directory the session's tools run in,
        /// overriding the call-site directory. Absolute paths are
        /// used as-is; relative paths resolve against the current
        /// directory. When omitted, the working directory is the
        /// parent of a `--sessions-root` override, or the call-site
        /// directory when no override is given.
        #[arg(long)]
        cwd: Option<PathBuf>,
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
    /// Resolves the config like the loop subcommands (`--config`, then
    /// `$CONFIG`, then the Nix side-by-side layout, then CWD) and
    /// prints its raw contents. Useful for dumping the active config to
    /// disk for tweaking, e.g. `rushi config > my-config.toml`.
    Config,
}

fn main() {
    let args = Args::parse();

    // The --config flag sets the config path; the CONFIG env var is
    // the fallback when the flag is omitted (issue #36).
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

        Command::Run {
            session,
            task,
            sessions_root,
            cwd,
        } => {
            let mut cfg = config::HarnessConfig::load(&PathBuf::from(config_path));

            // The --sessions-root override (if any) resolves to an
            // absolute path. It becomes the basis for the default
            // working directory below.
            let flag_sessions_root = match &sessions_root {
                Some(root) => {
                    let resolved = config::resolve_dir_override(root);
                    cfg.sessions_root = resolved.clone();
                    Some(resolved)
                }
                None => None,
            };

            // Effective working directory for the call. Precedence:
            // the --cwd flag, else the parent of a --sessions-root
            // override, else the live process CWD (handled inside
            // run).
            let work_dir =
                config::working_dir_for(cwd.as_deref(), flag_sessions_root.as_deref());

            let session_dir = cfg.resolve_session(&session);
            signals::install();
            run_loop::run(
                &cfg,
                &session,
                &session_dir,
                task.as_deref(),
                work_dir.as_deref(),
            );
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// The `--sessions-root` flag parses and lands on the Run variant.
    #[test]
    fn run_accepts_sessions_root_flag() {
        let args = Args::try_parse_from([
            "rushi",
            "run",
            "mysess",
            "--sessions-root",
            "/x/sessions",
        ])
        .unwrap();
        match args.command.unwrap() {
            Command::Run {
                session,
                task,
                sessions_root,
                ..
            } => {
                assert_eq!(session, "mysess");
                assert!(task.is_none());
                assert_eq!(sessions_root, Some(PathBuf::from("/x/sessions")));
            }
            _ => panic!("expected the Run subcommand"),
        }
    }

    /// Without the flag, `sessions_root` is `None` (config value wins).
    #[test]
    fn run_defaults_sessions_root_to_none() {
        let args = Args::try_parse_from(["rushi", "run", "mysess"]).unwrap();
        match args.command.unwrap() {
            Command::Run {
                session,
                task,
                sessions_root,
                ..
            } => {
                assert_eq!(session, "mysess");
                assert!(task.is_none());
                assert!(sessions_root.is_none());
            }
            _ => panic!("expected the Run subcommand"),
        }
    }

    /// A relative value parses and is kept verbatim (resolved to CWD
    /// later, in `resolve_dir_override`).
    #[test]
    fn run_sessions_root_accepts_relative_path() {
        let args = Args::try_parse_from([
            "rushi",
            "run",
            "mysess",
            "hello",
            "--sessions-root",
            "rel/sessions",
        ])
        .unwrap();
        match args.command.unwrap() {
            Command::Run {
                session,
                task,
                sessions_root,
                ..
            } => {
                assert_eq!(session, "mysess");
                assert_eq!(task, Some("hello".to_string()));
                assert_eq!(sessions_root, Some(PathBuf::from("rel/sessions")));
            }
            _ => panic!("expected the Run subcommand"),
        }
    }

    /// The flag shows up in the run help text.
    #[test]
    fn run_help_lists_sessions_root_flag() {
        let cmd = crate::Args::command();
        let mut sub = cmd
            .get_subcommands()
            .find(|c| c.get_name() == "run")
            .cloned()
            .unwrap();
        let rendered = sub.render_help().to_string();
        assert!(
            rendered.contains("sessions-root"),
            "missing flag:\n{rendered}"
        );
    }

    /// The `--cwd` flag parses and is kept verbatim.
    #[test]
    fn run_accepts_cwd_flag() {
        let args = Args::try_parse_from([
            "rushi",
            "run",
            "mysess",
            "--cwd",
            "/w",
        ])
        .unwrap();
        match args.command.unwrap() {
            Command::Run {
                session,
                task,
                sessions_root,
                cwd,
            } => {
                assert_eq!(session, "mysess");
                assert!(task.is_none());
                assert!(sessions_root.is_none());
                assert_eq!(cwd, Some(PathBuf::from("/w")));
            }
            _ => panic!("expected the Run subcommand"),
        }
    }

    /// A relative `--cwd` value is kept verbatim (resolved to CWD
    /// later, in `resolve_dir_override`).
    #[test]
    fn run_cwd_accepts_relative_path() {
        let args = Args::try_parse_from([
            "rushi",
            "run",
            "mysess",
            "--cwd",
            "rel/w",
        ])
        .unwrap();
        match args.command.unwrap() {
            Command::Run { cwd, .. } => {
                assert_eq!(cwd, Some(PathBuf::from("rel/w")));
            }
            _ => panic!("expected the Run subcommand"),
        }
    }
}