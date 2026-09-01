#![deny(clippy::todo, clippy::unimplemented, clippy::unreachable)]

use clap::Parser;
use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;

/// Validate model output and emit execution events
#[derive(Parser)]
#[command(
    name = "parse",
    about = "Validate model output and emit execution events"
)]
struct Args {
    /// Path to config file
    #[arg(long, default_value = "config.toml")]
    config: String,
}

fn main() {
    let args = Args::parse();

    let config_path = &args.config;
    let config_content = match fs::read_to_string(config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: cannot read config: {e}");
            std::process::exit(1);
        }
    };

    let config: toml::Value = match config_content.parse() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Error: invalid config TOML: {e}");
            std::process::exit(1);
        }
    };

    let tools_root = config
        .get("paths")
        .and_then(|p| p.get("tools_root"))
        .and_then(|t| t.as_str())
        .unwrap_or("tools");

    // Read model output from stdin
    let mut input_str = String::new();
    io::stdin()
        .read_to_string(&mut input_str)
        .expect("Failed to read stdin");

    let model_output: serde_json::Value = match serde_json::from_str(&input_str) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Error: malformed JSON: {e}");
            std::process::exit(1);
        }
    };

    let mut text: String = model_output
        .get("text")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();

    let mut tool_calls: Vec<serde_json::Value> = model_output
        .get("tool_calls")
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default();

    let stop_reason = model_output
        .get("stop_reason")
        .and_then(|s| s.as_str())
        .unwrap_or("stop");

    // A small model may embed tool calls in the text instead of the
    // structured field. Recover them so the loop keeps running.
    if tool_calls.is_empty() && matches!(stop_reason, "stop" | "tool_calls") {
        match extract_text_tool_calls(&text) {
            TextCalls::Absent => {}
            TextCalls::Found { calls, clean } => {
                tool_calls = calls;
                text = clean;
            }
            TextCalls::Bad(msg) => {
                let ts = chrono_utc_now();
                let error_event = serde_json::json!({
                    "v": 1,
                    "type": "error",
                    "ts": ts,
                    "message": format!("Model embedded tool calls in text. Could not parse them: {msg}")
                });
                println!("{}", error_event);
                std::process::exit(2);
            }
        }
    }

    // Usage is optional. Omit it when the model returns null.
    let usage = model_output.get("usage").filter(|u| !u.is_null()).cloned();

    // Reasoning items from the model response. Each item is the
    // server's own item. Forward it verbatim to the event log so the
    // next request can send the thinking back (handoff work item A).
    let reasoning: Vec<serde_json::Value> = model_output
        .get("reasoning")
        .and_then(|r| r.as_array())
        .map(|items| {
            items
                .iter()
                .filter(|i| i.get("type").and_then(|t| t.as_str()) == Some("reasoning"))
                .cloned()
                .collect()
        })
        .unwrap_or_default();

    // The model binary attaches a failure detail to error results
    // (stream truncation, API failure). Pass it through to the error event.
    let detail = model_output
        .get("detail")
        .and_then(|d| d.as_str())
        .map(str::to_string);

    // Load valid tool names from tools/
    let tools_root_path = PathBuf::from(&tools_root);
    let mut valid_tools: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Ok(entries) = fs::read_dir(&tools_root_path) {
        for entry in entries.flatten() {
            let tool_path = entry.path();
            if tool_path.is_dir() {
                let tool_toml = tool_path.join("tool.toml");
                if tool_toml.exists() {
                    if let Some(name) = tool_path.file_name() {
                        valid_tools.insert(name.to_string_lossy().to_string());
                    }
                }
            }
        }
    }

    // Validate tool calls
    for tc in &tool_calls {
        let tc_id = tc.get("id").and_then(|id| id.as_str()).unwrap_or("unknown");
        let tc_name = tc.get("name").and_then(|n| n.as_str()).unwrap_or("unknown");

        // Check arguments parse as JSON object
        if let Some(args_val) = tc.get("arguments") {
            match normalize_arguments(Some(args_val)) {
                Some(serde_json::Value::Object(_)) => {}
                _ => {
                    let ts = chrono_utc_now();
                    let error_event = serde_json::json!({
                        "v": 1,
                        "type": "error",
                        "ts": ts,
                        "message": format!("Model emitted malformed tool arguments for call {}.", tc_id)
                    });
                    println!("{}", error_event);
                    std::process::exit(2);
                }
            }
        } else {
            let ts = chrono_utc_now();
            let error_event = serde_json::json!({
                "v": 1,
                "type": "error",
                "ts": ts,
                "message": format!("Model emitted malformed tool arguments for call {}.", tc_id)
            });
            println!("{}", error_event);
            std::process::exit(2);
        }

        // Check tool name matches manifest
        if !valid_tools.contains(tc_name) {
            let ts = chrono_utc_now();
            let error_event = serde_json::json!({
                "v": 1,
                "type": "error",
                "ts": ts,
                "message": format!("Model called unknown tool {}.", tc_name)
            });
            println!("{}", error_event);
            std::process::exit(2);
        }
    }

    let ts = chrono_utc_now();

    // Emit events based on stop_reason
    match stop_reason {
        "stop" | "tool_calls" => {
            // Normal completion: assistant message plus any tool calls.
            emit_assistant_and_tool_calls(
                &text,
                &tool_calls,
                stop_reason,
                usage.as_ref(),
                &reasoning,
            );
        }
        "error" | "aborted" => {
            let message = match &detail {
                Some(d) => format!("Model stop reason: {stop_reason}. {d}"),
                None => format!("Model stop reason: {stop_reason}."),
            };
            let error_event = serde_json::json!({
                "v": 1,
                "type": "error",
                "ts": ts,
                "message": message
            });
            println!("{}", error_event);
            std::process::exit(2);
        }
        "length" => {
            // Emit assistant_message with truncated tool results
            let mut assistant_tool_calls: Vec<serde_json::Value> = Vec::new();
            for tc in &tool_calls {
                let args =
                    normalize_arguments(tc.get("arguments")).unwrap_or(serde_json::json!({}));
                assistant_tool_calls.push(serde_json::json!({
                    "id": tc.get("id").and_then(|id| id.as_str()).unwrap_or(""),
                    "name": tc.get("name").and_then(|n| n.as_str()).unwrap_or(""),
                    "arguments": args
                }));
            }

            let mut assistant_message = serde_json::json!({
                "v": 1,
                "type": "assistant_message",
                "ts": ts,
                "content": text,
                "tool_calls": assistant_tool_calls,
                "stop_reason": stop_reason
            });
            if let Some(u) = usage.as_ref() {
                assistant_message["usage"] = u.clone();
            }
            if !reasoning.is_empty() {
                assistant_message["reasoning"] = serde_json::json!(reasoning);
            }
            println!("{}", assistant_message);

            for tc in &tool_calls {
                let tc_id = tc.get("id").and_then(|id| id.as_str()).unwrap_or("");
                let tool_result = serde_json::json!({
                    "v": 1,
                    "type": "tool_result",
                    "ts": ts,
                    "id": tc_id,
                    "value": {
                        "text": "Arguments may be truncated. Re-issue the call with shorter arguments."
                    },
                    "is_error": true
                });
                println!("{}", tool_result);
            }
            std::process::exit(2);
        }
        other => {
            // Unknown stop reason: treat as an error so the loop stops.
            let message = match &detail {
                Some(d) => format!("Model stop reason: {other}. {d}"),
                None => format!("Model stop reason: {other}."),
            };
            let error_event = serde_json::json!({
                "v": 1,
                "type": "error",
                "ts": ts,
                "message": message
            });
            println!("{}", error_event);
            std::process::exit(2);
        }
    }
}

