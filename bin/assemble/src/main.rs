#![deny(clippy::todo, clippy::unimplemented, clippy::unreachable)]

use clap::Parser;
use std::fs;
use std::path::PathBuf;

/// Project session log to ModelRequest
#[derive(Parser)]
#[command(name = "assemble", about = "Project the session log into a ModelRequest")]
struct Args {
    /// Session directory path
    #[arg(long)]
    session: String,

    /// Path to config file
    #[arg(long, default_value = "config.toml")]
    config: String,
}

struct ModelSettings {
    model_id: String,
    max_output_tokens: u64,
    context_tokens: usize,
    chars_per_token: usize,
}

/// One log event, projected to model input items.
enum Ev {
    User { text: String },
    /// `reasoning` holds the server's own items from this turn, kept
    /// verbatim. They sit in the request after the turn's user
    /// message and before the turn's function_call items.
    Assistant {
        text: String,
        calls: Vec<Call>,
        reasoning: Vec<serde_json::Value>,
    },
    ToolResult { id: String, text: String },
}

struct Call {
    id: String,
    name: String,
    args_str: String,
}

/// Truncation caps for the compact form of old events.
#[derive(Clone, Copy, PartialEq)]
struct Caps {
    result: usize,
    text: usize,
}

/// Resolve the active model name from the MODEL env var or config.
fn resolve_active_model(config: &toml::Value) -> String {
    std::env::var("MODEL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            config
                .get("active")
                .and_then(|a| a.get("model"))
                .and_then(|m| m.as_str())
                .unwrap_or("deepseek")
                .to_string()
        })
}

fn val_str(v: &toml::Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(|s| s.to_string())
}

fn val_int(v: &toml::Value, key: &str) -> Option<i64> {
    v.get(key).and_then(|x| x.as_integer())
}

/// Resolve settings for a named model. Per-model values override defaults.
fn resolve_model_settings(config: &toml::Value, name: &str) -> ModelSettings {
    let empty = toml::Value::Table(toml::map::Map::new());
    let model_root = config.get("model").unwrap_or(&empty);
    let mdl = model_root.get(name).unwrap_or(&empty);
    ModelSettings {
        model_id: val_str(mdl, "model_id").unwrap_or_else(|| name.to_string()),
        max_output_tokens: val_int(mdl, "max_output_tokens")
            .or_else(|| val_int(model_root, "max_output_tokens"))
            .unwrap_or(4096) as u64,
        context_tokens: val_int(mdl, "context_tokens").unwrap_or(131072) as usize,
        chars_per_token: val_int(model_root, "chars_per_token").unwrap_or(4) as usize,
    }
}

/// Cut a string to at most `limit` chars and add a compact marker.
fn trim_chars(s: &str, limit: usize) -> String {
    let total = s.chars().count();
    if total <= limit {
        return s.to_string();
    }
    let kept: String = s.chars().take(limit).collect();
    let kept_n = kept.chars().count();
    format!("{kept}\n[compacted: {total} -> {kept_n} chars]")
}

/// Apply the long-standing tool result clip. Keeps the old marker text.
fn clip_full(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        return s.to_string();
    }
    // Back up to a char boundary so we never split a multi-byte character.
    let idx = s.floor_char_boundary(limit);
    let clipped = &s[..idx];
    format!(
        "{clipped}\n[tool result clipped: {} -> {} chars]",
        s.len(),
        clipped.len()
    )
}

/// Model input items for one event, full form.
fn full_items(ev: &Ev, clip_chars: usize) -> Vec<serde_json::Value> {
    match ev {
        Ev::User { text } => vec![serde_json::json!({
            "type": "message",
            "role": "user",
            "content": text
        })],
        Ev::Assistant { text, calls, reasoning } => {
            let mut items: Vec<serde_json::Value> = Vec::new();
            // The reasoning items are the model's own thinking. Send
            // them verbatim, pi-style: no cap, no trim, no slice.
            for r in reasoning {
                items.push(with_reasoning_type(r.clone()));
            }
            items.push(serde_json::json!({
                "type": "message",
                "role": "assistant",
                "content": text
            }));
            for c in calls {
                items.push(serde_json::json!({
                    "type": "function_call",
                    "call_id": c.id,
                    "name": c.name,
                    "arguments": c.args_str
                }));
            }
            items
        }
        Ev::ToolResult { id, text } => vec![serde_json::json!({
            "type": "function_call_output",
            "call_id": id,
            "output": clip_full(text, clip_chars)
        })],
    }
}

