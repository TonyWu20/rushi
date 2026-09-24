//! `hooks` — lifecycle-window pipeline dispatcher (docs/loop-lifecycle-hooks.md
//! section 12, the pipeline model settled 2026-09-24, issue #38).
//!
//! A hook is a short-lived command (like a stage binary). The harness
//! spawns it at a fixed window, feeding it the accumulated state JSON
//! on stdin and reading one JSON state object from stdout.
//!
//! Windows are pipelines: an ordered list of named step definitions
//! (config `hooks.defs` + `hooks.pipeline`). Steps run sequentially
//! over the accumulated state; step N+1 receives step N's output
//! (matrix-product semantics). The first terminal event stops the
//! chain:
//!
//! - exit 0 — `ok` (a JSON state object on stdout) or `noop`
//!   (empty / `{}` stdout). The step's output becomes the
//!   accumulated state.
//! - exit 2 — `abort`: veto the window's default action. Sticky; the
//!   chain halts. An optional `reason` field on the stdout payload
//!   is carried into the log.
//! - exit 3 — `fail`: step-level failure. The chain halts; the
//!   window falls back to its default.
//! - any other exit, a crash, a spawn error, or a timeout — `fail`
//!   with a detail. The loop never wedges (P4).
//!
//! The exit code is the type tag. The stdout JSON is the data
//! channel and is honored on every status. The per-window decision
//! vocabs are retired: effects travel as concrete state fields, and
//! `abort` is the one generic control word.

use std::collections::BTreeMap;

use serde_json::Value;

/// A named lifecycle window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
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

/// One named hook definition from `[hooks.defs.<name>]`.
///
/// A pipeline (`[hooks.pipeline."<window>"]`) references defs by
/// these names in its ordered `steps` list.
#[derive(Clone, Debug)]
pub struct HookDef {
    /// The def name, as keyed under `hooks.defs`.
    pub name: String,
    /// The program to spawn. A bare name is resolved against the
    /// package layout at config-load time (the sibling `bin/` dir,
    /// then the package `hooks/` dir); a path is used as-is.
    pub command: String,
    pub args: Vec<String>,
    /// Per-def timeout override in ms. `None` = the global
    /// `[hooks] timeout_ms`.
    pub timeout_ms: Option<u64>,
}

/// The outcome of one pipeline step (docs/loop-lifecycle-hooks.md
/// 12.7: `noop` / `ok` / `abort(<reason>)` / `fail(<detail>)`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepStatus {
    /// Exit 0 with empty or `{}` stdout: the step contributed no
    /// state. The accumulated state is unchanged.
    Noop,
    /// Exit 0 with a JSON object on stdout: the step's output became
    /// the accumulated state.
    Ok,
    /// Exit 2: veto the window's default action. Sticky; the chain
    /// stops here.
    Abort,
    /// Exit 3, an unknown exit code, a crash, a spawn error, or a
    /// timeout. The chain stops and the window falls back to its
    /// default.
    Fail,
}

/// One step's result in a pipeline run.
#[derive(Clone, Debug)]
pub struct StepOutcome {
    /// The def name the step ran.
    pub def: String,
    /// The command that ran (for error markers).
    pub command: String,
    pub status: StepStatus,
    /// `Abort`: the optional reason from the payload.
    /// `Fail`: the failure detail. Empty for `Noop` / `Ok`.
    pub detail: String,
}

/// The outcome of running one window's pipeline.
#[derive(Clone, Debug)]
pub struct PipelineRun {
    pub window: Window,
    /// One entry per step that actually ran, in list order. Steps
    /// after the stop point never run and have no entry.
    pub steps: Vec<StepOutcome>,
    /// The index (into `steps`) of the step that stopped the chain.
    /// `None` when the whole list completed.
    pub stop: Option<usize>,
    /// True when the chain stopped on an `Abort` (sticky veto).
    pub aborted: bool,
    /// True when the chain stopped on a step failure.
    pub failed: bool,
    /// The accumulated state after the last step that ran. On an
    /// abort or failure this is the state *before* the stopping
    /// step's effect — the window resolves to its default.
    pub state: Value,
    /// The per-step output states (the accumulated state right after
    /// each step that produced one), for the caller's own
    /// post-processing (for example the `model.before`
    /// `prompt_fragments` join across the whole chain).
    pub step_outputs: Vec<Value>,
}

impl PipelineRun {
    /// True when every listed step ran and none aborted or failed.
    pub fn completed(&self) -> bool {
        !self.aborted && !self.failed
    }

