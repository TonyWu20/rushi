//! One-step pipeline for `harness step` (docs/phase-2-plan.md section 4.2).
//!
//! Mirrors `scripts/step.sh`: publish `model_thinking` at entry, run
//! `claim`, then dispatch on the derived state.

use std::path::Path;

use serde_json::Value;

use harness_common::event_validation;
use harness_common::hooks::{self, Window};
use harness_common::logline::LogLine;
use harness_common::stage::{
    AssembleOpts, CompactOutcome, CompactOpts, CompactReason, CompactStatus,
    ModelOutput, RouteEnv, SessionDir, StageRunner, ToolCallEvent,
};

use crate::classifier::is_overflow;
use crate::config::HarnessConfig;
use crate::signals;
use crate::stage_runner::SubprocessRunner;

/// Create a `SubprocessRunner` from the resolved config.
pub fn make_runner(cfg: &HarnessConfig) -> SubprocessRunner {
    SubprocessRunner::new(
        cfg.config_path.clone(),
        cfg.schemas_dir.clone(),
        cfg.model_bin.clone(),
        cfg.compact_bin.clone(),
        cfg.assemble_bin.clone(),
        cfg.route_bin.clone(),
        cfg.claim_bin.clone(),
        cfg.parse_bin.clone(),
    )
}

/// How a caught signal exits the process (docs/phase-2-plan.md 4.7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepMode {
    /// `harness step`: a caught signal exits with code 1.
    Step,
    /// `harness run`: a caught signal exits 143 (SIGTERM) or 130 (SIGINT).
    Run,
}

/// One step. Exits 0 on any clean stop. Exits 1 on a hard failure.
///
/// `mode` picks the signal exit code: `Step` exits 1 on a caught signal,
/// `Run` exits 143 (SIGTERM) or 130 (SIGINT).
pub fn do_step(cfg: &HarnessConfig, session_dir: &Path, mode: StepMode) {
    let runner = make_runner(cfg);
    let session = SessionDir {
        path: session_dir.to_path_buf(),
    };

    // Resolve the model description once and reuse it for the
    // `model_thinking` marker and the guard model id
    // (docs/phase-2-plan.md section 4.3 step 2).
    let describe = describe_model(cfg);

    // Step entry: publish `model_thinking` on-change before claim.
    publish_model_thinking(cfg, session_dir, &describe);

    let claim = match runner.claim(&session) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("harness: claim failed: {e}");
            std::process::exit(1);
        }
    };

    // Fire the `step.start` observation window (docs/loop-lifecycle-hooks.md
    // section 3.2), after claim and before the branch dispatch.
    fire_step_start(cfg, &session, &claim);

    check_signal(mode);

    match claim.state.as_str() {
        "idle" => {
            if claim.pending_follow_ups.is_empty() {
                // No follow-up: the `run` wrapper decides whether to
                // keep going via the `run.idle` window.
                return;
            }
            // Idle with follow-ups: drain one batch as a new turn.
            run_awaiting_model(cfg, &runner, &session, &claim, true, &describe, mode);
        }
        "exhausted" => {
            // Nothing owed. The handoff or in-session shadow compact
            // already closed this state.
        }
        "awaiting_tool_result" => {
            run_awaiting_tool_result(cfg, &runner, &session, &claim);
        }
        "awaiting_model" => {
            run_awaiting_model(cfg, &runner, &session, &claim, false, &describe, mode);
        }
        "awaiting_approval" => {
            run_awaiting_approval(cfg, &runner, &session, &claim, mode);
        }
        other => {
            eprintln!("harness: unknown claim state `{other}`");
            std::process::exit(1);
        }
    }

    check_signal(mode);
}

/// Check for a caught signal and exit with the mode-appropriate code.
/// `Step` exits 1; `Run` exits 143 (SIGTERM) or 130 (SIGINT).
fn check_signal(mode: StepMode) {
    match mode {
        StepMode::Step => signals::check_and_exit(1),
        StepMode::Run => signals::check_and_exit_for_run(),
    }
}

/// Fire the `step.start` observation window (docs/loop-lifecycle-hooks.md
/// section 3.2). Observation only, no decision.
fn fire_step_start(
    cfg: &HarnessConfig,
    session: &SessionDir,
    claim: &harness_common::stage::Claim,
) {
    let payload = serde_json::json!({
        "window": "step.start",
        "session": session.path.to_string_lossy(),
        "claim_state": claim.state.as_str(),
    });
    let results = hooks::fire_hooks(
        &cfg.hooks,
        Window::StepStart,
        &payload,
        &hook_env(cfg, session, "step", Window::StepStart),
        cfg.hooks_timeout_ms,
    );
    log_hook_window(cfg, session, "step.start", "", &results);
}

// ---------------------------------------------------------------------------
// model_thinking marker
// ---------------------------------------------------------------------------

/// Publish `model_thinking` ext_status on-change using a pre-resolved `Describe`.
fn publish_model_thinking(cfg: &HarnessConfig, session_dir: &Path, describe: &Describe) {
    let level = match describe.thinking_level {
        l if (l as u64) <= 4 => l as u64,
        _ => return,
    };

    let last = read_last_model_thinking(session_dir);
    if last == Some(level) {
        return;
    }

    let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let event = serde_json::json!({
        "v": 1,
        "type": "ext_status",
        "ts": ts,
        "id": "model_thinking",
        "value": level,
    });
    append_event(cfg, session_dir, &event);
}

