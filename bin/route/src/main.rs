#![deny(clippy::todo, clippy::unimplemented, clippy::unreachable)]

use harness_common::logline::LogLine;

use clap::Parser;
use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

/// Dispatch tool calls to subprocesses
#[derive(Parser)]
#[command(name = "route", about = "Dispatch tool calls to tool subprocesses")]
struct Args {
    /// Path to tools directory
    #[arg(long, default_value = "tools")]
    tools: String,

    /// Tool result max chars
    #[arg(long, default_value = "20000")]
    tool_result_max_chars: usize,

    /// Working directory for tool subprocesses
    #[arg(long)]
    cwd: Option<PathBuf>,

    /// Per-session tool log. When set, the full tool output appends to
    /// this file (one record per call, `LogLine` commit) and the
    /// emitted `tool_result` event is a slim index: status, byte
    /// length, a pointer into the tool log, and a short head/tail
    /// preview (docs/tool-log-design_from_human.md). When absent, the
    /// legacy inline full-text event stays.
    #[arg(long)]
    tool_log: Option<PathBuf>,
}

/// One tool call's outcome, either a run or a not-run error.
struct Outcome {
    /// The exit code of the run. `None` when the tool never ran.
    exit: Option<i64>,
    /// The tool's raw stdout, unclipped. Empty when the tool never ran.
    stdout: String,
    /// The tool's raw stderr, unclipped. Empty when the tool never ran.
    stderr: String,
    /// The error message when the tool never ran.
    error: Option<String>,
}

impl Outcome {
    /// A call the tool layer rejected before spawning: schema
    /// validation, unknown tool, spawn, timeout, or wait failure.
    fn not_run(msg: String) -> Self {
        Outcome {
            exit: None,
            stdout: String::new(),
            stderr: String::new(),
            error: Some(msg),
        }
    }

    fn is_error(&self) -> bool {
        self.error.is_some() || self.exit.is_some_and(|c| c != 0)
    }

    /// The display text of this result, unclipped. This is what the
    /// event log's index previews and what `assemble` feeds the model
    /// (with its caps). A not-run call shows its error message. An
    /// error exit shows stderr when it is non-empty, else the exit
    /// code line. A clean run shows the JSON-aware stdout.
    fn display_text(&self, max_chars: usize) -> String {
        if let Some(e) = &self.error {
            return e.clone();
        }
        match self.exit {
            Some(0) => process_stdout(&self.stdout, max_chars),
            Some(c) => {
                if self.stderr.is_empty() {
                    format!("Tool exited with code {c}.")
                } else {
                    self.stderr.clone()
                }
            }
            None => String::new(),
        }
    }
}

/// One record of the per-session tool log: the full stdout, stderr,
/// and exit status of one call, in order, keyed by the call id.
/// `text` is the unclipped display form: what the slim index previews
/// and what `assemble` feeds the model. It keeps the model view
/// stable across the split: the record owns the display text.
fn tool_log_record(o: &Outcome, ts: &str, id: &str) -> String {
    let text = o.display_text(usize::MAX);
    let mut rec = serde_json::json!({
        "v": 1,
        "ts": ts,
        "id": id,
        "exit": o.exit.map(serde_json::Value::from),
        "stdout": o.stdout,
        "stderr": o.stderr,
        "text": text,
        "is_error": o.is_error(),
    });
    if let Some(e) = &o.error {
        rec["error"] = serde_json::json!(e);
    }
    serde_json::to_string(&rec).unwrap_or_default()
}

/// A short head/tail preview of a longer text. Short text passes
/// through whole, so schema-error messages always stay intact in the
/// index. The elision marker carries both counts.
const PREVIEW_HEAD_CHARS: usize = 200;
const PREVIEW_TAIL_CHARS: usize = 200;

fn preview(text: &str) -> String {
    let total = text.chars().count();
    if total <= PREVIEW_HEAD_CHARS + PREVIEW_TAIL_CHARS {
        return text.to_string();
    }
    let head: String = text.chars().take(PREVIEW_HEAD_CHARS).collect();
    let tail: String = text
        .chars()
        .rev()
        .take(PREVIEW_TAIL_CHARS)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let elided = total - PREVIEW_HEAD_CHARS - PREVIEW_TAIL_CHARS;
    format!("{head}\n[{elided} of {total} chars elided; full body in the tool log]\n{tail}")
}

