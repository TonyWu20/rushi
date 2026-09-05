//! The `goal` commands extension for the TUI command palette
//! (docs/tui-command-palette.md section 10). Registers the pi-goal
//! port's user-facing commands: `goal`, `goal edit`, and `goal resume`.
//!
//! Protocol (docs/ui-extension.md section 4): one JSON line per op on
//! stdin, one JSON reply per op on stdout. The host sends a
//! `commands` op when the palette opens; the extension replies with a
//! `commands_list`. An `invoke` op (the user committed a palette item)
//! is executed, then replying with an `invoke_reply`.
//!
//! User-facing commands (this extension):
//!   - `goal`       — arm goal mode; user types the goal in the input
//!     box and sends it. The model calls the `goal` tool.
//!   - `goal_edit`  — arm goal-edit mode; user types the new goal in
//!     the input box and sends it. The model calls the `goal` tool to
//!     update the active goal.
//!   - `goal_resume`— re-activate a previously blocked or completed
//!     goal by rewriting `goal.json` directly.
//!
//! Agent-side tools (tools/goal, tools/goal_complete, tools/goal_blocked)
//! are NOT registered here — they are called by the model, not by the
//! user through the palette.
//!
//! Session resolution: the `commands` op carries the active session
//! name. The extension stores it and, on `invoke`, resolves the
//! session directory as `<config_dir>/<sessions_root>/<session>` where
//! `sessions_root` comes from `[paths] sessions_root` in the config
//! file named by the `CONFIG` env var (default `sessions`).

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
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    session: Option<String>,
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
                            "help": "Edit the active goal. Type the new goal description in the input box and send it.",
                            "options": []
                        },
                        {
                            "id": "goal_resume",
                            "label": "goal resume",
                            "kind": "run",
                            "hint": "",
                            "help": "Resume a blocked or completed goal.",
                            "options": []
                        }
                    ]
                });
                let _ = writeln!(out, "{reply}");
                let _ = out.flush();
            }
            Some("invoke") => {
                let Some(req) = op.req else {
                    continue;
                };
                let id = op.id.as_deref().unwrap_or("");
                let (ok, message) = handle_invoke(
                    id,
                    op.value.as_deref(),
                    &session,
                    &sessions_root,
                    &mut out,
                );
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

/// Execute one goal command.
fn handle_invoke<W: Write>(
    id: &str,
    _value: Option<&str>,
    session: &Option<String>,
    sessions_root: &Option<PathBuf>,
    out: &mut W,
) -> (bool, String) {
    let Some(sess) = session else {
        return (
            false,
            "no active session: open the palette first so the host reports the session".to_string(),
        );
    };
    let Some(root) = sessions_root else {
        return (false, "cannot resolve sessions root from CONFIG".to_string());
    };
    let session_dir = root.join(sess);

    match id {
        "goal" => {
            // A start requires no open goal: an open goal is edited via
            // `goal edit` or, when closed, re-opened via `goal resume`.
            if let Some(g) = goal_state::GoalState::load(&session_dir) {
                if g.is_open() {
                    return (
                        false,
                        format!(
                            "A goal is already open: \"{}\". Use 'goal edit' to change it, or 'goal resume' after it closes.",
                            g.goal
                        ),
                    );
                }
            }
            // Arm goal mode: append a marker event so tools/hooks can
            // detect that the next user message is a goal description.
            append_marker(out, "start");
            (
                true,
                "Goal mode armed. Type your goal description in the input box and send it.".to_string(),
            )
        }
        "goal_edit" => {
            // Verify a goal exists before arming edit mode.
            match goal_state::GoalState::load(&session_dir) {
                Some(g) if g.is_open() => {
                    append_marker(out, "edit");
                    (
                        true,
                        format!(
                            "Goal edit armed (current: \"{}\"). Type the updated goal in the input box and send it.",
                            g.goal
                        ),
                    )
                }
                Some(g) => (
                    false,
                    format!(
                        "Cannot edit: goal is {} (not active). Use 'goal resume' first.",
                        if g.blocked { "blocked" } else { "completed" }
                    ),
                ),
                None => (
                    false,
                    "No active goal to edit. Use 'goal' to start a new goal.".to_string(),
                ),
            }
        }
        "goal_resume" => {
            let Some(mut g) = goal_state::GoalState::load(&session_dir) else {
                return (
                    false,
                    "No goal found in this session. Use 'goal' to start one.".to_string(),
                );
            };
            if g.is_open() {
                return (
                    false,
                    format!("Goal is already active: \"{}\"", g.goal),
                );
            }
            // Resume: re-activate the goal.
            g.resume();
            match g.save(&session_dir) {
                Ok(()) => (
                    true,
                    format!("Goal resumed: \"{}\"", g.goal),
                ),
                Err(e) => (false, format!("Failed to write goal.json: {e}")),
            }
        }
        _ => (false, format!("unknown goal command: {id}")),
    }
}

/// Append an `ext_status` marker event to the session log via the
/// extension's `append` capability. The marker records that goal mode
/// is armed and what mode (start or edit), so that tools and hooks
/// can parse it.
fn append_marker<W: Write>(out: &mut W, mode: &str) {
    let event = json!({
        "v": 1,
        "type": "ext_status",
        "ts": ts_now(),
        "id": "goal_armed",
        "value": mode,
    });
    let op = json!({
        "v": 1,
        "op": "append",
        "event": event,
    });
    let _ = writeln!(out, "{op}");
    let _ = out.flush();
}