/// Read the last `model_thinking` value from the log tail.
fn read_last_model_thinking(session_dir: &Path) -> Option<u64> {
    let path = session_dir.join("events.jsonl");
    let data = std::fs::read_to_string(&path).ok()?;
    let lines: Vec<&str> = data.lines().rev().take(4096).collect();
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(line).ok()?;
        if v.get("type").and_then(|t| t.as_str()) != Some("ext_status") {
            continue;
        }
        if v.get("id").and_then(|i| i.as_str()) != Some("model_thinking") {
            continue;
        }
        return v.get("value").and_then(|v| v.as_u64());
    }
    None
}

// ---------------------------------------------------------------------------
// Appending helpers
// ---------------------------------------------------------------------------

/// Append one event through the shared `LogLine` and validator.
pub fn append_event(cfg: &HarnessConfig, session_dir: &Path, event: &Value) {
    let json_line = serde_json::to_string(event).unwrap_or_else(|e| {
        eprintln!("harness: cannot serialize event: {e}");
        std::process::exit(1);
    });
    let schemas = event_validation::load_schemas(
        cfg.schemas_dir.to_str().unwrap_or_default(),
    );
    if let Err(e) = event_validation::validate_value(event, &schemas) {
        eprintln!("harness: event validation failed: {e}");
        std::process::exit(1);
    }
    let line = LogLine::from_json(&json_line);
    let log_path = session_dir.join("events.jsonl");
    if let Err(e) = line.commit(&log_path) {
        eprintln!("harness: log append failed: {e}");
        std::process::exit(1);
    }
}

/// Append one already-serialized JSON line to the log.
pub fn append_line(cfg: &HarnessConfig, session_dir: &Path, line: &str) {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return;
    }
    let schemas = event_validation::load_schemas(
        cfg.schemas_dir.to_str().unwrap_or_default(),
    );
    let Ok(parsed) = serde_json::from_str::<Value>(trimmed) else {
        eprintln!("harness: log line is not valid JSON: {trimmed}");
        std::process::exit(1);
    };
    if let Err(e) = event_validation::validate_value(&parsed, &schemas) {
        eprintln!("harness: event validation failed: {e}");
        std::process::exit(1);
    }
    let ll = LogLine::from_json(trimmed);
    if let Err(e) = ll.commit(&session_dir.join("events.jsonl")) {
        eprintln!("harness: log append failed: {e}");
        std::process::exit(1);
    }
}

/// Publish one `loop_phase` marker.
pub fn publish_loop_phase(cfg: &HarnessConfig, session_dir: &Path, phase: &str) {
    let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let event = serde_json::json!({
        "v": 1,
        "type": "ext_status",
        "ts": ts,
        "id": "loop_phase",
        "value": phase,
    });
    append_event(cfg, session_dir, &event);
}

/// Append a terminal error event.
fn append_terminal_error(cfg: &HarnessConfig, session_dir: &Path, message: &str) {
    let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let event = serde_json::json!({
        "v": 1,
        "type": "error",
        "ts": ts,
        "message": message,
    });
    append_event(cfg, session_dir, &event);
}

// ---------------------------------------------------------------------------
// awaiting_tool_result: re-route pending calls without a model call.
// ---------------------------------------------------------------------------

fn run_awaiting_tool_result(
    cfg: &HarnessConfig,
    runner: &SubprocessRunner,
    session: &SessionDir,
    claim: &harness_common::stage::Claim,
) {
    publish_loop_phase(cfg, &session.path, "tools");
    let calls = extract_tool_calls(claim.pending_tool_calls.iter());
    let results = route_batch(cfg, runner, &calls, session);
    for r in &results {
        append_line(
            cfg,
            &session.path,
            &serde_json::to_string(&r.value).unwrap_or_default(),
        );
    }
}

// ---------------------------------------------------------------------------
// awaiting_model (and idle-with-followups): the full pipeline.
// ---------------------------------------------------------------------------

fn run_awaiting_model(
    cfg: &HarnessConfig,
    runner: &SubprocessRunner,
    session: &SessionDir,
    claim: &harness_common::stage::Claim,
    inject_follow: bool,
    describe: &Describe,
    mode: StepMode,
) {
    publish_loop_phase(cfg, &session.path, "wait");

    let guard_model = describe.model_id.clone();

    // Proactive threshold check.
    if cfg.compact_enabled {
        let estimated = estimate_context(cfg, &session.path);
        if cfg.trigger_level() > 0 && estimated > cfg.trigger_level() {
            let reason = CompactReason::Threshold;
            let status =
                try_compact_with_hooks(cfg, runner, session, reason, false, false);
            post_compact_sanity(cfg, session, &status);
        }
    }

    // Assemble.
    let opts = AssembleOpts {
        inject_follow: inject_follow || !claim.pending_follow_ups.is_empty(),
    };
    let mut request = match runner.assemble(session, &opts) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("harness: assemble failed: {e}");
            std::process::exit(1);
        }
    };

    // Assemble error form: append and stop.
    if request.json.get("type").and_then(|t| t.as_str()) == Some("error") {
        append_event(cfg, &session.path, &request.json);
        return;
    }

    // context_exhausted form: fire exhausted.handle window.
    if request.json.get("type").and_then(|t| t.as_str()) == Some("context_exhausted") {
        let should_continue = fire_and_handle_exhausted(cfg, runner, session, &describe);
        if !should_continue {
            return;
        }
        // Reassemble after the in-session compact and fall through to the model retry loop.
        reassemble(runner, session, &mut request, false);
    }

    // The model retry loop.
    let (output, terminal_logged) = model_retry_loop(cfg, runner, session, &mut request, &guard_model, &describe, mode);

    check_signal(mode);

    // If a terminal error was already appended, do not parse or route.
    if terminal_logged {
        return;
    }

    // Parse.
    let parsed = match runner.parse(&output) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("harness: parse failed: {e}");
            std::process::exit(1);
        }
    };

    // Append all parsed events first (matches step.sh: cat parsed routed | log).
    for line in &parsed.lines {
        append_line(cfg, &session.path, line);
    }

    if parsed.exit == 1 {
        publish_loop_phase(cfg, &session.path, "tools");
        let calls: Vec<ToolCallEvent> = parsed
            .lines
            .iter()
            .filter_map(|line| {
                let v: Value = serde_json::from_str(line).ok()?;
                if v.get("type").and_then(|t| t.as_str()) != Some("tool_call") {
                    return None;
                }
                Some(ToolCallEvent {
                    id: v.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string(),
                    name: v.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string(),
                    arguments: v.get("arguments").cloned().unwrap_or(Value::Null),
                })
            })
            .collect();
        route_and_append(cfg, runner, session, &calls, mode);
    }
}

