//! `goal_blocked` — mark the active goal as blocked with a reason.
//!
//! The loop stops pursuing a blocked goal. The reason is recorded in
//! `goal.json` so the user can see why the goal stopped.

use std::io::Read;
use std::path::PathBuf;

use goal_state::GoalState;

fn main() {
    let args = read_stdin_json();

    let reason = args
        .get("reason")
        .and_then(|r| r.as_str())
        .unwrap_or("")
        .to_string();
    if reason.is_empty() {
        fail("Missing required field: reason. Explain why the goal is blocked.");
    }

    let session_dir = match session_dir() {
        Some(d) => d,
        None => fail("HARNESS_SESSION_DIR is not set; cannot update goal.json."),
    };

    let mut state = match GoalState::load(&session_dir) {
        Some(s) => s,
        None => fail("No goal.json found: start a goal first with the `goal` tool."),
    };

    state.mark_blocked(&reason);

    if let Err(e) = state.save(&session_dir) {
        fail(&format!("Failed to write goal.json: {e}"));
    }

    let out = serde_json::json!({
        "text": format!("Goal blocked: {} — {reason}", state.goal),
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