/// The slim `tool_result` index event for the session log: status,
/// byte length, the tool-log pointer, and a head/tail preview. The
/// full body lives in the tool log, keyed by the same call id.
fn slim_result_event(o: &Outcome, ts: &str, id: &str, log_name: &str) -> String {
    let text = o.display_text(usize::MAX);
    let bytes = text.len();
    serde_json::to_string(&serde_json::json!({
        "v": 1,
        "type": "tool_result",
        "ts": ts,
        "id": id,
        "value": { "text": preview(&text) },
        "is_error": o.is_error(),
        "bytes": bytes,
        "tool_log": log_name,
    }))
    .unwrap_or_default()
}

/// The legacy inline `tool_result` event: the clipped display text in
/// the log, no tool log pointer. Keeps the old clip behavior exactly:
/// the JSON-aware stdout clip on success, the stderr clip on error.
fn legacy_result_event(o: &Outcome, ts: &str, id: &str, max_chars: usize) -> String {
    let text = match &o.error {
        Some(e) => e.clone(),
        None => match o.exit {
            Some(0) => process_stdout(&o.stdout, max_chars),
            Some(c) => {
                if o.stderr.is_empty() {
                    format!("Tool exited with code {c}.")
                } else {
                    let mut text = o.stderr.clone();
                    if text.len() > max_chars {
                        text = text.chars().take(max_chars).collect();
                    }
                    text
                }
            }
            None => String::new(),
        },
    };
    serde_json::to_string(&serde_json::json!({
        "v": 1,
        "type": "tool_result",
        "ts": ts,
        "id": id,
        "value": { "text": text },
        "is_error": o.is_error(),
    }))
    .unwrap_or_default()
}

/// Commit one tool log record, in call order, through the locked
/// single-write `LogLine` commit (FT-005). A failed commit stops the
/// stage: the tool activity must not outrun its log.
fn write_tool_log_line(args: &Args, ts: &str, id: &str, o: &Outcome) {
    let Some(path) = &args.tool_log else {
        return;
    };
    let line = LogLine::from_json(&tool_log_record(o, ts, id));
    if let Err(e) = line.commit(path) {
        eprintln!("Error: cannot write tool log: {e}");
        std::process::exit(1);
    }
}

/// Emit one result: the tool log record when a tool log is set, then
/// the event on stdout (slim index or legacy full text).
fn emit_result(o: &Outcome, ts: &str, id: &str, args: &Args) -> String {
    match &args.tool_log {
        Some(path) => {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "tools.jsonl".to_string());
            write_tool_log_line(args, ts, id, o);
            slim_result_event(o, ts, id, &name)
        }
        None => legacy_result_event(o, ts, id, args.tool_result_max_chars),
    }
}