// ---------------------------------------------------------------------------
// Model description and context estimate.
// ---------------------------------------------------------------------------

struct Describe {
    active: String,
    model_id: String,
    #[allow(dead_code)]
    thinking_level: u32,
}

fn describe_model(cfg: &HarnessConfig) -> Describe {
    let mut cmd = std::process::Command::new(&cfg.model_bin);
    cmd.arg("--describe").arg("--config").arg(&cfg.config_path);
    let out = match cmd.output() {
        Ok(o) => o,
        Err(_) => {
            return Describe {
                active: String::new(),
                model_id: String::new(),
                thinking_level: 0,
            };
        }
    };
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    Describe {
        active: v
            .get("active")
            .and_then(|a| a.as_str())
            .unwrap_or("")
            .to_string(),
        model_id: v
            .get("model_id")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string(),
        thinking_level: v
            .get("thinking_level")
            .and_then(|l| l.as_u64())
            .unwrap_or(0) as u32,
    }
}

/// Estimate the context size: last measured input + trailing estimate.
fn estimate_context(cfg: &HarnessConfig, session_dir: &Path) -> u64 {
    let path = session_dir.join("events.jsonl");
    let Ok(data) = std::fs::read_to_string(&path) else {
        return 0;
    };
    let caps = harness_common::compact_math::Caps {
        result: Some(cfg.compact_result_chars),
        text: Some(cfg.compact_text_chars),
    };

    // Parse all events once.
    let events: Vec<Value> = data
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
        .collect();

    // Find the LAST assistant_message that carries a measured
    // input_tokens reading. That reading reflects the provider's
    // actual context size after any compaction boundary.
    let last_meas_idx = events.iter().rposition(|v| {
        v.get("type").and_then(|t| t.as_str()) == Some("assistant_message")
            && v.get("usage")
                .and_then(|u| u.get("input_tokens"))
                .and_then(|i| i.as_u64())
                .is_some()
    });

    let Some(idx) = last_meas_idx else {
        return 0;
    };
    let measured = events[idx]
        .get("usage")
        .and_then(|u| u.get("input_tokens"))
        .and_then(|i| i.as_u64())
        .unwrap_or(0);

    // Trailing events after the last measurement: estimate via chars/4.
    let est: u64 = events[idx + 1..]
        .iter()
        .map(|v| {
            harness_common::compact_math::est_tokens(
                &harness_common::compact_math::project_event(v),
                &caps,
            )
        })
        .sum();
    measured + est
}

// ---------------------------------------------------------------------------
// exhausted.handle window
// ---------------------------------------------------------------------------

/// Fire the `exhausted.handle` window and act on the decision.
///
/// Returns `true` when the compact succeeded and the caller should
/// reassemble and continue with the model call. Returns `false` when a
/// terminal error was logged or the hook requested a stop.
fn fire_and_handle_exhausted(
    cfg: &HarnessConfig,
    runner: &SubprocessRunner,
    session: &SessionDir,
    describe: &Describe,
) -> bool {
    let payload = serde_json::json!({
        "window": "exhausted.handle",
        "session": session.path.to_string_lossy(),
        "active_model": describe.active,
        "input_budget": cfg.input_budget,
        "context_tokens": cfg.context_tokens,
    });
    let results = hooks::fire_hooks(
        &cfg.hooks,
        Window::ExhaustedHandle,
        &payload,
        &hook_env(cfg, session, "step", Window::ExhaustedHandle),
        cfg.hooks_timeout_ms,
    );
    let (decision_opt, _) = hooks::fold_decision(&results, Window::ExhaustedHandle);
    let decision = decision_opt
        .unwrap_or_else(|| hooks::window_default(Window::ExhaustedHandle).to_string());
    log_hook_window(cfg, session, "exhausted.handle", &decision, &results);

    match decision.as_str() {
        "stay_compact" => {
            let status =
                try_compact_with_hooks(cfg, runner, session, CompactReason::LastResort, true, false);
            if status.outcome == CompactOutcome::Compacted {
                eprintln!("harness: in-session shadow compact ran");
                post_compact_sanity(cfg, session, &status);
                true
            } else {
                append_terminal_error(
                    cfg,
                    &session.path,
                    "the last-resort compaction did not recover the session",
                );
                false
            }
        }
        "stop" => {
            append_terminal_error(cfg, &session.path, "exhausted.handle hook stopped the loop");
            false
        }
        _ => {
            let _ = try_compact_with_hooks(
                cfg, runner, session, CompactReason::LastResort, true, false,
            );
            true
        }
    }
}