/// A reasoning item must carry its type. Insert the key when a
/// logged item lacks it, and send every other key verbatim.
fn with_reasoning_type(item: serde_json::Value) -> serde_json::Value {
    if item.get("type").and_then(|t| t.as_str()) == Some("reasoning") {
        return item;
    }
    let mut obj = item.as_object().cloned().unwrap_or_default();
    obj.insert("type".to_string(), serde_json::json!("reasoning"));
    serde_json::Value::Object(obj)
}

/// Model input items for one event, compact form.
///
/// User text stays full. The task statement must survive.
/// Old assistant text and tool results shrink to the caps.
/// Reasoning items do not shrink: a half-trimmed thinking item is
/// worse than none. The compact form drops them. A summary call
/// folds the old thinking back in later (handoff work item B).
fn compact_items(ev: &Ev, caps: &Caps) -> Vec<serde_json::Value> {
    match ev {
        Ev::User { .. } => full_items(ev, 0),
        Ev::Assistant { text, calls, .. } => {
            let mut items = vec![serde_json::json!({
                "type": "message",
                "role": "assistant",
                "content": trim_chars(text, caps.text)
            })];
            for c in calls {
                items.push(serde_json::json!({
                    "type": "function_call",
                    "call_id": c.id,
                    "name": c.name,
                    "arguments": trim_chars(&c.args_str, caps.text)
                }));
            }
            items
        }
        Ev::ToolResult { id, text } => vec![serde_json::json!({
            "type": "function_call_output",
            "call_id": id,
            "output": trim_chars(text, caps.result)
        })],
    }
}

/// Build the input item list.
///
/// The last `keep` events stay full. Older events use the compact form.
/// A `keep` at or above the event count keeps everything full.
fn build_items(events: &[&Ev], keep: usize, caps: &Caps, clip_chars: usize) -> Vec<serde_json::Value> {
    let split = events.len().saturating_sub(keep);
    events
        .iter()
        .enumerate()
        .flat_map(|(i, ev)| {
            if i < split {
                compact_items(ev, caps)
            } else {
                full_items(ev, clip_chars)
            }
        })
        .collect()
}

/// Keep-window search order. Halve down to a floor of two.
fn next_keeps(k0: usize) -> Vec<usize> {
    let mut out = vec![k0.max(1)];
    let mut k = out[0];
    while k > 2 {
        k /= 2;
        out.push(k.max(2));
    }
    out
}

/// The caps value the halving loop ends at: halve until a further
/// halving stays at the floor.
fn floor_of(base: usize, min: usize) -> usize {
    let mut v = base;
    loop {
        let next = (v / 2).max(min);
        if next == v {
            return v;
        }
        v = next;
    }
}

/// Step groups over the projected events, in log order. A step group
/// is one assistant message, its tool calls, and the results that
/// follow it. User messages are standalone groups. A leading result,
/// before the first assistant message, is a standalone group.
fn step_groups(events: &[&Ev]) -> Vec<(usize, usize)> {
    let mut groups = Vec::new();
    let mut i = 0;
    while i < events.len() {
        match events[i] {
            Ev::Assistant { .. } => {
                let start = i;
                i += 1;
                while i < events.len() {
                    match events[i] {
                        Ev::ToolResult { .. } => {
                            i += 1;
                        }
                        _ => break,
                    }
                }
                groups.push((start, i));
            }
            _ => {
                groups.push((i, i + 1));
                i += 1;
            }
        }
    }
    groups
}

