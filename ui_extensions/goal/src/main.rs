//! The `goal` commands extension for the TUI command palette
//! (docs/goal-ux.md §1.1, §1.4, §1.8). Registers the pi-goal
//! port's user-facing commands: `goal`, `goal_edit`, `goal_pause`,
//! `goal_clear`, and `goal_resume`.
//!
//! Protocol (docs/ui-extension.md section 4): one JSON line per op on
//! stdin, one JSON reply per op on stdout. The host sends a
//! `commands` op when the palette opens; the extension replies with a
//! `commands_list`. An `invoke` op (the user committed a palette item)
//! is executed, then replying with an `invoke_reply`.
//!
//! Event-driven goal setting (§1.1, §1.8):
//! When the user selects `goal` or `goal_edit` from the palette, the
//! extension sets an in-memory `armed` flag. The next `user_message`
//! event forwarded by the TUI (via `kinds = ["user_message"]` in
//! ext.toml) triggers a direct write of the session's goal files —
//! no agent round-trip needed.
//!
//! User-facing commands (this extension):
//!   - `goal`       — arm goal mode; user types the goal in the input
//!     box and sends it. The goal files are written on the next
//!     user_message.
//!   - `goal_edit`  — arm goal-edit mode; user types the new goal in the
//!     input box and sends it. The goal state is edited in place.
//!   - `goal_pause` — set `active = false` in the goal's state file.
//!     The loop stops at the next `run.idle` window.
//!   - `goal_clear` — delete the `goal.json` pointer (irreversible for
//!     the current goal; the per-goal `goal-<id>.json` traces
//!     remain).
//!   - `goal_resume`— re-activate a previously blocked or completed
//!     goal by rewriting its state file directly.
//!
//! Agent-side tools (tools/goal, tools/goal_complete, tools/goal_blocked)
//! are NOT registered here — they are called by the model, not by the
//! user through the palette.

use serde::Deserialize;
use serde_json::json;
use std::io::{BufRead, Write};
use std::path::PathBuf;

/// One host op. Fields the extension does not use are optional: serde
/// skips unknown fields, so the host may grow the op later.
#[derive(Debug, Deserialize)]
struct Op {
    #[serde(default)]
    v: Option<u64>,
    #[serde(default)]
    op: Option<String>,
    #[serde(default)]
    req: Option<u64>,
    /// The command id for `invoke` ops (a string like "goal") or the
    /// event index for `event` ops (a u64). Kept as a generic Value
    /// so both wire shapes deserialize without a type error.
    #[serde(default)]
    id: Option<serde_json::Value>,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    session: Option<String>,
    /// The full log event object, forwarded when the op type is `event`
    /// and the event's type matches the extension's `kinds` filter.
    #[serde(default)]
    event: Option<serde_json::Value>,
}

/// RFC 3339-ish timestamp without a chrono dependency: `t+<epoch-seconds>s`.
/// Matches the convention used by `goal-state`.
fn ts_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("t+{secs}s")
}

