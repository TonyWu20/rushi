//! The `rushi run` turn loop (docs/phase-2-plan.md section 4.1).
//!
//! Replaces `scripts/turn.sh`. Acquires the session lock, then loops:
//! step → claim → (`run.idle` window) → repeat until idle-without-follow
//! or exhausted.
//!
//! # Exit codes
//! - `0` — clean stop (idle with no pending follow-ups, exhausted, or a
//!   logged terminal error event)
//! - `1` — hard failure (config unreadable, log append fails, lock held)
//! - `143` — SIGTERM
//! - `130` — SIGINT

use std::path::Path;

use rushi_common::event::{Event, UserMessage};
use rushi_common::hooks::Window;
use rushi_common::stage::{Claim, SessionDir, StageRunner};

use crate::config::HarnessConfig;
use crate::signals;
use crate::step::hook::{log_pipeline, run_window};
use crate::step::{append_line, make_runner, StepMode};

/// The `rushi run` entry point.
///
/// `task`: optional initial prompt (`rushi run SESSION [TASK]`). When
/// present it is logged as a steer `user_message` before the loop
/// starts, so `claim` reports `awaiting_model` and the first step runs
/// a model turn on it — the subagent spawn contract
/// (docs/subagent-design.md section 4). `no_run`: log the task and
/// exit without running the loop.
pub fn run(cfg: &HarnessConfig, session_dir: &Path, task: Option<&str>, no_run: bool) {
    // The session dir must exist (create it).
    if let Err(e) = std::fs::create_dir_all(session_dir) {
        eprintln!("rushi: cannot create session dir: {e}");
        std::process::exit(1);
    }

    // Acquire the exclusive session lock for the process life.
    acquire_lock(session_dir);

    // Write loop.pid after the lock. The harness is the only writer
    // (the TUI probe reads it; the TUI no longer writes it).
    let my_pid = std::process::id();
    if let Err(e) = std::fs::write(session_dir.join("loop.pid"), format!("{my_pid}\n")) {
        eprintln!("rushi: warning: cannot write loop.pid: {e}");
    }

    // Install signal handlers (SIGTERM → 143, SIGINT → 130).
    signals::install();

    // Seed the initial user message before the first step. A steer
    // message leaves the claim in `awaiting_model`, so the loop runs
    // the prompt instead of idling out.
    if let Some(task) = task {
        if task.trim().is_empty() {
            eprintln!("rushi: task must not be empty");
            std::process::exit(1);
        }
        let line = seed_initial_message(cfg, session_dir, task);
        if no_run {
            println!("{line}");
            std::process::exit(0);
        }
    }

    let runner = make_runner(cfg);
    let session = SessionDir {
        path: session_dir.to_path_buf(),
    };

    // Fire the session.start window (observation only).
    fire_observation(cfg, &session, Window::SessionStart, &serde_json::json!({
        "window": "session.start",
        "session": session.path.to_string_lossy(),
    }));

    // The turn loop.
    loop {
        crate::step::do_step(cfg, session_dir, StepMode::Run);

        // Fire step.end (observation, after the step completes).
        fire_observation(cfg, &session, Window::StepEnd, &serde_json::json!({
            "window": "step.end",
            "session": session.path.to_string_lossy(),
        }));

        // Check for a caught signal between steps.
        if let Some(sig) = signals::poll() {
            fire_session_end(cfg, &session, "signal");
            crate::stage_runner::cancel_live_child();
            std::process::exit(sig.run_exit_code());
        }

        let claim: Claim = match runner.claim(&session) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("rushi: claim failed: {e}");
                std::process::exit(1);
            }
        };

        if claim.state == "idle" && claim.pending_follow_ups.is_empty() {
            // Fire the run.idle pipeline before stopping
            // (docs/loop-lifecycle-hooks.md 12.5): the hook appends
            // its own follow-up `user_message` to the session log via
            // the `LOG_BIN` env var, and the loop drains pending
            // messages on the next iteration. An `abort` (exit 2)
            // vetoes the default stop and keeps the loop alive; a
            // failed chain stops the loop (P4).
            let last_msg_id = read_last_assistant_message_id(session_dir);
            let payload = serde_json::json!({
                "window": "run.idle",
                "session": session.path.to_string_lossy(),
                "last_assistant_message_id": last_msg_id,
            });
            let run = run_window(cfg, &session, Window::RunIdle, &payload, "run");
            let keep_alive = if run.failed {
                false
            } else if run.aborted {
                true
            } else if !run.steps.is_empty() {
                // The pipeline may have appended a follow-up
                // `user_message` itself; drain it on the next turn.
                matches!(
                    runner.claim(&session),
                    Ok(c) if !c.pending_follow_ups.is_empty() || c.state != "idle"
                )
            } else {
                false
            };
            let resolution = if keep_alive { "continue" } else { "stop" };
            log_pipeline(cfg, &session, Window::RunIdle, &run, Some(resolution), Some(&payload));
            if keep_alive {
                continue;
            }
            // The window default (or a failed chain): stop the loop.
            fire_session_end(cfg, &session, "idle");
            break;
        }

        if claim.state == "exhausted" {
            fire_session_end(cfg, &session, "exhausted");
            break;
        }

        // awaiting_model / awaiting_tool_result / awaiting_approval:
        // the next iteration steps into the owed work.
    }

    std::process::exit(0);
}

