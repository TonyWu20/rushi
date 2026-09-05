//! `harness-hook-goal-idle` — the goal-continuation hook.
//!
//! Registered on the `run.idle` window. When the loop is about to stop
//! on an idle claim, this hook checks whether a goal is still open.
//! If the goal is active and its token budget is not exhausted, it
//! returns a `continue` decision with a continuation prompt so the loop
//! keeps working toward the goal. Otherwise it returns `{}` and the
//! loop stops.
//!
//! Decision contract (docs/loop-lifecycle-hooks.md §4.3):
//! - exit 0 + `{}` → no decision, the loop stops (window default)
//! - exit 0 + `{"decision":"continue","payload":{"message":"..."}}`
//!   → the loop appends a follow `user_message` and continues.

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
    if payload.get("window").and_then(|w| w.as_str()) != Some("run.idle") {
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

    // No goal file: nothing to continue.
    let mut goal = match GoalState::load(&session_dir) {
        Some(g) => g,
        None => {
            println!("{{}}");
            return;
        }
    };

    // A completed or blocked goal stops the loop.
    if !goal.is_open() {
        println!("{{}}");
        return;
    }

    // Token accounting (docs/pi-goal-readiness.md): advance the
    // goal's used_tokens by the output_tokens of the most recent
    // assistant message, then persist. This is what makes the loop
    // terminate when the budget is exhausted.
    let events_path = session_dir.join("events.jsonl");
    if let Some(usage) = GoalState::read_last_assistant_output_tokens(&events_path) {
        goal.add_used(usage);
    }
    let _ = goal.save(&session_dir);

    if goal.budget_exhausted() {
        // Budget is gone: close the goal as blocked so the user sees
        // why it stopped, then let the loop stop.
        goal.mark_blocked("token budget exhausted");
        let _ = goal.save(&session_dir);
        println!("{{}}");
        return;
    }

    let prompt = goal.continuation_prompt();
    let resp = serde_json::json!({
        "decision": "continue",
        "payload": {
            "message": prompt,
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

/// Resolve the session directory from the hook env.
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
    println!("harness-hook-goal-idle — goal-continuation hook (run.idle)");
    println!();
    println!("Window: run.idle");
    println!("Input (stdin): window JSON with keys window, session, last_assistant_message_id");
    println!("Output (stdout):");
    println!("  {{}}  — stop the loop (no goal, closed goal, or exhausted budget)");
    println!("  {{\"decision\":\"continue\",\"payload\":{{\"message\":\"...\"}}}} — keep going");
    println!("Exit codes: 0 = ok");
}
