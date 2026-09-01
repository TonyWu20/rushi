#![deny(clippy::todo, clippy::unimplemented, clippy::unreachable)]

mod logline;
use logline::LogLine;

use clap::Parser;
use std::fs;
use std::io::{self, BufRead};
use std::path::PathBuf;

/// Append events to the session log
#[derive(Parser)]
#[command(name = "log", about = "Append events to the session log")]
struct Args {
    /// Session directory path
    #[arg(long)]
    session: String,

    /// Path to schema directory
    #[arg(long, default_value = "schemas/events/v1")]
    schemas: String,
}

fn main() {
    let args = Args::parse();

    let log_path = PathBuf::from(&args.session).join("events.jsonl");

    // Ensure session directory exists
    if !PathBuf::from(&args.session).exists() {
        if let Err(e) = fs::create_dir_all(&args.session) {
            eprintln!("Error: cannot create session directory: {e}");
            std::process::exit(1);
        }
    }

    // Load all schemas
    // The list mirrors the session log's event vocabulary. ext_status
    // is shared UI state: the loop publishes the loop_phase marker
    // through this binary (docs/tui-model-wait-indicator.md), so the
    // schema must validate here, not only in the TUI.
    let schema_files = [
        "user_message.json",
        "assistant_message.json",
        "tool_call.json",
        "tool_result.json",
        "error.json",
        "context_exhausted.json",
        "ext_status.json",
    ];

    let mut schemas: Vec<(String, serde_json::Value)> = Vec::new();
    for sf in &schema_files {
        let sp = PathBuf::from(&args.schemas).join(sf);
        let content = match fs::read_to_string(&sp) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let val: serde_json::Value = match serde_json::from_str(&content) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("Error: invalid schema {sf}: {e}");
                std::process::exit(1);
            }
        };
        let event_type = val
            .get("properties")
            .and_then(|p| p.get("type"))
            .and_then(|t| t.get("const"))
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();
        schemas.push((event_type, val));
    }

    // Read all lines from stdin first for validation
    let stdin = io::stdin();
    let mut lines: Vec<String> = Vec::new();
    let mut line_num = 0;
    for line in stdin.lock().lines() {
        line_num += 1;
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("Error: cannot read line {line_num}: {e}");
                std::process::exit(1);
            }
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        lines.push(line);
    }

    // Validate each line against schemas
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let parsed: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("Error: line {} is not valid JSON: {e}", idx + 1);
                std::process::exit(1);
            }
        };

        let event_type = parsed
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("");

        let mut valid = false;
        for (etype, schema) in &schemas {
            if etype == event_type {
                if validate_against_schema(&parsed, schema) {
                    valid = true;
                    break;
                } else {
                    eprintln!(
                        "Error: line {} does not match schema for event type '{}'",
                        idx + 1,
                        event_type
                    );
                    std::process::exit(1);
                }
            }
        }
        if !valid {
            eprintln!("Error: line {} has unknown event type '{}'", idx + 1, event_type);
            std::process::exit(1);
        }
    }

    // One locked single-write append per line (FT-005). `LogLine` is
    // the only type that may write the session log.
    for line in &lines {
        if let Err(e) = LogLine::from_json(line).commit(&log_path) {
            eprintln!("Error: cannot write to log: {e}");
            std::process::exit(1);
        }
    }
}

fn validate_against_schema(value: &serde_json::Value, schema: &serde_json::Value) -> bool {
    // Check const constraint
    if let Some(const_val) = schema.get("const") {
        return value == const_val;
    }
    let schema_type = schema.get("type").and_then(|t| t.as_str());
    match schema_type {
        Some("object") => {
            if let Some(obj) = value.as_object() {
                if let Some(required) = schema.get("required").and_then(|r| r.as_array()) {
                    for req in required {
                        if let Some(field) = req.as_str() {
                            if !obj.contains_key(field) {
                                return false;
                            }
                        }
                    }
                }
                if let Some(properties) = schema.get("properties") {
                    if let Some(props) = properties.as_object() {
                        for (key, prop_schema) in props {
                            if let Some(val) = obj.get(key) {
                                if !validate_against_schema(val, prop_schema) {
                                    return false;
                                }
                            }
                        }
                    }
                }
                return true;
            }
        }
        Some("string") => { return value.is_string(); }
        Some("integer") => { return value.is_i64(); }
        Some("number") => { return value.is_f64(); }
        Some("boolean") => { return value.is_boolean(); }
        Some("array") => {
            if let Some(arr) = value.as_array() {
                if let Some(items_schema) = schema.get("items") {
                    for item in arr {
                        if !validate_against_schema(item, items_schema) {
                            return false;
                        }
                    }
                }
                return true;
            }
        }
        _ => { return true; }
    }
    false
}
