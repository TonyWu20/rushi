#![deny(clippy::todo, clippy::unimplemented, clippy::unreachable)]

use rushi_common::event;
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

    // The typed event vocabulary is the validator
    // (docs/typed-events.md): each line must parse into an `Event`.
    for line in &lines {
        if let Err(e) = event::parse_event(line) {
            eprintln!("Error: event validation failed: {e}");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_events_parse_as_typed_events() {
        // The three compaction marker events parse through the typed
        // vocabulary (docs/typed-events.md).
        let events = [
            r#"{"v":1,"type":"compaction_started","ts":"t","reason":"threshold","tokens_before":212000}"#,
            r#"{"v":1,"type":"compaction_failed","ts":"t","reason":"overflow","detail":"the summary call stopped with error","last_user_seq":7}"#,
            r#"{"v":1,"type":"compaction_summary","ts":"t","summary":"s","first_kept_seq":312,"version":1,"parent_version":0,"diverge_seq":0,"reason":"threshold","tokens_before":212000,"tokens_after":33000,"read_files":["a.txt"],"modified_files":["b.rs"],"usage":{"input_tokens":10,"output_tokens":5}}"#,
        ];
        for ev in &events {
            assert!(event::parse_event(ev).is_ok(), "{ev}");
        }
    }

    #[test]
    fn unknown_type_tag_is_rejected() {
        // A type outside the 14-type vocabulary is unknown.
        let bogus = r#"{"v":1,"type":"totally_bogus","ts":"t"}"#;
        assert!(event::parse_event(bogus).is_err(), "bogus tag must fail");
    }
}