fn main() {
    let args = Args::parse();

    // Read tool_call events from stdin
    let stdin = io::stdin();
    let mut tool_calls: Vec<serde_json::Value> = Vec::new();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let event: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if event.get("type").and_then(|t| t.as_str()) == Some("tool_call") {
            tool_calls.push(event);
        }
    }

    let tools_root = PathBuf::from(&args.tools);

    // Load tool manifests and validate arguments
    let mut tool_manifests: HashMap<String, (String, serde_json::Value)> = HashMap::new();
    if let Ok(entries) = fs::read_dir(&tools_root) {
        let entries_vec: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        for entry in entries_vec {
            let tool_path = entry.path();
            if tool_path.is_dir() {
                let tool_toml = tool_path.join("tool.toml");
                if tool_toml.exists() {
                    if let Some(name) = tool_path.file_name() {
                        let name_str = name.to_string_lossy().to_string();
                        if let Ok(content) = fs::read_to_string(&tool_toml) {
                            if let Ok(config) = content.parse::<toml::Value>() {
                                let raw_command = config
                                    .get("tool")
                                    .and_then(|t| t.get("command"))
                                    .and_then(|c| c.as_str())
                                    .unwrap_or(&name_str)
                                    .to_string();
                                // Resolve the tool binary. The binary name is
                                // the manifest `command` when it differs from
                                // the tool dir name (e.g. `harness-bash` for
                                // the `bash` tool, so the built binary never
                                // shadows a system tool on PATH), otherwise the
                                // tool dir name. Prefer the cargo build output
                                // (this binary's own directory), then the
                                // tools/<name>/bin/ copy, then a bare PATH
                                // lookup.
                                let binary_name = if raw_command == name_str {
                                    name_str.clone()
                                } else {
                                    raw_command.clone()
                                };
                                let exe_dir = std::env::current_exe()
                                    .ok()
                                    .and_then(|p| p.parent().map(|d| d.to_path_buf()));
                                let candidates = [
                                    exe_dir.as_ref().map(|d| d.join(&binary_name)),
                                    Some(tool_path.join("bin").join(&binary_name)),
                                ];
                                let command = candidates
                                    .iter()
                                    .flatten()
                                    .find(|p| p.is_file())
                                    .map(|p| p.to_string_lossy().to_string())
                                    .unwrap_or(raw_command);
                                let args_val = config
                                    .get("tool")
                                    .and_then(|t| t.get("args"))
                                    .cloned()
                                    .unwrap_or(toml::Value::Array(toml::value::Array::new()));
                                let timeout_ms = config
                                    .get("tool")
                                    .and_then(|t| t.get("timeout_ms"))
                                    .and_then(|t| t.as_integer())
                                    .unwrap_or(30000)
                                    as u64;
                                let schema = config
                                    .get("tool")
                                    .and_then(|t| t.get("schema"))
                                    .cloned()
                                    .unwrap_or_else(|| {
                                        let mut tbl = toml::value::Table::new();
                                        tbl.insert(
                                            "type".to_string(),
                                            toml::Value::String("object".to_string()),
                                        );
                                        tbl.insert(
                                            "properties".to_string(),
                                            toml::Value::Table(toml::value::Table::new()),
                                        );
                                        tbl.insert(
                                            "required".to_string(),
                                            toml::Value::Array(toml::value::Array::new()),
                                        );
                                        toml::Value::Table(tbl)
                                    });
                                let args_json: serde_json::Value = toml_to_json(&args_val);
                                let schema_json: serde_json::Value = toml_to_json(&schema);
                                tool_manifests.insert(
                                    name_str.clone(),
                                    (
                                        command,
                                        serde_json::json!({
                                            "args": args_json,
                                            "timeout_ms": timeout_ms,
                                            "schema": schema_json
                                        }),
                                    ),
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    // Process each tool call
    let ts = chrono_utc_now();
    for tc in &tool_calls {
        let tc_id = tc.get("id").and_then(|id| id.as_str()).unwrap_or("");
        let tc_name = tc.get("name").and_then(|n| n.as_str()).unwrap_or("");
        // Arguments may be a JSON string or already a JSON object
        let tc_args: serde_json::Value = match tc.get("arguments") {
            Some(serde_json::Value::String(s)) => match serde_json::from_str(s) {
                Ok(v) => v,
                Err(_) => {
                    let o = Outcome::not_run(
                            "Tool arguments failed schema validation: invalid JSON. The arguments string is not valid JSON. Resend the call with a JSON object."
                                .to_string(),
                        );
                    println!("{}", emit_result(&o, &ts, tc_id, &args));
                    continue;
                }
            },
            Some(v) => v.clone(),
            None => serde_json::json!({}),
        };
        let tc_args_str = serde_json::to_string(&tc_args).unwrap_or_default();

        // Check if tool manifest exists
        let manifest = match tool_manifests.get(tc_name) {
            Some(m) => m,
            None => {
                let o = Outcome::not_run(format!("Unknown tool {tc_name}."));
                println!("{}", emit_result(&o, &ts, tc_id, &args));
                continue;
            }
        };

        let schema = manifest.1.get("schema").unwrap();
        if let Some(field) = validate_args(&tc_args, schema) {
            // The first sentence is the stable prefix: the compact pass
            // in `assemble` keys the schema-error pairs off it.
            // The suffix tells the model what to fix (correction 59).
            let o = if field == "arguments" {
                Outcome::not_run(
                    "Tool arguments failed schema validation: arguments. The arguments value must be a JSON object. Resend the call with a JSON object."
                        .to_string(),
                )
            } else {
                Outcome::not_run(format!(
                    "Tool arguments failed schema validation: {field}. Required fields are missing from the call. Resend the call with all required fields filled in."
                ))
            };
            println!("{}", emit_result(&o, &ts, tc_id, &args));
            continue;
        }

        // Spawn subprocess
        let command = &manifest.0;
        let args_json = manifest.1.get("args").unwrap();
        let timeout_ms = manifest
            .1
            .get("timeout_ms")
            .and_then(|t| t.as_u64())
            .unwrap_or(30000);

        let mut cmd = Command::new(command);
        if let Some(ref cwd) = args.cwd {
            cmd.current_dir(cwd);
        }
        let mut child = match cmd
            .args(
                args_json
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str())
                            .map(|s| s.to_string())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
                    .iter(),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                let o = Outcome::not_run(format!("Failed to spawn tool: {e}"));
                println!("{}", emit_result(&o, &ts, tc_id, &args));
                continue;
            }
        };

        // Write arguments to stdin
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(tc_args_str.as_bytes());
        }

        // Wait for subprocess with timeout
        let output = if timeout_ms > 0 {
            // Use a simple timeout approach
            match child.wait_with_output_timeout(timeout_ms as u64) {
                Ok(o) => o,
                Err(_) => {
                    let _ = child.kill();
                    let o = Outcome::not_run(format!("Tool timed out after {timeout_ms} ms."));
                    println!("{}", emit_result(&o, &ts, tc_id, &args));
                    continue;
                }
            }
        } else {
            match child.wait_with_output() {
                Ok(o) => o,
                Err(e) => {
                    let o = Outcome::not_run(format!("Failed to wait for tool: {e}"));
                    println!("{}", emit_result(&o, &ts, tc_id, &args));
                    continue;
                }
            }
        };

        let exit_code = output.status.code().unwrap_or(1);
        let stdout_str = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr_str = String::from_utf8_lossy(&output.stderr).to_string();

        // The outcome holds the unclipped bodies. The tool log takes
        // them whole; the event path picks its own clip.
        let o = Outcome {
            exit: Some(i64::from(exit_code)),
            stdout: stdout_str,
            stderr: stderr_str,
            error: None,
        };
        println!("{}", emit_result(&o, &ts, tc_id, &args));
    }
}

fn process_stdout(stdout: &str, max_chars: usize) -> String {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return "".to_string();
    }

    // Try to parse as JSON object with text field
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if let Some(obj) = val.as_object() {
            if let Some(text) = obj.get("text").and_then(|t| t.as_str()) {
                return text.to_string();
            }
            // JSON object without text field
            return serde_json::to_string(&val).unwrap_or_default();
        }
        // JSON but not an object
        return serde_json::to_string(&val).unwrap_or_default();
    }

    // Non-JSON text
    let mut text = trimmed.to_string();
    if text.len() > max_chars {
        text = text.chars().take(max_chars).collect();
    }
    text
}

fn validate_args(args: &serde_json::Value, schema: &serde_json::Value) -> Option<String> {
    if let Some(obj) = args.as_object() {
        let mut missing: Vec<String> = Vec::new();
        if let Some(required) = schema.get("required").and_then(|r| r.as_array()) {
            for req in required {
                if let Some(field) = req.as_str() {
                    if !obj.contains_key(field) {
                        missing.push(field.to_string());
                    }
                }
            }
        }
        return (!missing.is_empty()).then(|| missing.join(", "));
    }
    Some("arguments".to_string())
}

fn chrono_utc_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn toml_to_json(val: &toml::Value) -> serde_json::Value {
    match val {
        toml::Value::String(s) => serde_json::json!(s),
        toml::Value::Integer(i) => serde_json::json!(*i),
        toml::Value::Float(f) => serde_json::json!(*f),
        toml::Value::Boolean(b) => serde_json::json!(*b),
        toml::Value::Array(arr) => {
            serde_json::json!(arr.iter().map(toml_to_json).collect::<Vec<_>>())
        }
        toml::Value::Table(tbl) => {
            let mut map = serde_json::Map::new();
            for (k, v) in tbl {
                map.insert(k.clone(), toml_to_json(v));
            }
            serde_json::Value::Object(map)
        }
        toml::Value::Datetime(dt) => serde_json::json!(dt.to_string()),
    }
}

trait WaitWithOutput {
    fn wait_with_output_timeout(&mut self, timeout_ms: u64) -> io::Result<std::process::Output>;
}

impl WaitWithOutput for Child {
    fn wait_with_output_timeout(&mut self, timeout_ms: u64) -> io::Result<std::process::Output> {
        let start = std::time::Instant::now();
        let duration = std::time::Duration::from_millis(timeout_ms);

        while start.elapsed() < duration {
            match self.try_wait() {
                Ok(Some(status)) => {
                    let stdout = self
                        .stdout
                        .take()
                        .map(|mut s| {
                            let mut buf = Vec::new();
                            io::Read::read_to_end(&mut s, &mut buf).unwrap_or(0);
                            buf
                        })
                        .unwrap_or_default();
                    let stderr = self
                        .stderr
                        .take()
                        .map(|mut s| {
                            let mut buf = Vec::new();
                            io::Read::read_to_end(&mut s, &mut buf).unwrap_or(0);
                            buf
                        })
                        .unwrap_or_default();
                    return Ok(std::process::Output {
                        status,
                        stdout,
                        stderr,
                    });
                }
                Ok(None) => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(e) => return Err(e),
            }
        }

        Err(io::Error::new(io::ErrorKind::TimedOut, "Timeout"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_outcome() -> Outcome {
        Outcome {
            exit: Some(0),
            stdout: "line one\nline two".to_string(),
            stderr: String::new(),
            error: None,
        }
    }

    #[test]
    fn preview_passes_short_text_through_whole() {
        let msg = "Tool arguments failed schema validation: command.";
        assert_eq!(preview(msg), msg, "short text must stay intact");
    }

    #[test]
    fn preview_keeps_head_and_tail_of_long_text() {
        let mut text = String::new();
        for i in 0..1000 {
            text.push('x');
            if i % 97 == 0 {
                text.push('\n');
            }
        }
        let p = preview(&text);
        assert!(p.starts_with("x"), "the head must survive");
        assert!(p.ends_with('x'), "the tail must survive");
        assert!(
            p.contains("chars elided"),
            "the elision marker must name the gap"
        );
        let head: String = text.chars().take(PREVIEW_HEAD_CHARS).collect();
        let tail: String = text
            .chars()
            .rev()
            .take(PREVIEW_TAIL_CHARS)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        assert!(p.starts_with(&head));
        assert!(p.ends_with(&tail));
    }

    #[test]
    fn preview_is_char_boundary_safe() {
        // 3-byte chars: a byte-based cut would land mid-char.
        let text = "\u{2500}".repeat(500);
        let p = preview(&text);
        let total: String = p.lines().next().unwrap().to_string();
        assert_eq!(total.chars().count(), PREVIEW_HEAD_CHARS);
    }

    #[test]
    fn not_run_outcome_carry_the_error_text() {
        let o = Outcome::not_run("Tool arguments failed schema validation: file_path.".to_string());
        assert!(o.is_error());
        assert_eq!(
            o.display_text(usize::MAX),
            "Tool arguments failed schema validation: file_path."
        );
    }

    #[test]
    fn validate_args_lists_the_missing_required_fields() {
        let schema = serde_json::json!({"required": ["command", "timeout_secs"]});
        assert_eq!(
            validate_args(&serde_json::json!({}), &schema),
            Some("command, timeout_secs".to_string())
        );
        assert_eq!(
            validate_args(&serde_json::json!({"command": "pwd"}), &schema),
            Some("timeout_secs".to_string())
        );
        assert_eq!(
            validate_args(
                &serde_json::json!({"command": "pwd", "timeout_secs": 5}),
                &schema
            ),
            None
        );
    }

    #[test]
    fn validate_args_flags_a_non_object_arguments_value() {
        let schema = serde_json::json!({"required": ["command"]});
        assert_eq!(
            validate_args(&serde_json::json!(["cmd"]), &schema),
            Some("arguments".to_string())
        );
    }

    #[test]
    fn clean_run_display_text_is_the_stdout() {
        let o = run_outcome();
        assert!(!o.is_error());
        assert_eq!(o.display_text(usize::MAX), "line one\nline two");
    }

    #[test]
    fn error_run_shows_stderr_when_present() {
        let o = Outcome {
            exit: Some(1),
            stdout: "out".to_string(),
            stderr: "boom".to_string(),
            error: None,
        };
        assert!(o.is_error());
        assert_eq!(o.display_text(usize::MAX), "boom");
    }

    #[test]
    fn error_run_without_stderr_shows_the_exit_code() {
        let o = Outcome {
            exit: Some(3),
            stdout: "out".to_string(),
            stderr: String::new(),
            error: None,
        };
        assert_eq!(o.display_text(usize::MAX), "Tool exited with code 3.");
    }

    #[test]
    fn slim_event_is_an_index_with_pointer_and_preview() {
        let o = run_outcome();
        let ev: serde_json::Value =
            serde_json::from_str(&slim_result_event(&o, "t", "c1", "tools.jsonl")).unwrap();
        assert_eq!(ev["type"], "tool_result");
        assert_eq!(ev["id"], "c1");
        assert_eq!(ev["is_error"], false);
        assert_eq!(ev["tool_log"], "tools.jsonl");
        assert!(ev["bytes"].is_u64());
        // Short body: the preview is the whole text, no elision marker.
        assert_eq!(ev["value"]["text"], "line one\nline two");
    }

    #[test]
    fn legacy_event_keeps_the_clipped_inline_text() {
        let o = Outcome {
            exit: Some(0),
            stdout: "a".repeat(5000),
            stderr: String::new(),
            error: None,
        };
        let ev: serde_json::Value =
            serde_json::from_str(&legacy_result_event(&o, "t", "c1", 100)).unwrap();
        assert_eq!(ev["value"]["text"].as_str().unwrap().len(), 100);
        assert!(
            ev.get("tool_log").is_none(),
            "legacy events carry no pointer"
        );
        assert!(
            ev.get("bytes").is_none(),
            "legacy events carry no byte length"
        );
    }

    #[test]
    fn tool_log_record_holds_the_full_bodies() {
        let o = Outcome {
            exit: Some(1),
            stdout: "stdout body".to_string(),
            stderr: "stderr body".to_string(),
            error: None,
        };
        let rec: serde_json::Value = serde_json::from_str(&tool_log_record(&o, "t", "c1")).unwrap();
        assert_eq!(rec["id"], "c1");
        assert_eq!(rec["exit"], 1);
        assert_eq!(rec["stdout"], "stdout body");
        assert_eq!(rec["stderr"], "stderr body");
        assert_eq!(rec["is_error"], true);
        assert!(rec.get("error").is_none());
    }

    #[test]
    fn tool_log_record_for_not_run_carry_the_error() {
        let o = Outcome::not_run("Unknown tool nope.".to_string());
        let rec: serde_json::Value = serde_json::from_str(&tool_log_record(&o, "t", "c2")).unwrap();
        assert_eq!(rec["exit"], serde_json::Value::Null);
        assert_eq!(rec["error"], "Unknown tool nope.");
        assert_eq!(rec["stdout"], "");
    }

    #[test]
    fn emit_result_writes_the_tool_log_and_emits_the_slim_index() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("tools.jsonl");
        let args = Args {
            tools: "tools".to_string(),
            tool_result_max_chars: 20000,
            cwd: None,
            tool_log: Some(log_path.clone()),
        };
        let o = run_outcome();
        let ev = emit_result(&o, "t", "c9", &args);
        // The record landed in the tool log, keyed by the call id.
        let record = std::fs::read_to_string(&log_path).unwrap();
        let rec: serde_json::Value = serde_json::from_str(record.trim_end()).unwrap();
        assert_eq!(rec["id"], "c9");
        assert_eq!(rec["stdout"], "line one\nline two");
        // The event on stdout is the slim index, not the full body.
        let ev: serde_json::Value = serde_json::from_str(&ev).unwrap();
        assert_eq!(ev["tool_log"], "tools.jsonl");
        assert_eq!(ev["value"]["text"], "line one\nline two");
    }
}