/// Auto-compact search. Returns the input items of the first request
/// that fits the budget, or `None` when even the floor caps with the
/// oldest step groups dropped do not fit.
///
/// Search order: the full log. Then halving caps from `base` down to
/// the `min` floor, at each caps the keep windows from `keep_events`
/// down to two. Then, at the floor caps and the smallest keep window,
/// dropping the oldest step groups one at a time. User messages are
/// never dropped: the task statement must survive. The keep window is
/// never dropped: it is the recent context.
fn compact_search(
    events: &[&Ev],
    budget: usize,
    base: Caps,
    min: Caps,
    keep_events: usize,
    clip_chars: usize,
    req_chars: impl Fn(&Vec<serde_json::Value>) -> usize,
) -> Option<Vec<serde_json::Value>> {
    let full = build_items(events, events.len(), &Caps { result: 0, text: 0 }, clip_chars);
    if req_chars(&full) <= budget {
        return Some(full);
    }

    let keeps = next_keeps(keep_events);
    let mut caps = base;
    loop {
        for keep in &keeps {
            let items = build_items(events, *keep, &caps, clip_chars);
            if req_chars(&items) <= budget {
                return Some(items);
            }
        }
        let next = Caps {
            result: (caps.result / 2).max(min.result),
            text: (caps.text / 2).max(min.text),
        };
        // A no-op halving means the floor is reached.
        if next == caps {
            break;
        }
        caps = next;
    }

    // Last resort: the floor caps still do not fit. Drop the oldest
    // step groups, one at a time, until the request fits.
    let caps = Caps {
        result: floor_of(base.result, min.result),
        text: floor_of(base.text, min.text),
    };
    let keep = *next_keeps(keep_events).last().unwrap_or(&1);
    let groups = step_groups(events);
    let mut keep_idx: Vec<usize> = (0..events.len()).collect();
    for &(start, end) in &groups {
        // User groups are never dropped. Groups that touch the keep
        // tail are never dropped: the tail is the recent context.
        if !matches!(events[start], Ev::Assistant { .. }) {
            continue;
        }
        let tail_start = events.len().saturating_sub(keep);
        if start >= tail_start || end > tail_start {
            break;
        }
        keep_idx.retain(|i| !(start..end).contains(i));
        let sel: Vec<&Ev> = keep_idx.iter().map(|i| events[*i]).collect();
        let items = build_items(&sel, keep, &caps, clip_chars);
        if req_chars(&items) <= budget {
            return Some(items);
        }
    }
    None
}

