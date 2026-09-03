//! `stage_runner` — the default `StageRunner` implementation that spawns
//! the stage binaries via `std::process::Command`.
//!
//! The stage CLIs (matched to the existing binaries):
//! - `claim --session <dir>`
//! - `assemble --session <dir> --config <cfg> [--inject-follow]`
//! - `model --config <cfg>` (the request JSON on stdin)
//! - `parse --config <cfg>` (the model output JSON on stdin)
//! - `route --tools <dir> [--cwd <dir>] [--tool-log <path>]
//!   [--tool-result-max-chars N]` (tool_call events on stdin, one
//!   JSON per line)
//! - `compact <session> --config <cfg> --reason <r> [--force]
//!   [--strip-last-assistant]`
//!
//! The pid of the in-flight child is tracked in `LIVE_CHILD` so the
//! signal path can cancel it (docs/phase-2-plan.md 4.7).

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};

use serde_json::Value;

use harness_common::stage::{
    AssembleOpts, Claim, CompactOutcome, CompactOpts, CompactReason, CompactStatus,
    ModelOutput, ParsedEvents, RequestFile, RouteEnv, SessionDir, StageError,
    StageRunner, ToolCallEvent, ToolResultEvent,
};

/// The pid of the in-flight stage child, 0 when none runs.
static LIVE_CHILD: AtomicI64 = AtomicI64::new(0);

/// The pid of the in-flight stage child, or 0.
#[allow(dead_code)]
pub fn live_child_pid() -> i64 {
    LIVE_CHILD.load(Ordering::SeqCst)
}

/// Kill the in-flight stage child (the signal path, docs 4.7).
pub fn cancel_live_child() {
    let pid = LIVE_CHILD.load(Ordering::SeqCst);
    if pid > 0 {
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGKILL);
        }
    }
}

/// The default `StageRunner` that spawns each stage as a subprocess.
pub struct SubprocessRunner {
    config_path: PathBuf,
    schemas_dir: PathBuf,
    model_bin: PathBuf,
    compact_bin: PathBuf,
    assemble_bin: PathBuf,
    route_bin: PathBuf,
    claim_bin: PathBuf,
    parse_bin: PathBuf,
}

impl SubprocessRunner {
    /// Create a new `SubprocessRunner`.
    pub fn new(
        config_path: PathBuf,
        schemas_dir: PathBuf,
        model_bin: PathBuf,
        compact_bin: PathBuf,
        assemble_bin: PathBuf,
        route_bin: PathBuf,
        claim_bin: PathBuf,
        parse_bin: PathBuf,
    ) -> Self {
        Self {
            config_path,
            schemas_dir,
            model_bin,
            compact_bin,
            assemble_bin,
            route_bin,
            claim_bin,
            parse_bin,
        }
    }
}

/// Spawn a command with all three stdios piped. The pid is tracked in
/// `LIVE_CHILD` until `finish` clears it.
fn spawn_tracked(
    cmd: &mut Command,
    name: &'static str,
) -> Result<std::process::Child, StageError> {
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let child = cmd
        .spawn()
        .map_err(|e| StageError::Other {
            name,
            msg: format!("spawn: {e}"),
        })?;
    LIVE_CHILD.store(i64::from(child.id()), Ordering::SeqCst);
    Ok(child)
}

/// Wait for a tracked child and clear the pid slot.
fn finish(
    child: std::process::Child,
    name: &'static str,
) -> Result<std::process::Output, StageError> {
    let out = child
        .wait_with_output()
        .map_err(|e| StageError::Io { name, source: e })?;
    LIVE_CHILD.store(0, Ordering::SeqCst);
    Ok(out)
}