// ---------------------------------------------------------------------------
// Compact with compact.before / compact.after hooks.
// ---------------------------------------------------------------------------

fn try_compact_with_hooks(
    cfg: &HarnessConfig,
    runner: &SubprocessRunner,
    session: &SessionDir,
    reason: CompactReason,
    force: bool,
    strip_last: bool,
) -> harness_common::stage::CompactStatus {
    let payload = serde_json::json!({
        "window": "compact.before",
        "session": session.path.to_string_lossy(),
        "reason": reason.as_str(),
        "force": force,
    });
    let results = hooks::fire_hooks(
        &cfg.hooks,
        Window::CompactBefore,
        &payload,
        &hook_env(cfg, session, "step", Window::CompactBefore),
        cfg.hooks_timeout_ms,
    );
    let (decision_opt, payload_val) =
        hooks::fold_decision(&results, Window::CompactBefore);
    let decision = decision_opt
        .unwrap_or_else(|| hooks::window_default(Window::CompactBefore).to_string());
    log_hook_window(cfg, session, "compact.before", &decision, &results);

    if decision == "cancel" {
        eprintln!("harness: compact.before hook cancelled the compaction");
        return CompactStatus::noop();
    }

    if decision == "replace" {
        // The hook supplies its own summary and boundary.
        let summary = payload_val
            .get("summary")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        let first_kept_seq = payload_val
            .get("first_kept_seq")
            .and_then(|v| v.as_u64())
            .unwrap_or(1);
        write_handoff(session, &summary);
        // No compact binary ran on this path, so the loop appends the
        // boundary marker itself.
        append_compaction_summary(cfg, session, &summary, first_kept_seq, &reason);
        eprintln!("harness: compact.before hook replaced the compaction");
        return CompactStatus {
            outcome: CompactOutcome::Compacted,
            first_kept_seq: Some(first_kept_seq),
            tokens_before: None,
            tokens_after: None,
            summary: Some(summary),
        };
    }

    let opts = CompactOpts {
        reason,
        force,
        strip_last_assistant: strip_last,
    };
    match runner.compact(session, &opts) {
        Ok(status) => {
            // Write the handoff document (docs/phase-2-plan.md 3.5).
            if status.outcome == CompactOutcome::Compacted {
                if let Some(ref summary_text) = status.summary {
                    write_handoff(session, summary_text);
                }
            }
            let status_str = match status.outcome {
                CompactOutcome::Compacted => "compacted",
                CompactOutcome::Noop => "noop",
                CompactOutcome::Failed => "failed",
            };
            let payload2 = serde_json::json!({
                "window": "compact.after",
                "session": session.path.to_string_lossy(),
                "reason": reason.as_str(),
                "status": status_str,
            });
            let _ = hooks::fire_hooks(
                &cfg.hooks,
                Window::CompactAfter,
                &payload2,
                &hook_env(cfg, session, "step", Window::CompactAfter),
                cfg.hooks_timeout_ms,
            );
            status
        }
        Err(e) => {
            eprintln!("harness: compact failed: {e}");
            CompactStatus::noop()
        }
    }
}

/// Post-compact sanity: if the projected post-compact size still
/// exceeds the trigger level, log a warning marker.
fn post_compact_sanity(
    cfg: &HarnessConfig,
    session: &SessionDir,
    status: &harness_common::stage::CompactStatus,
) {
    if status.outcome != CompactOutcome::Compacted {
        return;
    }
    let Some(tokens_after) = status.tokens_after else {
        return;
    };
    let trigger = cfg.trigger_level();
    if trigger > 0 && tokens_after > trigger {
        let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let event = serde_json::json!({
            "v": 1,
            "type": "ext_status",
            "ts": ts,
            "id": "compact_sanity",
            "value": format!("post-compact estimate {tokens_after} still exceeds trigger {trigger}"),
        });
        append_event(cfg, &session.path, &event);
    }
}

// ---------------------------------------------------------------------------
// Model retry loop.
// ---------------------------------------------------------------------------