/// Emit the assistant_message plus one tool_call event per call.
fn emit_assistant_and_tool_calls(
    text: &str,
    tool_calls: &[serde_json::Value],
    stop_reason: &str,
    usage: Option<&serde_json::Value>,
    reasoning: &[serde_json::Value],
) {
    let ts = chrono_utc_now();

    let assistant_tool_calls: Vec<serde_json::Value> = tool_calls
        .iter()
        .map(|tc| {
            serde_json::json!({
                "id": tc.get("id").and_then(|id| id.as_str()).unwrap_or(""),
                "name": tc.get("name").and_then(|n| n.as_str()).unwrap_or(""),
                "arguments": normalize_arguments(tc.get("arguments")).unwrap_or(serde_json::json!({}))
            })
        })
        .collect();

    let mut assistant_message = serde_json::json!({
        "v": 1,
        "type": "assistant_message",
        "ts": ts,
        "content": text,
        "tool_calls": assistant_tool_calls,
        "stop_reason": stop_reason
    });
    if let Some(u) = usage {
        assistant_message["usage"] = u.clone();
    }
    if !reasoning.is_empty() {
        assistant_message["reasoning"] = serde_json::json!(reasoning);
    }
    println!("{}", assistant_message);

    if tool_calls.is_empty() {
        std::process::exit(2);
    }

    for tc in tool_calls {
        let tool_call_event = serde_json::json!({
            "v": 1,
            "type": "tool_call",
            "ts": ts,
            "id": tc.get("id").and_then(|id| id.as_str()).unwrap_or(""),
            "name": tc.get("name").and_then(|n| n.as_str()).unwrap_or(""),
            "arguments": normalize_arguments(tc.get("arguments")).unwrap_or(serde_json::json!({}))
        });
        println!("{}", tool_call_event);
    }
    std::process::exit(1);
}