/// Feed `stdin_text` to the child and drop the handle.
fn feed_stdin(
    child: &mut std::process::Child,
    stdin_text: &str,
    name: &'static str,
) -> Result<(), StageError> {
    let Some(mut stdin) = child.stdin.take() else {
        if stdin_text.is_empty() {
            return Ok(());
        }
        return Err(StageError::Io {
            name,
            source: std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "no stdin pipe",
            ),
        });
    };
    if !stdin_text.is_empty() {
        stdin
            .write_all(stdin_text.as_bytes())
            .map_err(|e| StageError::Io { name, source: e })?;
    }
    Ok(())
}

/// Run a stage whose stdout is one JSON object. A non-zero exit is a
/// stage failure.
fn run_json(
    cmd: &mut Command,
    stdin_text: &str,
    name: &'static str,
) -> Result<Value, StageError> {
    let mut child = spawn_tracked(cmd, name)?;
    feed_stdin(&mut child, stdin_text, name)?;
    let out = finish(child, name)?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(StageError::StageFailed {
            name,
            code: out.status.code().unwrap_or(-1),
            detail: stderr.trim().to_string(),
        });
    }
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    serde_json::from_str(&stdout).map_err(|e| StageError::Other {
        name,
        msg: format!("stdout is not JSON ({e}): {stdout}"),
    })
}

