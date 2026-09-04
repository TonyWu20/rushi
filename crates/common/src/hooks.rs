//! `hooks` — lifecycle-window dispatcher and decision types.
//!
//! A hook is a short-lived command (like a stage binary). The harness
//! spawns it at a fixed window, feeding it one JSON object on stdin
//! and reading one JSON decision from stdout. It exits 0 or 2.
//!
//! See `docs/loop-lifecycle-hooks.md` for the full window list,
//! decision vocabulary, and ABI.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::Value;

/// A named lifecycle window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Window {
    SessionStart,
    SessionEnd,
    StepStart,
    StepEnd,
    ModelBefore,
    ModelAfter,
    CompactBefore,
    CompactAfter,
    OverflowResolve,
    ExhaustedHandle,
    ToolBefore,
    ToolAfter,
    RunIdle,
}

impl Window {
    /// The stable string name of the window (used in config and logs).
    pub fn name(&self) -> &'static str {
        match self {
            Window::SessionStart => "session.start",
            Window::SessionEnd => "session.end",
            Window::StepStart => "step.start",
            Window::StepEnd => "step.end",
            Window::ModelBefore => "model.before",
            Window::ModelAfter => "model.after",
            Window::CompactBefore => "compact.before",
            Window::CompactAfter => "compact.after",
            Window::OverflowResolve => "overflow.resolve",
            Window::ExhaustedHandle => "exhausted.handle",
            Window::ToolBefore => "tool.before",
            Window::ToolAfter => "tool.after",
            Window::RunIdle => "run.idle",
        }
    }

    /// Parse a window name from a config string.
    pub fn parse(s: &str) -> Option<Window> {
        match s {
            "session.start" => Some(Window::SessionStart),
            "session.end" => Some(Window::SessionEnd),
            "step.start" => Some(Window::StepStart),
            "step.end" => Some(Window::StepEnd),
            "model.before" => Some(Window::ModelBefore),
            "model.after" => Some(Window::ModelAfter),
            "compact.before" => Some(Window::CompactBefore),
            "compact.after" => Some(Window::CompactAfter),
            "overflow.resolve" => Some(Window::OverflowResolve),
            "exhausted.handle" => Some(Window::ExhaustedHandle),
            "tool.before" => Some(Window::ToolBefore),
            "tool.after" => Some(Window::ToolAfter),
            "run.idle" => Some(Window::RunIdle),
            _ => None,
        }
    }
}

/// One hook registration from the `[hooks]` config.
#[derive(Clone, Debug)]
pub struct HookRegistration {
    pub window: Window,
    pub command: String,
    pub args: Vec<String>,
}

/// The result of firing a single hook at a window.
#[derive(Clone, Debug, Default)]
pub struct HookResult {
    /// The decision word from the hook, or `None` for the default.
    pub decision: Option<String>,
    /// The payload of the decision (e.g. `reason`, `message`, `calls`).
    pub payload: Value,
    /// True when the hook exited 2 (blocking default for the window).
    pub blocking_default: bool,
    /// True when the hook failed with an unexpected exit code.
    pub failed: bool,
    /// The error detail when `failed` is true.
    pub failure_detail: String,
}

/// The default decision for a window when no hook answers.
pub fn window_default(window: Window) -> &'static str {
    match window {
        Window::OverflowResolve | Window::ExhaustedHandle => "stay_compact",
        Window::ToolBefore => "proceed",
        Window::RunIdle => "stop",
        Window::CompactBefore => "proceed",
        _ => "noop",
    }
}

/// The blocking decision a window applies when a hook exits 2.
///
/// A hook that exits 2 aborts the window's default action
/// (docs/loop-lifecycle-hooks.md section 4.3). The blocking word is
/// per window: `tool.before` blocks the batch, `run.idle` stops the
/// loop, `overflow.resolve` and `exhausted.handle` stop the strategy
/// cycle, and `compact.before` cancels the compaction. Observation
/// windows have no default action to abort.
pub fn window_blocking_default(window: Window) -> Option<&'static str> {
    match window {
        Window::ToolBefore => Some("block"),
        Window::RunIdle => Some("stop"),
        Window::OverflowResolve | Window::ExhaustedHandle => Some("stop"),
        Window::CompactBefore => Some("cancel"),
        _ => None,
    }
}

