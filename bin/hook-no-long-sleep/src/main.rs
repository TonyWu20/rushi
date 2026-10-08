//! `harness-hook-no-long-sleep` — a `tool.before` guard against
//! long blocking waits.
//!
//! Registered on the `tool.before` window via `[hooks.defs]` +
//! `[hooks.pipeline."tool.before"]` (docs/reference/monitoring.md).
//! Reads the pending tool batch from stdin (JSON), inspects each
//! call's shell `command` argument, and emits the window state on
//! stdout:
//!
//! - `{}` — proceed (the step is a no-op; the accumulated state is
//!   unchanged).
//! - `{"blocked_calls":[{"id":..., "reason":...}]}` plus the input
//!   state — block the listed calls; the loop synthesizes a failed
//!   `tool_result` for each.
//!
//! Blocked calls carry a reason that points at the monitor pattern:
//! run the work detached and wake this session with
//! `rushi run <session> "<msg>"` when the state changes.

mod sleepcheck;

use clap::Parser;
use serde_json::{json, Value};
use std::io::Read;

/// The default cap for a single `sleep`, in seconds. Override with
/// the `RUSHI_SLEEP_MAX_S` environment variable.
const DEFAULT_MAX_S: f64 = 60.0;

/// The hook's command line. All arguments are absorbed and ignored.
/// A strict parser would turn an unknown arg into exit 2 (abort) in
/// the pipeline, so the hook stays lenient.
#[derive(Parser)]
#[command(
    name = "harness-hook-no-long-sleep",
    about = "tool.before guard for long blocking waits",
    version,
    allow_hyphen_values = true,
    after_help = r#"Window: tool.before
Input (stdin): {window, session, calls:[{id, name, arguments}]}
Checks every call whose `arguments.command` string is a shell
command. It blocks:
  - a `sleep` longer than the cap (default 60s, the sum of
    consecutive values, `s`/`m`/`h`/`d` suffixes);
  - a `while`/`until` loop that contains `sleep` (an unbounded
    in-call poll);
  - a `for` loop whose estimated total wait (iterations x
    per-iteration sleep) exceeds the cap, or whose iteration
    count or sleep duration is not a literal (uncountable).
Known limits: sleeps inside `$( )` substitution and variable
durations outside a loop are not detected; `for` iteration
counts are estimated from literal lists, `{A..B}` ranges, and
`$(seq ...)` only.
Output (stdout):
  {} — proceed (no violations in this step)
  {"blocked_calls":[{"id":...,"reason":...}]} — block the listed
  calls, merged with blocks an earlier step recorded. The loop
  synthesizes a failed `tool_result` per blocked call, and the
  reason points at the monitor pattern (`rushi docs monitoring`).
Environment: RUSHI_SLEEP_MAX_S — the cap in seconds (default 60).
Exit codes: 0 = ok (proceed, or block via `blocked_calls`)
  (the hook never exits non-zero; a crash would fail the chain
   and let the window default proceed)"#
)]
struct Args {
    /// Trailing args. Absorbed, never read. The hook payload arrives
    /// on stdin; the `[hooks.defs]` args pass through here.
    #[arg(trailing_var_arg = true, required = false)]
    extra: Vec<String>,
}

fn main() {
    let _args = Args::parse();

    let payload = read_stdin_json();

    // Not our window: no-op step, state unchanged.
    if payload.get("window").and_then(|w| w.as_str()) != Some("tool.before") {
        println!("{}", json!({}));
        return;
    }

    let max_s = std::env::var("RUSHI_SLEEP_MAX_S")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| *v > 0.0)
        .unwrap_or(DEFAULT_MAX_S);

    // A call carries a shell command when `arguments.command` is a
    // string. This covers the kernel `bash` tool and any extension
    // tool that follows the same argument name.
    let calls = payload
        .get("calls")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    // Merge with blocks an earlier step recorded, so this step works
    // at any pipeline position.
    let mut blocked = payload
        .get("blocked_calls")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    for call in &calls {
        let Some(command) = call
            .get("arguments")
            .and_then(|a| a.get("command"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let Some(block) = sleepcheck::check_command(command, max_s) else {
            continue;
        };
        let id = call
            .get("id")
            .and_then(|i| i.as_str())
            .unwrap_or("")
            .to_string();
        let truncated = truncate(command, 300);
        blocked.push(json!({
            "id": id,
            "reason": reason_text(&block, &truncated, max_s),
        }));
    }

    if blocked.is_empty() {
        println!("{}", json!({}));
    } else {
        // Carry the input state through so later steps and the
        // kernel still see `calls` and the effect fields earlier
        // steps set.
        let mut out = payload.clone();
        out["blocked_calls"] = Value::Array(blocked);
        println!("{out}");
    }
}

/// The agent-facing reason. Mirrors the `no-find-grep` message
/// style: the detection, the offending command, and the fix.
fn reason_text(block: &sleepcheck::Block, cmd: &str, max_s: f64) -> String {
    match block.kind {
        "poll-loop" => format!(
            "POLL LOOP DETECTED! {detail}\n\
             A looping wait blocks the tool call for a long or unbounded\n\
             time.\n\
             Your command: {cmd}\n\
             Fix: move the wait into a monitor script. Write it to a file,\n\
             launch it detached with `nohup`, and have it wake this session\n\
             with `rushi run <session> \"<msg>\"` when the condition holds.\n\
             See `rushi docs monitoring`.",
            detail = block.detail
        ),
        "long-sleep" => format!(
            "LONG SLEEP DETECTED! The command waits {wait} past the {cap}s cap.\n\
             A blocking wait stalls the loop and spams the context with one\n\
             poll per turn.\n\
             Your command: {cmd}\n\
             Fix: for a wait longer than one tool call, write a monitor\n\
             script, launch it detached with `nohup`, and have it send this\n\
             session one short message with `rushi run <session> \"<msg>\"`\n\
             when the state changes or the task finishes.\n\
             See `rushi docs monitoring`.",
            wait = block.detail,
            cap = max_s
        ),
        other => format!("WAIT PATTERN DETECTED! ({other}) Your command: {cmd}"),
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.len() <= max_chars {
        return s.to_string();
    }
    // Cut on a char boundary.
    let mut end = max_chars;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

fn read_stdin_json() -> Value {
    let mut buf = String::new();
    if std::io::stdin()
        .read_to_string(&mut buf)
        .is_err()
        || buf.trim().is_empty()
    {
        return json!({});
    }
    serde_json::from_str(&buf).unwrap_or(json!({}))
}