/// Model retry loop. Returns `(output, terminal_logged)` where
/// `terminal_logged` is `true` when a terminal `error` event has
/// already been appended to the log and the caller must skip parse.
fn model_retry_loop(
    cfg: &HarnessConfig,
    runner: &SubprocessRunner,
    session: &SessionDir,
    request: &mut harness_common::stage::RequestFile,
    guard_model: &str,
    describe: &Describe,
    mode: StepMode,
) -> (ModelOutput, bool) {
    let mut empty_attempts = 0usize;
    let mut model_err_retries = 0usize;
    let mut overflow_recovered = false;
    let mut last_resort = false;
    let mut last_output = ModelOutput { json: Value::Null };
    let terminal_logged = false;

    'outer: loop {
        check_signal(mode);

        // model.before observation window.
        let payload = serde_json::json!({
            "window": "model.before",
            "session": session.path.to_string_lossy(),
            "model": describe.model_id,
            "projected_tokens": estimate_context(cfg, &session.path),
        });
        let _ = hooks::fire_hooks(
            &cfg.hooks,
            Window::ModelBefore,
            &payload,
            &hook_env(cfg, session, "step", Window::ModelBefore),
            cfg.hooks_timeout_ms,
        );

        let output = match runner.model(request) {
            Ok(o) => o,
            Err(e) => {
                model_err_retries += 1;
                if model_err_retries <= 2 {
                    eprintln!(
                        "harness: model spawn/IO failed ({e}); retry {model_err_retries}/2 in 3s"
                    );
                    std::thread::sleep(std::time::Duration::from_secs(3));
                    continue;
                }
                append_terminal_error(cfg, &session.path, "model binary failed to run after retries");
                return (last_output, true);
            }
        };
        last_output = output.clone();

        // model.after observation window.
        let payload = serde_json::json!({
            "window": "model.after",
            "session": session.path.to_string_lossy(),
            "stop_reason": last_output.json.get("stop_reason").cloned().unwrap_or(Value::Null),
            "detail": last_output.json.get("detail").cloned().unwrap_or(Value::Null),
            "usage": last_output.json.get("usage").cloned().unwrap_or(Value::Null),
        });
        let _ = hooks::fire_hooks(
            &cfg.hooks,
            Window::ModelAfter,
            &payload,
            &hook_env(cfg, session, "step", Window::ModelAfter),
            cfg.hooks_timeout_ms,
        );

        let stop_reason = last_output
            .json
            .get("stop_reason")
            .and_then(|s| s.as_str())
            .unwrap_or("none");
        let detail = last_output
            .json
            .get("detail")
            .and_then(|d| d.as_str())
            .unwrap_or("");
        let req_model = request.json.get("model").and_then(|m| m.as_str()).unwrap_or("");

        // Fire overflow.resolve on overflow classification. The
        // decision (`stay_compact` or `stop`) decides whether the
        // strategy cycle continues (docs/loop-lifecycle-hooks.md 3.4).
        let is_overflow = !req_model.is_empty()
            && req_model == guard_model
            && is_overflow(detail);
        if stop_reason == "error" && is_overflow {
            let usage_val = last_output.json.get("usage").cloned().unwrap_or(Value::Null);
            let ovf_decision = fire_overflow_resolve(
                cfg,
                session,
                &describe.active,
                stop_reason,
                detail,
                &usage_val,
                !last_resort,
            );
            if ovf_decision == "stop" {
                append_terminal_error(
                    cfg,
                    &session.path,
                    "overflow.resolve hook stopped the strategy cycle",
                );
                return (last_output, true);
            }
        }

        if stop_reason == "error" {
            if is_overflow {
                if last_resort {
                    append_terminal_error(
                        cfg,
                        &session.path,
                        &format!(
                            "context overflow: the last-resort compaction did not recover: {detail}"
                        ),
                    );
                    return (last_output, true);
                }
                if cfg.compact_enabled && !overflow_recovered {
                    overflow_recovered = true;
                    let status =
                        try_compact_with_hooks(cfg, runner, session, CompactReason::Overflow, false, false);
                    if status.outcome != CompactOutcome::Compacted {
                        last_resort = true;
                        let _ = try_compact_with_hooks(
                            cfg, runner, session, CompactReason::LastResort, true, false,
                        );
                        reassemble(runner, session, request, false);
                        continue;
                    }
                    reassemble(runner, session, request, false);
                    continue;
                }
                last_resort = true;
                let _ = try_compact_with_hooks(
                    cfg, runner, session, CompactReason::LastResort, true, false,
                );
                reassemble(runner, session, request, false);
                continue;
            }
            // Transport-level model failure.
            model_err_retries += 1;
            if last_resort {
                append_terminal_error(
                    cfg,
                    &session.path,
                    &format!("model API call failed after the last-resort compaction: {detail}"),
                );
                return (last_output, true);
            }
            if model_err_retries <= 2 {
                eprintln!(
                    "harness: model API error ({detail}); retry {model_err_retries}/2 in 3s"
                );
                std::thread::sleep(std::time::Duration::from_secs(3));
                continue;
            }
            append_terminal_error(
                cfg,
                &session.path,
                &format!("model API call failed after retries: {detail}"),
            );
            return (last_output, true);
        }

        // Silent overflow: successful call, measured input >= budget.
        if stop_reason != "length" {
            if let Some(measured) = last_output
                .json
                .get("usage")
                .and_then(|u| u.get("input_tokens"))
                .and_then(|i| i.as_u64())
            {
                if !req_model.is_empty()
                    && req_model == guard_model
                    && measured >= cfg.input_budget
                {
                    if cfg.compact_enabled {
                        let _ = try_compact_with_hooks(
                            cfg, runner, session, CompactReason::Overflow, false, false,
                        );
                    } else {
                        let _ = try_compact_with_hooks(
                            cfg, runner, session, CompactReason::LastResort, true, false,
                        );
                    }
                }
            }
        }

        // Length-stop recovery.
        if stop_reason == "length" {
            let out_tokens = last_output
                .json
                .get("usage")
                .and_then(|u| u.get("output_tokens"))
                .and_then(|t| t.as_u64())
                .unwrap_or(0);
            if out_tokens < cfg.max_output_tokens {
                if !overflow_recovered {
                    overflow_recovered = true;
                    log_truncated_group(cfg, session, &last_output.json);
                    let _ = try_compact_with_hooks(
                        cfg,
                        runner,
                        session,
                        CompactReason::Overflow,
                        cfg.compact_enabled,
                        true,
                    );
                    reassemble(runner, session, request, false);
                    continue;
                }
                last_resort = true;
                let _ = try_compact_with_hooks(
                    cfg, runner, session, CompactReason::LastResort, true, true,
                );
                reassemble(runner, session, request, false);
                continue;
            }
        }

        // Empty-turn guard.
        if is_empty_turn(&last_output.json) {
            empty_attempts += 1;
            if empty_attempts < 3 {
                continue;
            }
            append_terminal_error(
                cfg,
                &session.path,
                "model returned an empty turn after retries",
            );
            return (last_output, true);
        }

        break 'outer;
    }

    (last_output, terminal_logged)
}

