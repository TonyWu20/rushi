//! `harness-hook-goal-arm` — goal-mode prompt injection.
//!
//! Registered on the `model.before` window. When the TUI `goal` /
//! `goal_edit` extension command has armed goal mode (appended a
//! `goal_armed` ext_status event to the session log) and the goal
//! tool has not yet been called, this hook injects an instruction
//! into the model request telling it to call the `goal` tool with
//! the user's message as the goal description.
//!
//! Decision contract (docs/loop-lifecycle-hooks.md §3.3):
//! - exit 0 + `{}` → proceed unchanged.
//! - exit 0 + `{"decision":"transform","payload":{"request":{...}}}`
//!   → the harness replaces the request with the hook's version.
//!
//! The marker is considered consumed once a `goal` tool_call appears
//! in the log after the last `goal_armed` marker, so the injection
//! fires only until the model actually calls the tool.

use std::io::Read;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return;
    }

    let payload = read_stdin_json();

    if payload.get("window").and_then(|w| w.as_str()) != Some("model.before") {
        println!("{{}}");
        return;
    }

    let session_dir = match resolve_session_dir() {
        Some(d) => d,
        None => {
            println!("{{}}");
            return;
        }
    };

    // Find the last goal_armed marker in the log and check whether a
    // goal tool_call has appeared after it (consumed).
    let Some(marker_mode) = pending_goal_armed(&session_dir) else {
        println!("{{}}");
        return;
    };

    // Build the injection text based on the marker mode.
    let request = payload
        .get("request")
        .cloned()
        .unwrap_or(serde_json::json!({}));

    let mut req = request.clone();
    let instructions = req
        .get("instructions")
        .and_then(|i| i.as_str())
        .unwrap_or("");

    let injection = match marker_mode.as_str() {
        "edit" => {
            "GOAL MODE (edit): The user has just typed a new goal description \
             to replace the current active goal. Call the `goal` tool with \
             the user's message text as the `goal` argument."
        }
        _ => "GOAL MODE (start): The user has just typed a goal description. \
             Call the `goal` tool with the user's message text as the `goal` \
             argument. Do not start working on the task yourself.",
    };

    let new_instructions = format!("{}\n\n{injection}", instructions);
    req["instructions"] = serde_json::json!(new_instructions);

    let resp = serde_json::json!({
        "decision": "transform",
        "payload": {
            "request": req,
        },
    });
    println!("{}", resp);
}

/// Scan the session log for a pending `goal_armed` ext_status event.
/// Returns the marker's `value` (e.g. "start" or "edit") when the
/// marker is still pending (no `goal` tool_call after it), or `None`
/// when no marker exists or it has been consumed.
fn pending_goal_armed(session_dir: &std::path::Path) -> Option<String> {
    let log_path = session_dir.join("events.jsonl");
    let data = std::fs::read_to_string(&log_path).ok()?;

    // Walk the log from the end. The first `goal` tool_call we meet
    // (most recent first) sits after the marker, so the marker is
    // consumed. The first `goal_armed` marker we meet is pending.
    for line in data.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if ty == "tool_call" && v.get("name").and_then(|n| n.as_str()) == Some("goal") {
            return None;
        }
        if ty == "ext_status" && v.get("id").and_then(|i| i.as_str()) == Some("goal_armed") {
            return Some(
                v.get("value")
                    .and_then(|m| m.as_str())
                    .unwrap_or("start")
                    .to_string(),
            );
        }
    }
    None
}

fn resolve_session_dir() -> Option<std::path::PathBuf> {
    let session = std::env::var("SESSION").ok()?;
    let sessions_root = std::env::var("SESSIONS_ROOT").unwrap_or_else(|_| "sessions".into());
    Some(
        if session.contains('/') || session.contains('\\') {
            std::path::PathBuf::from(&session)
        } else {
            std::path::PathBuf::from(&sessions_root).join(&session)
        },
    )
}

fn read_stdin_json() -> serde_json::Value {
    let mut buf = String::new();
    if std::io::stdin().read_to_string(&mut buf).is_err() || buf.trim().is_empty() {
        return serde_json::json!({});
    }
    serde_json::from_str(&buf).unwrap_or(serde_json::json!({}))
}

fn print_help() {
    println!("harness-hook-goal-arm — goal-mode prompt injection (model.before)");
    println!();
    println!("Window: model.before");
    println!("Input (stdin): {{window, session, model, projected_tokens, request}}");
    println!("Output (stdout):");
    println!("  {{}}  — no pending goal_armed marker; proceed unchanged");
    println!(
        "  {{\"decision\":\"transform\",\"payload\":{{\"request\":{{...}}}}}} \
         — goal instruction injected"
    );
    println!("Exit codes: 0 = ok");
}
