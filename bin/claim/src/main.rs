#![deny(clippy::todo, clippy::unimplemented, clippy::unreachable)]

use clap::Parser;
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

/// Derive step state from the session log
#[derive(Parser)]
#[command(name = "claim", about = "Determine what work is owed from the session log")]
struct Args {
    /// Session directory path
    #[arg(long)]
    session: String,
}

fn main() {
    let args = Args::parse();

    let log_path = PathBuf::from(&args.session).join("events.jsonl");

    if !log_path.exists() {
        // No log means idle
        let session_name = PathBuf::from(&args.session)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        println!(
            "{}",
            serde_json::json!({
                "session": session_name,
                "state": "idle",
                "last_user_message_seq": 0,
                "pending_tool_calls": []
            })
        );
        return;
    }

    let lines = match fs::read_to_string(&log_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: cannot read log: {e}");
            std::process::exit(1);
        }
    };

    let (state, last_user_message_seq, pending_tool_calls) = derive_state(&lines);

    let session_name = PathBuf::from(&args.session)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    println!(
        "{}",
        serde_json::json!({
            "session": session_name,
            "state": state,
            "last_user_message_seq": last_user_message_seq,
            "pending_tool_calls": pending_tool_calls
        })
    );
}

/// The state machine over the log lines. Returns the state, the
/// 1-based sequence of the last user message, and the unresolved
/// tool calls.
///
/// States:
/// - `idle`: nothing owed (no log activity, or a terminal event).
/// - `awaiting_model`: the loop owes a model call.
/// - `awaiting_tool_result`: routed calls still lack results.
/// - `exhausted`: a `context_exhausted` event closed the session
///   through the automatic handoff (correction 57). The TUI offers a
///   one-key resume in the seeded session.
fn derive_state(lines: &str) -> (String, usize, Vec<serde_json::Value>) {
    let mut last_user_message_seq: usize = 0;
    let mut state = "idle".to_string();
    let mut pending_tool_calls: Vec<serde_json::Value> = Vec::new();
    let mut i = 0;

    for line in lines.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        i += 1;
        let event: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let event_type = event.get("type").and_then(|t| t.as_str()).unwrap_or("");

        match event_type {
            "user_message" => {
                last_user_message_seq = i;
                state = "awaiting_model".to_string();
            }
            "tool_result" => {
                state = "awaiting_model".to_string();
            }
            "assistant_message" => {
                if let Some(tool_calls) = event.get("tool_calls") {
                    if let Some(tc_arr) = tool_calls.as_array() {
                        if !tc_arr.is_empty() {
                            state = "awaiting_tool_result".to_string();
                            pending_tool_calls.clear();
                            for tc in tc_arr {
                                pending_tool_calls.push(tc.clone());
                            }
                        } else {
                            state = "idle".to_string();
                            pending_tool_calls.clear();
                        }
                    }
                } else {
                    state = "idle".to_string();
                    pending_tool_calls.clear();
                }
            }
            "error" => {
                state = "idle".to_string();
                pending_tool_calls.clear();
            }
            // The handoff closed the turn. The seeded session holds
            // the task; this one is done.
            "context_exhausted" => {
                state = "exhausted".to_string();
                pending_tool_calls.clear();
            }
            _ => {}
        }
    }

    // If state is awaiting_tool_result, filter out resolved tool calls
    if state == "awaiting_tool_result" {
        let mut resolved_ids: HashSet<String> = HashSet::new();
        for line in lines.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let event: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if event.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                if let Some(id) = event.get("id").and_then(|id| id.as_str()) {
                    resolved_ids.insert(id.to_string());
                }
            }
        }

        pending_tool_calls.retain(|tc| {
            if let Some(id) = tc.get("id").and_then(|id| id.as_str()) {
                !resolved_ids.contains(id)
            } else {
                false
            }
        });

        if pending_tool_calls.is_empty() {
            state = "idle".to_string();
        }
    }

    (state, last_user_message_seq, pending_tool_calls)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(v: &serde_json::Value) -> String {
        v.to_string()
    }

    #[test]
    fn empty_log_is_idle() {
        let (state, seq, pending) = derive_state("");
        assert_eq!(state, "idle");
        assert_eq!(seq, 0);
        assert!(pending.is_empty());
    }

    #[test]
    fn user_message_awaits_model() {
        let log = line(&serde_json::json!({"v":1,"type":"user_message","ts":"t","content":"go"}));
        let (state, seq, _) = derive_state(&log);
        assert_eq!(state, "awaiting_model");
        assert_eq!(seq, 1);
    }

    #[test]
    fn context_exhausted_reports_the_exhausted_state() {
        let log = format!(
            "{}\n{}",
            line(&serde_json::json!({"v":1,"type":"user_message","ts":"t","content":"go"})),
            line(&serde_json::json!({"v":1,"type":"context_exhausted","ts":"t","message":"m","new_session":"s1_h1","summary_request":{}}))
        );
        let (state, _, pending) = derive_state(&log);
        assert_eq!(state, "exhausted");
        assert!(pending.is_empty());
    }

    /// A user message after the exhaustion reopens normal work: the
    /// loop runs the new turn, and a later exhaustion re-closes it.
    #[test]
    fn user_message_after_exhaustion_reopens_the_loop() {
        let log = format!(
            "{}\n{}\n{}",
            line(&serde_json::json!({"v":1,"type":"context_exhausted","ts":"t","message":"m","new_session":""})),
            line(&serde_json::json!({"v":1,"type":"user_message","ts":"t","content":"continue"})),
            line(&serde_json::json!({"v":1,"type":"context_exhausted","ts":"t","message":"m","new_session":"s1_h2"}))
        );
        let (state, seq, _) = derive_state(&log);
        assert_eq!(state, "exhausted");
        assert_eq!(seq, 2);
    }

    #[test]
    fn error_after_exhaustion_stays_exhausted() {
        // The handoff flow logs a failed summary error before the
        // marker event. The marker is the last word on the state.
        let log = format!(
            "{}\n{}",
            line(&serde_json::json!({"v":1,"type":"error","ts":"t","message":"summary call failed"})),
            line(&serde_json::json!({"v":1,"type":"context_exhausted","ts":"t","message":"m","new_session":""}))
        );
        let (state, _, _) = derive_state(&log);
        assert_eq!(state, "exhausted");
    }
}