/// Fold the results of a window firing into the effective decision.
///
/// Order of precedence: the first explicit decision from a hook that
/// did not fail, then the blocking default when any hook exited 2,
/// then `None` for the window default. The second return is the
/// payload of the chosen decision (null for the defaults).
pub fn fold_decision(results: &[HookResult], window: Window) -> (Option<String>, Value) {
    for r in results {
        if !r.failed && r.decision.is_some() {
            return (r.decision.clone(), r.payload.clone());
        }
    }
    if results.iter().any(|r| r.blocking_default) {
        if let Some(word) = window_blocking_default(window) {
            return (Some(word.to_string()), Value::Null);
        }
    }
    (None, Value::Null)
}

/// True when a hook in the results produced an explicit decision or
/// a blocking default. Callers log the `hook.<window>` decision
/// marker only in that case: a no-hooks run logs nothing, so the
/// default path stays byte-identical (docs/loop-lifecycle-hooks.md
/// section 11).
pub fn decision_produced(results: &[HookResult]) -> bool {
    results
        .iter()
        .any(|r| r.decision.is_some() || r.blocking_default)
}

/// Build the env pairs passed to hooks: `SESSION`, `SESSIONS_ROOT`,
/// `CONFIG`, `HARNESS_PHASE`, `HARNESS_WINDOW`.
pub fn hook_env(
    session: &str,
    sessions_root: &str,
    config: &str,
    phase: &str,
    window: Window,
) -> Vec<(&'static str, String)> {
    vec![
        ("SESSION", session.to_string()),
        ("SESSIONS_ROOT", sessions_root.to_string()),
        ("CONFIG", config.to_string()),
        ("HARNESS_PHASE", phase.to_string()),
        ("HARNESS_WINDOW", window.name().to_string()),
    ]
}

/// Fire all hooks registered on `window` in order.
///
/// Returns one `HookResult` per hook that was fired. The caller
/// inspects the results to find the first non-default decision and
/// logs any failures.
pub fn fire_hooks(
    hooks: &[HookRegistration],
    window: Window,
    stdin_payload: &Value,
    env: &[(&str, String)],
    timeout_ms: u64,
) -> Vec<HookResult> {
    let input = serde_json::to_string(stdin_payload).unwrap_or_default();
    let mut results = Vec::new();

    for h in hooks.iter().filter(|h| h.window == window) {
        let result = fire_one(h, &input, env, timeout_ms);
        results.push(result);
    }

    results
}

