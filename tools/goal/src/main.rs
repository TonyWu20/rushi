//! `goal` — start a goal that the loop pursues across turns.
//!
//! Reads `HARNESS_SESSION_DIR` to locate the session's `goal.json` and
//! writes a fresh active goal there. The `run.idle` hook continues the
//! goal until `goal_complete` or `goal_blocked` closes it.

use std::io::Read;
use std::path::PathBuf;

use goal_state::GoalState;

fn main() {
    let args = read_stdin_json();

    let goal = args
        .get("goal")
        .and_then(|g| g.as_str())
        .unwrap_or("")
        .to_string();
    if goal.is_empty() {
        fail("Missing required field: goal.");
    }

    let budget_tokens = args
        .get("budget_tokens")
        .and_then(|b| b.as_u64());

    let session_dir = match session_dir() {
        Some(d) => d,
        None => fail("HARNESS_SESSION_DIR is not set; cannot write goal.json."),
    };

    // If an active goal already exists, edit it in place; otherwise
    // create a fresh one.
    let mut state = match GoalState::load(&session_dir) {
        Some(mut existing) if existing.is_open() => {
            existing.edit_goal(&goal);
            existing
        }
        _ => GoalState::new(&goal, budget_tokens),
    };
    state.active = true;

    match state.save(&session_dir) {
        Ok(()) => {
            let out = serde_json::json!({
                "text": format!("Goal set: {goal}"),
                "state": state,
            });
            println!("{}", out);
        }
        Err(e) => fail(&format!("Failed to write goal.json: {e}")),
    }
}

fn session_dir() -> Option<PathBuf> {
    std::env::var("HARNESS_SESSION_DIR").ok().filter(|s| !s.is_empty()).map(PathBuf::from)
}

fn read_stdin_json() -> serde_json::Value {
    let mut buf = String::new();
    if std::io::stdin().read_to_string(&mut buf).is_err() || buf.trim().is_empty() {
        return serde_json::json!({});
    }
    serde_json::from_str(&buf).unwrap_or(serde_json::json!({}))
}

fn fail(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(1);
}
