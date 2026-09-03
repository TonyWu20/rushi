//! The reference `tool_result` renderer, Rust port
//! (ui-extension-plan stage 4). The bash reference is
//! ui_extensions-demos/tool_result/.
//!
//! The `render` kind owner for `tool_result`. The host forwards
//! every tool_result event (live and, at start, the visible
//! transcript). For each one the binary answers with a `lines`
//! reply:
//! - a header line: an [ext] marker, the tool_call id, and the
//!   exit status. Green when ok, red when the result is an error
//!   (the pi macchiato `success` / `error` hexes, docs/tui-color-
//!   pi-alignment.md)
//! - the result body. A body that is a complete JSON document gets
//!   JSON syntax highlighting (keys, strings, numbers, literals,
//!   punctuation), one multi-span line per hard line. Any other
//!   body renders in one muted tone, not a single gray
//!   (docs/tui-color-tones.md section 4: stop the gray abuse)
//!
//! Body precedence mirrors the built-in render (docs/tui.md 13.1):
//! value.text, then stdout plus stderr, then a string value, then
//! the compact JSON of the value. The body is shown in full: this
//! reply protocol has no fold control yet. The built-in render
//! folds long bodies to a preview cap (docs/tui-tool-display-port.md);
//! the rescoped rule keeps the no-truncation promise on the
//! `content` field of user and assistant messages only (docs/
//! tui-tool-result-truncation.md section 4).
//!
//! Colors are catppuccin-macchiato hex values. The host lowers
//! them to the terminal capability level at storage time, so the
//! reply shows what the TUI actually emits.
//!
//! When this binary dies the host exhausts the restart budget,
//! drops the cached replies, and the built-in render returns
//! (ui-extension-plan stage 4 acceptance).

use serde_json::{json, Value};
use std::io::{BufRead, Write};

/// Muted body tone that replaces the single darkgray (the gray
/// abuse of docs/tui-color-tones.md). The pi catppuccin-macchiato
/// `toolOutput` value (`text` var), docs/tui-color-pi-alignment.md;
/// the host lowers it to the capability level at storage time.
const BODY_TONE: &str = "#cad3f5";
/// JSON token colors (the pi `syntax*` roles the JSON walk colors
/// through, docs/tui-color-pi-alignment.md): keys through
/// `syntaxVariable`, strings through `syntaxString`, numbers and
/// the `true`/`false`/`null` literals through `syntaxNumber`, and
/// the punctuation through `syntaxPunctuation`.
const JSON_KEY: &str = "#cad3f5";
const JSON_STRING: &str = "#a6da95";
const JSON_NUMBER: &str = "#f5a97f";
const JSON_LITERAL: &str = "#f5a97f";
const JSON_NULL: &str = "#f5a97f";
const JSON_PUNCT: &str = "#939ab7";

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

/// Whether `text` is a complete JSON document: it trims to a `{` or
/// `[` start and parses as a whole.
fn looks_like_json(text: &str) -> bool {
    let t = text.trim();
    (t.starts_with('{') || t.starts_with('['))
        && serde_json::from_str::<Value>(t).is_ok()
}

/// One wire style object for the ext reply (the host lowers the
/// hex at storage time).
fn fg(f: &str) -> Value {
    json!({"fg": f})
}

