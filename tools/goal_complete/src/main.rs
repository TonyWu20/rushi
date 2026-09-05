//! `goal_complete` — mark the active goal as done.
//!
//! Reads `HARNESS_SESSION_DIR`, loads `goal.json`, marks the goal
//! completed, and persists it. The next `run.idle` window sees the
//! closed goal and stops the loop.

use std::io::Read;
use std::path::PathBuf;

use goal_state::GoalState;

fn main() {
    let args = read_stdin_json();

    let session_dir = match session_dir() {
        Some(d) => d,
        None => fail("HARNESS_SESSION_DIR is not set; cannot update goal.json."),
    };

    let mut state = match GoalState::load(&session_dir) {
        Some(s) => s,
        None => fail("No goal.json found: start a goal first with the `goal` tool."),
    };

    state.mark_completed();

    let summary = args.get("summary").and_then(|s| s.as_str()).unwrap_or("").to_string();

    if let Err(e) = state.save(&session_dir) {
        fail(&format!("Failed to write goal.json: {e}"));
    }

    let text = if summary.is_empty() {
        format!("Goal completed: {}", state.goal)
    } else {
        format!("Goal completed: {} — {}", state.goal, summary)
    };
    let out = serde_json::json!({
        "text": text,
        "state": state,
    });
    println!("{}", out);
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
