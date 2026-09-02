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
    // schema must validate here, not only in the TUI. The three
    // compaction marker types join the list (docs/auto-compact-plan.md
    // section 4.1): without the entry, log rejects every marker as
    // an unknown event type.
    let schemas = load_schemas(&args.schemas);

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

    validate_lines(&lines, &schemas);

    // One locked single-write append per line (FT-005). `LogLine` is
    // the only type that may write the session log.
    for line in &lines {
        if let Err(e) = LogLine::from_json(line).commit(&log_path) {
            eprintln!("Error: cannot write to log: {e}");
            std::process::exit(1);
        }
    }
}

/// The schema list of the session log's event vocabulary. A marker
/// type missing from the list rejects as unknown at validation time.
fn schema_files() -> Vec<&'static str> {
    vec![
        "user_message.json",
        "assistant_message.json",
        "tool_call.json",
        "tool_result.json",
        "error.json",
        "context_exhausted.json",
        "ext_status.json",
        "compaction_started.json",
        "compaction_failed.json",
        "compaction_summary.json",
    ]
}

/// Load the schemas of one schema directory into (type, schema) pairs.
fn load_schemas(schemas_dir: &str) -> Vec<(String, serde_json::Value)> {
    let mut schemas: Vec<(String, serde_json::Value)> = Vec::new();
    for sf in &schema_files() {
        let sp = PathBuf::from(schemas_dir).join(sf);
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
    schemas
}

/// Validate every log line against the schemas. Exits 1 on a bad
/// line: unknown type, malformed JSON, or a schema mismatch.
fn validate_lines(lines: &[String], schemas: &[(String, serde_json::Value)]) {
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

        let event_type = parsed.get("type").and_then(|t| t.as_str()).unwrap_or("");

        let mut valid = false;
        for (etype, schema) in schemas {
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
            eprintln!(
                "Error: line {} has unknown event type '{}'",
                idx + 1,
                event_type
            );
            std::process::exit(1);
        }
    }
}

fn validate_against_schema(value: &serde_json::Value, schema: &serde_json::Value) -> bool {
    // Check const constraint
    if let Some(const_val) = schema.get("const") {
        return value == const_val;
    }
    // Check enum constraint: the value must equal one of the
    // listed values (docs/tui-pending-user-messages.md stage 2: the
    // user_message queue field).
    if let Some(allowed) = schema.get("enum").and_then(|e| e.as_array()) {
        return allowed.iter().any(|a| value == a);
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
        Some("string") => {
            return value.is_string();
        }
        Some("integer") => {
            return value.is_i64();
        }
        Some("number") => {
            return value.is_f64();
        }
        Some("boolean") => {
            return value.is_boolean();
        }
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
        _ => {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a schemas dir holding one schema per name, and load the
    /// pairs through the same path `main` uses.
    fn schemas_with(names: &[&str], dir: &std::path::Path) -> Vec<(String, serde_json::Value)> {
        let mut out = Vec::new();
        for n in names {
            let p = dir.join(n);
            let val: serde_json::Value = serde_json::from_str(
                &fs::read_to_string(&p).expect("schema file"),
            )
            .unwrap();
            let etype = val
                .get("properties")
                .and_then(|p| p.get("type"))
                .and_then(|t| t.get("const"))
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            out.push((etype, val));
        }
        out
    }

    fn repo_schema_dir() -> std::path::PathBuf {
        // The tests run from the crate dir; the schemas live at the
        // workspace root.
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../schemas/events/v1")
    }

    #[test]
    fn marker_schemas_cover_the_three_compaction_types() {
        let names: Vec<&str> = schema_files()
            .iter()
            .filter(|s| s.contains("compaction"))
            .map(|s| *s)
            .collect();
        assert_eq!(
            names,
            vec![
                "compaction_started.json",
                "compaction_failed.json",
                "compaction_summary.json",
            ],
            "the three marker schemas join the hardcoded list"
        );
        // The marker schemas load and carry their type const.
        let schemas = schemas_with(&names, &repo_schema_dir());
        let types: Vec<String> = schemas.iter().map(|(t, _)| t.clone()).collect();
        assert!(types.contains(&"compaction_started".to_string()));
        assert!(types.contains(&"compaction_failed".to_string()));
        assert!(types.contains(&"compaction_summary".to_string()));
    }

    #[test]
    fn marker_events_validate_against_their_schemas() {
        let dir = repo_schema_dir();
        let schemas =
            schemas_with(&schema_files().iter().map(|s| *s).collect::<Vec<_>>(), &dir);
        let events = [
            r#"{"v":1,"type":"compaction_started","ts":"t","reason":"threshold","tokens_before":212000}"#,
            r#"{"v":1,"type":"compaction_failed","ts":"t","reason":"overflow","detail":"the summary call stopped with error","last_user_seq":7}"#,
            r#"{"v":1,"type":"compaction_summary","ts":"t","summary":"s","first_kept_seq":312,"reason":"threshold","tokens_before":212000,"tokens_after":33000,"read_files":["a.txt"],"modified_files":["b.rs"],"usage":{"input_tokens":10,"output_tokens":5}}"#,
        ];
        for ev in &events {
            let parsed: serde_json::Value = serde_json::from_str(ev).unwrap();
            let ty = parsed.get("type").and_then(|t| t.as_str()).unwrap();
            let schema = schemas
                .iter()
                .find(|(t, _)| t == ty)
                .expect("the type is in the schema list");
            assert!(validate_against_schema(&parsed, &schema.1), "{ev}");
        }
    }

    #[test]
    fn missing_list_entry_rejects_the_type_as_unknown() {
        // A schemas dir without the compaction_summary schema must
        // reject the event as an unknown type.
        let dir = tempfile::tempdir().unwrap();
        for n in ["user_message.json", "error.json"] {
            let src = repo_schema_dir().join(n);
            fs::copy(&src, dir.path().join(n)).unwrap();
        }
        let schemas =
            schemas_with(&["user_message.json", "error.json"], dir.path());
        let ok: serde_json::Value =
            serde_json::from_str(r#"{"v":1,"type":"user_message","ts":"t","content":"hi"}"#).unwrap();
        assert!(validate_against_schema(&ok, &schemas[0].1));
        let marker: serde_json::Value =
            serde_json::from_str(r#"{"v":1,"type":"compaction_summary","ts":"t","summary":"s","first_kept_seq":1,"reason":"threshold","tokens_before":0}"#).unwrap();
        let etype = marker.get("type").and_then(|t| t.as_str()).unwrap().to_string();
        assert!(
            !schemas.iter().any(|(t, _)| t == &etype),
            "the type is absent from the trimmed list"
        );
    }
}
