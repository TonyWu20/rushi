//! Log I/O for the step pipeline: appending typed events to
//! `events.jsonl`, publishing `ext_status` markers, and scanning the
//! session log.

use std::path::Path;

use serde_json::Value;

use rushi_common::event;
use rushi_common::logline::LogLine;

use crate::config::HarnessConfig;

// ---------------------------------------------------------------------------
// Appending helpers
// ---------------------------------------------------------------------------

/// Append one event through the shared `LogLine` and the typed event
/// validator (docs/typed-events.md).
pub fn append_event(_cfg: &HarnessConfig, session_dir: &Path, event: &Value) {
    let json_line = serde_json::to_string(event).unwrap_or_else(|e| {
        eprintln!("rushi: cannot serialize event: {e}");
        std::process::exit(1);
    });
    if let Err(e) = event::parse_event(&json_line) {
        eprintln!("rushi: event validation failed: {e}");
        std::process::exit(1);
    }
    let line = LogLine::from_json(&json_line);
    let log_path = session_dir.join("events.jsonl");
    if let Err(e) = line.commit(&log_path) {
        eprintln!("rushi: log append failed: {e}");
        std::process::exit(1);
    }
}

/// Append one already-serialized JSON line to the log.
pub fn append_line(_cfg: &HarnessConfig, session_dir: &Path, line: &str) {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return;
    }
    if let Err(e) = event::parse_event(trimmed) {
        eprintln!("rushi: event validation failed: {e}");
        std::process::exit(1);
    }
    let ll = LogLine::from_json(trimmed);
    if let Err(e) = ll.commit(&session_dir.join("events.jsonl")) {
        eprintln!("rushi: log append failed: {e}");
        std::process::exit(1);
    }
}

/// Publish one `loop_phase` marker.
pub fn publish_loop_phase(cfg: &HarnessConfig, session_dir: &Path, phase: &str) {
    let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let event = serde_json::json!({
        "v": 1,
        "type": "ext_status",
        "ts": ts,
        "id": "loop_phase",
        "value": phase,
    });
    append_event(cfg, session_dir, &event);
}

/// Publish one arbitrary `ext_status` marker (docs/typed-events.md).
/// Used for the hook pipeline markers (`hook.<window>.chain`,
/// `hook.<window>.error`, `hook.<window>`; docs/loop-lifecycle-hooks.md
/// 12.7).
pub fn publish_ext_status(
    cfg: &HarnessConfig,
    session_dir: &Path,
    id: &str,
    value: &Value,
) {
    let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let event = serde_json::json!({
        "v": 1,
        "type": "ext_status",
        "ts": ts,
        "id": id,
        "value": value,
    });
    append_event(cfg, session_dir, &event);
}

/// Append a terminal error event.
pub fn append_terminal_error(cfg: &HarnessConfig, session_dir: &Path, message: &str) {
    let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let event = serde_json::json!({
        "v": 1,
        "type": "error",
        "ts": ts,
        "message": message,
    });
    append_event(cfg, session_dir, &event);
}

// ---------------------------------------------------------------------------
// Log scanning
// ---------------------------------------------------------------------------

/// The 1-based seq of the last `user_message` in the session log, 0
/// when the log is absent or holds no user message. The count follows
/// the claim convention: every non-empty line owns a seq, including
/// lines that fail to parse (docs/tui-pending-user-messages.md P7).
pub fn last_user_message_seq(session_dir: &Path) -> usize {
    let Ok(data) = std::fs::read_to_string(session_dir.join("events.jsonl")) else {
        return 0;
    };
    let mut last = 0usize;
    let mut seq = 0usize;
    for line in data.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        seq += 1;
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            if v.get("type").and_then(|t| t.as_str()) == Some("user_message") {
                last = seq;
            }
        }
    }
    last
}
