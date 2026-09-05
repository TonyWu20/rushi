//! `harness-hook-goal-compact` — the goal compaction-veto hook.
//!
//! Registered on the `compact.before` window. When a goal is active
//! and its remaining token budget is low, the hook vetoes the
//! compaction (`cancel`) so the goal wraps up within its budget rather
//! than spending tokens on a compact that will not change the outcome.
//!
//! Decision contract (docs/loop-lifecycle-hooks.md §4.3):
//! - exit 0 + `{}` → no decision, compaction proceeds (window default)
//! - exit 0 + `{"decision":"cancel"}` → veto the compaction

use std::io::Read;

use goal_state::GoalState;

/// The default remaining-budget threshold below which a compaction is
/// vetoed for an active goal. Overridable via
/// `GOAL_COMPACT_VETO_THRESHOLD`.
const DEFAULT_VETO_THRESHOLD: u64 = 8192;

fn main() {
    // Self-documentation.
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return;
    }

    let payload = read_stdin_json();

    // Not our window: no-op.
    if payload.get("window").and_then(|w| w.as_str()) != Some("compact.before") {
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

    let goal = match GoalState::load(&session_dir) {
        Some(g) => g,
        None => {
            println!("{{}}");
            return;
        }
    };

    // Veto only when the goal is still open and its remaining budget
    // is low. A closed goal or a healthy budget lets compaction
    // proceed normally.
    let threshold = std::env::var("GOAL_COMPACT_VETO_THRESHOLD")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_VETO_THRESHOLD);

    let low = goal
        .remaining_budget()
        .map(|rem| rem <= threshold)
        .unwrap_or(false);

    if goal.is_open() && low {
        let resp = serde_json::json!({
            "decision": "cancel",
            "payload": {
                "reason": format!(
                    "goal budget low ({} tokens remaining <= {threshold}); \
                     finishing the goal rather than compacting",
                    goal.remaining_budget().unwrap_or(0)
                ),
            },
        });
        println!("{}", resp);
        return;
    }

    println!("{{}}");
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
    println!("harness-hook-goal-compact — goal compaction-veto hook (compact.before)");
    println!();
    println!("Window: compact.before");
    println!("Input (stdin): window JSON with keys window, session, reason, force");
    println!("Output (stdout):");
    println!("  {{}}  — no decision, compaction proceeds");
    println!(
        "  {{\"decision\":\"cancel\"}} — veto when goal is open and budget is \
         low (<= GOAL_COMPACT_VETO_THRESHOLD, default {})",
        DEFAULT_VETO_THRESHOLD
    );
    println!("Exit codes: 0 = ok");
}