fn normalize_arguments(args: Option<&serde_json::Value>) -> Option<serde_json::Value> {
    match args {
        Some(serde_json::Value::String(s)) => serde_json::from_str::<serde_json::Value>(s).ok(),
        Some(v) => Some(v.clone()),
        None => None,
    }
}

/// Tool calls found inside the assistant text.
#[derive(Debug, PartialEq)]
enum TextCalls {
    /// No marker in the text. Treat it as plain prose.
    Absent,
    /// Recovered calls. `clean` is the text with the blocks removed.
    Found {
        calls: Vec<serde_json::Value>,
        clean: String,
    },
    /// A marker is present but the block does not parse.
    Bad(String),
}

/// Byte forms of the tool-call markers. Built from byte arrays so
/// the literals survive tool transports that strip raw markers.
/// The A family holds JSON inside. The P family holds a bare
/// name line and bare parameter keys inside.
const A_OPEN: &[u8] = &[0x0a, 0x3c, 0x69, 0x6e, 0x76, 0x6f, 0x6b, 0x65, 0x3e];
const A_CLOSE: &[u8] = &[0x0a, 0x3c, 0x69, 0x6e, 0x76, 0x6f, 0x6b, 0x65, 0x3e];
const P_OPEN: &[u8] = &[0x0a, 0x3c, 0x69, 0x6e, 0x76, 0x6f, 0x6b, 0x65];
const P_CLOSE: &[u8] = &[0x0a, 0x3c, 0x2f, 0x69, 0x6e, 0x76, 0x6f, 0x6b, 0x65];
const PAR_OPEN: &[u8] = &[0x3c, 0x70, 0x61, 0x72, 0x61, 0x6d, 0x65, 0x74, 0x65, 0x72];
const PAR_CLOSE: &[u8] = &[
    0x3c, 0x2f, 0x70, 0x61, 0x72, 0x61, 0x6d, 0x65, 0x74, 0x65, 0x72,
];

/// Byte search from `from`. Returns a byte offset.
fn find_bytes(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    let last = hay.len() - needle.len();
    if from > last {
        return None;
    }
    for i in from..=last {
        if hay[i..i + needle.len()] == needle[..] {
            return Some(i);
        }
    }
    None
}

