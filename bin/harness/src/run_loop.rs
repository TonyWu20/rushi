//! The `harness run` turn loop (docs/phase-2-plan.md section 4.1).
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

use harness_common::hooks::{self, Window};
use harness_common::stage::{Claim, SessionDir, StageRunner};

use crate::config::HarnessConfig;
use crate::signals;
use crate::step::{append_event, hook_env_for_run, make_runner};

/// The `harness run` entry point.
pub fn run(cfg: &HarnessConfig, session_dir: &Path) {
    // The session dir must exist (create it).
    if let Err(e) = std::fs::create_dir_all(session_dir) {
        eprintln!("harness: cannot create session dir: {e}");
        std::process::exit(1);
    }

    // Acquire the exclusive session lock for the process life.
    acquire_lock(session_dir);

    // Write loop.pid after the lock. The harness is the only writer
    // (the TUI probe reads it; the TUI no longer writes it).
    let my_pid = std::process::id();
    if let Err(e) = std::fs::write(session_dir.join("loop.pid"), format!("{my_pid}\n")) {
        eprintln!("harness: warning: cannot write loop.pid: {e}");
    }

    // Install signal handlers (SIGTERM → 143, SIGINT → 130).
    signals::install();

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
        crate::step::do_step(cfg, &session_dir);

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
                eprintln!("harness: claim failed: {e}");
                std::process::exit(1);
            }
        };

        if claim.state == "idle" && claim.pending_follow_ups.is_empty() {
            // Fire the run.idle window before stopping.
            let last_msg_id = read_last_assistant_message_id(session_dir);
            let payload = serde_json::json!({
                "window": "run.idle",
                "session": session.path.to_string_lossy(),
                "last_assistant_message_id": last_msg_id,
            });
            let results = hooks::fire_hooks(
                &cfg.hooks,
                Window::RunIdle,
                &payload,
                &hook_env_for_run(cfg, &session),
                cfg.hooks_timeout_ms,
            );
            log_hook_results(cfg, &session, "run.idle", &results);

            let (decision, payload_val) = first_decision(&results);
            if decision == Some("continue") {
                let message = payload_val
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("")
                    .to_string();
                let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
                let event = serde_json::json!({
                    "v": 1,
                    "type": "user_message",
                    "ts": ts,
                    "content": message,
                    "queue": "follow",
                });
                append_event(cfg, &session.path, &event);
                continue;
            }
            // Decision is `stop` (default) or absent: stop the loop.
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
            eprintln!("harness: cannot create lock file: {e}");
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
            eprintln!("harness: session lock is held by another loop");
        } else {
            eprintln!(
                "harness: session lock is held by another loop (pid {pid_hint})"
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

/// Fire a fire-and-forget observation window and log its failures.
fn fire_observation(
    cfg: &HarnessConfig,
    session: &SessionDir,
    window: Window,
    payload: &serde_json::Value,
) {
    let results = hooks::fire_hooks(
        &cfg.hooks,
        window,
        payload,
        &hook_env_for_run(cfg, session),
        cfg.hooks_timeout_ms,
    );
    log_hook_results(cfg, session, window.name(), &results);
}

/// Fire the session.end window.
fn fire_session_end(cfg: &HarnessConfig, session: &SessionDir, reason: &str) {
    let payload = serde_json::json!({
        "window": "session.end",
        "session": session.path.to_string_lossy(),
        "reason": reason,
    });
    let _ = hooks::fire_hooks(
        &cfg.hooks,
        Window::SessionEnd,
        &payload,
        &hook_env_for_run(cfg, session),
        cfg.hooks_timeout_ms,
    );
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

/// The first non-failed hook decision in the results, with its payload.
fn first_decision(
    results: &[harness_common::hooks::HookResult],
) -> (Option<&str>, serde_json::Value) {
    for r in results {
        if !r.failed {
            if let Some(d) = &r.decision {
                return (Some(d.as_str()), r.payload.clone());
            }
        }
    }
    (None, serde_json::Value::Null)
}

/// Log a hook window's results: one `ext_status` per failure, one for
/// a non-empty decision.
fn log_hook_results(
    cfg: &HarnessConfig,
    session: &SessionDir,
    window: &str,
    results: &[harness_common::hooks::HookResult],
) {
    for r in results {
        if r.failed {
            let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            let event = serde_json::json!({
                "v": 1,
                "type": "ext_status",
                "ts": ts,
                "id": format!("hook.{window}.error"),
                "value": r.failure_detail.clone(),
            });
            append_event(cfg, &session.path, &event);
        }
    }
}