/// Acquire the exclusive `flock` on `sessions/<n>/.loop.lock`. Exits 1
/// when a live loop holds it, naming the holder from `loop.pid`.
fn acquire_lock(session_dir: &Path) {
    let lock_path = session_dir.join(".loop.lock");
    let lock_file = match std::fs::File::create(&lock_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("rushi: cannot create lock file: {e}");
            std::process::exit(1);
        }
    };

    use std::os::unix::io::AsRawFd;
    let fd = lock_file.as_raw_fd();
    let locked: i32 = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if locked != 0 {
        let pid_hint = std::fs::read_to_string(session_dir.join("loop.pid"))
            .ok()
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if pid_hint.is_empty() {
            eprintln!("rushi: session lock is held by another loop");
        } else {
            eprintln!(
                "rushi: session lock is held by another loop (pid {pid_hint})"
            );
        }
        // Keep the fd open for the process life so the lock is held.
        std::mem::forget(lock_file);
        std::process::exit(1);
    }

    // Hold the lock for the process life: forget the guard so the fd
    // (and the flock) are never released by a drop.
    std::mem::forget(lock_file);
}

/// Log the initial `user_message` into a fresh (or resumed) session and
/// return the committed JSON line.
///
/// Mirrors what `bin/user` does for the interactive flow, in-process —
/// this is what keeps `rushi` self-contained for seeding a session when
/// the `user` binary is not shipped (plain `install.sh` ships only the
/// `rushi` binary; docs/subagent-design.md section 4).
///
/// - The message is a steer `user_message` (no `queue` field), so
///   `claim` reports `awaiting_model` and the loop's first step runs a
///   model turn on it.
/// - The entry point owns the `cwd` decision: the working directory is
///   recorded in `sessions/<n>/cwd` only when the file does not yet
///   exist (same rule as `bin/user`; `assemble`/`route` only read it).
fn seed_initial_message(cfg: &HarnessConfig, session_dir: &Path, task: &str) -> String {
    let cwd_path = session_dir.join("cwd");
    if !cwd_path.exists() {
        if let Ok(cwd) = std::env::current_dir() {
            if let Err(e) = std::fs::write(&cwd_path, cwd.to_string_lossy().as_bytes()) {
                eprintln!("rushi: warning: cannot write cwd file: {e}");
            }
        }
    }

    let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let event = Event::UserMessage(UserMessage {
        v: 1,
        ts,
        content: task.to_string(),
        queue: None,
        id: None,
    });
    let line = serde_json::to_string(&event)
        .unwrap_or_else(|e| {
            eprintln!("rushi: cannot serialize user_message: {e}");
            std::process::exit(1);
        });
    append_line(cfg, session_dir, &line);
    line
}