/// Find tool calls the model embedded in the text.
///
/// Two marker families appear in the wild. The A family holds
/// JSON: a call object or an array of call objects. Each call
/// holds name and arguments. The P family holds a bare name line
/// and parameter keys. Each key holds one argument field.
fn extract_text_tool_calls(text: &str) -> TextCalls {
    let bytes = text.as_bytes();
    let opens: [&[u8]; 2] = [A_OPEN, P_OPEN];
    let closes: [&[u8]; 2] = [A_CLOSE, P_CLOSE];
    let mut calls: Vec<serde_json::Value> = Vec::new();
    let mut clean = String::new();
    let mut pos = 0usize;
    let mut found = false;

    while pos < bytes.len() {
        // Find the earliest opening marker from pos.
        let mut best: Option<(usize, usize)> = None;
        for (k, open) in opens.iter().enumerate() {
            if let Some(at) = find_bytes(bytes, open, pos) {
                match best {
                    None => best = Some((at, k)),
                    Some((bp, _)) if at < bp => best = Some((at, k)),
                    _ => {}
                }
            }
        }
        let (at, kind) = match best {
            Some(b) => b,
            None => {
                clean.push_str(&text[pos..]);
                break;
            }
        };

        let open = opens[kind];
        let close = closes[kind];
        match find_bytes(bytes, close, at + open.len()) {
            None => {
                // An unterminated marker is prose. Keep it, move past it.
                clean.push_str(&text[pos..at + open.len()]);
                pos = at + open.len();
            }
            Some(end) => {
                let inner = &text[at + open.len()..end];
                clean.push_str(&text[pos..at]);
                match parse_block(kind, inner) {
                    Block::Ok(items) => {
                        for item in items {
                            let id = format!("call_txt_{}", calls.len());
                            calls.push(serde_json::json!({
                                "id": id,
                                "name": item.name,
                                "arguments": item.args
                            }));
                        }
                        found = true;
                    }
                    Block::Bad(msg) => return TextCalls::Bad(msg),
                }
                pos = end + close.len();
            }
        }
    }

    if !found {
        TextCalls::Absent
    } else {
        let clean = clean.trim_end().to_string();
        TextCalls::Found { calls, clean }
    }
}

struct BlockCall {
    name: String,
    args: serde_json::Value,
}

enum Block {
    Ok(Vec<BlockCall>),
    Bad(String),
}

/// Parse one closed marker block. `kind` is 0 for the A family,
/// 1 for the P family.
fn parse_block(kind: usize, inner: &str) -> Block {
    let inner = inner.trim();
    if kind == 0 {
        let v: serde_json::Value = match serde_json::from_str(inner) {
            Ok(v) => v,
            Err(e) => return Block::Bad(format!("the A-family block holds no JSON. Error: {e}")),
        };
        let v = match v {
            serde_json::Value::Array(a) => a,
            other => vec![other],
        };
        let mut out: Vec<BlockCall> = Vec::new();
        for item in v {
            out.push(match call_from_json(&item) {
                Ok(c) => c,
                Err(m) => return Block::Bad(m),
            });
        }
        Block::Ok(out)
    } else {
        parse_pi_block(inner)
    }
}

/// Read name and arguments from one JSON call object.
fn call_from_json(v: &serde_json::Value) -> Result<BlockCall, String> {
    let obj = v
        .as_object()
        .ok_or_else(|| "the block item is not an object".to_string())?;
    let name = obj
        .get("name")
        .and_then(|n| n.as_str())
        .ok_or_else(|| "the block item has no tool name".to_string())?
        .to_string();
    let args_raw = obj
        .get("arguments")
        .cloned()
        .unwrap_or(serde_json::json!({}));
    let args = match args_raw {
        serde_json::Value::String(s) => serde_json::from_str(&s)
            .map_err(|e| format!("the arguments string is not JSON. Error: {e}"))?,
        other => other,
    };
    if !args.is_object() {
        return Err("the arguments value is not a JSON object".to_string());
    }
    Ok(BlockCall {
        name,
        args: serde_json::Value::Object(args.as_object().unwrap().clone()),
    })
}

/// Parse one P-family block: a bare name line plus parameter keys.
fn parse_pi_block(inner: &str) -> Block {
    let bytes = inner.as_bytes();
    let s = inner.trim_start_matches(char::from(0x0a));
    let sb = s.as_bytes();
    let base = inner.len() - s.len();
    let first_nl = match find_bytes(sb, &[0x0a], 0) {
        Some(n) => n,
        None => return Block::Bad("no tool name in the P-family block".to_string()),
    };
    let name = s[..first_nl].trim().to_string();
    if name.is_empty() {
        return Block::Bad("no tool name in the P-family block".to_string());
    }

    let mut args = serde_json::Map::new();
    let mut rest = base + first_nl + 1;
    while let Some(x) = find_bytes(&bytes[rest..], PAR_OPEN, 0) {
        let p = rest + x;
        let key_start = p + PAR_OPEN.len();
        let key_end = match find_bytes(&bytes[key_start..], &[0x0a], 0) {
            Some(n) => key_start + n,
            None => return Block::Bad("an unterminated parameter line".to_string()),
        };
        let key = inner[key_start..key_end].trim().to_string();
        if key.is_empty() {
            return Block::Bad("an empty parameter key".to_string());
        }
        let val_start = key_end + 1;
        let close = match find_bytes(&bytes[val_start..], PAR_CLOSE, 0) {
            Some(c) => val_start + c,
            None => return Block::Bad("a missing parameter close marker".to_string()),
        };
        let val = inner[val_start..close].trim().to_string();
        args.insert(key, parse_param_value(&val));
        rest = close + PAR_CLOSE.len();
    }

    if args.is_empty() {
        return Block::Ok(vec![BlockCall {
            name,
            args: serde_json::Value::Object(serde_json::Map::new()),
        }]);
    }

    // A single JSON arguments parameter carries the whole object.
    if args.len() == 1 {
        if let Some(v) = args.get("arguments") {
            if let Some(obj) = v.as_object() {
                return Block::Ok(vec![BlockCall {
                    name,
                    args: serde_json::Value::Object(obj.clone()),
                }]);
            }
        }
    }
    Block::Ok(vec![BlockCall {
        name,
        args: serde_json::Value::Object(args),
    }])
}

