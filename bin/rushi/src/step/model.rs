//! The model-facing half of a step: model description, context
//! estimation, the `awaiting_model` pipeline (assemble, retry loop,
//! parse, tool routing), and the overflow/length recovery paths.

use std::path::Path;

use serde_json::Value;

use rushi_common::compact_math;
use rushi_common::hooks::{PipelineRun, StepStatus, Window};
use rushi_common::stage::{
    AssembleOpts, Claim, CompactOutcome, CompactReason, ModelOutput, RequestFile, SessionDir,
    StageRunner, ToolCallEvent,
};

use crate::classifier::is_overflow;
use crate::config::HarnessConfig;
use crate::stage_runner::SubprocessRunner;
use crate::step::compact::{post_compact_sanity, try_compact_with_hooks};
use crate::step::hook::{
    fire_observation, fire_overflow_resolve, log_pipeline, resolve_strategy_window, run_window,
};
use crate::step::logio::{
    append_event, append_line, append_terminal_error, last_user_message_seq, publish_ext_status,
    publish_loop_phase,
};
use crate::step::tool::route_and_append;
use crate::step::{check_signal, StepMode};

// ---------------------------------------------------------------------------
// model_thinking marker
// ---------------------------------------------------------------------------

/// Publish `model_thinking` ext_status on-change using a pre-resolved `Describe`.
pub fn publish_model_thinking(cfg: &HarnessConfig, session_dir: &Path, describe: &Describe) {
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
// awaiting_model (and idle-with-followups): the full pipeline.
// ---------------------------------------------------------------------------

pub fn run_awaiting_model(
    cfg: &HarnessConfig,
    runner: &SubprocessRunner,
    session: &SessionDir,
    claim: &Claim,
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
            eprintln!("rushi: assemble failed: {e}");
            std::process::exit(1);
        }
    };
    log_and_strip_hard_trim(cfg, session, &mut request);

    // Assemble error form: append and stop.
    if request.json.get("type").and_then(|t| t.as_str()) == Some("error") {
        append_event(cfg, &session.path, &request.json);
        return;
    }

    // context_exhausted form: fire exhausted.handle window.
    if request.json.get("type").and_then(|t| t.as_str()) == Some("context_exhausted") {
        let should_continue = fire_and_handle_exhausted(cfg, runner, session, describe);
        if !should_continue {
            return;
        }
        // Reassemble after the in-session compact and fall through to the model retry loop.
        reassemble(cfg, runner, session, &mut request, false);

        // If the reassembled request is still a context_exhausted form the
        // compact did not shrink the context enough; do not feed a
        // non-request JSON to the model binary.  Log a terminal error and
        // stop the step.
        if request.json.get("type").and_then(|t| t.as_str()) == Some("context_exhausted") {
            let msg = "context still exhausted after in-session compact; stopping";
            append_terminal_error(cfg, &session.path, msg);
            return;
        }
    }

    // The model retry loop.
    // Create the session-local stream channel so the model can write
    // live deltas while the TUI polls it (docs/tui-streaming-response.md §5.1).
    let stream_path = session.path.join(crate::stream_channel::MODEL_STREAM_FILE);
    if let Err(e) = std::fs::File::create(&stream_path) {
        eprintln!("rushi: warning: cannot create stream channel {stream_path:?}: {e}");
    }
    crate::stream_channel::register(&stream_path);

    let (output, terminal_logged) = model_retry_loop(
        cfg, runner, session, &mut request, &guard_model, describe, mode, Some(&stream_path),
    );

    // The call returned (success or terminal error): close the channel.
    crate::stream_channel::unregister();
    let _ = std::fs::remove_file(&stream_path);

    check_signal(mode);

    // If a terminal error was already appended, do not parse or route.
    if terminal_logged {
        return;
    }

    // Parse.
    let parsed = match runner.parse(&output) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("rushi: parse failed: {e}");
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

pub(crate) struct Describe {
    pub active: String,
    pub model_id: String,
    pub thinking_level: u32,
}

