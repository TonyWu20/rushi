//! `harness-hook-goal-arm` — goal-mode prompt injection.
//!
//! Registered on the `model.before` window. Reads the session's
//! current goal (the `goal.json` pointer plus its `goal-<id>.json`
//! state file) from the session directory. When an active goal
//! exists, appends the
//! cache-stable goal block (docs/goal-ux.md §1.1b/§1.1c) as the
//! **last item** of `request.input` — after the conversation, not in
//! `instructions`. This keeps the `[system][history…]` prefix
//! untouched for provider KV/prefix caches.
//!
//! The block is a pure function of `(goal, goal_id)` (§1.1c, P17):
//! byte-stable across calls, no per-turn counters, no timestamps.
//!
//! Decision contract (docs/loop-lifecycle-hooks.md §4.3):
//! - exit 0 + `{}` → proceed unchanged.
//! - exit 0 + `{"decision":"transform","payload":{"request":{...}}}`
//!   → the harness replaces the request with the hook's version.

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

    // Read the session's current goal — the sole source of truth for
    // goal state (docs/goal-ux.md §1.1b: goal-file-driven, not
    // log-derived).
    let goal = match GoalState::load(&session_dir) {
        Some(g) if g.is_open() => g,
        _ => {
            // No active goal: no injection.
            println!("{{}}");
            return;
        }
    };

    // Build the cache-stable goal block. Pure function of
    // (goal text, goal_id) — no iteration counter, no timestamps,
    // no token counts (§1.1c).
    let block = goal.build_goal_block();

    // Append the block as the last item of request.input, after the
    // conversation. This keeps the [system][history…] prefix intact
    // for provider KV/prefix caches (§1.1c).
    let request = payload
        .get("request")
        .cloned()
        .unwrap_or(serde_json::json!({}));

    let mut req = request;
    let block_item = serde_json::json!({
        "type": "message",
        "role": "user",
        "content": block
    });

    if let Some(input) = req.get_mut("input").and_then(|i| i.as_array_mut()) {
        input.push(block_item);
    } else {
        // Defensive fallback: if `input` is not an array, append to
        // instructions (§1.1c fallback, should not happen in practice).
        let instructions = req
            .get("instructions")
            .and_then(|i| i.as_str())
            .unwrap_or("");
        let new_instructions = format!("{instructions}\n\n{block}");
        req["instructions"] = serde_json::json!(new_instructions);
    }

    let resp = serde_json::json!({
        "decision": "transform",
        "payload": {
            "request": req,
        },
    });
    println!("{}", resp);
}

/// Resolve the session directory from the hook env.
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
    println!("  {{}}  — no active goal; proceed unchanged");
    println!(
        "  {{\"decision\":\"transform\",\"payload\":{{\"request\":{{...}}}}}} \
         — goal block appended to request.input"
    );
    println!("Exit codes: 0 = ok");
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_active_goal(dir: &std::path::Path) -> GoalState {
        let g = GoalState::new("fix the parser bug");
        let mut g = g;
        g.iteration = 3;
        g.used_tokens = 50_000;
        g.save(dir).unwrap();
        g
    }

    #[test]
    fn test_goal_prompt_injected() {
        // P6: active goal → block is appended to request.input.
        let dir = TempDir::new().unwrap();
        let goal = make_active_goal(dir.path());

        let payload = serde_json::json!({
            "window": "model.before",
            "request": {
                "instructions": "system prompt",
                "input": [
                    {"type": "message", "role": "user", "content": "hello"},
                    {"type": "message", "role": "assistant", "content": "hi"}
                ]
            }
        });

        // Simulate what main() does: load goal, build block, append.
        assert!(goal.is_open());
        let block = goal.build_goal_block();
        assert!(block.contains("fix the parser bug"));
        assert!(block.contains("Goal-mode rules:"));
        assert!(block.contains("<goal_objective>"));

        // The block is the last item in input after appending.
        let mut input = payload["request"]["input"].as_array().unwrap().clone();
        input.push(serde_json::json!({
            "type": "message",
            "role": "user",
            "content": block
        }));
        let last = input.last().unwrap();
        assert_eq!(last["content"].as_str().unwrap(), block);
        assert_eq!(input.len(), 3);
    }

    #[test]
    fn test_no_goal_no_injection() {
        // P15: no goal.json → no block in input.
        let dir = TempDir::new().unwrap();
        let goal = GoalState::load(dir.path());
        assert!(goal.is_none());
    }

    #[test]
    fn test_goal_block_pure_and_stable() {
        // P16: two GoalStates with same (goal, id) but different
        // iteration/used_tokens produce identical blocks.
        let _dir = TempDir::new().unwrap();
        let mut g1 = GoalState::new("build a parser");
        g1.id = "g-deadbeef".to_string();
        g1.iteration = 0;
        g1.used_tokens = 0;

        let mut g2 = g1.clone();
        g2.iteration = 42;
        g2.used_tokens = 999_999;

        assert_eq!(g1.build_goal_block(), g2.build_goal_block());
    }

    #[test]
    fn test_block_byte_stable_across_turns() {
        // P17: consecutive calls with unchanged (goal, id) emit
        // identical bytes.
        let _dir = TempDir::new().unwrap();
        let mut g = GoalState::new("implement the feature");
        g.iteration = 0;
        let b1 = g.build_goal_block();

        g.iteration = 1;
        g.add_used(1000);
        let b2 = g.build_goal_block();

        g.iteration = 2;
        g.add_used(2000);
        let b3 = g.build_goal_block();

        assert_eq!(b1, b2, "iteration/used_tokens must not leak into the block");
        assert_eq!(b2, b3);
    }
}