fn reassemble(
    runner: &SubprocessRunner,
    session: &SessionDir,
    request: &mut harness_common::stage::RequestFile,
    inject_follow: bool,
) {
    let opts = AssembleOpts { inject_follow };
    match runner.assemble(session, &opts) {
        Ok(r) => *request = r,
        Err(e) => {
            eprintln!("harness: reassemble failed: {e}");
            std::process::exit(1);
        }
    }
}

fn is_empty_turn(output: &Value) -> bool {
    let has_text = output
        .get("text")
        .and_then(|t| t.as_str())
        .map(|t| !t.trim().is_empty())
        .unwrap_or(false);
    let calls = output.get("tool_calls").and_then(|c| c.as_array());
    let has_calls = matches!(calls, Some(a) if !a.is_empty());
    !(has_text || has_calls)
}

/// Log the truncated group from a length-stop response.
fn log_truncated_group(cfg: &HarnessConfig, session: &SessionDir, output: &Value) {
    let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let text = output.get("text").and_then(|t| t.as_str()).unwrap_or("");
    let calls: Vec<Value> = output
        .get("tool_calls")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();
    let assistant = serde_json::json!({
        "v": 1,
        "type": "assistant_message",
        "ts": ts,
        "content": text,
        "tool_calls": calls,
        "usage": output.get("usage").cloned().unwrap_or(Value::Null),
        "stop_reason": "length",
    });
    append_event(cfg, &session.path, &assistant);
    for call in &calls {
        let id = call.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
        let result = serde_json::json!({
            "v": 1,
            "type": "tool_result",
            "ts": ts,
            "id": id,
            "value": { "text": "Arguments may be truncated: the model stopped in the middle of this call." },
            "is_error": true,
        });
        append_event(cfg, &session.path, &result);
    }
}

// ---------------------------------------------------------------------------
// Tool routing with tool.before / tool.after hooks and approval.
// ---------------------------------------------------------------------------