    /// The chain marker value for `hook.<window>.chain`: an ordered
    /// list of per-step outcome strings plus the stop point.
    /// (docs/loop-lifecycle-hooks.md 12.7.)
    pub fn chain_value(&self) -> Value {
        let steps = self
            .steps
            .iter()
            .map(|s| match s.status {
                StepStatus::Noop => "noop".to_string(),
                StepStatus::Ok => "ok".to_string(),
                StepStatus::Abort => {
                    if s.detail.is_empty() {
                        "abort".to_string()
                    } else {
                        format!("abort({})", s.detail)
                    }
                }
                StepStatus::Fail => format!("fail({})", s.detail),
            })
            .collect::<Vec<_>>();
        let stop_kind = if self.aborted {
            "abort"
        } else if self.failed {
            "fail"
        } else {
            "complete"
        };
        serde_json::json!({
            "steps": steps,
            "stop": self.stop,
            "stop_kind": stop_kind,
        })
    }
}

/// The default decision for a window when no hook answers.
pub fn window_default(window: Window) -> &'static str {
    match window {
        Window::OverflowResolve | Window::ExhaustedHandle => "stay_compact",
        Window::ToolBefore => "proceed",
        Window::RunIdle => "stop",
        Window::CompactBefore => "proceed",
        Window::ModelBefore => "proceed",
        _ => "noop",
    }
}

/// Build the env pairs passed to hooks: `SESSION`, `SESSIONS_ROOT`,
/// `CONFIG`, `HARNESS_PHASE`, `HARNESS_WINDOW`, and `LOG_BIN`.
///
/// `SESSION` and `SESSIONS_ROOT` are absolute paths resolved at
/// config-load time (issue #16). Consumers must not re-anchor them
/// on the config file's directory.
///
/// `LOG_BIN` is the `log` stage binary, so a hook may append events
/// to the session log itself (for example the `run.idle` follow-up
/// `user_message`, docs/loop-lifecycle-hooks.md 12.5).
pub fn hook_env(
    session: &str,
    sessions_root: &str,
    config: &str,
    phase: &str,
    window: Window,
    log_bin: &str,
) -> Vec<(&'static str, String)> {
    vec![
        ("SESSION", session.to_string()),
        ("SESSIONS_ROOT", sessions_root.to_string()),
        ("CONFIG", config.to_string()),
        ("HARNESS_PHASE", phase.to_string()),
        ("HARNESS_WINDOW", window.name().to_string()),
        ("LOG_BIN", log_bin.to_string()),
    ]
}

/// Fire one pipeline step: spawn the def's command, feed it the
/// accumulated state on stdin, and interpret the typed exit status.
///
/// Returns the step outcome plus, on success or abort, the step's
/// stdout state object (the data channel is honored on every
/// status). The watchdog kills the child at the deadline: a timed-out
/// step is a failure that carries the timeout detail, and the loop
/// never wedges (P5, P4).
fn fire_step(
    def: &HookDef,
    state: &Value,
    env: &[(&str, String)],
    timeout_ms: u64,
) -> (StepStatus, Value, String) {
    use std::io::Write;
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    let input = serde_json::to_string(state).unwrap_or_default();

    let mut cmd = Command::new(&def.command);
    cmd.args(&def.args);
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return (
                StepStatus::Fail,
                Value::Null,
                format!("spawn {}: {e}", def.command),
            );
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }

    // The watchdog kills the child at the deadline. It kills by pid,
    // so it runs while the main thread owns the child. A kill that
    // lands (a live child) sets the flag; a kill that misses (the
    // hook already exited) leaves it clear.
    //
    // The watchdog waits on a channel with recv_timeout rather than a
    // bare sleep, so the join returns immediately when the child
    // finishes early instead of blocking for the full timeout.
    let pid = child.id();
    let timed_out = Arc::new(AtomicBool::new(false));
    let (mut stop_tx, watchdog) = if timeout_ms > 0 {
        let flag = Arc::clone(&timed_out);
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            match rx.recv_timeout(Duration::from_millis(timeout_ms)) {
                Ok(()) => {}
                Err(_) => {
                    let killed = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
                    if killed == 0 {
                        flag.store(true, Ordering::SeqCst);
                    }
                }
            }
        });
        (Some(tx), Some(handle))
    } else {
        (None, None)
    };

    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => {
            if let Some(tx) = &mut stop_tx {
                let _ = tx.send(());
            }
            if let Some(w) = watchdog {
                let _ = w.join();
            }
            return (
                StepStatus::Fail,
                Value::Null,
                format!("wait {}: {e}", def.command),
            );
        }
    };

    if let Some(tx) = &mut stop_tx {
        let _ = tx.send(());
    }
    if let Some(w) = watchdog {
        let _ = w.join();
    }

    if timed_out.load(Ordering::SeqCst) {
        return (
            StepStatus::Fail,
            Value::Null,
            format!("timed out after {timeout_ms} ms"),
        );
    }

    let exit_code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    // The data channel: the stdout JSON, honored on every status.
    let state_out: Value = if stdout.is_empty() || stdout == "{}" {
        Value::Null
    } else {
        serde_json::from_str(&stdout).unwrap_or(Value::Null)
    };

    match exit_code {
        0 => {
            if stdout.is_empty() || stdout == "{}" {
                (StepStatus::Noop, Value::Null, String::new())
            } else if state_out.is_null() {
                // Exit 0 with non-JSON stdout: the step claimed ok but
                // gave no state. A malformed transform would poison
                // the chain, so it is a step failure (P4: the window
                // falls back to its default).
                (
                    StepStatus::Fail,
                    Value::Null,
                    format!("non-JSON stdout: {stdout}"),
                )
            } else {
                (StepStatus::Ok, state_out, String::new())
            }
        }
        2 => {
            let reason = state_out
                .get("reason")
                .and_then(|r| r.as_str())
                .map(String::from)
                .unwrap_or_default();
            (StepStatus::Abort, state_out, reason)
        }
        3 => {
            let detail = state_out
                .get("reason")
                .and_then(|r| r.as_str())
                .map(String::from)
                .unwrap_or_else(|| "step failed (exit 3)".to_string());
            (StepStatus::Fail, state_out, detail)
        }
        _ => {
            let detail = if state_out.is_object() {
                state_out
                    .get("reason")
                    .and_then(|r| r.as_str())
                    .map(String::from)
                    .unwrap_or_else(|| format!("unknown exit {exit_code}: {stderr}"))
            } else {
                format!("unknown exit {exit_code}: {stderr}")
            };
            (StepStatus::Fail, state_out, detail)
        }
    }
}