fn main() {
    let args = Args::parse();

    let config_path = &args.config;

    // Read config
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

    let mut system_prompt = config
        .get("system_prompt")
        .and_then(|p| p.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();

    // Inject the session working directory, recorded at the entry point.
    let cwd_file = PathBuf::from(&args.session).join("cwd");
    if let Ok(cwd) = fs::read_to_string(&cwd_file) {
        let cwd = cwd.trim();
        if !cwd.is_empty() {
            system_prompt.push_str(&format!(
                "\n\nCurrent working directory: {cwd}\n\
                 Relative paths in tool calls resolve against this directory."
            ));
        }
    }

    let limits = config.get("limits").cloned().unwrap_or(toml::Value::String(String::new()));
    let limits = if limits.is_table() {
        limits
    } else {
        toml::Value::Table(toml::map::Map::new())
    };
    let tool_result_max_chars: usize = val_int(&limits, "tool_result_max_chars").unwrap_or(20000) as usize;
    let compact_keep_events: usize = val_int(&limits, "compact_keep_events").unwrap_or(24) as usize;
    let compact_result_chars: usize = val_int(&limits, "compact_result_chars").unwrap_or(500) as usize;
    let compact_text_chars: usize = val_int(&limits, "compact_text_chars").unwrap_or(200) as usize;
    let compact_min_result_chars: usize =
        val_int(&limits, "compact_min_result_chars").unwrap_or(128) as usize;
    let compact_min_text_chars: usize = val_int(&limits, "compact_min_text_chars").unwrap_or(64) as usize;

    let tools_root = config
        .get("paths")
        .and_then(|p| p.get("tools_root"))
        .and_then(|t| t.as_str())
        .unwrap_or("tools");

    // Resolve the active model and derive the context budget from its window.
    let active_model = resolve_active_model(&config);
    let model_settings = resolve_model_settings(&config, &active_model);
    let derived_budget = model_settings
        .context_tokens
        .saturating_sub(model_settings.max_output_tokens as usize)
        .saturating_mul(model_settings.chars_per_token);
    let explicit_budget: Option<usize> =
        val_int(&limits, "context_budget_chars").map(|v| v as usize);
    let context_budget_chars = match explicit_budget {
        Some(e) => e.min(derived_budget),
        None => derived_budget,
    };

    // Read events
    let log_path = PathBuf::from(&args.session).join("events.jsonl");
    let lines = match fs::read_to_string(&log_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: cannot read log: {e}");
            std::process::exit(1);
        }
    };

    // Parse events into projections
    let mut events: Vec<Ev> = Vec::new();
    for line in lines.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let event: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let event_type = event.get("type").and_then(|t| t.as_str()).unwrap_or("");

        match event_type {
            "user_message" => {
                let text = event
                    .get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string();
                events.push(Ev::User { text });
            }
            "assistant_message" => {
                let text = event
                    .get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string();
                let mut calls: Vec<Call> = Vec::new();
                if let Some(tc_arr) = event.get("tool_calls").and_then(|t| t.as_array()) {
                    for tc in tc_arr {
                        let id = tc.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
                        let name = tc.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                        // The log stores arguments as a JSON object.
                        // The Responses API needs them as a JSON string.
                        let args_val = tc.get("arguments").cloned().unwrap_or(serde_json::json!({}));
                        let args_str = serde_json::to_string(&args_val).unwrap_or_else(|_| "{}".into());
                        calls.push(Call { id, name, args_str });
                    }
                }
                // Reasoning items from this turn. Keep objects only:
                // a malformed entry drops, the rest ride on.
                let reasoning = event
                    .get("reasoning")
                    .and_then(|r| r.as_array())
                    .map(|items| {
                        items
                            .iter()
                            .filter(|i| i.is_object())
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                events.push(Ev::Assistant { text, calls, reasoning });
            }
            "tool_result" => {
                let id = event.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
                let text = event
                    .get("value")
                    .and_then(|v| v.get("text"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                events.push(Ev::ToolResult { id, text });
            }
            // error events are terminal. Skip them, as before.
            "error" => {}
            _ => {}
        }
    }

    // Load tool schemas
    let tools_root_path = PathBuf::from(&tools_root);
    let mut tool_schemas: Vec<serde_json::Value> = Vec::new();

    if let Ok(entries) = fs::read_dir(&tools_root_path) {
        let mut tool_names: Vec<String> = Vec::new();
        for entry in entries.flatten() {
            let tool_path = entry.path();
            if tool_path.is_dir() {
                let tool_toml = tool_path.join("tool.toml");
                if tool_toml.exists() {
                    if let Some(name) = tool_path.file_name() {
                        tool_names.push(name.to_string_lossy().to_string());
                    }
                }
            }
        }
        tool_names.sort();

        for name in &tool_names {
            let tool_toml = tools_root_path.join(name).join("tool.toml");
            let Ok(content) = fs::read_to_string(&tool_toml) else {
                continue;
            };
            let Ok(tool_config) = content.parse::<toml::Value>() else {
                continue;
            };
            let Some(tool_def) = tool_config.get("tool") else {
                continue;
            };
            let desc = tool_def
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .to_string();
            let params = match tool_def.get("schema") {
                Some(p) => toml_to_json(p),
                None => serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "required": []
                }),
            };

            tool_schemas.push(serde_json::json!({
                "type": "function",
                "name": name,
                "description": desc,
                "parameters": params
            }));
        }
    }

    let make_request = |items: &Vec<serde_json::Value>| {
        serde_json::json!({
            "model": model_settings.model_id,
            "instructions": system_prompt,
            "input": items,
            "tools": tool_schemas
        })
    };

    // Auto-compact: keep the session alive when context grows.
    // First try the full log. Then compact old events with shrinking
    // caps and keep windows, down to the configured floor. Then drop
    // the oldest step groups. End the session only when nothing fits.
    let req_chars = |items: &Vec<serde_json::Value>| {
        let r = make_request(items);
        serde_json::to_string(&r).unwrap().len()
    };

    let chosen: Option<Vec<serde_json::Value>> = compact_search(
        &events.iter().collect::<Vec<_>>(),
        context_budget_chars,
        Caps {
            result: compact_result_chars,
            text: compact_text_chars,
        },
        Caps {
            result: compact_min_result_chars,
            text: compact_min_text_chars,
        },
        compact_keep_events,
        tool_result_max_chars,
        req_chars,
    );

    match chosen {
        Some(items) => println!("{}", make_request(&items)),
        None => {
            let ts = chrono_utc_now();
            let error_event = serde_json::json!({
                "v": 1,
                "type": "error",
                "ts": ts,
                "message": "Context budget exceeded after compaction. Start a new session or reduce scope."
            });
            println!("{}", error_event);
        }
    }
}

fn chrono_utc_now() -> String {
    chrono::Utc::now()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn toml_to_json(val: &toml::Value) -> serde_json::Value {
    match val {
        toml::Value::String(s) => serde_json::json!(s),
        toml::Value::Integer(i) => serde_json::json!(*i),
        toml::Value::Float(f) => serde_json::json!(*f),
        toml::Value::Boolean(b) => serde_json::json!(*b),
        toml::Value::Array(arr) => serde_json::json!(arr.iter().map(toml_to_json).collect::<Vec<_>>()),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ev_res(id: &str, text: &str) -> Ev {
        Ev::ToolResult {
            id: id.to_string(),
            text: text.to_string(),
        }
    }

    fn caps() -> Caps {
        Caps {
            result: 50,
            text: 20,
        }
    }

    #[test]
    fn trim_caps_and_marks() {
        let out = trim_chars("abcdefghij", 3);
        assert!(out.starts_with("abc"));
        assert!(out.contains("[compacted: 10 -> 3 chars]"));
        assert_eq!(trim_chars("abc", 5), "abc");
    }

    #[test]
    fn clip_full_keeps_old_marker() {
        let out = clip_full(&"x".repeat(30), 10);
        assert!(out.starts_with("xxxxxxxxxx"));
        assert!(out.contains("[tool result clipped: 30 -> 10 chars]"));
    }

    #[test]
    fn clip_full_never_splits_a_multibyte_char() {
        // 7000 box-drawing chars = 21000 bytes. The 20000-byte cut lands
        // inside a 3-byte char, so the slice must back up to a boundary.
        let s = "\u{2500}".repeat(7000);
        let out = clip_full(&s, 20000);
        let kept = out.lines().next().unwrap();
        assert_eq!(kept.chars().count(), 6666);
        assert!(out.contains("[tool result clipped: 21000 -> 19998 chars]"));
    }

    #[test]
    fn keep_all_when_window_covers_log() {
        let events = vec![ev_res("a", "one"), ev_res("b", "two")];
        let refs: Vec<&Ev> = events.iter().collect();
        let items = build_items(&refs, 10, &caps(), 20000);
        // No compact marker, no clip marker, plain outputs.
        assert!(!items.iter().any(|i| i.to_string().contains("compacted")));
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn compact_marks_old_keeps_recent_full() {
        let mut events = Vec::new();
        for i in 0..30 {
            events.push(ev_res(&format!("id{i}"), &"R".repeat(400)));
        }
        let refs: Vec<&Ev> = events.iter().collect();
        let items = build_items(&refs, 4, &Caps { result: 50, text: 20 }, 20000);
        let first = &items[0];
        let last = items.last().unwrap();
        assert!(first.to_string().contains("[compacted: 400 -> 50 chars]"));
        assert!(!last.to_string().contains("compacted"));
        assert_eq!(items.len(), 30);
    }

    #[test]
    fn compact_keeps_user_text_full() {
        let events = vec![
            Ev::User {
                text: "do the task".to_string(),
            },
            ev_res("a", &"R".repeat(400)),
        ];
        let refs: Vec<&Ev> = events.iter().collect();
        let items = build_items(&refs, 0, &caps(), 20000);
        assert!(items[0].to_string().contains("do the task"));
        assert!(items[1].to_string().contains("compacted"));
    }

    #[test]
    fn next_keeps_halves_to_floor() {
        assert_eq!(next_keeps(24), vec![24, 12, 6, 3, 2]);
        assert_eq!(next_keeps(8), vec![8, 4, 2]);
        assert_eq!(next_keeps(2), vec![2]);
    }

    #[test]
    fn assistant_compact_shrinks_text_and_args() {
        let ev = Ev::Assistant {
            text: "A".repeat(300),
            calls: vec![Call {
                id: "c1".to_string(),
                name: "bash".to_string(),
                args_str: format!("{{\"command\": \"{}\"}}", "B".repeat(300)),
            }],
            reasoning: vec![],
        };
        let items = compact_items(&ev, &Caps { result: 50, text: 20 });
        let s = items.iter().map(|i| i.to_string()).collect::<String>();
        assert!(s.contains("[compacted: 300 -> 20 chars]"));
    }

    /// The full form sends the reasoning item verbatim, placed after
    /// the user message and before the function_call items.
    #[test]
    fn full_items_send_reasoning_item_verbatim() {
        let items = full_items(
            &Ev::Assistant {
                text: "doing it".to_string(),
                calls: vec![Call {
                    id: "c1".to_string(),
                    name: "bash".to_string(),
                    args_str: "{\"command\": \"ls\"}".to_string(),
                }],
                reasoning: vec![serde_json::json!({
                    "type": "reasoning",
                    "id": "rs_1",
                    "status": "completed",
                    "content": [{ "type": "reasoning_text", "text": "plan" }],
                    "summary": [],
                    "encrypted_content": "enc-9"
                })],
            },
            20000,
        );
        // Order: reasoning, assistant message, function_call.
        assert_eq!(items.len(), 3);
        assert_eq!(items[0]["type"], "reasoning");
        assert_eq!(items[0]["id"], "rs_1");
        assert_eq!(items[0]["encrypted_content"], "enc-9");
        assert_eq!(items[0]["content"][0]["text"], "plan");
        assert_eq!(items[1]["type"], "message");
        assert_eq!(items[1]["role"], "assistant");
        assert_eq!(items[2]["type"], "function_call");
    }

    /// A logged item without a type key still carries the type in the
    /// request; every other key stays verbatim.
    #[test]
    fn with_reasoning_type_inserts_missing_type() {
        let item = serde_json::json!({
            "id": "rs_2",
            "content": [{ "type": "reasoning_text", "text": "x" }]
        });
        let out = with_reasoning_type(item);
        assert_eq!(out["type"], "reasoning");
        assert_eq!(out["id"], "rs_2");
    }

    /// The compact form drops reasoning items. They never shrink to
    /// a slice: a half-trimmed thinking item is worse than none.
    #[test]
    fn compact_items_drop_reasoning() {
        let items = compact_items(
            &Ev::Assistant {
                text: "A".to_string(),
                calls: vec![Call {
                    id: "c1".to_string(),
                    name: "bash".to_string(),
                    args_str: "{}".to_string(),
                }],
                reasoning: vec![serde_json::json!({
                    "type": "reasoning",
                    "id": "rs_3",
                    "content": [{ "type": "reasoning_text", "text": "plan" }],
                    "summary": [],
                    "encrypted_content": null
                })],
            },
            &Caps { result: 50, text: 20 },
        );
        assert_eq!(items.len(), 2, "compact form keeps message and call only");
        assert!(!items.iter().any(|i| i.get("type").and_then(|t| t.as_str()) == Some("reasoning")));
    }

    /// A session whose reasoning items outgrow the budget still fits
    /// after compaction. The compacted events drop the items. The
    /// full tail keeps them. The task statement survives.
    #[test]
    fn compact_search_drops_reasoning_from_compacted_events() {
        let big_thinking = "t".repeat(5000);
        let mut events = vec![Ev::User {
            text: "the task".to_string(),
        }];
        for i in 0..5 {
            events.push(Ev::Assistant {
                text: "x".repeat(100),
                calls: vec![Call {
                    id: format!("c{i}"),
                    name: "bash".to_string(),
                    args_str: "{}".to_string(),
                }],
                reasoning: vec![serde_json::json!({
                    "type": "reasoning",
                    "id": format!("rs_{i}"),
                    "status": "completed",
                    "content": [{ "type": "reasoning_text", "text": big_thinking }],
                    "summary": [],
                    "encrypted_content": null
                })],
            });
        }
        let refs: Vec<&Ev> = events.iter().collect();
        let req_chars = |items: &Vec<serde_json::Value>| {
            serde_json::to_string(items).unwrap().len()
        };
        let full = build_items(&refs, refs.len(), &Caps { result: 0, text: 0 }, 20000);
        assert!(req_chars(&full) > 12000, "the full form must outgrow the budget");
        let out = compact_search(
            &refs,
            12000,
            Caps { result: 8000, text: 2000 },
            Caps { result: 128, text: 64 },
            24,
            20000,
            &req_chars,
        )
        .expect("the compacted request must fit");
        assert!(req_chars(&out) <= 12000);
        let s = serde_json::to_string(&out).unwrap();
        assert!(s.contains("the task"), "the task statement must survive");
        // The compacted events drop their items. The full tail keeps
        // its items.
        assert!(!s.contains("rs_0"), "compacted turn 0 must drop its item");
        assert!(!s.contains("rs_1"), "compacted turn 1 must drop its item");
        assert!(s.contains("rs_3"), "the full tail keeps its items");
        assert!(s.contains("rs_4"), "the full tail keeps its items");
    }

    #[test]
    fn floor_of_halves_to_the_floor() {
        assert_eq!(floor_of(8000, 128), 128);
        assert_eq!(floor_of(100, 128), 128);
        assert_eq!(floor_of(50, 5), 5);
        assert_eq!(floor_of(8, 0), 0);
        assert_eq!(floor_of(4, 4), 4);
    }

    #[test]
    fn step_groups_split_into_steps() {
        let evs = vec![
            Ev::User { text: "task".into() },
            ev_res("1", "r1"),
            Ev::Assistant { text: "a".into(), calls: vec![], reasoning: vec![] },
            ev_res("2", "r2"),
            ev_res("3", "r3"),
            Ev::User { text: "more".into() },
            Ev::Assistant { text: "b".into(), calls: vec![], reasoning: vec![] },
        ];
        let refs: Vec<&Ev> = evs.iter().collect();
        assert_eq!(
            step_groups(&refs),
            vec![(0, 1), (1, 2), (2, 5), (5, 6), (6, 7)]
        );
    }

    /// A session that outgrows every compact stage still fits after
    /// the oldest step groups are dropped. The task statement and the
    /// keep tail survive.
    #[test]
    fn compact_search_drops_oldest_steps_when_no_caps_fit() {
        let mut events = vec![Ev::User {
            text: "the task".to_string(),
        }];
        for i in 0..200 {
            events.push(Ev::Assistant {
                text: "x".repeat(300),
                calls: vec![Call {
                    id: format!("c{i}"),
                    name: "bash".to_string(),
                    args_str: format!("{{\"command\": \"{}\"}}", "y".repeat(300)),
                }],
                reasoning: vec![],
            });
            events.push(ev_res(&format!("r{i}"), &"R".repeat(5000)));
        }
        let refs: Vec<&Ev> = events.iter().collect();
        let req_chars = |items: &Vec<serde_json::Value>| {
            serde_json::to_string(items).unwrap().len()
        };
        // Budget smaller than any capped request, but large enough for
        // the task, the keep tail, and a handful of floored steps.
        let out = compact_search(
            &refs,
            40000,
            Caps { result: 8000, text: 2000 },
            Caps { result: 128, text: 64 },
            24,
            20000,
            &req_chars,
        )
        .expect("a dropped-oldest request must fit");
        let s = serde_json::to_string(&out).unwrap();
        assert!(s.contains("the task"), "the task statement must survive");
        assert!(s.contains("[compacted: 5000 -> 128 chars]"), "old results sit at the floor");
        // The dropped steps are gone, the recent ones remain.
        assert!(!s.contains("\"c0\""));
        assert!(s.contains(&format!("\"c199\"")));
    }

    #[test]
    fn compact_search_fits_without_dropping_when_caps_alone_fit() {
        let mut events = vec![Ev::User {
            text: "the task".to_string(),
        }];
        for i in 0..20 {
            events.push(Ev::Assistant {
                text: "x".repeat(100),
                calls: vec![Call {
                    id: format!("c{i}"),
                    name: "bash".to_string(),
                    args_str: "{}".to_string(),
                }],
                reasoning: vec![],
            });
            events.push(ev_res(&format!("r{i}"), &"R".repeat(5000)));
        }
        let refs: Vec<&Ev> = events.iter().collect();
        let req_chars = |items: &Vec<serde_json::Value>| {
            serde_json::to_string(items).unwrap().len()
        };
        let out = compact_search(
            &refs,
            100000,
            Caps { result: 8000, text: 2000 },
            Caps { result: 128, text: 64 },
            24,
            20000,
            &req_chars,
        )
        .expect("the capped request must fit");
        let s = serde_json::to_string(&out).unwrap();
        assert!(s.contains("\"c0\""), "no step is dropped when caps alone fit");
    }

    #[test]
    fn compact_search_returns_none_when_even_dropping_cannot_fit() {
        let mut events = vec![Ev::User {
            text: "the task".to_string(),
        }];
        for i in 0..20 {
            events.push(ev_res(&format!("r{i}"), &"R".repeat(5000)));
        }
        let refs: Vec<&Ev> = events.iter().collect();
        let req_chars = |items: &Vec<serde_json::Value>| {
            serde_json::to_string(items).unwrap().len()
        };
        // No assistant step to drop. The floored results still outgrow
        // the budget. The session must end.
        let out = compact_search(
            &refs,
            1000,
            Caps { result: 8000, text: 2000 },
            Caps { result: 128, text: 64 },
            24,
            20000,
            &req_chars,
        );
        assert!(out.is_none());
    }
}