fn main() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = std::io::LineWriter::new(stdout.lock());

    // Resolve the sessions root once at startup from CONFIG.
    let sessions_root = std::env::var("CONFIG").ok().and_then(|c| {
        sessions_root(std::path::Path::new(&c))
    });

    // The active session name, remembered from the last `commands` op.
    let mut session: Option<String> = None;
    // Armed mode: "start" or "edit". Set on invoke, consumed on the
    // next user_message event (docs/goal-ux.md §1.8).
    let mut armed: Option<String> = None;

    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            break;
        };
        let Ok(op) = serde_json::from_str::<Op>(&line) else {
            // A malformed line: no reply, no state change.
            continue;
        };
        if op.v != Some(1) {
            continue;
        }
        match op.op.as_deref() {
            Some("commands") => {
                if let Some(s) = &op.session {
                    session = Some(s.clone());
                }
                let reply = json!({
                    "v": 1,
                    "op": "commands_list",
                    "commands": [
                        {
                            "id": "goal",
                            "label": "goal",
                            "kind": "run",
                            "hint": "",
                            "help": "Start a goal. Type the goal description in the input box and send it.",
                            "options": []
                        },
                        {
                            "id": "goal_edit",
                            "label": "goal edit",
                            "kind": "run",
                            "hint": "",
                            "help": "Edit the active goal. Type the updated goal in the input box and send it.",
                            "options": []
                        },
                        {
                            "id": "goal_pause",
                            "label": "goal pause",
                            "kind": "run",
                            "hint": "",
                            "help": "Pause the active goal. The loop stops at the next idle window. Resume with 'goal resume'.",
                            "options": []
                        },
                        {
                            "id": "goal_clear",
                            "label": "goal clear",
                            "kind": "run",
                            "hint": "",
                            "help": "Delete the goal.json pointer. Past goal files (goal-*.json) stay as traces. The loop stops immediately.",
                            "options": []
                        },
                        {
                            "id": "goal_resume",
                            "label": "goal resume",
                            "kind": "run",
                            "hint": "",
                            "help": "Resume a paused or blocked goal. Resets the iteration counter.",
                            "options": []
                        }
                    ]
                });
                let _ = writeln!(out, "{reply}");
                let _ = out.flush();
            }
            Some("event") => {
                // Forwarded event from the TUI (docs/goal-ux.md §1.1, §1.8).
                // When armed, a user_message event carries the goal text.
                let Some(event_obj) = &op.event else {
                    continue;
                };
                let event_type = event_obj
                    .get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("");
                if event_type != "user_message" {
                    continue;
                }
                let Some(mode) = armed.take() else {
                    // Not armed: ignore the event.
                    continue;
                };
                let Some(sess) = &session else {
                    continue;
                };
                let Some(root) = &sessions_root else {
                    continue;
                };
                let session_dir = root.join(sess);
                let content = event_obj
                    .get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if content.is_empty() {
                    continue;
                }
                match mode.as_str() {
                    "start" => {
                        let state = goal_state::GoalState::new(&content);
                        if state.save(&session_dir).is_ok() {
                            append_ext_status(&mut out, &session_dir, "goal_set", "start");
                        }
                    }
                    "edit" => {
                        if let Some(mut state) = goal_state::GoalState::load(&session_dir) {
                            state.edit_goal(&content);
                            if state.save(&session_dir).is_ok() {
                                append_ext_status(&mut out, &session_dir, "goal_edited", "edit");
                            }
                        }
                    }
                    _ => {}
                }
            }
            Some("invoke") => {
                let Some(req) = op.req else {
                    continue;
                };
                let Some(id_val) = &op.id else {
                    continue;
                };
                let id = id_val.as_str().unwrap_or("");
                let (ok, message, arm) = handle_invoke(
                    id,
                    op.value.as_deref(),
                    &session,
                    &sessions_root,
                    &mut out,
                );
                // `goal` / `goal_edit` arm event-driven goal writing:
                // the next user_message event writes goal.json
                // (docs/goal-ux.md §1.1, §1.8).
                armed = arm;
                let reply = json!({
                    "v": 1,
                    "op": "invoke_reply",
                    "req": req,
                    "ok": ok,
                    "message": message,
                });
                let _ = writeln!(out, "{reply}");
                let _ = out.flush();
            }
            // Unknown ops: no reply (G5 fallback, docs/ui-extension.md).
            _ => {}
        }
    }
}

/// Execute one goal command. Returns `(ok, message, arm_mode)`.
/// `arm_mode` is `Some("start")` or `Some("edit")` when the command
/// arms event-driven goal writing (the next `user_message` event will
/// create or edit `goal.json`).
fn handle_invoke<W: Write>(
    id: &str,
    _value: Option<&str>,
    session: &Option<String>,
    sessions_root: &Option<PathBuf>,
    out: &mut W,
) -> (bool, String, Option<String>) {
    let Some(sess) = session else {
        return (
            false,
            "no active session: open the palette first so the host reports the session".to_string(),
            None,
        );
    };
    let Some(root) = sessions_root else {
        return (false, "cannot resolve sessions root from CONFIG".to_string(), None);
    };
    let session_dir = root.join(sess);

    match id {
        "goal" => {
            // A start requires no open goal: an open goal is edited via
            // `goal_edit` or, when closed, re-opened via `goal_resume`.
            if let Some(g) = goal_state::GoalState::load(&session_dir) {
                if g.is_open() {
                    return (
                        false,
                        format!(
                            "A goal is already open: \"{}\". Use 'goal edit' to change it, or 'goal resume' after it closes.",
                            g.goal
                        ),
                        None,
                    );
                }
            }
            // Arm goal mode: the next user_message event will write goal.json.
            // We signal this by returning ok=true; the TUI sets
            // `goal_armed` and switches to Insert mode.
            (
                true,
                "Goal mode armed. Type your goal description in the input box and send it.".to_string(),
                Some("start".to_string()),
            )
        }
        "goal_edit" => {
            // Verify a goal exists before arming edit mode.
            match goal_state::GoalState::load(&session_dir) {
                Some(g) if g.is_open() => (
                    true,
                    format!(
                        "Goal edit armed (current: \"{}\"). Type the updated goal in the input box and send it.",
                        g.goal
                    ),
                    Some("edit".to_string()),
                ),
                Some(g) => (
                    false,
                    format!(
                        "Cannot edit: goal is {} (not active). Use 'goal resume' first.",
                        if g.blocked { "blocked" } else { "completed" }
                    ),
                    None,
                ),
                None => (
                    false,
                    "No active goal to edit. Use 'goal' to start a new goal.".to_string(),
                    None,
                ),
            }
        }
        "goal_pause" => {
            let Some(mut g) = goal_state::GoalState::load(&session_dir) else {
                return (false, "No goal found in this session.".to_string(), None);
            };
            if !g.is_open() {
                return (
                    false,
                    format!(
                        "Goal is already {} — cannot pause.",
                        if g.blocked { "blocked" } else { "completed" }
                    ),
                    None,
                );
            }
            g.active = false;
            match g.save(&session_dir) {
                Ok(()) => {
                    append_ext_status(out, &session_dir, "goal_paused", "pause");
                    (
                        true,
                        format!("Goal paused: \"{}\"", g.goal),
                        None,
                    )
                }
                Err(e) => (false, format!("Failed to write goal.json: {e}"), None),
            }
        }
        "goal_clear" => {
            let path = goal_state::GoalState::path(&session_dir);
            if !path.exists() {
                return (false, "No goal.json found — nothing to clear.".to_string(), None);
            }
            // Only the pointer is deleted; the per-goal files
            // (goal-<id>.json) stay as traces of the session's goals.
            match goal_state::GoalState::clear(&session_dir) {
                true => {
                    append_ext_status(out, &session_dir, "goal_cleared", "clear");
                    (
                        true,
                        "Goal cleared: goal.json deleted. Past goal files (goal-*.json) stay as traces.".to_string(),
                        None,
                    )
                }
                false => (false, "Failed to delete goal.json.".to_string(), None),
            }
        }
        "goal_resume" => {
            let Some(mut g) = goal_state::GoalState::load(&session_dir) else {
                return (
                    false,
                    "No goal found in this session. Use 'goal' to start one.".to_string(),
                    None,
                );
            };
            if g.is_open() {
                return (
                    false,
                    format!("Goal is already active: \"{}\"", g.goal),
                    None,
                );
            }
            // Resume: re-activate the goal.
            g.resume();
            match g.save(&session_dir) {
                Ok(()) => {
                    append_ext_status(out, &session_dir, "goal_resumed", "resume");
                    (true, format!("Goal resumed: \"{}\"", g.goal), None)
                }
                Err(e) => (false, format!("Failed to write goal.json: {e}"), None),
            }
        }
        _ => (false, format!("unknown goal command: {id}"), None),
    }
}