/// Run one window's pipeline: the ordered steps over the accumulated
/// state, matrix-product style.
///
/// `step_names` is the window's `steps` list (empty = zero steps,
/// the window default applies and nothing is logged by the caller).
/// `defs` maps def names to their definitions (config-load
/// validation guarantees every name exists; a missing entry is still
/// a step failure, not a panic).
///
/// The first `Abort` or `Fail` stops the chain (sticky veto, and
/// stop-on-fail is the only failure behavior). The loop never
/// wedges: on either stop the window resolves to its default, and
/// the error marker names the failing step.
pub fn run_pipeline(
    defs: &BTreeMap<String, HookDef>,
    step_names: &[String],
    window: Window,
    initial: &Value,
    env: &[(&str, String)],
    default_timeout_ms: u64,
) -> PipelineRun {
    let mut state = initial.clone();
    let mut steps: Vec<StepOutcome> = Vec::new();
    let mut step_outputs: Vec<Value> = Vec::new();
    let mut stop: Option<usize> = None;
    let mut aborted = false;
    let mut failed = false;

    for (i, name) in step_names.iter().enumerate() {
        let Some(def) = defs.get(name) else {
            // Defensive: config-load validation rejects unknown step
            // names. If one slips through, it is a step failure, not
            // a crash.
            steps.push(StepOutcome {
                def: name.clone(),
                command: String::new(),
                status: StepStatus::Fail,
                detail: format!("def '{name}' has no definition"),
            });
            stop = Some(i);
            failed = true;
            break;
        };

        let timeout_ms = def.timeout_ms.unwrap_or(default_timeout_ms);
        let (status, state_out, detail) = fire_step(def, &state, env, timeout_ms);

        let outcome = StepOutcome {
            def: name.clone(),
            command: def.command.clone(),
            status,
            detail,
        };
        steps.push(outcome.clone());

        match status {
            StepStatus::Noop => {
                // The accumulated state is unchanged; nothing new to
                // record as a step output.
            }
            StepStatus::Ok => {
                state = state_out.clone();
                step_outputs.push(state.clone());
            }
            StepStatus::Abort => {
                // The veto is sticky: the chain halts, and the
                // accumulated state stays at the last `Ok` step's
                // output (the window resolves to its default).
                stop = Some(i);
                aborted = true;
                break;
            }
            StepStatus::Fail => {
                stop = Some(i);
                failed = true;
                break;
            }
        }
    }

    PipelineRun {
        window,
        steps,
        stop,
        aborted,
        failed,
        state,
        step_outputs,
    }
}

