use clap::Parser;
use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;

/// Validate model output and emit execution events
#[derive(Parser)]
#[command(name = "parse", about = "Validate model output and emit execution events")]
struct Args {
    /// Path to config file
    #[arg(long, default_value = "config.toml")]
    config: String,
}

fn main() {
    let args = Args::parse();

    let config_path = &args.config;
    let config_content = match fs::read_to_string(config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: cannot read config: {e}");
            std::process::exit(1);
        }
    };

    let config: toml::Value = match config_content.parse() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Error: invalid config TOML: {e}");
            std::process::exit(1);
        }
    };

    let tools_root = config
        .get("paths")
        .and_then(|p| p.get("tools_root"))
        .and_then(|t| t.as_str())
        .unwrap_or("tools");

    // Read model output from stdin
    let mut input_str = String::new();
    io::stdin()
        .read_to_string(&mut input_str)
        .expect("Failed to read stdin");

    let model_output: serde_json::Value = match serde_json::from_str(&input_str) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Error: malformed JSON: {e}");
            std::process::exit(1);
        }
    };

    let text = model_output
        .get("text")
        .and_then(|t| t.as_str())
        .unwrap_or("");

    let tool_calls: Vec<serde_json::Value> = model_output
        .get("tool_calls")
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default();

    let stop_reason = model_output
        .get("stop_reason")
        .and_then(|s| s.as_str())
        .unwrap_or("stop");

    let usage = model_output.get("usage").cloned();

    // Load valid tool names from tools/
    let tools_root_path = PathBuf::from(&tools_root);
    let mut valid_tools: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Ok(entries) = fs::read_dir(&tools_root_path) {
        for entry in entries.flatten() {
            let tool_path = entry.path();
            if tool_path.is_dir() {
                let tool_toml = tool_path.join("tool.toml");
                if tool_toml.exists() {
                    if let Some(name) = tool_path.file_name() {
                        valid_tools.insert(name.to_string_lossy().to_string());
                    }
                }
            }
        }
    }

    // Validate tool calls
    for tc in &tool_calls {
        let tc_id = tc.get("id").and_then(|id| id.as_str()).unwrap_or("unknown");
        let tc_name = tc.get("name").and_then(|n| n.as_str()).unwrap_or("unknown");

        // Check arguments parse as JSON object
        if let Some(args_val) = tc.get("arguments") {
            match normalize_arguments(Some(args_val)) {
                Some(serde_json::Value::Object(_)) => {}
                _ => {
                    let ts = chrono_utc_now();
                    let error_event = serde_json::json!({
                        "v": 1,
                        "type": "error",
                        "ts": ts,
                        "message": format!("Model emitted malformed tool arguments for call {}.", tc_id)
                    });
                    println!("{}", error_event);
                    std::process::exit(2);
                }
            }
        } else {
            let ts = chrono_utc_now();
            let error_event = serde_json::json!({
                "v": 1,
                "type": "error",
                "ts": ts,
                "message": format!("Model emitted malformed tool arguments for call {}.", tc_id)
            });
            println!("{}", error_event);
            std::process::exit(2);
        }

        // Check tool name matches manifest
        if !valid_tools.contains(tc_name) {
            let ts = chrono_utc_now();
            let error_event = serde_json::json!({
                "v": 1,
                "type": "error",
                "ts": ts,
                "message": format!("Model called unknown tool {}.", tc_name)
            });
            println!("{}", error_event);
            std::process::exit(2);
        }
    }

    let ts = chrono_utc_now();

    // Emit events based on stop_reason
    match stop_reason {
        "error" | "aborted" => {
            let error_event = serde_json::json!({
                "v": 1,
                "type": "error",
                "ts": ts,
                "message": format!("Model stop reason: {}.", stop_reason)
            });
            println!("{}", error_event);
            std::process::exit(2);
        }
        "length" => {
            // Emit assistant_message with truncated tool results
            let mut assistant_tool_calls: Vec<serde_json::Value> = Vec::new();
            for tc in &tool_calls {
                let args = normalize_arguments(tc.get("arguments")).unwrap_or(serde_json::json!({}));
                assistant_tool_calls.push(serde_json::json!({
                    "id": tc.get("id").and_then(|id| id.as_str()).unwrap_or(""),
                    "name": tc.get("name").and_then(|n| n.as_str()).unwrap_or(""),
                    "arguments": args
                }));
            }

            let assistant_message = serde_json::json!({
                "v": 1,
                "type": "assistant_message",
                "ts": ts,
                "content": text,
                "tool_calls": assistant_tool_calls,
                "stop_reason": stop_reason,
                "usage": usage
            });
            println!("{}", assistant_message);

            for tc in &tool_calls {
                let tc_id = tc.get("id").and_then(|id| id.as_str()).unwrap_or("");
                let tool_result = serde_json::json!({
                    "v": 1,
                    "type": "tool_result",
                    "ts": ts,
                    "id": tc_id,
                    "value": {
                        "text": "Arguments may be truncated. Re-issue the call with shorter arguments."
                    },
                    "is_error": true
                });
                println!("{}", tool_result);
            }
            std::process::exit(2);
        }
        _ => {
            // Emit assistant_message
            let mut assistant_tool_calls: Vec<serde_json::Value> = Vec::new();
            for tc in &tool_calls {
                let args = normalize_arguments(tc.get("arguments")).unwrap_or(serde_json::json!({}));
                assistant_tool_calls.push(serde_json::json!({
                    "id": tc.get("id").and_then(|id| id.as_str()).unwrap_or(""),
                    "name": tc.get("name").and_then(|n| n.as_str()).unwrap_or(""),
                    "arguments": args
                }));
            }

            let assistant_message = serde_json::json!({
                "v": 1,
                "type": "assistant_message",
                "ts": ts,
                "content": text,
                "tool_calls": assistant_tool_calls,
                "stop_reason": stop_reason,
                "usage": usage
            });
            println!("{}", assistant_message);

            // Emit tool_call events if there are tool calls
            if tool_calls.is_empty() {
                std::process::exit(2);
            }

            for tc in &tool_calls {
                let tc_id = tc.get("id").and_then(|id| id.as_str()).unwrap_or("");
                let tc_name = tc.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let tc_args = normalize_arguments(tc.get("arguments")).unwrap_or(serde_json::json!({}));

                let tool_call_event = serde_json::json!({
                    "v": 1,
                    "type": "tool_call",
                    "ts": ts,
                    "id": tc_id,
                    "name": tc_name,
                    "arguments": tc_args
                });
                println!("{}", tool_call_event);
            }
            std::process::exit(1);
        }
    }
}

fn normalize_arguments(args: Option<&serde_json::Value>) -> Option<serde_json::Value> {
    match args {
        Some(serde_json::Value::String(s)) => serde_json::from_str::<serde_json::Value>(s).ok(),
        Some(v) => Some(v.clone()),
        None => None,
    }
}

fn chrono_utc_now() -> String {
    chrono::Utc::now()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}