/// Fire a single hook command and interpret its output.
///
/// `timeout_ms` bounds the hook lifetime: at the deadline the child
/// is killed and the result is a failure that carries the timeout
/// detail. A hook must never wedge the loop
/// (docs/loop-lifecycle-hooks.md section 4.3). Zero means no bound.
fn fire_one(
    h: &HookRegistration,
    input: &str,
    env: &[(&str, String)],
    timeout_ms: u64,
) -> HookResult {
    use std::io::Write;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    let mut cmd = Command::new(&h.command);
    cmd.args(&h.args);
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return HookResult {
                decision: None,
                payload: Value::Null,
                blocking_default: false,
                failed: true,
                failure_detail: format!("spawn {}: {e}", h.command),
            };
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }

    // The watchdog kills the child at the deadline. It kills by pid,
    // so it runs while the main thread owns the child. A kill that
    // lands (a live child) sets the flag; a kill that misses (the
    // hook already exited) leaves it clear.
    let pid = child.id();
    let timed_out = Arc::new(AtomicBool::new(false));
    let watchdog = if timeout_ms > 0 {
        let flag = Arc::clone(&timed_out);
        let pid = pid;
        Some(std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(timeout_ms));
            let killed = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
            if killed == 0 {
                flag.store(true, Ordering::SeqCst);
            }
        }))
    } else {
        None
    };

    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => {
            if let Some(w) = watchdog {
                let _ = w.join();
            }
            return HookResult {
                decision: None,
                payload: Value::Null,
                blocking_default: false,
                failed: true,
                failure_detail: format!("wait {}: {e}", h.command),
            };
        }
    };

    if let Some(w) = watchdog {
        let _ = w.join();
    }

    if timed_out.load(Ordering::SeqCst) {
        return HookResult {
            decision: None,
            payload: Value::Null,
            blocking_default: false,
            failed: true,
            failure_detail: format!("hook timed out after {} ms", timeout_ms),
        };
    }

    let exit_code = output.status.code().unwrap_or(-1);

    match exit_code {
        0 => {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if stdout.is_empty() || stdout == "{}" {
                HookResult {
                    decision: None,
                    payload: Value::Null,
                    blocking_default: false,
                    failed: false,
                    failure_detail: String::new(),
                }
            } else {
                match serde_json::from_str::<Value>(&stdout) {
                    Ok(val) => {
                        let decision = val
                            .get("decision")
                            .and_then(|d| d.as_str())
                            .map(|s| s.to_string());
                        let payload = val.get("payload").cloned().unwrap_or(Value::Null);
                        HookResult {
                            decision,
                            payload,
                            blocking_default: false,
                            failed: false,
                            failure_detail: String::new(),
                        }
                    }
                    Err(_) => HookResult {
                        decision: None,
                        payload: Value::Null,
                        blocking_default: false,
                        failed: true,
                        failure_detail: format!("hook produced non-JSON stdout: {stdout}"),
                    },
                }
            }
        }
        2 => HookResult {
            decision: None,
            payload: Value::Null,
            blocking_default: true,
            failed: false,
            failure_detail: String::new(),
        },
        other => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            HookResult {
                decision: None,
                payload: Value::Null,
                blocking_default: false,
                failed: true,
                failure_detail: format!("exit {other}: {stderr}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_roundtrip() {
        for w in [
            Window::SessionStart,
            Window::RunIdle,
            Window::ExhaustedHandle,
            Window::ToolBefore,
        ] {
            assert_eq!(Window::parse(w.name()), Some(w));
        }
        assert_eq!(Window::parse("bogus"), None);
    }

    #[test]
    fn window_defaults() {
        assert_eq!(window_default(Window::OverflowResolve), "stay_compact");
        assert_eq!(window_default(Window::ToolBefore), "proceed");
        assert_eq!(window_default(Window::RunIdle), "stop");
        assert_eq!(window_default(Window::CompactBefore), "proceed");
    }

    #[test]
    fn blocking_defaults() {
        assert_eq!(window_blocking_default(Window::ToolBefore), Some("block"));
        assert_eq!(window_blocking_default(Window::OverflowResolve), Some("stop"));
        assert_eq!(window_blocking_default(Window::ExhaustedHandle), Some("stop"));
        assert_eq!(window_blocking_default(Window::RunIdle), Some("stop"));
        assert_eq!(window_blocking_default(Window::CompactBefore), Some("cancel"));
        assert_eq!(window_blocking_default(Window::ModelBefore), None);
    }

    #[test]
    fn fold_prefers_the_first_explicit_decision() {
        let results = vec![
            HookResult {
                decision: None,
                payload: Value::Null,
                blocking_default: false,
                failed: false,
                failure_detail: String::new(),
            },
            HookResult {
                decision: Some("stop".to_string()),
                payload: Value::Null,
                blocking_default: false,
                failed: false,
                failure_detail: String::new(),
            },
        ];
        let (d, _) = fold_decision(&results, Window::ExhaustedHandle);
        assert_eq!(d.as_deref(), Some("stop"));
    }

    #[test]
    fn fold_maps_exit_2_to_the_window_blocking_word() {
        let results = vec![HookResult {
            decision: None,
            payload: Value::Null,
            blocking_default: true,
            failed: false,
            failure_detail: String::new(),
        }];
        let (d, _) = fold_decision(&results, Window::ToolBefore);
        assert_eq!(d.as_deref(), Some("block"));
        let (d, _) = fold_decision(&results, Window::RunIdle);
        assert_eq!(d.as_deref(), Some("stop"));
    }

    #[test]
    fn fold_failed_hooks_yield_no_decision() {
        let results = vec![HookResult {
            decision: Some("stop".to_string()),
            payload: Value::Null,
            blocking_default: false,
            failed: true,
            failure_detail: "exit 3".to_string(),
        }];
        let (d, _) = fold_decision(&results, Window::ExhaustedHandle);
        assert_eq!(d, None);
    }

    #[test]
    fn a_slow_hook_times_out() {
        let reg = HookRegistration {
            window: Window::ToolBefore,
            command: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), "sleep 5".to_string()],
        };
        let results = fire_hooks(&[reg], Window::ToolBefore, &Value::Null, &[], 200);
        assert_eq!(results.len(), 1);
        assert!(results[0].failed, "the kill must mark the hook failed");
        assert!(
            results[0].failure_detail.contains("timed out"),
            "{:#?}",
            results[0]
        );
    }
}