fn route_and_append(
    cfg: &HarnessConfig,
    runner: &SubprocessRunner,
    session: &SessionDir,
    calls: &[ToolCallEvent],
    mode: StepMode,
) {
    // tool.before decision window.
    let calls_json: Vec<Value> = calls
        .iter()
        .map(|c| {
            serde_json::json!({
                "id": c.id,
                "name": c.name,
                "arguments": c.arguments,
            })
        })
        .collect();
    let payload = serde_json::json!({
        "window": "tool.before",
        "session": session.path.to_string_lossy(),
        "calls": calls_json,
    });
    let results = hooks::fire_hooks(
        &cfg.hooks,
        Window::ToolBefore,
        &payload,
        &hook_env(cfg, session, "step", Window::ToolBefore),
        cfg.hooks_timeout_ms,
    );
    log_hook_window(cfg, session, "tool.before", "", &results);

    let mut decision = "proceed".to_string();
    let mut payload_val = Value::Null;
    for r in &results {
        if !r.failed {
            if let Some(d) = &r.decision {
                decision = d.clone();
                payload_val = r.payload.clone();
            }
        }
    }

    let mut to_route: Vec<ToolCallEvent> = calls.to_vec();
    let mut synthetic: Vec<Value> = Vec::new();

    match decision.as_str() {
        "block" => {
            let reason = payload_val
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("tool call blocked by hook")
                .to_string();
            let blocked_ids: Vec<String> = payload_val
                .get("calls")
                .and_then(|c| c.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_else(|| calls.iter().map(|c| c.id.clone()).collect());
            let mut remaining = Vec::new();
            for call in calls {
                if blocked_ids.iter().any(|id| id == &call.id) {
                    let ts = chrono::Utc::now()
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
                    synthetic.push(serde_json::json!({
                        "v": 1,
                        "type": "tool_result",
                        "ts": ts,
                        "id": call.id,
                        "value": { "text": reason.clone() },
                        "is_error": true,
                    }));
                } else {
                    remaining.push(call.clone());
                }
            }
            to_route = remaining;
        }
        "approve" => {
            let prompt = payload_val
                .get("prompt")
                .and_then(|v| v.as_str())
                .unwrap_or("approve this tool call?")
                .to_string();
            let call_id = payload_val
                .get("call_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            let req_id = format!("apr-{call_id}-{ts}");
            let approval_request = serde_json::json!({
                "v": 1,
                "type": "approval_request",
                "ts": ts,
                "id": req_id,
                "tool_call_id": call_id,
                "prompt": prompt,
            });
            append_event(cfg, &session.path, &approval_request);

            // Route the unblocked calls now.
            let blocked_call = calls.iter().find(|c| c.id == call_id).cloned();
            let unblocked: Vec<ToolCallEvent> =
                calls.iter().filter(|c| c.id != call_id).cloned().collect();
            let unblocked_results = route_batch(cfg, runner, &unblocked, session);
            for r in &unblocked_results {
                append_line(
                    cfg,
                    &session.path,
                    &serde_json::to_string(&r.value).unwrap_or_default(),
                );
            }

            // Fire tool.after for the unblocked results.
            fire_tool_after(cfg, session, &unblocked_results);

            // Wait for the approval.
            if let Some(call) = blocked_call {
                wait_for_approval(cfg, runner, session, &req_id, &call, &prompt, mode);
            }
            return;
        }
        _ => {}
    }

    // Route the remaining calls.
    let routed = route_batch(cfg, runner, &to_route, session);

    // tool.after observation window.
    fire_tool_after(cfg, session, &routed);

    for v in &synthetic {
        append_event(cfg, &session.path, v);
    }
    for r in &routed {
        append_line(
            cfg,
            &session.path,
            &serde_json::to_string(&r.value).unwrap_or_default(),
        );
    }
}

/// Fire the `tool.after` observation window with the routed results.
fn fire_tool_after(
    cfg: &HarnessConfig,
    session: &SessionDir,
    results: &[harness_common::stage::ToolResultEvent],
) {
    let after_payload = serde_json::json!({
        "window": "tool.after",
        "session": session.path.to_string_lossy(),
        "results": results
            .iter()
            .map(|r| &r.value)
            .collect::<Vec<_>>(),
    });
    let _ = hooks::fire_hooks(
        &cfg.hooks,
        Window::ToolAfter,
        &after_payload,
        &hook_env(cfg, session, "step", Window::ToolAfter),
        cfg.hooks_timeout_ms,
    );
}

// ---------------------------------------------------------------------------
// Approval round-trip.
// ---------------------------------------------------------------------------

fn wait_for_approval(
    cfg: &HarnessConfig,
    runner: &SubprocessRunner,
    session: &SessionDir,
    request_id: &str,
    call: &ToolCallEvent,
    prompt: &str,
    mode: StepMode,
) {
    let poll = std::time::Duration::from_millis(250);
    let deadline = cfg
        .approval_timeout_s
        .map(|s| std::time::Instant::now() + std::time::Duration::from_secs(s));

    loop {
        check_signal(mode);

        if let Some(ans) = read_approval(session, request_id) {
            let decision = ans.get("decision").and_then(|d| d.as_str()).unwrap_or("deny");
            if decision == "allow" {
                let mut edited = call.clone();
                if let Some(args) = ans.get("arguments") {
                    edited.arguments = args.clone();
                }
                let results =
                    route_batch(cfg, runner, std::slice::from_ref(&edited), session);
                for r in &results {
                    append_line(
                        cfg,
                        &session.path,
                        &serde_json::to_string(&r.value).unwrap_or_default(),
                    );
                }
            } else {
                let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
                let deny = serde_json::json!({
                    "v": 1,
                    "type": "tool_result",
                    "ts": ts,
                    "id": call.id,
                    "value": { "text": prompt.to_string() },
                    "is_error": true,
                });
                append_event(cfg, &session.path, &deny);
            }
            return;
        }
        if let Some(dl) = deadline {
            if std::time::Instant::now() >= dl {
                let n = cfg.approval_timeout_s.unwrap_or(0);
                let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
                let deny = serde_json::json!({
                    "v": 1,
                    "type": "tool_result",
                    "ts": ts,
                    "id": call.id,
                    "value": { "text": format!("approval timed out after {n} s") },
                    "is_error": true,
                });
                append_event(cfg, &session.path, &deny);
                return;
            }
        }
        std::thread::sleep(poll);
    }
}

/// Read the matching `approval` event from the log.
fn read_approval(session: &SessionDir, request_id: &str) -> Option<Value> {
    let path = session.path.join("events.jsonl");
    let data = std::fs::read_to_string(&path).ok()?;
    let mut found: Option<Value> = None;
    for line in data.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) != Some("approval") {
            continue;
        }
        if v.get("id").and_then(|i| i.as_str()) != Some(request_id) {
            continue;
        }
        found = Some(v);
    }
    found
}

// ---------------------------------------------------------------------------
// awaiting_approval state: resume after TUI restart or crash.
// ---------------------------------------------------------------------------

fn run_awaiting_approval(
    cfg: &HarnessConfig,
    runner: &SubprocessRunner,
    session: &SessionDir,
    _claim: &harness_common::stage::Claim,
    mode: StepMode,
) {
    let path = session.path.join("events.jsonl");
    let Ok(data) = std::fs::read_to_string(&path) else {
        return;
    };

    // Find the last unanswered approval_request and its tool_call.
    let mut pending_req: Option<Value> = None;
    let mut pending_call: Option<Value> = None;

    for line in data.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match ty {
            "approval_request" => {
                pending_req = Some(v.clone());
                pending_call = None;
            }
            "tool_call" => {
                if let Some(req) = &pending_req {
                    let cid = req.get("tool_call_id").and_then(|i| i.as_str());
                    if let Some(cid) = cid {
                        if v.get("id").and_then(|i| i.as_str()) == Some(cid) {
                            pending_call = Some(v.clone());
                        }
                    }
                }
            }
            "approval" => {
                if let Some(req) = &pending_req {
                    let rid = req.get("id").and_then(|i| i.as_str());
                    if v.get("id").and_then(|i| i.as_str()) == rid {
                        pending_req = None;
                        pending_call = None;
                    }
                }
            }
            _ => {}
        }
    }

    let Some(req) = pending_req else {
        return;
    };
    let req_id = req.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
    let call_id = req
        .get("tool_call_id")
        .and_then(|i| i.as_str())
        .unwrap_or("")
        .to_string();
    let prompt = req
        .get("prompt")
        .and_then(|p| p.as_str())
        .unwrap_or("")
        .to_string();

    let tc = ToolCallEvent {
        id: call_id,
        name: pending_call
            .as_ref()
            .and_then(|c| c.get("name").and_then(|n| n.as_str()))
            .unwrap_or("")
            .to_string(),
        arguments: pending_call
            .as_ref()
            .and_then(|c| c.get("arguments"))
            .cloned()
            .unwrap_or(Value::Null),
    };

    wait_for_approval(cfg, runner, session, &req_id, &tc, &prompt, mode);
}

