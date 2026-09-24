//! `harness-hook-compact` — the in-session shadow-compact hook.
//!
//! Registered on the `overflow.resolve` and `exhausted.handle`
//! windows via `[hooks.defs]` + `[hooks.pipeline."<window>"]`
//! (docs/loop-lifecycle-hooks.md section 12, issue #38).
//!
//! Pipeline-ABI contract (12.3, 12.5):
//! - stdin carries the accumulated window state JSON.
//! - exit 0 — `ok`. The step's stdout JSON becomes the accumulated
//!   state. This hook is an identity transform: on a successful
//!   compaction it passes the input state through and adds no state
//!   fields (the window's default, `stay_compact`, applies).
//! - exit 3 — `fail`: the compact binary failed or could not be run.
//!   The kernel stops the chain, logs `hook.<window>.error`, and the
//!   window falls back to its default (the loop never wedges, P4).
//! - exit 2 — `abort`: not used by this hook.
//! - any other exit code is treated as `fail` with an "unknown exit
//!   N" detail by the kernel.

use std::io::{self, Read};
use std::process::{Command, Stdio};

fn main() {
    // --help / self-documentation (docs/loop-lifecycle-hooks.md 4.5, 12.3)
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
            // Not our window; no-op step (empty state contribution).
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
            // Spawn failure: a step-level failure. The kernel logs
            // `hook.<window>.error` and the window falls back to its
            // default (P4: the loop never wedges).
            fail(format!("failed to spawn compact: {e}"));
        }
    };

    let stdout_str = String::from_utf8_lossy(&output.stdout).to_string();
    let status: serde_json::Value =
        serde_json::from_str(&stdout_str).unwrap_or(serde_json::Value::Null);
    let status_str = status.get("status").and_then(|s| s.as_str()).unwrap_or("failed");

    match status_str {
        "compacted" | "noop" => {
            // The compaction completed. No state-field effect is
            // needed: the window default (`stay_compact`) continues
            // the strategy cycle. Pass the accumulated state through
            // unchanged.
            println!("{}", payload);
            std::process::exit(0);
        }
        _ => {
            // The compact failed: a step-level failure, not a veto.
            // The kernel stops the chain, logs the error, and the
            // window resolves to its default (the kernel's own
            // shadow-compact strategy continues; P4).
            fail(format!(
                "compact failed (status={status_str}): {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }
}

/// Emit the failure detail on stdout (the kernel reads `reason` from
/// stdout on a `fail` exit) and exit 3.
fn fail(reason: String) -> ! {
    eprintln!("harness-hook-compact: {reason}");
    println!("{}", serde_json::json!({ "reason": reason }));
    std::process::exit(3);
}

fn print_help() {
    println!("harness-hook-compact — in-session shadow-compact hook");
    println!();
    println!("Windows: overflow.resolve, exhausted.handle");
    println!("Input (stdin): the accumulated window state JSON object:");
    println!("  window: string");
    println!("  session: string (also in $SESSION)");
    println!("  force: bool (optional)");
    println!("Output (stdout): the step's state JSON object;");
    println!("  on success this hook passes the input state through unchanged.");
    println!("Exit codes (docs/loop-lifecycle-hooks.md 12.3):");
    println!("  0  ok — compaction succeeded, no state-field effect");
    println!("  3  fail — compact binary failed or could not run; the");
    println!("      kernel logs the error and the window falls back to its");
    println!("      default (stay_compact)");
    println!("  (exit 2 abort is unused by this hook; any other exit code");
    println!("   is treated as fail with an \"unknown exit N\" detail)");
}