/// Fire a fire-and-forget observation window pipeline (phase `run`)
/// and log its results. A window with no pipeline entry logs nothing
/// (the byte-identical no-hooks default, P1).
fn fire_observation(
    cfg: &HarnessConfig,
    session: &SessionDir,
    window: Window,
    payload: &serde_json::Value,
) {
    let run = run_window(cfg, session, window, payload, "run");
    log_pipeline(cfg, session, window, &run, None, None);
}

/// Fire the session.end window (observation only).
fn fire_session_end(cfg: &HarnessConfig, session: &SessionDir, reason: &str) {
    let payload = serde_json::json!({
        "window": "session.end",
        "session": session.path.to_string_lossy(),
        "reason": reason,
    });
    fire_observation(cfg, session, Window::SessionEnd, &payload);
}

/// Read the id of the last `assistant_message` from the log.
fn read_last_assistant_message_id(session_dir: &Path) -> Option<String> {
    let path = session_dir.join("events.jsonl");
    let data = std::fs::read_to_string(&path).ok()?;
    for line in data.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) == Some("assistant_message") {
            return v.get("id").and_then(|i| i.as_str()).map(String::from);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A minimal in-memory config for seeding tests: no model calls,
    /// no hooks, no tool paths.
    fn test_cfg(root: &Path) -> HarnessConfig {
        HarnessConfig {
            config_path: root.join("config.toml"),
            config_dir: root.to_path_buf(),
            sessions_root: root.join("sessions"),
            native_tool_paths: Vec::new(),
            extension_tool_paths: Vec::new(),
            active_model: "stub".into(),
            model_id: "stub".into(),
            max_output_tokens: 32768,
            context_tokens: 8192,
            input_budget: 1000,
            context_budget: 1000,
            last_measured_input: 0,
            compact_enabled: false,
            compact_strategy: "compact".into(),
            compact_reserve_tokens: 16384,
            compact_keep_tokens: 20000,
            compact_text_chars: 200,
            estimate_chars_per_token: 4,
            approval_timeout_s: None,
            hooks_timeout_ms: 30000,
            hook_defs: std::collections::BTreeMap::new(),
            hook_pipelines: std::collections::BTreeMap::new(),
            model_bin: PathBuf::from("model"),
            compact_bin: PathBuf::from("compact"),
            assemble_bin: PathBuf::from("assemble"),
            route_bin: PathBuf::from("route"),
            claim_bin: PathBuf::from("claim"),
            parse_bin: PathBuf::from("parse"),
            log_bin: PathBuf::from("log"),
        }
    }

    #[test]
    fn seed_initial_message_appends_steer_user_message() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = test_cfg(dir.path());
        let sess = dir.path().join("sess");
        std::fs::create_dir_all(&sess).unwrap();

        let line = seed_initial_message(&cfg, &sess, "do the thing");
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["type"], "user_message");
        assert_eq!(v["content"], "do the thing");
        assert_eq!(v["v"], 1);
        assert!(v.get("queue").is_none(), "steer queue leaves the field absent");
        // The typed validator (P8) accepts the seeded line.
        assert!(rushi_common::event::parse_event(&line).is_ok());
        // The event was committed to the session log.
        let log = std::fs::read_to_string(sess.join("events.jsonl")).unwrap();
        assert!(log.lines().any(|l| l == line));
    }

    #[test]
    fn seed_initial_message_records_cwd_once() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = test_cfg(dir.path());
        let sess = dir.path().join("sess");
        std::fs::create_dir_all(&sess).unwrap();
        // Pre-existing cwd file (an earlier entry point owned it): kept.
        std::fs::write(sess.join("cwd"), "/pre-existing\n").unwrap();
        seed_initial_message(&cfg, &sess, "task");
        assert_eq!(
            std::fs::read_to_string(sess.join("cwd")).unwrap(),
            "/pre-existing\n"
        );

        // Fresh session: the entry point records the CWD.
        let sess2 = dir.path().join("sess2");
        std::fs::create_dir_all(&sess2).unwrap();
        seed_initial_message(&cfg, &sess2, "task");
        let cwd = std::fs::read_to_string(sess2.join("cwd")).unwrap();
        assert_eq!(cwd, std::env::current_dir().unwrap().to_string_lossy());
    }
}
