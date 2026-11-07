#![deny(clippy::todo, clippy::unimplemented, clippy::unreachable)]

use rushi_common::event;
use rushi_common::logstream;
use rushi_common::rewind;
use rushi_common::logline::LogLine;

use clap::Parser;
use std::fs;
use std::io::{self, BufRead};
use std::path::Path;
use std::path::PathBuf;

/// Append events to the session log
#[derive(Parser)]
#[command(
    name = "rushi-log",
    about = "Append events to the session log",
    version
)]
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

    // The kernel guard at the write boundary (P8,
    // docs/rewind-fork-design.md): a rewind that would strand a
    // tool-call pair in the projected context is refused with a
    // named error, and nothing is appended. The read-side P4
    // backstop in `bin/assemble` then covers only hand-edited
    // logs.
    guard_rewind_batch(&lines, &log_path);

    // One locked single-write append per line (FT-005). `LogLine` is
    // the only type that may write the session log. Rewind markers
    // take the kernel append entry point (P8): the guard re-runs
    // against the on-disk log at commit time, so a marker never
    // lands stranded even when a batch mixes markers and events.
    for line in &lines {
        match event::parse_event(line) {
            Ok(event::Event::Rewind(_)) => {
                if let Err(rej) = rewind::append_rewind(line, &log_path) {
                    match rej {
                        rewind::AppendRejection::Guard(g) => {
                            eprintln!("rushi-log: refusing the append: {g}");
                            eprintln!("rushi-log: the log is unchanged. Rewind to a settled point: a user_message, a tool_result, or an assistant_message with no outstanding tool calls.");
                        }
                        other => {
                            eprintln!("rushi-log: {other}");
                        }
                    }
                    std::process::exit(1);
                }
            }
            _ => {
                if let Err(e) = LogLine::from_json(line).commit(&log_path) {
                    eprintln!("Error: cannot write to log: {e}");
                    std::process::exit(1);
                }
            }
        }
    }
}

/// The P8 write-boundary guard: refuse, loudly, a `rewind` line
/// whose projected context strands a tool-call pair. The existing
/// log reads through `logstream` (plain or zstd stream; a missing
/// log is empty). The batch walks a virtual log: each accepted line
/// extends the context the later rewind lines are judged against.
fn guard_rewind_batch(lines: &[String], log_path: &Path) {
    let existing: Vec<serde_json::Value> = match logstream::read_log_text(log_path) {
        Ok(text) => text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect(),
        // A missing log is an empty log: every rewind target is
        // out of range, and the guard says so by name.
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            eprintln!("Error: cannot read session log: {e}");
            std::process::exit(1);
        }
    };
    let mut virtual_log: Vec<serde_json::Value> = existing;
    for line in lines {
        if let Ok(event::Event::Rewind(r)) = event::parse_event(line) {
            let before = r.mode == event::RewindMode::Before;
            if let Err(rej) = rewind::guard_rewind(&virtual_log, r.target_seq, before) {
                eprintln!("rushi-log: refusing the append: {rej}");
                eprintln!("rushi-log: the log is unchanged. Rewind to a settled point: a user_message, a tool_result, or an assistant_message with no outstanding tool calls.");
                std::process::exit(1);
            }
        }
        if let Ok(v) = serde_json::from_str(line) {
            virtual_log.push(v);
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