// ---------------------------------------------------------------------------
// Shared helpers.
// ---------------------------------------------------------------------------

/// The `HARNESS_*` env pairs for a hook invocation. `phase` is the
/// harness subcommand (`step` or `run`); `window` is the exact window
/// being fired so `HARNESS_WINDOW` always reports the real window
/// (docs/loop-lifecycle-hooks.md section 4.2).
pub fn hook_env(
    cfg: &HarnessConfig,
    session: &SessionDir,
    phase: &'static str,
    window: Window,
) -> Vec<(&'static str, String)> {
    hooks::hook_env(
        &session.path.to_string_lossy(),
        &cfg.sessions_root.to_string_lossy(),
        &cfg.config_path.to_string_lossy(),
        phase,
        window,
    )
}

fn extract_tool_calls<'a>(calls: impl Iterator<Item = &'a Value> + 'a) -> Vec<ToolCallEvent> {
    calls
        .filter_map(|v| {
            Some(ToolCallEvent {
                id: v.get("id")?.as_str()?.to_string(),
                name: v.get("name")?.as_str()?.to_string(),
                arguments: v.get("arguments")?.clone(),
            })
        })
        .collect()
}

/// Route a batch of tool calls through `route`.
fn route_batch(
    cfg: &HarnessConfig,
    runner: &SubprocessRunner,
    calls: &[ToolCallEvent],
    session: &SessionDir,
) -> Vec<harness_common::stage::ToolResultEvent> {
    if calls.is_empty() {
        return Vec::new();
    }
    let cwd = {
        let p = session.path.join("cwd");
        std::fs::read_to_string(&p)
            .ok()
            .map(|s| std::path::PathBuf::from(s.trim()))
    };

    let env = RouteEnv {
        tools_root: cfg.tools_root.clone(),
        cwd,
        tool_log: Some(session.path.join("tools.jsonl")),
        tool_result_max_chars: 20000,
    };
    runner.route(calls, &env).unwrap_or_else(|e| {
        eprintln!("harness: route failed: {e}");
        std::process::exit(1);
    })
}

/// Log hook window results.
fn log_hook_window(
    cfg: &HarnessConfig,
    session: &SessionDir,
    window: &str,
    decision: &str,
    results: &[hooks::HookResult],
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
    if !decision.is_empty() {
        let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let event = serde_json::json!({
            "v": 1,
            "type": "ext_status",
            "ts": ts,
            "id": format!("hook.{window}"),
            "value": decision,
        });
        append_event(cfg, &session.path, &event);
    }
}

/// Fire the `overflow.resolve` window on overflow classification.
///
/// Carries the fields docs/loop-lifecycle-hooks.md section 3.4 names:
/// `kind`, `stop_reason`, `detail`, `usage`, `input_budget`,
/// `context_tokens`, and `can_recover`. Returns the folded decision:
/// the first explicit decision, then the exit-2 blocking default
/// (`stop`), then the window default (`stay_compact`).
fn fire_overflow_resolve(
    cfg: &HarnessConfig,
    session: &SessionDir,
    active_model: &str,
    stop_reason: &str,
    detail: &str,
    usage: &Value,
    can_recover: bool,
) -> String {
    let payload = serde_json::json!({
        "window": "overflow.resolve",
        "session": session.path.to_string_lossy(),
        "active_model": active_model,
        "kind": "overflow",
        "stop_reason": stop_reason,
        "detail": detail,
        "usage": usage,
        "input_budget": cfg.input_budget,
        "context_tokens": cfg.context_tokens,
        "can_recover": can_recover,
    });
    let results = hooks::fire_hooks(
        &cfg.hooks,
        Window::OverflowResolve,
        &payload,
        &hook_env(cfg, session, "step", Window::OverflowResolve),
        cfg.hooks_timeout_ms,
    );
    let (decision, _) = hooks::fold_decision(&results, Window::OverflowResolve);
    let decision = decision
        .unwrap_or_else(|| hooks::window_default(Window::OverflowResolve).to_string());
    log_hook_window(cfg, session, "overflow.resolve", &decision, &results);
    decision
}

/// Save the handoff document to `sessions/<n>/handoff.md`. The
/// `compaction_summary` boundary event is appended by the `compact`
/// binary itself, so the loop only persists the document here
/// (docs/phase-2-plan.md 3.5, 4.3 step 5).
fn write_handoff(
    session: &SessionDir,
    summary: &str,
) {
    if let Err(e) = std::fs::write(session.path.join("handoff.md"), summary) {
        eprintln!("harness: cannot write handoff.md: {e}");
    }
}

/// Append the `compaction_summary` boundary marker. Used only on the
/// `replace` path, where a hook supplied its own summary and boundary
/// and the `compact` binary did not run (docs/phase-2-plan.md 4.3).
fn append_compaction_summary(
    cfg: &HarnessConfig,
    session: &SessionDir,
    summary: &str,
    first_kept_seq: u64,
    reason: &CompactReason,
) {
    let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let reason_str = match reason {
        CompactReason::Threshold => "threshold",
        CompactReason::Overflow | CompactReason::LastResort => "overflow",
    };
    let event = serde_json::json!({
        "v": 1,
        "type": "compaction_summary",
        "ts": ts,
        "summary": summary,
        "first_kept_seq": first_kept_seq,
        "reason": reason_str,
        "tokens_before": 0,
    });
    append_event(cfg, &session.path, &event);
}