/// Read `[paths] sessions_root` from the config file named by the
/// `CONFIG` env var. Returns the sessions root resolved against the
/// config directory. Defaults to `<config_dir>/sessions` when absent.
fn sessions_root(config_path: &std::path::Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(config_path).ok()?;
    let v: toml::Value = text.parse().ok()?;
    let config_dir = config_path.parent()?.to_path_buf();
    if let Some(root) = v
        .get("paths")
        .and_then(|p| p.get("sessions_root"))
        .and_then(|r| r.as_str())
    {
        let p = std::path::Path::new(root);
        return Some(if p.is_absolute() {
            p.to_path_buf()
        } else {
            config_dir.join(p)
        });
    }
    Some(config_dir.join("sessions"))
}

/// Append an `ext_status` event to the session log via the
/// extension's `append` capability. The marker records that a
/// goal state transition occurred.
fn append_ext_status<W: Write>(out: &mut W, session_dir: &std::path::Path, id: &str, value: &str) {
    let _ = session_dir; // session_dir is used for context but the append goes via the host
    let event = json!({
        "v": 1,
        "type": "ext_status",
        "ts": ts_now(),
        "id": id,
        "value": value,
    });
    let op = json!({
        "v": 1,
        "op": "append",
        "event": event,
    });
    let _ = writeln!(out, "{op}");
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_goal_pause() {
        let dir = tempfile::tempdir().unwrap();
        let session_dir = dir.path();
        let g = goal_state::GoalState::new("test goal");
        g.save(session_dir).unwrap();

        // Simulate pause
        let mut loaded = goal_state::GoalState::load(session_dir).unwrap();
        assert!(loaded.is_open());
        loaded.active = false;
        loaded.save(session_dir).unwrap();

        let reloaded = goal_state::GoalState::load(session_dir).unwrap();
        assert!(!reloaded.is_open());
        assert!(!reloaded.active);
        assert!(!reloaded.blocked);
        assert!(!reloaded.completed);
        assert_eq!(reloaded.goal, "test goal");
    }

    #[test]
    fn test_goal_clear() {
        let dir = tempfile::tempdir().unwrap();
        let session_dir = dir.path();
        let g = goal_state::GoalState::new("test goal");
        g.save(session_dir).unwrap();
        assert!(session_dir.join("goal.json").exists());

        assert!(goal_state::GoalState::clear(session_dir));
        assert!(!session_dir.join("goal.json").exists());
        assert!(goal_state::GoalState::load(session_dir).is_none());
        // The goal's own state file survives as a trace.
        let trace = session_dir.join(format!("goal-{}.json", g.id));
        assert!(trace.exists(), "per-goal trace file must survive clear");
    }

    #[test]
    fn test_goal_resume() {
        let dir = tempfile::tempdir().unwrap();
        let session_dir = dir.path();
        let mut g = goal_state::GoalState::new("test goal");
        g.mark_blocked("some blocker");
        g.iteration = 5;
        g.used_tokens = 999;
        g.save(session_dir).unwrap();

        let mut loaded = goal_state::GoalState::load(session_dir).unwrap();
        assert!(loaded.blocked);
        loaded.resume();
        loaded.save(session_dir).unwrap();

        let reloaded = goal_state::GoalState::load(session_dir).unwrap();
        assert!(reloaded.is_open());
        assert!(!reloaded.blocked);
        assert_eq!(reloaded.iteration, 0);
        assert_eq!(reloaded.used_tokens, 0);
    }
}
