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

    // Parse all events and track state
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