/// Parse one parameter value. JSON objects and arrays keep their type.
/// Everything else stays a string.
fn parse_param_value(v: &str) -> serde_json::Value {
    let v = v.trim();
    if v.starts_with('{') || v.starts_with('[') || v == "true" || v == "false" || v == "null" {
        if let Ok(x) = serde_json::from_str(v) {
            return x;
        }
    }
    serde_json::Value::String(v.to_string())
}

fn chrono_utc_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_when_plain_text() {
        let text = "done, nothing to do";
        assert!(matches!(extract_text_tool_calls(text), TextCalls::Absent));
    }

    #[test]
    fn found_antml_json_array() {
        let text = "\n<invoke>\n[\n  {\"name\": \"bash\", \"arguments\": {\"command\": \"ls\"}},\n  {\"name\": \"read\", \"arguments\": \"{\\\"file_path\\\": \\\"a.txt\\\"}\"}\n]\n<invoke>";
        match extract_text_tool_calls(text) {
            TextCalls::Found { calls, clean } => {
                assert_eq!(calls.len(), 2);
                assert_eq!(calls[0]["name"], "bash");
                assert_eq!(calls[0]["arguments"]["command"], "ls");
                assert_eq!(calls[1]["arguments"]["file_path"], "a.txt");
                assert!(!clean.contains("\n\n"));
            }
            other => panic!("expected found calls, got {other:?}"),
        }
    }

    #[test]
    fn found_pi_xml_block() {
        let text = "\n<invoke\nbash\n<parametercommand\necho hi\n</parameter\n</invoke";
        match extract_text_tool_calls(text) {
            TextCalls::Found { calls, .. } => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0]["name"], "bash");
                assert_eq!(calls[0]["id"], "call_txt_0");
                assert_eq!(calls[0]["arguments"]["command"], "echo hi");
            }
            other => panic!("expected found calls, got {other:?}"),
        }
    }

    #[test]
    fn found_pi_xml_json_args_parameter() {
        let text = "\n<invoke\nread\n<parameterarguments\n{\"file_path\": \"a.txt\"}\n</parameter\n</invoke";
        match extract_text_tool_calls(text) {
            TextCalls::Found { calls, .. } => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0]["arguments"]["file_path"], "a.txt");
            }
            other => panic!("expected found calls, got {other:?}"),
        }
    }

    #[test]
    fn ids_are_sequential() {
        let text = "\n<invoke\nbash\n<parametercommand\necho one\n</parameter\n</invoke\n\n<invoke\nbash\n<parametercommand\necho two\n</parameter\n</invoke";
        match extract_text_tool_calls(text) {
            TextCalls::Found { calls, .. } => {
                assert_eq!(calls.len(), 2);
                assert_eq!(calls[0]["id"], "call_txt_0");
                assert_eq!(calls[1]["id"], "call_txt_1");
            }
            other => panic!("expected found calls, got {other:?}"),
        }
    }

    #[test]
    fn unterminated_marker_is_prose() {
        let text = "the model mentioned \n<invoke> in prose";
        assert!(matches!(extract_text_tool_calls(text), TextCalls::Absent));
    }

    #[test]
    fn bad_antml_inner_reports_error() {
        let text = "\n<invoke>\nnot json\n\n<invoke>";
        assert!(matches!(extract_text_tool_calls(text), TextCalls::Bad(_)));
    }
}
