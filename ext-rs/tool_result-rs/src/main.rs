//! The reference `tool_result` renderer, Rust port
//! (ui-extension-plan stage 4). The bash reference is
//! ui_extensions/tool_result/.
//!
//! The `render` kind owner for `tool_result`. The host forwards
//! every tool_result event (live and, at start, the visible
//! transcript). For each one the binary answers with a `lines`
//! reply:
//! - a header line: an [ext] marker, the tool_call id, and the
//!   exit status. Green when ok, red when the result is an error
//! - the result body, one dim line per hard line of the text
//!
//! Body precedence mirrors the built-in render (docs/tui.md 13.1):
//! value.text, then stdout plus stderr, then a string value, then
//! the compact JSON of the value. Nothing is truncated: the body
//! is shown in full, like the built-in render.
//!
//! When this binary dies the host exhausts the restart budget,
//! drops the cached replies, and the built-in render returns
//! (ui-extension-plan stage 4 acceptance).

use serde_json::{json, Value};
use std::io::{BufRead, Write};

/// The body of a tool result value, in the built-in precedence
/// (docs/tui.md 13.1): `text`, then `stdout` plus `stderr`, then a
/// string value, then the compact JSON of the value.
fn body_of(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Object(o) => {
            let t = o.get("text").and_then(|x| x.as_str()).unwrap_or("");
            let so = o.get("stdout").and_then(|x| x.as_str()).unwrap_or("");
            let se = o.get("stderr").and_then(|x| x.as_str()).unwrap_or("");
            if !t.is_empty() {
                t.to_string()
            } else if !so.is_empty() || !se.is_empty() {
                let mut out = so.to_string();
                if !se.is_empty() {
                    out.push_str("\n[stderr]\n");
                    out.push_str(se);
                }
                out
            } else {
                value.to_string()
            }
        }
        // A number or boolean value: compact JSON.
        other => other.to_string(),
    }
}

fn main() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = std::io::LineWriter::new(stdout.lock());
    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            break;
        };
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            // A malformed line: no state change, no reply.
            continue;
        };
        if v.get("op").and_then(|o| o.as_str()) != Some("event") {
            continue;
        }
        let ev = v.get("event").unwrap_or(&Value::Null);
        if ev.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
            continue;
        }
        // The reply's `event_id` is the op's `id` (the log index
        // the host caches by).
        let Some(event_id) = v.get("id").and_then(|x| x.as_u64()) else {
            continue;
        };
        let tid = ev
            .get("id")
            .and_then(|x| x.as_str())
            .unwrap_or("?")
            .to_string();
        let is_error = ev.get("is_error").and_then(|x| x.as_bool()).unwrap_or(false);
        let code = ev
            .get("value")
            .and_then(|v| v.get("exit_code"))
            .and_then(|x| x.as_i64())
            .or_else(|| ev.get("value").and_then(|v| v.get("exit")).and_then(|x| x.as_i64()));
        let status = match code {
            Some(c) => {
                if is_error {
                    format!("exit {c} (error)")
                } else {
                    format!("exit {c}")
                }
            }
            None => {
                if is_error {
                    "error".to_string()
                } else {
                    "ok".to_string()
                }
            }
        };
        let value = ev.get("value").unwrap_or(&Value::Null);
        let body = body_of(value);
        let header_style = if is_error {
            json!({"fg": "red", "bold": true})
        } else {
            json!({"fg": "green", "bold": true})
        };
        let mut lines: Vec<Value> =
            vec![json!([format!("[ext] tool:{tid}  {status}"), header_style])];
        for hard in body.split('\n') {
            if hard.is_empty() {
                continue;
            }
            lines.push(json!([hard, {"fg": "darkgray"}]));
        }
        let reply = json!({
            "v": 1,
            "op": "lines",
            "event_id": event_id,
            "lines": lines,
        });
        let _ = writeln!(out, "{reply}");
        let _ = out.flush();
    }
}
