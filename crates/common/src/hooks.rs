//! `hooks` — lifecycle-window dispatcher and decision types.
//!
//! A hook is a short-lived command (like a stage binary). The harness
//! spawns it at a fixed window, feeding it one JSON object on stdin
//! and reading one JSON decision from stdout. It exits 0 or 2.
//!
//! See `docs/loop-lifecycle-hooks.md` for the full window list,
//! decision vocabulary, and ABI.

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
    _timeout_ms: u64,
) -> Vec<HookResult> {
    let input = serde_json::to_string(stdin_payload).unwrap_or_default();
    let mut results = Vec::new();

    for h in hooks.iter().filter(|h| h.window == window) {
        let result = fire_one(h, &input, env);
        results.push(result);
    }

    results
}

/// Fire a single hook command and interpret its output.
fn fire_one(
    h: &HookRegistration,
    input: &str,
    env: &[(&str, String)],
) -> HookResult {
    use std::io::Write;
    use std::process::{Command, Stdio};

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

    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => {
            return HookResult {
                decision: None,
                payload: Value::Null,
                blocking_default: false,
                failed: true,
                failure_detail: format!("wait {}: {e}", h.command),
            };
        }
    };

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
}
