//! `harness-hook-goal-tools` — the stale-goal-call guard.
//!
//! Registered on the `tool.before` window. It inspects the pending tool
//! batch and blocks goal-tool calls that are inconsistent with the
//! current goal state:
//! - `goal` when a goal is already open (double start).
//! - `goal_complete` / `goal_blocked` when there is no open goal.
//!
//! Decision contract (docs/loop-lifecycle-hooks.md §4.3):
//! - exit 0 + `{}` → proceed with the whole batch (window default)
//! - exit 0 + `{"decision":"block","payload":{"reason":...,"calls":[ids]}}`
//!   → the loop synthesizes a failed `tool_result` per blocked call.

use std::io::Read;

use goal_state::GoalState;

fn main() {
    // Self-documentation.
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return;
    }

    let payload = read_stdin_json();

    // Not our window: no-op.
    if payload.get("window").and_then(|w| w.as_str()) != Some("tool.before") {
        println!("{{}}");
        return;
    }

    let calls = payload
        .get("calls")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();

    let session_dir = match resolve_session_dir() {
        Some(d) => d,
        None => {
            println!("{{}}");
            return;
        }
    };

    let goal = GoalState::load(&session_dir);

    // Collect the ids of the calls to block, with a reason each.
    let mut blocked: Vec<serde_json::Value> = Vec::new();
    for call in &calls {
        let name = call.get("name").and_then(|n| n.as_str()).unwrap_or("");
        let id = call.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
        match name {
            "goal" => {
                if let Some(g) = &goal {
                    if g.is_open() {
                        blocked.push(serde_json::json!({
                            "id": id,
                            "reason": format!(
                                "A goal is already open: \"{}\". Use goal_complete or goal_blocked to close it before starting a new goal.",
                                g.goal
                            ),
                        }));
                    }
                }
            }
            "goal_complete" | "goal_blocked" => {
                match &goal {
                    None => blocked.push(serde_json::json!({
                        "id": id,
                        "reason": format!(
                            "{name} failed: no goal is open. Start one with the `goal` tool first."
                        ),
                    })),
                    Some(g) if !g.is_open() => blocked.push(serde_json::json!({
                        "id": id,
                        "reason": format!(
                            "{name} failed: the goal \"{}\" is already {}.",
                            g.goal,
                            if g.blocked { "blocked" } else { "complete" }
                        ),
                    })),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    if blocked.is_empty() {
        println!("{{}}");
        return;
    }

    // The loop reads `calls` as a list of ids (or as objects with
    // `reason` when present). Provide both an id list and the reasons so
    // the synthesized tool_result carries the guidance.
    let ids: Vec<String> = blocked.iter().map(|b| b.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string()).collect();
    let reasons: Vec<String> = blocked.iter().map(|b| b.get("reason").and_then(|r| r.as_str()).unwrap_or("blocked").to_string()).collect();
    let joined = reasons.join(" ");

    let resp = serde_json::json!({
        "decision": "block",
        "payload": {
            "reason": joined,
            "calls": ids,
        },
    });
    println!("{}", resp);
}

fn read_stdin_json() -> serde_json::Value {
    let mut buf = String::new();
    if std::io::stdin().read_to_string(&mut buf).is_err() || buf.trim().is_empty() {
        return serde_json::json!({});
    }
    serde_json::from_str(&buf).unwrap_or(serde_json::json!({}))
}

fn resolve_session_dir() -> Option<std::path::PathBuf> {
    let session = std::env::var("SESSION").ok()?;
    let sessions_root = std::env::var("SESSIONS_ROOT").unwrap_or_else(|_| "sessions".into());
    let p = if session.contains('/') || session.contains('\\') {
        std::path::PathBuf::from(&session)
    } else {
        std::path::PathBuf::from(&sessions_root).join(&session)
    };
    Some(p)
}

fn print_help() {
    println!("harness-hook-goal-tools — stale goal-call guard (tool.before)");
    println!();
    println!("Window: tool.before");
    println!("Input (stdin): window JSON with keys window, session, calls[]");
    println!("Output (stdout):");
    println!("  {{}}  — proceed with the batch");
    println!(
        "  {{\"decision\":\"block\",\"payload\":{{\"reason\":...,\"calls\":[...]}}}} \
         — block stale goal calls"
    );
    println!("Exit codes: 0 = ok");
}
