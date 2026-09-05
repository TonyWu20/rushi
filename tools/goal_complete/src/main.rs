//! `goal_complete` — mark the active goal as done.
//!
//! Reads `HARNESS_SESSION_DIR`, loads `goal.json`, verifies the
//! `goal_id` argument against the goal's id (the stale-turn guard,
//! docs/goal-ux.md §1.3, P8), rejects a `summary` that contradicts
//! the completion claim (the completion guard, §1.5, P9), and only
//! then marks the goal completed and persists it. The next
//! `run.idle` window sees the closed goal and stops the loop.
//!
//! A rejection exits 1 with the reason on stderr: the loop records
//! an error `tool_result` carrying that text, and `goal.json` stays
//! active so the loop keeps pursuing the goal.

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

    // Stale-turn guard (P8): the goal_id must be present and match
    // the goal's id.
    if let Err(msg) = check_goal_id(&args, &state) {
        fail(&msg);
    }

    // Completion guard (P9): a summary that contradicts the
    // completion claim is rejected; goal.json stays active.
    let summary = args.get("summary").and_then(|s| s.as_str()).unwrap_or("").to_string();
    if let Some(why) = contradiction_reason(&summary) {
        fail(&format!(
            "goal_complete rejected: {why}. goal.json remains active — finish the work (or call goal_blocked with a reason), then call goal_complete again with a summary that does not contradict it."
        ));
    }

    state.mark_completed();

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

/// The `goal_id` argument check (docs/goal-ux.md §1.3, P8). Returns
/// the rejection reason when the argument is missing or does not
/// match the goal's id. The model learns the id from the `<goal_id>`
/// block in the injected goal prompt.
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

/// The completion guard (docs/goal-ux.md §1.5, P9): reject a summary
/// that contradicts the completion claim. Ported from pi-goal's
/// `CONTRADICTORY_COMPLETION_PATTERNS` in `src/runtime.ts`; all
/// patterns are case-insensitive. Returns the human-readable reason
/// when the summary matches one of them.
fn contradiction_reason(summary: &str) -> Option<String> {
    let s = summary.trim();
    if s.is_empty() {
        return None;
    }
    // (regex, stable rejection reason). The patterns are the pi-goal
    // set, all matched case-insensitively.
    let patterns: [(&str, &str); 3] = [
        (
            r"(?i)^not (yet )?(complete|completed|done|finished)",
            "the summary says the goal is not complete",
        ),
        (
            r"(?i)still incomplete|still failing|still fails",
            "the summary says the work is still incomplete or failing",
        ),
        (
            r"(?i)because .*tests? fail",
            "the summary attributes the result to failing tests",
        ),
    ];
    for (re, why) in patterns {
        if regex::Regex::new(re).unwrap().is_match(s) {
            return Some(why.to_string());
        }
    }
    None
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
    use serde_json::json;

    #[test]
    fn test_goal_id_mismatch() {
        let g = GoalState::new("some goal");
        let id = g.id.clone();
        // A wrong id is rejected with the stable phrase.
        let err = check_goal_id(&json!({ "goal_id": "g-wrong" }), &g).unwrap_err();
        assert!(err.contains("goal_id does not match"), "{err}");
        // A missing id is rejected.
        assert!(check_goal_id(&json!({}), &g).is_err());
        // The exact id passes.
        assert!(check_goal_id(&json!({ "goal_id": id }), &g).is_ok());
    }

    #[test]
    fn test_contradictory_completion() {
        // P9: each pi-goal contradiction pattern is rejected.
        let rejected = [
            "not complete",
            "Not yet finished",
            "not completed",
            "The fix is still incomplete",
            "build still failing",
            "tests still fails",
            "shipped because tests fail",
            "merged because a test fails",
        ];
        for s in rejected {
            assert!(
                contradiction_reason(s).is_some(),
                "{s:?} must be rejected"
            );
        }
        // Consistent summaries pass.
        for s in [
            "All requirements verified and green",
            "fixed the parser and the test suite passes",
            "",
        ] {
            assert!(
                contradiction_reason(s).is_none(),
                "{s:?} must not be rejected"
            );
        }
    }
}
