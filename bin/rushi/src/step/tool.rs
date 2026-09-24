//! Tool routing for the step pipeline: re-routing pending calls,
//! the `tool.before` / `tool.after` decision windows, and the batch
//! call to the `route` binary.

use serde_json::Value;

use rushi_common::hooks::{StepStatus, Window};
use rushi_common::stage::{
    Claim, RouteEnv, SessionDir, StageRunner, ToolCallEvent, ToolResultEvent,
};

use crate::config::HarnessConfig;
use crate::stage_runner::SubprocessRunner;
use crate::step::approval::wait_for_approval;
use crate::step::hook::{log_pipeline, run_window};
use crate::step::logio::{append_event, append_line, publish_loop_phase};
use crate::step::StepMode;

// ---------------------------------------------------------------------------
// awaiting_tool_result: re-route pending calls without a model call.
// ---------------------------------------------------------------------------

pub fn run_awaiting_tool_result(
    cfg: &HarnessConfig,
    runner: &SubprocessRunner,
    session: &SessionDir,
    claim: &Claim,
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
// Tool routing with tool.before / tool.after hooks and approval.
// ---------------------------------------------------------------------------

pub fn route_and_append(
    cfg: &HarnessConfig,
    runner: &SubprocessRunner,
    session: &SessionDir,
    calls: &[ToolCallEvent],
    mode: StepMode,
) {
    // tool.before decision window (docs/loop-lifecycle-hooks.md 12.5):
    // an ordered pipeline of named defs. Steps accumulate state; the
    // first abort or fail stops the chain.
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
    let run = run_window(cfg, session, Window::ToolBefore, &payload, "step");
    let state = &run.state;

    let mut to_route: Vec<ToolCallEvent> = calls.to_vec();
    let mut synthetic: Vec<Value> = Vec::new();

    // Resolution (12.5 effect table). A failed chain keeps the window
    // default (proceed, P4). An abort vetoes the default and blocks
    // the whole batch. Otherwise the state fields `approval` and
    // `blocked_calls` drive the outcome.
    let mut outcome = "proceed";

    if run.aborted {
        outcome = "block";
        let reason = run
            .steps
            .iter()
            .rev()
            .find(|s| s.status == StepStatus::Abort)
            .map(|s| s.detail.clone())
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| "tool call blocked by hook".to_string());
        for call in calls {
            let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            synthetic.push(serde_json::json!({
                "v": 1,
                "type": "tool_result",
                "ts": ts,
                "id": call.id,
                "value": { "text": reason.clone() },
                "is_error": true,
            }));
        }
        to_route.clear();
    } else if let Some(approval) = state.get("approval").filter(|v| v.is_object()) {
        outcome = "approve";
        let prompt = approval
            .get("prompt")
            .and_then(|v| v.as_str())
            .unwrap_or("approve this tool call?")
            .to_string();
        let call_id = approval
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

        // tool.after window: a hook may rewrite results by call id.
        let final_unblocked = fire_tool_after(cfg, session, &unblocked, &unblocked_results);
        for r in &final_unblocked {
            append_line(
                cfg,
                &session.path,
                &serde_json::to_string(r).unwrap_or_default(),
            );
        }

        // Wait for the approval.
        if let Some(call) = blocked_call {
            wait_for_approval(cfg, runner, session, &req_id, &call, &prompt, mode);
        }
        log_pipeline(cfg, session, Window::ToolBefore, &run, Some(outcome), Some(&payload));
        return;
    } else if let Some(blocked) = state
        .get("blocked_calls")
        .and_then(|v| v.as_array())
        .filter(|a| !a.is_empty())
    {
        outcome = "block";
        for entry in blocked {
            let id = entry
                .get("id")
                .and_then(|v| v.as_str())
                .map(String::from)
                .or_else(|| entry.as_str().map(String::from));
            let reason = entry
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("tool call blocked by hook")
                .to_string();
            let Some(id) = id else {
                continue;
            };
            let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            synthetic.push(serde_json::json!({
                "v": 1,
                "type": "tool_result",
                "ts": ts,
                "id": id,
                "value": { "text": reason },
                "is_error": true,
            }));
            to_route.retain(|c| c.id != id);
        }
    }

    log_pipeline(cfg, session, Window::ToolBefore, &run, Some(outcome), Some(&payload));

    // Route the remaining calls.
    let routed = route_batch(cfg, runner, &to_route, session);

    // tool.after window: a hook may rewrite results by call id.
    let final_results = fire_tool_after(cfg, session, &to_route, &routed);

    for v in &synthetic {
        append_event(cfg, &session.path, v);
    }
    for r in &final_results {
        append_line(
            cfg,
            &session.path,
            &serde_json::to_string(r).unwrap_or_default(),
        );
    }
}