pub fn describe_model(cfg: &HarnessConfig) -> Describe {
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

/// Estimate the context size from the session log.
///
/// Delegates to [`compact_math::estimate_from_events`] which applies
/// rewind masking, boundary detection, and the appropriate estimation
/// strategy (measured anchor for clean logs, full-form + margin for
/// compacted logs).
fn estimate_context(cfg: &HarnessConfig, session_dir: &Path) -> u64 {
    let path = session_dir.join("events.jsonl");
    let Ok(data) = std::fs::read_to_string(&path) else {
        return 0;
    };
    let events: Vec<Value> = data
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
        .collect();
    compact_math::estimate_from_events(&events, cfg.estimate_chars_per_token)
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
    let run = run_window(cfg, session, Window::ExhaustedHandle, &payload, "step");
    let decision = resolve_strategy_window(&run).to_string();
    log_pipeline(cfg, session, Window::ExhaustedHandle, &run, Some(&decision), Some(&payload));

    match decision.as_str() {
        "stay_compact" => {
            let status =
                try_compact_with_hooks(cfg, runner, session, CompactReason::LastResort, true, false);
            if status.outcome == CompactOutcome::Compacted {
                eprintln!("rushi: in-session shadow compact ran");
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

/// Apply a `model.before` pipeline run (docs/loop-lifecycle-hooks.md
/// 12.6, issue #38).
///
/// A completed chain hands over the accumulated request object; the
/// kernel merges `prompt_fragments` into `instructions` and strips
/// the field exactly once at the end of the pipeline. Fragments stay
/// visible for the whole chain: the merge walks the original request
/// and every step's output in order, keeps first-seen id order, and
/// later step text wins on a same-id collision.
///
/// An aborted or failed chain leaves the original request in place
/// (the window default, P4); the chain marker records the stop
/// point and a failed step's `hook.model.before.error` marker. A
/// final state that is not a request object (for instance the
/// legacy `decision`/`payload` envelope of a hook not yet migrated
/// to the pipeline ABI) is a non-blocking failure: the log carries
/// `hook.model.before.error` and the original request proceeds.
///
/// issue #24 no-op detection: a chain whose final request is
/// semantically equal to the original breaks no cache, so no
/// `hook_applied` marker is written; a changed request writes one
/// marker per step that ran ok.
fn apply_model_before_pipeline(
    cfg: &HarnessConfig,
    session: &SessionDir,
    run: &PipelineRun,
    request: &mut RequestFile,
) {
    if run.aborted || run.failed {
        log_pipeline(cfg, session, Window::ModelBefore, run, Some("noop"), None);
        return;
    }

    let original = request.json.clone();

    // A completed chain's state is the request object itself (12.6).
    // A state that is not a request object fails the window
    // non-blocking (P4): log the error, keep the original request.
    let is_request = run.state.is_object()
        && run.state.get("input").map(|v| !v.is_null()).unwrap_or(false);
    if !is_request {
        publish_ext_status(
            cfg,
            &session.path,
            "hook.model.before.error",
            &Value::String(
                "model.before pipeline state is not a request object; the original request proceeds"
                    .to_string(),
            ),
        );
        log_pipeline(cfg, session, Window::ModelBefore, run, Some("noop"), None);
        return;
    }

    request.json = run.state.clone();

    // Join `prompt_fragments` into `instructions` and strip the
    // field exactly once at the end of the pipeline
    // (docs/system-prompt-generation.md D5). The merge walks the
    // original request and each step's output in list order;
    // first-seen id order is kept, later step text wins on a
    // same-id collision. The key list is logged (keys only, never
    // the values: cache + privacy discipline).
    let mut order: Vec<String> = Vec::new();
    let mut texts: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    for src in std::iter::once(&original).chain(run.step_outputs.iter()) {
        if let Some(arr) = src.get("prompt_fragments").and_then(|f| f.as_array()) {
            for pair in arr {
                let n = pair.as_array().map_or(0, |a| a.len());
                if n < 2 {
                    continue;
                }
                let Some(id) = pair.get(0).and_then(|v| v.as_str()) else {
                    continue;
                };
                let Some(text) = pair.get(1).and_then(|v| v.as_str()) else {
                    continue;
                };
                if text.is_empty() {
                    continue;
                }
                if !texts.contains_key(id) {
                    order.push(id.to_string());
                }
                texts.insert(id.to_string(), text.to_string());
            }
        }
    }
    if !order.is_empty() {
        let joined: String = order
            .iter()
            .map(|id| texts[id].as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        let base = request
            .json
            .get("instructions")
            .and_then(|i| i.as_str())
            .unwrap_or("");
        let new_instructions = if base.is_empty() {
            joined
        } else {
            format!("{base}\n\n{joined}")
        };
        request.json["instructions"] = Value::String(new_instructions);
        if let Some(obj) = request.json.as_object_mut() {
            obj.remove("prompt_fragments");
        }
        let ts =
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let marker = serde_json::json!({
            "v": 1,
            "type": "ext_status",
            "ts": ts,
            "id": "hook.model.before.transform",
            "value": { "fragments": order },
        });
        append_event(cfg, &session.path, &marker);
    }

    let changed = request.json != original;
    log_pipeline(
        cfg,
        session,
        Window::ModelBefore,
        run,
        Some(if changed { "transform" } else { "noop" }),
        None,
    );
    if changed {
        // One `hook_applied` marker per step that ran ok (issue #19,
        // step order). The value is the step's command (docs 4.5).
        for s in run.steps.iter().filter(|s| s.status == StepStatus::Ok) {
            publish_ext_status(cfg, &session.path, "hook_applied", &Value::String(s.command.clone()));
        }
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
    request: &mut RequestFile,
    guard_model: &str,
    describe: &Describe,
    mode: StepMode,
    delta_file: Option<&std::path::Path>,
) -> (ModelOutput, bool) {
    let mut empty_attempts = 0usize;
    let mut model_err_retries = 0usize;
    let mut overflow_recovered = false;
    let mut last_resort = false;
    let mut last_output = ModelOutput { json: Value::Null };
    let terminal_logged = false;

    'outer: loop {
        check_signal(mode);

        // model.before pipeline (docs/loop-lifecycle-hooks.md 12.6):
        // the accumulated state is the request object itself. Steps
        // run in list order; step N+1 receives step N's output. An
        // aborted or failed chain leaves the original request in
        // place.
        let run = run_window(cfg, session, Window::ModelBefore, &request.json, "step");
        apply_model_before_pipeline(cfg, session, &run, request);

        // Delivery-boundary marker (docs/tui-pending-user-messages.md P7):
        // record the last user_message seq in the log. The claim state
        // machine uses this to keep a steer message that arrived while
        // this call ran pending for the next step.
        let user_seq = last_user_message_seq(&session.path);
        let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let marker = serde_json::json!({
            "v": 1,
            "type": "ext_status",
            "ts": ts,
            "id": "model_call_context",
            "value": { "user_seq": user_seq },
        });
        append_event(cfg, &session.path, &marker);

        let output = match runner.model(request, delta_file) {
            Ok(o) => o,
            Err(e) => {
                model_err_retries += 1;
                if model_err_retries <= 2 {
                    eprintln!(
                        "rushi: model spawn/IO failed ({e}); retry {model_err_retries}/2 in 3s"
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
        fire_observation(cfg, session, Window::ModelAfter, &payload);

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
                        reassemble(cfg, runner, session, request, false);
                        continue;
                    }
                    reassemble(cfg, runner, session, request, false);
                    continue;
                }
                last_resort = true;
                let _ = try_compact_with_hooks(
                    cfg, runner, session, CompactReason::LastResort, true, false,
                );
                reassemble(cfg, runner, session, request, false);
                continue;
            }
            // Silent-overflow early detection.  Some providers (e.g.
            // SGLang) report a context overflow as an HTTP-200 SSE
            // stream that ends in `response.failed` with a null error
            // body.  The `detail` field is empty, so the
            // `is_overflow` pattern match above sees no signal.  When
            // the context estimate already exceeds the trigger, compact
            // immediately instead of burning 3 futile retries.
            if detail.trim().is_empty()
                && cfg.compact_enabled
                && !overflow_recovered
                && !last_resort
            {
                let est = estimate_context(cfg, &session.path);
                let trigger = cfg.trigger_level();
                if trigger > 0 && est > trigger {
                    eprintln!(
                        "rushi: silent overflow suspected (empty detail, estimate {est} > trigger {trigger}); compacting before retry"
                    );
                    overflow_recovered = true;
                    let status = try_compact_with_hooks(
                        cfg, runner, session, CompactReason::Overflow, false, false,
                    );
                    match status.outcome {
                        // The compact shrank the context: re-include the
                        // smaller context and retry immediately.
                        CompactOutcome::Compacted => {
                            reassemble(cfg, runner, session, request, false);
                            model_err_retries = 0;
                            continue;
                        }
                        // Nothing left to compact: the kept region already
                        // fits the keep budget, so the estimate still reads
                        // above the trigger only because of its 25% safety
                        // margin. Do NOT escalate to last_resort here: a
                        // transient empty-detail model error should get the
                        // ordinary retry budget, not an immediate terminal
                        // stop. Reset the retry counter and fall back to
                        // the transport path below; a genuine
                        // unresolvable overflow still stops once the
                        // retries exhaust (the post-failure rescue at the
                        // bottom is gated on !overflow_recovered, which is
                        // now set, so it will not double-fire).
                        CompactOutcome::Noop => {
                            eprintln!(
                                "rushi: silent-overflow compact was a noop; context already at its keep budget, retrying without escalating"
                            );
                            reassemble(cfg, runner, session, request, false);
                            model_err_retries = 0;
                            continue;
                        }
                        // The compact genuinely failed (summary call error,
                        // empty summary, ...): escalate to the more
                        // aggressive last-resort compact before giving up.
                        CompactOutcome::Failed => {
                            last_resort = true;
                            let _ = try_compact_with_hooks(
                                cfg, runner, session, CompactReason::LastResort, true, false,
                            );
                            reassemble(cfg, runner, session, request, false);
                            model_err_retries = 0;
                            continue;
                        }
                    }
                }
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
                    "rushi: model API error ({detail}); retry {model_err_retries}/2 in 3s"
                );
                std::thread::sleep(std::time::Duration::from_secs(3));
                continue;
            }
            // Retries exhausted. Before declaring failure, check whether
            // our own context estimate exceeds the trigger. This catches
            // the stale-anchor case where the measured input_tokens in the
            // log undercounts the true context size.
            if cfg.compact_enabled && !overflow_recovered {
                let est = estimate_context(cfg, &session.path);
                let trigger = cfg.trigger_level();
                if trigger > 0 && est > trigger {
                    eprintln!(
                        "rushi: context estimate {est} exceeds trigger {trigger} after API failure; attempting compact"
                    );
                    overflow_recovered = true;
                    let status = try_compact_with_hooks(
                        cfg, runner, session, CompactReason::Overflow, false, false,
                    );
                    match status.outcome {
                        // The compact shrank the context: re-include the
                        // smaller context and retry immediately.
                        CompactOutcome::Compacted => {
                            reassemble(cfg, runner, session, request, false);
                            model_err_retries = 0;
                            continue;
                        }
                        // Nothing left to compact: the kept region already
                        // fits the keep budget, so the estimate still reads
                        // above the trigger only because of its 25% safety
                        // margin. Do NOT escalate to last_resort here: a
                        // transient model error should get another retry
                        // budget, not an immediate terminal stop. Reset the
                        // retry counter and keep retrying; a genuine
                        // unresolvable overflow still stops once the next
                        // retries exhaust (overflow_recovered is now set, so
                        // this rescue will not double-fire).
                        CompactOutcome::Noop => {
                            eprintln!(
                                "rushi: post-failure compact was a noop; context already at its keep budget, retrying without escalating"
                            );
                            reassemble(cfg, runner, session, request, false);
                            model_err_retries = 0;
                            continue;
                        }
                        // The compact genuinely failed (summary call error,
                        // empty summary, ...): escalate to the more
                        // aggressive last-resort compact before giving up.
                        CompactOutcome::Failed => {
                            last_resort = true;
                            let _ = try_compact_with_hooks(
                                cfg, runner, session, CompactReason::LastResort, true, false,
                            );
                            reassemble(cfg, runner, session, request, false);
                            model_err_retries = 0;
                            continue;
                        }
                    }
                }
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
                    && measured >= cfg.compact_overflow_budget()
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
                    reassemble(cfg, runner, session, request, false);
                    continue;
                }
                last_resort = true;
                let _ = try_compact_with_hooks(
                    cfg, runner, session, CompactReason::LastResort, true, true,
                );
                reassemble(cfg, runner, session, request, false);
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
    cfg: &HarnessConfig,
    runner: &SubprocessRunner,
    session: &SessionDir,
    request: &mut RequestFile,
    inject_follow: bool,
) {
    let opts = AssembleOpts { inject_follow };
    match runner.assemble(session, &opts) {
        Ok(r) => {
            *request = r;
            log_and_strip_hard_trim(cfg, session, request);
        }
        Err(e) => {
            eprintln!("rushi: reassemble failed: {e}");
            std::process::exit(1);
        }
    }
}

/// Log the assemble hard-trim marker (docs/auto-compact-plan.md
/// section 9.8) as a `hard_trim` ext_status event and strip it from
/// the request: the marker is a log record only. The wire form the
/// provider sees must not carry it.
fn log_and_strip_hard_trim(
    cfg: &HarnessConfig,
    session: &SessionDir,
    request: &mut RequestFile,
) {
    let Some(marker) = request.json.get("hard_trim").cloned() else {
        return;
    };
    let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let event = serde_json::json!({
        "v": 1,
        "type": "ext_status",
        "ts": ts,
        "id": "hard_trim",
        "value": marker,
    });
    append_event(cfg, &session.path, &event);
    if let Some(obj) = request.json.as_object_mut() {
        obj.remove("hard_trim");
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

#[cfg(test)]
mod tests {
    use super::*;
    use rushi_common::hooks::{PipelineRun, StepOutcome, StepStatus};

    /// Minimal `HarnessConfig` for marker tests: `append_event`
    /// ignores the cfg, so every field just needs to exist.
    fn stub_cfg(root: &std::path::Path) -> HarnessConfig {
        let bin = root.join("bin");
        HarnessConfig {
            config_path: root.join("config.toml"),
            config_dir: root.to_path_buf(),
            sessions_root: root.to_path_buf(),
            native_tool_paths: vec![],
            extension_tool_paths: vec![],
            active_model: "stub".to_string(),
            model_id: "stub".to_string(),
            max_output_tokens: 32768,
            context_tokens: 131072,
            input_budget: 98304,
            context_budget: 98304,
            last_measured_input: 0,
            compact_enabled: false,
            compact_strategy: "compact".to_string(),
            compact_reserve_tokens: 16384,
            compact_keep_tokens: 16384,
            compact_text_chars: 5000,
            estimate_chars_per_token: 4,
            approval_timeout_s: None,
            hooks_timeout_ms: 10000,
            hook_defs: std::collections::BTreeMap::new(),
            hook_pipelines: std::collections::BTreeMap::new(),
            model_bin: bin.join("model"),
            compact_bin: bin.join("compact"),
            assemble_bin: bin.join("assemble"),
            route_bin: bin.join("route"),
            claim_bin: bin.join("claim"),
            parse_bin: bin.join("parse"),
            log_bin: bin.join("log"),
        }
    }

    /// Build a `model.before` `PipelineRun`: per-step statuses + def
    /// names, the final accumulated state, and the per-step outputs
    /// (so fragment accumulation across the chain is exercised).
    fn model_before_run(
        steps: &[(StepStatus, &str)],
        final_state: Value,
        step_outputs: Vec<Value>,
    ) -> PipelineRun {
        PipelineRun {
            window: Window::ModelBefore,
            steps: steps
                .iter()
                .map(|(st, def)| StepOutcome {
                    def: def.to_string(),
                    command: def.to_string(),
                    status: *st,
                    detail: String::new(),
                })
                .collect(),
            stop: None,
            aborted: steps.iter().any(|(st, _)| *st == StepStatus::Abort),
            failed: steps.iter().any(|(st, _)| *st == StepStatus::Fail),
            state: final_state,
            step_outputs,
        }
    }

    /// Read back the values of all `hook_applied` markers, in log order.
    fn hook_applied_values(session_dir: &Path) -> Vec<Value> {
        let data = std::fs::read_to_string(session_dir.join("events.jsonl")).unwrap();
        data.lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| {
                v.get("type").and_then(|t| t.as_str()) == Some("ext_status")
                    && v.get("id").and_then(|i| i.as_str()) == Some("hook_applied")
            })
            .map(|v| v.get("value").cloned().unwrap_or(Value::Null))
            .collect()
    }

    /// issue #19 (pipeline form, P7): a chain whose two steps both
    /// run ok produces one `hook_applied` marker per step that ran
    /// ok, in step order.
    #[test]
    fn two_ok_steps_emit_two_applied_markers() {
        let root = tempfile::tempdir().unwrap();
        let session_dir = root.path().join("sessions").join("test");
        std::fs::create_dir_all(&session_dir).unwrap();
        let cfg = stub_cfg(root.path());
        let session = SessionDir {
            path: session_dir.clone(),
        };

        let original = serde_json::json!({ "model": "m1", "instructions": "base", "input": [] });
        let step1_out = serde_json::json!({ "model": "m1", "instructions": "base", "input": [] });
        let final_state = serde_json::json!({ "model": "m2", "instructions": "base instructions", "input": [] });
        let run = model_before_run(
            &[(StepStatus::Ok, "harness-hook-goal-arm"), (StepStatus::Ok, "harness-simple-english")],
            final_state,
            vec![step1_out],
        );

        let mut request = RequestFile {
            json: original.clone(),
        };
        apply_model_before_pipeline(&cfg, &session, &run, &mut request);

        let values = hook_applied_values(&session_dir);
        assert_eq!(values.len(), 2, "expected two hook_applied markers");
        assert_eq!(
            values,
            vec![
                Value::String("harness-hook-goal-arm".to_string()),
                Value::String("harness-simple-english".to_string()),
            ],
            "markers must be in step order"
        );
        // The transform itself must have landed.
        assert_eq!(request.json.get("model").and_then(|m| m.as_str()), Some("m2"));
    }

    /// A mixed chain (noop, ok, ok) emits one marker per step that
    /// ran ok, in step order.
    #[test]
    fn mixed_steps_emit_one_marker_per_ok_step() {
        let root = tempfile::tempdir().unwrap();
        let session_dir = root.path().join("sessions").join("test");
        std::fs::create_dir_all(&session_dir).unwrap();
        let cfg = stub_cfg(root.path());
        let session = SessionDir {
            path: session_dir.clone(),
        };

        let original = serde_json::json!({ "model": "m1", "input": [] });
        let step1_out = original.clone();
        let step2_out = serde_json::json!({ "model": "m2", "input": [] });
        let final_state = serde_json::json!({ "model": "m3", "instructions": "x", "input": [] });
        let run = model_before_run(
            &[
                (StepStatus::Noop, "harness-hook-passthrough"),
                (StepStatus::Ok, "harness-hook-a"),
                (StepStatus::Ok, "harness-hook-b"),
            ],
            final_state,
            vec![step1_out, step2_out],
        );

        let mut request = RequestFile {
            json: original,
        };
        apply_model_before_pipeline(&cfg, &session, &run, &mut request);

        let values = hook_applied_values(&session_dir);
        assert_eq!(
            values,
            vec![
                Value::String("harness-hook-a".to_string()),
                Value::String("harness-hook-b".to_string()),
            ]
        );
    }

    /// issue #24 (pipeline form): a chain whose final request is
    /// semantically equal to the original (a fragment join that
    /// reconstructs the same `instructions`) breaks no cache, so no
    /// `hook_applied` marker is written. The `hook.model.before`
    /// resolution marker is still logged.
    #[test]
    fn noop_chain_writes_no_applied_marker() {
        let root = tempfile::tempdir().unwrap();
        let session_dir = root.path().join("sessions").join("test");
        std::fs::create_dir_all(&session_dir).unwrap();
        let cfg = stub_cfg(root.path());
        let session = SessionDir {
            path: session_dir.clone(),
        };

        // The original request already carries the joined fragment.
        let original = serde_json::json!({
            "model": "m1",
            "instructions": "base\n\ngoal-fragment",
            "input": []
        });
        // Step outputs re-expose the fragment instead of the joined
        // text; the kernel joins it back at the end of the pipeline.
        let step_out = serde_json::json!({
            "model": "m1",
            "instructions": "base",
            "input": [],
            "prompt_fragments": [["goal", "goal-fragment"]]
        });
        let run = model_before_run(
            &[(StepStatus::Ok, "harness-hook-goal-arm"), (StepStatus::Ok, "harness-simple-english")],
            step_out.clone(),
            vec![step_out],
        );

        let mut request = RequestFile {
            json: original.clone(),
        };
        apply_model_before_pipeline(&cfg, &session, &run, &mut request);

        let values = hook_applied_values(&session_dir);
        assert!(
            values.is_empty(),
            "no-op chain must not write hook_applied markers, got {values:?}"
        );
        // The applied request is back to the original.
        assert_eq!(request.json, original);
        // The `hook.model.before` resolution marker is still logged.
        let data = std::fs::read_to_string(session_dir.join("events.jsonl")).unwrap();
        let resolutions = data
            .lines()
            .filter(|l| l.contains("\"id\":\"hook.model.before\""))
            .count();
        assert_eq!(resolutions, 1, "the resolution marker stays logged on a no-op chain");
    }

    /// issue #24 (pipeline form): key order is irrelevant to the
    /// no-op check. A chain whose final request is semantically equal
    /// to the original but lists the keys in another order still
    /// compares equal and writes no marker.
    #[test]
    fn reordered_keys_compare_equal_and_write_no_marker() {
        let root = tempfile::tempdir().unwrap();
        let session_dir = root.path().join("sessions").join("test");
        std::fs::create_dir_all(&session_dir).unwrap();
        let cfg = stub_cfg(root.path());
        let session = SessionDir {
            path: session_dir.clone(),
        };

        let original = serde_json::json!({ "model": "m1", "instructions": "base", "input": [] });
        // Same content, keys in another order.
        let reordered = serde_json::json!({ "instructions": "base", "model": "m1", "input": [] });
        let run = model_before_run(
            &[(StepStatus::Ok, "harness-hook-goal-arm")],
            reordered.clone(),
            vec![reordered],
        );

        let mut request = RequestFile {
            json: original.clone(),
        };
        apply_model_before_pipeline(&cfg, &session, &run, &mut request);

        let values = hook_applied_values(&session_dir);
        assert!(
            values.is_empty(),
            "key-reordered no-op must not write hook_applied markers, got {values:?}"
        );
    }

    /// issue #24 (pipeline form): a step that changes one field of
    /// the request writes one `hook_applied` marker per step that
    /// ran ok.
    #[test]
    fn changed_field_writes_one_marker_per_ok_step() {
        let root = tempfile::tempdir().unwrap();
        let session_dir = root.path().join("sessions").join("test");
        std::fs::create_dir_all(&session_dir).unwrap();
        let cfg = stub_cfg(root.path());
        let session = SessionDir {
            path: session_dir.clone(),
        };

        let original = serde_json::json!({
            "model": "m1",
            "instructions": "base",
            "input": []
        });
        let final_state = serde_json::json!({
            "model": "m1",
            "instructions": "base, changed",
            "input": []
        });
        let run = model_before_run(
            &[(StepStatus::Ok, "harness-hook-goal-arm")],
            final_state.clone(),
            vec![final_state],
        );

        let mut request = RequestFile {
            json: original,
        };
        apply_model_before_pipeline(&cfg, &session, &run, &mut request);

        let values = hook_applied_values(&session_dir);
        assert_eq!(
            values,
            vec![Value::String("harness-hook-goal-arm".to_string())],
            "one marker per step that ran ok"
        );
    }
}