/// Tokenize one hard line of a JSON document into wire spans:
/// `[text, style]` pairs. A string that a colon follows is a key;
/// the other strings are values. Whitespace is one plain span
/// (null style). The tokenizer is a walk of the characters: a
/// string owns its escapes, a number owns its sign, fraction, and
/// exponent, a word owns its letters.
fn json_line_spans(line: &str) -> Vec<Value> {
    let cs: Vec<char> = line.chars().collect();
    let n = cs.len();
    let mut out: Vec<Value> = Vec::new();
    let mut i = 0usize;
    while i < n {
        let c = cs[i];
        match c {
            '"' => {
                // The string owns its escapes: walk to the close
                // quote; a backslash skips its pair.
                let mut j = i + 1;
                while j < n {
                    match cs[j] {
                        '\\' => {
                            j += 2;
                        }
                        '"' => {
                            j += 1;
                            break;
                        }
                        _ => {
                            j += 1;
                        }
                    }
                }
                let text: String = cs[i..j.min(n)].iter().collect();
                // A key position: a string that a colon follows,
                // past the whitespace.
                let mut k = j.min(n);
                while k < n && cs[k] == ' ' {
                    k += 1;
                }
                let is_key = k < n && cs[k] == ':';
                out.push(json!([text, if is_key { fg(JSON_KEY) } else { fg(JSON_STRING) }]));
                i = j.min(n);
            }
            '-' | '0'..='9' => {
                // A number: optional sign, digits, optional
                // fraction, optional exponent.
                let mut j = i;
                if cs[j] == '-' {
                    j += 1;
                }
                while j < n && cs[j].is_ascii_digit() {
                    j += 1;
                }
                if j < n && cs[j] == '.' {
                    j += 1;
                    while j < n && cs[j].is_ascii_digit() {
                        j += 1;
                    }
                }
                if j < n && (cs[j] == 'e' || cs[j] == 'E') {
                    j += 1;
                    if j < n && (cs[j] == '+' || cs[j] == '-') {
                        j += 1;
                    }
                    while j < n && cs[j].is_ascii_digit() {
                        j += 1;
                    }
                }
                let text: String = cs[i..j].iter().collect();
                out.push(json!([text, fg(JSON_NUMBER)]));
                i = j;
            }
            'a'..='z' | 'A'..='Z' => {
                // A literal word: true, false, or null.
                let mut j = i;
                while j < n && cs[j].is_ascii_alphabetic() {
                    j += 1;
                }
                let word: String = cs[i..j].iter().collect();
                let color = match word.as_str() {
                    "true" | "false" => JSON_LITERAL,
                    _ => JSON_NULL,
                };
                out.push(json!([word, fg(color)]));
                i = j;
            }
            '{' | '}' | '[' | ']' | ',' | ':' => {
                out.push(json!([c.to_string(), fg(JSON_PUNCT)]));
                i += 1;
            }
            // Whitespace: one plain span, null style.
            _ => {
                let mut j = i;
                while j < n && (cs[j] == ' ' || cs[j] == '\t') {
                    j += 1;
                }
                let text: String = cs[i..j].iter().collect();
                out.push(json!([text, Value::Null]));
                i = j;
            }
        }
    }
    out
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
            // The pi macchiato `error` hex (docs/tui-color-pi-
            // alignment.md), not a hard-coded swatch.
            json!({"fg": "#ed8796", "bold": true})
        } else {
            // The pi macchiato `success` hex.
            json!({"fg": "#a6da95", "bold": true})
        };
        let header = json!([format!("[ext] tool:{tid}  {status}"), header_style]);
        let mut lines: Vec<Value> = vec![header];
        if looks_like_json(&body) {
            // The JSON path: one multi-span line per hard line.
            for hard in body.split('\n') {
                if hard.trim().is_empty() {
                    continue;
                }
                lines.push(Value::Array(json_line_spans(hard)));
            }
        } else {
            // The plain path: each hard line the muted tone.
            for hard in body.split('\n') {
                if hard.is_empty() {
                    continue;
                }
                lines.push(json!([hard, fg(BODY_TONE)]));
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_body_gets_the_token_spans() {
        let body = r#"{"a":1,"s":"x","n":null}"#;
        assert!(looks_like_json(body));
        let spans = json_line_spans(body);
        // The key, the number, the string value, the null.
        let flat: Vec<(String, String)> = spans
            .iter()
            .map(|s| {
                (
                    s[0].as_str().unwrap().to_string(),
                    s[1]
                        .get("fg")
                        .and_then(|f| f.as_str())
                        .unwrap_or("plain")
                        .to_string(),
                )
            })
            .collect();
        let want: Vec<(String, String)> = vec![
            ("{".into(), JSON_PUNCT.into()),
            (r#""a""#.into(), JSON_KEY.into()),
            (":".into(), JSON_PUNCT.into()),
            ("1".into(), JSON_NUMBER.into()),
            (",".into(), JSON_PUNCT.into()),
            (r#""s""#.into(), JSON_KEY.into()),
            (":".into(), JSON_PUNCT.into()),
            (r#""x""#.into(), JSON_STRING.into()),
            (",".into(), JSON_PUNCT.into()),
            (r#""n""#.into(), JSON_KEY.into()),
            (":".into(), JSON_PUNCT.into()),
            ("null".into(), JSON_NULL.into()),
            ("}".into(), JSON_PUNCT.into()),
        ];
        assert_eq!(flat, want);
    }

    #[test]
    fn a_string_value_is_not_a_key() {
        let spans = json_line_spans(r#"[{"k": "v"}]"#);
        let flat: Vec<(String, String)> = spans
            .iter()
            .map(|s| {
                (
                    s[0].as_str().unwrap().to_string(),
                    s[1]
                        .get("fg")
                        .and_then(|f| f.as_str())
                        .unwrap_or("plain")
                        .to_string(),
                )
            })
            .collect();
        let v = flat
            .iter()
            .find(|(t, _)| t == r#""v""#
            )
            .expect("the string value shows");
        assert_eq!(v.1, JSON_STRING, "a value string is not a key");
    }

    #[test]
    fn numbers_own_their_parts() {
        let spans = json_line_spans("[-1.5e+2 3]");
        // The punctuation, the two numbers, the plain space span.
        assert_eq!(spans.len(), 5, "{spans:?}");
        assert_eq!(spans[1][0].as_str().unwrap(), "-1.5e+2");
        assert_eq!(spans[3][0].as_str().unwrap(), "3");
    }

    #[test]
    fn only_complete_documents_highlight() {
        assert!(looks_like_json(r#"{"a":1}"#));
        assert!(looks_like_json("[1, 2]"));
        assert!(!looks_like_json("123"));
        assert!(!looks_like_json("two docs {} {}"));
        assert!(!looks_like_json("{ \"broken\": 1"));
        assert!(!looks_like_json("plain text"));
    }
}
