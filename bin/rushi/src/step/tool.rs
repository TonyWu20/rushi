//! Tool routing for the step pipeline: re-routing pending calls,
//! the `tool.before` / `tool.after` decision windows, and the batch
//! call to the `route` binary.

use serde_json::Value;

use rushi_common::hooks::{self, Window};
use rushi_common::stage::{
    Claim, RouteEnv, SessionDir, StageRunner, ToolCallEvent, ToolResultEvent,
};

use crate::config::HarnessConfig;
use crate::stage_runner::SubprocessRunner;
use crate::step::approval::wait_for_approval;
use crate::step::hook::{hook_env, log_hook_window};
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
            return;
        }
        _ => {}
    }

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
/// The hook receives the routed `calls` (with their original arguments,
/// e.g. a read call's `file_path`) and the routed `results`. A hook may
/// emit a `transform` decision carrying per-call result rewrites in
/// `payload.results`, a keyed map from call id to new `tool_result`
/// JSON. The kernel splices each entry into the routed results by
/// matching the call id. Calls the hook does not mention keep their
/// routed result. A transform that lists every call id is a whole
/// swap. This is the extension point that turns a hard-rejected binary
/// read (an oversized image) into a success carrying the compressed
/// payload (docs/image-read-kiss.md).
///
/// One decision word, `transform`, is shared with the `model.before`
/// window. There the payload is the whole request object instead of a
/// keyed map, because a request is a single object (docs/loop-
/// lifecycle-hooks.md section 4.3).
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
    let results_out = hooks::fire_hooks(
        &cfg.hooks,
        Window::ToolAfter,
        &after_payload,
        &hook_env(cfg, session, "step", Window::ToolAfter),
        cfg.hooks_timeout_ms,
    );
    let (decision, payload_val) = hooks::fold_decision(&results_out, Window::ToolAfter);
    log_hook_window(
        cfg,
        session,
        "tool.after",
        decision.as_deref().unwrap_or(""),
        &results_out,
    );

    let mut final_values: Vec<Value> =
        results.iter().map(|r| r.value.clone()).collect();

    // `transform`: splice per-call result rewrites by call id.
    // payload.results is a keyed map: call id -> new tool_result JSON.
    // Listing every call id is a whole swap.
    if decision.as_deref() == Some("transform") {
        if let Some(results_map) = payload_val.get("results").and_then(|r| r.as_object()) {
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
                }
            }
        }
    }

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
