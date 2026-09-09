#![deny(clippy::todo, clippy::unimplemented, clippy::unreachable)]

use rushi_common::event_validation;
use rushi_common::logline::LogLine;

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

    // Load schemas by glob (the shared loader picks up every `*.json`
    // in the directory, so new event types need no code change here).
    let schemas = event_validation::load_schemas(&args.schemas);

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

    if let Err(msg) = event_validation::validate_lines(&lines, &schemas) {
        eprintln!("Error: {msg}");
        std::process::exit(1);
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

#[cfg(test)]
mod tests {
    use super::*;
    use rushi_common::event_validation;

    fn repo_schema_dir() -> std::path::PathBuf {
        // The tests run from the crate dir; the schemas live at the
        // workspace root.
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../schemas/events/v1")
    }

    #[test]
    fn marker_schemas_load_from_the_schema_dir() {
        // The glob loader picks up the three compaction marker schemas.
        let schemas =
            event_validation::load_schemas(repo_schema_dir().to_str().unwrap());
        let types: Vec<String> = schemas.iter().map(|(t, _)| t.clone()).collect();
        assert!(types.contains(&"compaction_started".to_string()));
        assert!(types.contains(&"compaction_failed".to_string()));
        assert!(types.contains(&"compaction_summary".to_string()));
        assert!(types.contains(&"context_exhausted".to_string()));
    }

    #[test]
    fn marker_events_validate_against_their_schemas() {
        let schemas =
            event_validation::load_schemas(repo_schema_dir().to_str().unwrap());
        let events = [
            r#"{"v":1,"type":"compaction_started","ts":"t","reason":"threshold","tokens_before":212000}"#,
            r#"{"v":1,"type":"compaction_failed","ts":"t","reason":"overflow","detail":"the summary call stopped with error","last_user_seq":7}"#,
            r#"{"v":1,"type":"compaction_summary","ts":"t","summary":"s","first_kept_seq":312,"version":1,"parent_version":0,"diverge_seq":0,"reason":"threshold","tokens_before":212000,"tokens_after":33000,"read_files":["a.txt"],"modified_files":["b.rs"],"usage":{"input_tokens":10,"output_tokens":5}}"#,
        ];
        for ev in &events {
            let parsed: serde_json::Value = serde_json::from_str(ev).unwrap();
            assert!(
                event_validation::validate_value(&parsed, &schemas).is_ok(),
                "{ev}"
            );
        }
    }

    #[test]
    fn missing_schema_file_rejects_the_type_as_unknown() {
        // A schemas dir without the compaction_summary schema must
        // reject the event as an unknown type.
        let dir = tempfile::tempdir().unwrap();
        for n in ["user_message.json", "error.json"] {
            let src = repo_schema_dir().join(n);
            fs::copy(&src, dir.path().join(n)).unwrap();
        }
        let schemas = event_validation::load_schemas(dir.path().to_str().unwrap());
        assert!(schemas.iter().any(|(t, _)| t == "user_message"));
        assert!(!schemas.iter().any(|(t, _)| t == "compaction_summary"));

        let ok: serde_json::Value =
            serde_json::from_str(r#"{"v":1,"type":"user_message","ts":"t","content":"hi"}"#)
                .unwrap();
        assert!(event_validation::validate_value(&ok, &schemas).is_ok());

        let marker: serde_json::Value = serde_json::from_str(
            r#"{"v":1,"type":"compaction_summary","ts":"t","summary":"s","first_kept_seq":1,"version":1,"parent_version":0,"diverge_seq":0,"reason":"threshold","tokens_before":0}"#,
        )
        .unwrap();
        assert!(
            event_validation::validate_value(&marker, &schemas).is_err(),
            "the type is absent from the trimmed dir"
        );
    }
}