/// Fire the `tool.after` window and return the final result values to
/// append to the log.
///
/// The pipeline receives the routed `calls` (with their original
/// arguments, e.g. a read call's `file_path`) and the routed
/// `results`. A step may emit a `results` state field: a keyed map
/// from call id to new `tool_result` JSON. The kernel splices each
/// entry into the routed results by matching the call id. Calls a
/// step does not mention keep their routed result. A rewrite that
/// lists every call id is a whole swap. This is the extension point
/// that turns a hard-rejected binary read (an oversized image) into a
/// success carrying the compressed payload (docs/image-read-kiss.md).
/// A stopped chain (abort or fail) publishes the routed results
/// unchanged.
fn fire_tool_after(
    cfg: &HarnessConfig,
    session: &SessionDir,
    calls: &[ToolCallEvent],
    results: &[ToolResultEvent],
) -> Vec<Value> {
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
    let results_json: Vec<Value> =
        results.iter().map(|r| r.value.clone()).collect();
    let after_payload = serde_json::json!({
        "window": "tool.after",
        "session": session.path.to_string_lossy(),
        "calls": calls_json,
        "results": results_json,
    });
    let run = run_window(cfg, session, Window::ToolAfter, &after_payload, "step");

    let mut final_values: Vec<Value> =
        results.iter().map(|r| r.value.clone()).collect();

    // The `results` state field (docs/loop-lifecycle-hooks.md 12.5)
    // splices per-call result rewrites by call id. It replaces the old
    // `transform` decision word. On a stopped chain the routed
    // results stand unchanged.
    let mut spliced = false;
    if run.completed() {
        if let Some(results_map) = run.state.get("results").and_then(|r| r.as_object()) {
            for (id, new_result) in results_map {
                if let Some(pos) = final_values
                    .iter()
                    .position(|v| v.get("id").and_then(|x| x.as_str()) == Some(id.as_str()))
                {
                    // Normalize the envelope so the spliced event passes
                    // schema validation. A hook that omits `ts` would
                    // otherwise be skipped by the claim main loop while
                    // the resolved-id scan still settles the session,
                    // hiding the owed model call.
                    let mut r = new_result.clone();
                    if let Some(obj) = r.as_object_mut() {
                        if obj.get("ts").is_none() {
                            let ts = chrono::Utc::now()
                                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
                            obj.insert("ts".to_string(), Value::String(ts));
                        }
                        if obj.get("v").is_none() {
                            obj.insert("v".to_string(), Value::Number(1.into()));
                        }
                        if obj.get("type").is_none() {
                            obj.insert(
                                "type".to_string(),
                                Value::String("tool_result".to_string()),
                            );
                        }
                        if obj.get("is_error").is_none() {
                            obj.insert("is_error".to_string(), Value::Bool(false));
                        }
                        if obj.get("id").is_none() {
                            obj.insert("id".to_string(), Value::String(id.clone()));
                        }
                    }
                    final_values[pos] = r;
                    spliced = true;
                }
            }
        }
    }

    log_pipeline(
        cfg,
        session,
        Window::ToolAfter,
        &run,
        Some(if spliced { "transform" } else { "noop" }),
        Some(&after_payload),
    );

    final_values
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
pub fn route_batch(
    cfg: &HarnessConfig,
    runner: &SubprocessRunner,
    calls: &[ToolCallEvent],
    session: &SessionDir,
) -> Vec<ToolResultEvent> {
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
        native_tool_paths: cfg.native_tool_paths.clone(),
        extension_tool_paths: cfg.extension_tool_paths.clone(),
        cwd,
        tool_log: Some(session.path.join("tools.jsonl")),
        tool_result_max_chars: 20000,
        session_dir: Some(session.path.clone()),
    };
    runner.route(calls, &env).unwrap_or_else(|e| {
        eprintln!("rushi: route failed: {e}");
        std::process::exit(1);
    })
}
