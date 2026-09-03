//! `harness-hook-compact` — the in-session shadow-compact hook.
//!
//! Registered on the `overflow.resolve` and `exhausted.handle` windows.
//! It reads the window JSON on stdin, runs the in-session shadow
//! compact by invoking the `compact` binary, and returns a decision
//! envelope on stdout.
//!
//! Decision contract (docs/loop-lifecycle-hooks.md §4.3):
//! - exit 0 + `{}` → no decision, apply window default
//! - exit 0 + `{"decision":"stay_compact",...}` → in-session shadow compact
//! - exit 2 → blocking default (stop the strategy cycle)
//! - exit ≠ 0, ≠ 2 → non-blocking failure, loop applies default

use std::io::{self, Read};
use std::process::{Command, Stdio};

fn main() {
    // --help / self-documentation (docs/loop-lifecycle-hooks.md §4.5)
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return;
    }

    let stdin_payload = read_stdin_json();

    // Determine the window from the payload.
    let window = stdin_payload
        .get("window")
        .and_then(|w| w.as_str())
        .unwrap_or("");

    match window {
        "overflow.resolve" | "exhausted.handle" => {
            handle_compact_window(&stdin_payload);
        }
        _ => {
            // Not our window; no-op.
            println!("{{}}");
        }
    }
}

fn read_stdin_json() -> serde_json::Value {
    let mut buf = String::new();
    if io::stdin()
        .read_to_string(&mut buf)
        .is_err()
        || buf.trim().is_empty()
    {
        return serde_json::json!({});
    }
    serde_json::from_str(&buf).unwrap_or(serde_json::json!({}))
}

fn handle_compact_window(payload: &serde_json::Value) {
    let session = std::env::var("SESSION").unwrap_or_default();
    let sessions_root = std::env::var("SESSIONS_ROOT").unwrap_or_else(|_| "sessions".into());
    let config = std::env::var("CONFIG").unwrap_or_else(|_| "config.toml".into());

    let session_dir = if session.contains('/') || session.contains('\\') {
        std::path::PathBuf::from(&session)
    } else {
        std::path::PathBuf::from(&sessions_root).join(&session)
    };

    // Resolve the compact binary: env override, then sibling.
    let compact_bin = std::env::var("COMPACT_BIN").unwrap_or_else(|_| {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("compact")))
            .unwrap_or_else(|| std::path::PathBuf::from("compact"))
            .to_string_lossy()
            .to_string()
    });

    let reason = if payload.get("window").and_then(|w| w.as_str()) == Some("exhausted.handle") {
        "threshold"
    } else {
        "overflow"
    };

    let force = payload
        .get("force")
        .and_then(|f| f.as_bool())
        .unwrap_or(false);

    let mut cmd = Command::new(&compact_bin);
    cmd.arg(&session_dir)
        .arg("--config")
        .arg(&config)
        .arg("--reason")
        .arg(reason);
    if force {
        cmd.arg("--force");
    }

    let output = match cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!("harness-hook-compact: failed to spawn compact: {e}");
            std::process::exit(1);
        }
    };

    let stdout_str = String::from_utf8_lossy(&output.stdout).to_string();
    let status: serde_json::Value =
        serde_json::from_str(&stdout_str).unwrap_or(serde_json::Value::Null);
    let status_str = status.get("status").and_then(|s| s.as_str()).unwrap_or("failed");

    match status_str {
        "compacted" | "noop" => {
            // The compact ran successfully. Return the stay_compact
            // decision so the loop knows the strategy completed.
            let resp = serde_json::json!({
                "decision": "stay_compact",
                "payload": {
                    "compact_status": status_str,
                    "status": status,
                },
            });
            println!("{}", resp);
            std::process::exit(0);
        }
        _ => {
            // The compact failed. Log to stderr and signal blocking
            // default (stop the strategy cycle).
            eprintln!(
                "harness-hook-compact: compact failed (status={status_str}): {}",
                String::from_utf8_lossy(&output.stderr)
            );
            std::process::exit(2);
        }
    }
}

fn print_help() {
    println!("harness-hook-compact — in-session shadow-compact hook");
    println!();
    println!("Windows: overflow.resolve, exhausted.handle");
    println!("Input (stdin): window JSON object with keys:");
    println!("  window: string");
    println!("  session: string (also in $SESSION)");
    println!("  force: bool (optional)");
    println!("Output (stdout): one JSON decision object");
    println!("  {{\"decision\":\"stay_compact\",\"payload\":{{\"compact_status\":\"...\"}}}}");
    println!("Exit codes:");
    println!("  0  success or no-op");
    println!("  2  blocking: compact failed, stop the strategy cycle");
    println!("  other  non-blocking failure (loop applies default)");
}