/// The per-step outcome strings for the `hook.<window>.chain` marker.
/// (Superseded by `PipelineRun::chain_value`, kept as the single
/// formatting site.)
#[allow(dead_code)]
pub fn step_outcome_strings(run: &PipelineRun) -> Vec<String> {
    let steps = run
        .steps
        .iter()
        .map(|s| match s.status {
            StepStatus::Noop => "noop".to_string(),
            StepStatus::Ok => "ok".to_string(),
            StepStatus::Abort => {
                if s.detail.is_empty() {
                    "abort".to_string()
                } else {
                    format!("abort({})", s.detail)
                }
            }
            StepStatus::Fail => format!("fail({})", s.detail),
        })
        .collect();
    steps
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defs_from(entries: &[(&str, &str)]) -> BTreeMap<String, HookDef> {
        entries
            .iter()
            .map(|(name, cmd)| {
                (
                    (*name).to_string(),
                    HookDef {
                        name: (*name).to_string(),
                        command: (*cmd).to_string(),
                        args: Vec::new(),
                        timeout_ms: None,
                    },
                )
            })
            .collect()
    }

    /// A hook that prints a fixed JSON state and exits 0.
    fn echo_hook(dir: &std::path::Path, name: &str, stdout_json: &str) -> String {
        exit_hook(dir, name, stdout_json, 0)
    }

    /// A hook that prints a fixed JSON state and exits with `code`.
    fn exit_hook(dir: &std::path::Path, name: &str, stdout_json: &str, code: u8) -> String {
        let p = dir.join(name);
        let mut content = String::from("#!/bin/sh\ncat > /dev/null\n");
        if !stdout_json.is_empty() {
            content.push_str(&format!("echo '{stdout_json}'\n"));
        }
        content.push_str(&format!("exit {code}\n"));
        std::fs::write(&p, content).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        p.to_string_lossy().into_owned()
    }

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
    fn two_ok_steps_compose_the_state() {
        let dir = tempfile::tempdir().unwrap();
        let h1 = echo_hook(dir.path(), "h1", r#"{"request":{"input":[{"type":"message","role":"user","content":"first"}]}}"#);
        let h2 = echo_hook(dir.path(), "h2", r#"{"request":{"input":[{"type":"message","role":"user","content":"second"}]}}"#);
        let defs = defs_from(&[("h1", &h1), ("h2", &h2)]);
        let initial = serde_json::json!({"request": {"model": "m"}});
        let run = run_pipeline(
            &defs,
            &["h1".to_string(), "h2".to_string()],
            Window::ModelBefore,
            &initial,
            &[],
            5000,
        );
        assert!(run.completed(), "{:#?}", run);
        assert_eq!(run.steps.len(), 2);
        assert_eq!(run.steps[0].status, StepStatus::Ok);
        assert_eq!(run.steps[1].status, StepStatus::Ok);
        // Matrix-product: the final state is step 2's output, which
        // received step 1's output as its input.
        assert_eq!(run.state["request"]["input"][0]["content"], "second");
        // Each ok step's output is recorded for post-processing.
        assert_eq!(run.step_outputs.len(), 2);
        assert_eq!(run.step_outputs[0]["request"]["input"][0]["content"], "first");
        assert_eq!(run.step_outputs[1]["request"]["input"][0]["content"], "second");
        assert!(run.stop.is_none());
    }

    #[test]
    fn a_noop_step_leaves_the_state_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let h1 = echo_hook(dir.path(), "h1", "{}");
        let h2 = echo_hook(dir.path(), "h2", r#"{"request":{"model":"m2"}}"#);
        let defs = defs_from(&[("h1", &h1), ("h2", &h2)]);
        let initial = serde_json::json!({"request": {"model": "m"}});
        let run = run_pipeline(
            &defs,
            &["h1".to_string(), "h2".to_string()],
            Window::ModelBefore,
            &initial,
            &[],
            5000,
        );
        assert!(run.completed());
        assert_eq!(run.steps[0].status, StepStatus::Noop);
        assert_eq!(run.steps[1].status, StepStatus::Ok);
        assert_eq!(run.state["request"]["model"], "m2");
    }

    #[test]
    fn the_first_abort_stops_the_chain_and_is_sticky() {
        let dir = tempfile::tempdir().unwrap();
        let abort = exit_hook(
            dir.path(),
            "abort",
            r#"{"reason":"goal not complete"}"#,
            2,
        );
        let h2 = echo_hook(dir.path(), "h2", r#"{"request":{"model":"m2"}}"#);
        let defs = defs_from(&[("abort", &abort), ("h2", &h2)]);
        let initial = serde_json::json!({"request": {"model": "m"}});
        let run = run_pipeline(
            &defs,
            &["abort".to_string(), "h2".to_string()],
            Window::RunIdle,
            &initial,
            &[],
            5000,
        );
        assert!(run.aborted, "{:#?}", run);
        assert!(!run.failed);
        assert_eq!(run.stop, Some(0));
        // Only the aborting step ran.
        assert_eq!(run.steps.len(), 1);
        assert_eq!(run.steps[0].status, StepStatus::Abort);
        assert_eq!(run.steps[0].detail, "goal not complete");
        // The state stays at the last ok step's output (the initial
        // state here): the window resolves to its default.
        assert_eq!(run.state, initial);
        let chain = run.chain_value();
        assert_eq!(chain["stop"], 0);
        assert_eq!(chain["stop_kind"], "abort");
        assert_eq!(chain["steps"][0], "abort(goal not complete)");
    }

    #[test]
    fn the_first_fail_stops_the_chain_and_the_default_applies() {
        let dir = tempfile::tempdir().unwrap();
        let fail = dir.path().join("fail");
        std::fs::write(
            &fail,
            "#!/bin/sh\ncat > /dev/null\nexit 3\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fail, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let h2 = echo_hook(dir.path(), "h2", r#"{"request":{"model":"m2"}}"#);
        let defs = defs_from(&[("fail", &fail.to_string_lossy()), ("h2", &h2)]);
        let initial = serde_json::json!({"request": {"model": "m"}});
        let run = run_pipeline(
            &defs,
            &["fail".to_string(), "h2".to_string()],
            Window::ModelBefore,
            &initial,
            &[],
            5000,
        );
        assert!(run.failed, "{:#?}", run);
        assert!(!run.aborted);
        assert_eq!(run.stop, Some(0));
        assert_eq!(run.steps.len(), 1);
        assert_eq!(run.steps[0].status, StepStatus::Fail);
        assert!(run.steps[0].detail.contains("exit 3"));
        // The window falls back to its default: the state is the
        // initial request, untransformed.
        assert_eq!(run.state, initial);
        let chain = run.chain_value();
        assert_eq!(chain["stop_kind"], "fail");
    }

    #[test]
    fn an_unknown_exit_code_is_a_fail() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad");
        std::fs::write(&bad, "#!/bin/sh\ncat > /dev/null\nexit 7\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let defs = defs_from(&[("bad", &bad.to_string_lossy())]);
        let run = run_pipeline(
            &defs,
            &["bad".to_string()],
            Window::ToolBefore,
            &serde_json::json!({}),
            &[],
            5000,
        );
        assert!(run.failed);
        assert_eq!(run.stop, Some(0));
        assert!(run.steps[0].detail.contains("unknown exit 7"));
    }

    #[test]
    fn a_slow_step_times_out_and_fails() {
        let defs: BTreeMap<String, HookDef> = BTreeMap::from([(
            "slow".to_string(),
            HookDef {
                name: "slow".to_string(),
                command: "sh".to_string(),
                args: vec!["-c".to_string(), "sleep 5".to_string()],
                timeout_ms: None,
            },
        )]);
        let run = run_pipeline(
            &defs,
            &["slow".to_string()],
            Window::ToolBefore,
            &serde_json::json!({}),
            &[],
            300,
        );
        assert!(run.failed, "{:#?}", run);
        assert!(
            run.steps[0].detail.contains("timed out"),
            "{:#?}",
            run
        );
    }

    #[test]
    fn a_step_with_no_def_is_a_fail_not_a_crash() {
        let defs: BTreeMap<String, HookDef> = BTreeMap::new();
        let run = run_pipeline(
            &defs,
            &["ghost".to_string()],
            Window::ToolBefore,
            &serde_json::json!({}),
            &[],
            5000,
        );
        assert!(run.failed);
        assert_eq!(run.stop, Some(0));
        assert!(run.steps[0].detail.contains("no definition"));
    }

    #[test]
    fn an_empty_pipeline_runs_zero_steps() {
        let defs: BTreeMap<String, HookDef> = BTreeMap::new();
        let initial = serde_json::json!({"request": {"model": "m"}});
        let run = run_pipeline(&defs, &[], Window::ModelBefore, &initial, &[], 5000);
        assert!(run.completed());
        assert!(run.steps.is_empty());
        assert!(run.stop.is_none());
        assert_eq!(run.state, initial);
        let chain = run.chain_value();
        assert_eq!(chain["steps"].as_array().unwrap().len(), 0);
        assert_eq!(chain["stop_kind"], "complete");
    }
}