impl StageRunner for SubprocessRunner {
    fn claim(&self, session: &SessionDir) -> Result<Claim, StageError> {
        let mut cmd = Command::new(&self.claim_bin);
        cmd.arg("--session").arg(&session.path);
        cmd.arg("--schemas").arg(&self.schemas_dir);
        let v = run_json(&mut cmd, "", "claim")?;
        Ok(Claim {
            state: v.get("state").and_then(|s| s.as_str()).unwrap_or("").to_string(),
            last_user_message_seq: v
                .get("last_user_message_seq")
                .and_then(|s| s.as_u64())
                .unwrap_or(0) as usize,
            pending_tool_calls: v
                .get("pending_tool_calls")
                .and_then(|a| a.as_array())
                .map(|a| a.clone())
                .unwrap_or_default(),
            pending_follow_ups: v
                .get("pending_follow_ups")
                .and_then(|a| a.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_u64().map(|n| n as usize))
                        .collect()
                })
                .unwrap_or_default(),
        })
    }

    fn assemble(
        &self,
        session: &SessionDir,
        opts: &AssembleOpts,
    ) -> Result<RequestFile, StageError> {
        let mut cmd = Command::new(&self.assemble_bin);
        cmd.arg("--session").arg(&session.path);
        cmd.arg("--config").arg(&self.config_path);
        if opts.inject_follow {
            cmd.arg("--inject-follow");
        }
        let v = run_json(&mut cmd, "", "assemble")?;
        Ok(RequestFile { json: v })
    }

    fn model(&self, request: &RequestFile) -> Result<ModelOutput, StageError> {
        let stdin_text = serde_json::to_string(&request.json)
            .map_err(|e| StageError::Other {
                name: "model",
                msg: format!("serialize request: {e}"),
            })?;
        let mut cmd = Command::new(&self.model_bin);
        cmd.arg("--config").arg(&self.config_path);
        let v = run_json(&mut cmd, &stdin_text, "model")?;
        Ok(ModelOutput { json: v })
    }

    fn parse(&self, model_output: &ModelOutput) -> Result<ParsedEvents, StageError> {
        let stdin_text = serde_json::to_string(&model_output.json)
            .map_err(|e| StageError::Other {
                name: "parse",
                msg: format!("serialize model output: {e}"),
            })?;

        // `parse` exits 0 (stop), 1 (route), or 2 (terminal error).
        // Only a crash is a stage failure.
        let mut cmd = Command::new(&self.parse_bin);
        cmd.arg("--config").arg(&self.config_path);
        let mut child = spawn_tracked(&mut cmd, "parse")?;
        feed_stdin(&mut child, &stdin_text, "parse")?;
        let out = finish(child, "parse")?;

        let code = out.status.code().unwrap_or(-1);
        if code != 0 && code != 1 && code != 2 {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(StageError::StageFailed {
                name: "parse",
                code,
                detail: stderr.trim().to_string(),
            });
        }

        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let lines: Vec<String> = stdout
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();

        Ok(ParsedEvents { exit: code, lines })
    }

    fn route(
        &self,
        calls: &[ToolCallEvent],
        env: &RouteEnv,
    ) -> Result<Vec<ToolResultEvent>, StageError> {
        if calls.is_empty() {
            return Ok(Vec::new());
        }

        // One tool_call event per line on stdin, as `route` reads it.
        let stdin_text: String = calls
            .iter()
            .map(|c| {
                serde_json::json!({
                    "v": 1,
                    "type": "tool_call",
                    "ts": "pending",
                    "id": c.id,
                    "name": c.name,
                    "arguments": c.arguments,
                })
            })
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        let mut cmd = Command::new(&self.route_bin);
        cmd.arg("--tools").arg(&env.tools_root);
        if let Some(cwd) = &env.cwd {
            cmd.arg("--cwd").arg(cwd);
        }
        if let Some(tool_log) = &env.tool_log {
            cmd.arg("--tool-log").arg(tool_log);
        }
        cmd.arg("--tool-result-max-chars")
            .arg(env.tool_result_max_chars.to_string());

        let mut child = spawn_tracked(&mut cmd, "route")?;
        feed_stdin(&mut child, &stdin_text, "route")?;
        let out = finish(child, "route")?;

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(StageError::StageFailed {
                name: "route",
                code: out.status.code().unwrap_or(-1),
                detail: stderr.trim().to_string(),
            });
        }

        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let results = stdout
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| {
                let v: Value = serde_json::from_str(l.trim()).ok()?;
                let is_error = v.get("is_error").and_then(|e| e.as_bool()).unwrap_or(false);
                let id = v.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
                Some(ToolResultEvent { value: v, id, is_error })
            })
            .collect();

        Ok(results)
    }

    fn compact(
        &self,
        session: &SessionDir,
        opts: &CompactOpts,
    ) -> Result<CompactStatus, StageError> {
        // The compact binary takes `threshold | overflow`. The
        // last-resort reason runs as a forced overflow compact.
        let reason = match opts.reason {
            CompactReason::Threshold => "threshold",
            CompactReason::Overflow | CompactReason::LastResort => "overflow",
        };

        let mut cmd = Command::new(&self.compact_bin);
        cmd.arg(&session.path);
        cmd.arg("--config").arg(&self.config_path);
        cmd.arg("--reason").arg(reason);
        if opts.force || matches!(opts.reason, CompactReason::LastResort) {
            cmd.arg("--force");
        }
        if opts.strip_last_assistant {
            cmd.arg("--strip-last-assistant");
        }

        // The compact binary exits 1 on the failure path but still
        // prints the status JSON, so the status word decides.
        let mut child = spawn_tracked(&mut cmd, "compact")?;
        feed_stdin(&mut child, "", "compact")?;
        let out = finish(child, "compact")?;

        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let v: Value = if stdout.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&stdout).map_err(|e| StageError::Other {
                name: "compact",
                msg: format!("stdout is not JSON ({e}): {stdout}"),
            })?
        };

        let status_str = v.get("status").and_then(|s| s.as_str()).unwrap_or("");
        let outcome = match status_str {
            "compacted" => CompactOutcome::Compacted,
            "noop" => CompactOutcome::Noop,
            "failed" => CompactOutcome::Failed,
            _ => {
                if out.status.success() {
                    CompactOutcome::Compacted
                } else {
                    CompactOutcome::Failed
                }
            }
        };

        Ok(CompactStatus {
            outcome,
            first_kept_seq: v
                .get("first_kept_seq")
                .and_then(|s| s.as_u64()),
            tokens_before: v.get("tokens_before").and_then(|t| t.as_u64()),
            tokens_after: v.get("tokens_after").and_then(|t| t.as_u64()),
        })
    }
}
