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

    // Stale-turn guard (docs/goal-ux.md §1.3): the goal_id must match
    // the active goal's id, so a call left over from an older goal
    // cannot close a fresh one.
    if let Err(msg) = check_goal_id(&args, &state) {
        fail(&msg);
    }

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

/// The `goal_id` argument check shared with `goal_complete`
/// (docs/goal-ux.md §1.3, P8). Returns the rejection reason when the
/// argument is missing or does not match the goal's id.
fn check_goal_id(args: &serde_json::Value, state: &GoalState) -> Result<(), String> {
    let goal_id = args.get("goal_id").and_then(|g| g.as_str()).unwrap_or("");
    if goal_id.is_empty() {
        return Err(
            "Missing required field: goal_id. Pass the goal_id from the goal prompt's <goal_id> block."
                .to_string(),
        );
    }
    if goal_id != state.id {
        return Err(format!(
            "goal_id does not match: the active goal's id is \"{}\", the call passed \"{}\". Re-read the goal prompt and pass its goal_id exactly.",
            state.id, goal_id
        ));
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_goal_id_mismatch() {
        let g = GoalState::new("some goal");
        let id = g.id.clone();
        // A wrong id is rejected with the stable phrase.
        let err = check_goal_id(&serde_json::json!({ "goal_id": "g-wrong" }), &g).unwrap_err();
        assert!(err.contains("goal_id does not match"), "{err}");
        // A missing id is rejected.
        assert!(check_goal_id(&serde_json::json!({}), &g).is_err());
        // The exact id passes.
        assert!(check_goal_id(&serde_json::json!({ "goal_id": id }), &g).is_ok());
    }
}
