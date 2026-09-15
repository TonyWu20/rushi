//! The approval round-trip: waiting on the `approval` event, and the
//! `awaiting_approval` resume path after a TUI restart or crash.

use serde_json::Value;

use rushi_common::stage::{Claim, SessionDir, ToolCallEvent};

use crate::config::HarnessConfig;
use crate::stage_runner::SubprocessRunner;
use crate::step::logio::{append_event, append_line};
use crate::step::tool::route_batch;
use crate::step::{check_signal, StepMode};

// ---------------------------------------------------------------------------
// Approval round-trip.
// ---------------------------------------------------------------------------

pub(crate) fn wait_for_approval(
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

pub fn run_awaiting_approval(
    cfg: &HarnessConfig,
    runner: &SubprocessRunner,
    session: &SessionDir,
    _claim: &Claim,
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
