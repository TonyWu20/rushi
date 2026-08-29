#![deny(clippy::todo, clippy::unimplemented, clippy::unreachable)]

use clap::Parser;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

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
    /// `usage_input` is the measured `usage.input_tokens` of the
    /// request that produced this turn (work item B). It drives the
    /// token-based context budget.
    Assistant {
        text: String,
        calls: Vec<Call>,
        reasoning: Vec<serde_json::Value>,
        usage_input: Option<usize>,
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

/// The compact search parameters: the base caps, the floor caps, the
/// keep window, and the tool-result clip cap.
#[derive(Clone, Copy, PartialEq)]
struct SearchConfig {
    base: Caps,
    min: Caps,
    keep_events: usize,
    clip_chars: usize,
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

/// Read the per-session tool log into call id -> full display text
/// (docs/tool-log-design_from_human.md). The log holds the full
/// stdout, stderr, and exit status of each call, in order. The
/// `text` field is the unclipped display form the model receives. A
/// missing file is an empty map: legacy logs inline the body in the
/// event and need no tool log. Malformed lines drop out; the rest
/// ride on. Later records for a call id win: a re-routed call has
/// run more recently.
fn tool_log_texts(session_dir: &Path) -> HashMap<String, String> {
    let path = session_dir.join("tools.jsonl");
    let Ok(content) = fs::read_to_string(&path) else {
        return HashMap::new();
    };
    let mut map: HashMap<String, String> = HashMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let rec: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let id = rec.get("id").and_then(|i| i.as_str()).unwrap_or("");
        if id.is_empty() {
            continue;
        }
        let text = match rec.get("text").and_then(|t| t.as_str()) {
            Some(s) => s.to_string(),
            // A legacy record carries no display text. Rebuild it
            // from the raw bodies.
            None => match record_text(&rec) {
                Some(s) => s,
                None => continue,
            },
        };
        map.insert(id.to_string(), text);
    }
    map
}

/// One tool log record without a `text` field, in the legacy shape:
/// rebuild the display text from the raw bodies. A not-run call shows
/// its error message. A clean run shows stdout. An error exit shows
/// stderr when it is non-empty, else the exit code line.
fn record_text(rec: &serde_json::Value) -> Option<String> {
    if let Some(e) = rec.get("error").and_then(|v| v.as_str()) {
        return Some(e.to_string());
    }
    let exit = rec.get("exit").and_then(|v| v.as_i64());
    let stdout = rec.get("stdout").and_then(|v| v.as_str()).unwrap_or("");
    let stderr = rec.get("stderr").and_then(|v| v.as_str()).unwrap_or("");
    match exit {
        Some(0) => Some(stdout.to_string()),
        Some(c) => {
            if stderr.is_empty() {
                Some(format!("Tool exited with code {c}."))
            } else {
                Some(stderr.to_string())
            }
        }
        None => None,
    }
}

/// The schema-validation error result of the empty-arguments class
/// (FT-008). The result text starts with this prefix, intact: it is
/// short, so it always survives the slim index preview.
const SCHEMA_ERROR_PREFIX: &str = "Tool arguments failed schema validation";

fn is_schema_error_text(text: &str) -> bool {
    text.starts_with(SCHEMA_ERROR_PREFIX)
}

/// The call ids of old schema-validation failures: the result events
/// before the split point whose text is a schema-validation error.
///
/// The failed pair (the call plus its error result) self-priming
/// (FT-008): each old pair in the request pushes the next call
/// toward the same empty-arguments glitch. The compact form drops
/// the pair from the request. The event log and the tool log keep
/// it. Pairs inside the keep window stay: the model may still be
/// recovering from the failure.
fn schema_error_pair_ids(events: &[&Ev], split: usize) -> HashSet<String> {
    let mut ids: HashSet<String> = HashSet::new();
    for &ev in events.iter().take(split) {
        if let Ev::ToolResult { id, text } = ev {
            if is_schema_error_text(text) {
                ids.insert(id.clone());
            }
        }
    }
    ids
}

/// Model input items for one event, full form.
fn full_items(
    ev: &Ev,
    clip_chars: usize,
    drop_pairs: &HashSet<String>,
) -> Vec<serde_json::Value> {
    match ev {
        Ev::User { text } => vec![serde_json::json!({
            "type": "message",
            "role": "user",
            "content": text
        })],
        Ev::Assistant { text, calls, reasoning, .. } => {
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
                // A dropped old schema-error pair: the call goes out
                // with its result (FT-008 self-priming).
                if drop_pairs.contains(&c.id) {
                    continue;
                }
                items.push(serde_json::json!({
                    "type": "function_call",
                    "call_id": c.id,
                    "name": c.name,
                    "arguments": c.args_str
                }));
            }
            items
        }
        Ev::ToolResult { id, text } => {
            if drop_pairs.contains(id) {
                return Vec::new();
            }
            vec![serde_json::json!({
                "type": "function_call_output",
                "call_id": id,
                "output": clip_full(text, clip_chars)
            })]
        }
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
fn compact_items(
    ev: &Ev,
    caps: &Caps,
    drop_pairs: &HashSet<String>,
) -> Vec<serde_json::Value> {
    match ev {
        Ev::User { .. } => full_items(ev, 0, drop_pairs),
        Ev::Assistant { text, calls, .. } => {
            let mut items = vec![serde_json::json!({
                "type": "message",
                "role": "assistant",
                "content": trim_chars(text, caps.text)
            })];
            for c in calls {
                if drop_pairs.contains(&c.id) {
                    continue;
                }
                items.push(serde_json::json!({
                    "type": "function_call",
                    "call_id": c.id,
                    "name": c.name,
                    "arguments": trim_chars(&c.args_str, caps.text)
                }));
            }
            items
        }
        Ev::ToolResult { id, text } => {
            if drop_pairs.contains(id) {
                return Vec::new();
            }
            vec![serde_json::json!({
                "type": "function_call_output",
                "call_id": id,
                "output": trim_chars(text, caps.result)
            })]
        }
    }
}

/// Project events to items, returning the item list plus the start
/// offset of each event's items in it. The offset list has one entry
/// per event plus a final entry, so `offsets[i + 1]` is the first item
/// of the event after `i`, or the end of the list.
fn build_items_off(
    events: &[&Ev],
    keep: usize,
    caps: &Caps,
    clip_chars: usize,
    drop_pairs: &HashSet<String>,
) -> (Vec<serde_json::Value>, Vec<usize>) {
    let split = events.len().saturating_sub(keep);
    let mut items: Vec<serde_json::Value> = Vec::new();
    let mut offsets: Vec<usize> = Vec::with_capacity(events.len() + 1);
    for (i, ev) in events.iter().enumerate() {
        offsets.push(items.len());
        let ev_items = if i < split {
            compact_items(ev, caps, drop_pairs)
        } else {
            full_items(ev, clip_chars, drop_pairs)
        };
        items.extend(ev_items);
    }
    offsets.push(items.len());
    (items, offsets)
}

/// Build the input item list.
///
/// The last `keep` events stay full. Older events use the compact form.
/// A `keep` at or above the event count keeps everything full.
/// `drop_pairs` holds call ids of old schema-validation failures;
/// their pairs go out of the request (FT-008 self-priming).
fn build_items(
    events: &[&Ev],
    keep: usize,
    caps: &Caps,
    clip_chars: usize,
    drop_pairs: &HashSet<String>,
) -> Vec<serde_json::Value> {
    build_items_off(events, keep, caps, clip_chars, drop_pairs).0
}

/// The JSON char count of an item list. The char heuristic of the
/// request content, without the request wrapper fields.
fn items_chars(items: &[serde_json::Value]) -> usize {
    serde_json::to_string(items).unwrap().len()
}

fn div_ceil(a: usize, b: usize) -> usize {
    a.div_ceil(b)
}

/// Token estimate of the next full-log request, driven by the
/// measured `usage.input_tokens` of the last measured turn.
///
/// The estimate is the last measured input token count plus the
/// projected chars of the events appended after that measured turn,
/// converted with the chars-per-token fallback. The measurement
/// already counts the request wrapper (instructions, tools), so only
/// the appended events convert. A log without any measurement is a
/// pre-measurement fallback: the whole request's char count over the
/// chars-per-token fallback.
fn estimate_full_log_tokens(
    measurements: &[(usize, usize)],
    full_request_chars: usize,
    growth_chars: usize,
    chars_per_token: usize,
) -> usize {
    let c = chars_per_token.max(1);
    match measurements.last() {
        Some((_, m)) => *m + div_ceil(growth_chars, c),
        None => div_ceil(full_request_chars, c),
    }
}

/// Whether the full log fits the token budget this turn.
///
/// With a measurement, the measured estimate decides: the char-based
/// size of the request never sets the budget, it only converts the
/// appended growth. Without a measurement (fresh session, or a server
/// that reports no usage), the pre-measurement char fallback decides.
fn full_log_fits(
    measurements: &[(usize, usize)],
    full_request_chars: usize,
    growth_chars: usize,
    chars_per_token: usize,
    budget_tokens: usize,
) -> bool {
    estimate_full_log_tokens(measurements, full_request_chars, growth_chars, chars_per_token)
        <= budget_tokens
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
/// that fits the budget, or the last candidate tried (the floored
/// caps, the smallest keep window, and the oldest step groups
/// dropped) when nothing fits. The caller turns that last candidate
/// into the handoff summary request (correction 57).
///
/// The budget is in request chars: the token budget multiplied by the
/// chars-per-token fallback. The compacted content carries no token
/// measurement by construction, so the char heuristic estimates it
/// (work item B: the measured tokens drive the budget; the char
/// heuristic is the pre-measurement fallback only).
///
/// Search order: the full log (when `include_full`), then halving
/// caps from `base` down to the `min` floor, at each caps the keep
/// windows from `keep_events` down to two. Then, at the floor caps
/// and the smallest keep window, dropping the oldest step groups one
/// at a time. User messages are never dropped: the task statement
/// must survive. The keep window is never dropped: it is the recent
/// context.
fn compact_search(
    events: &[&Ev],
    budget: usize,
    cfg: SearchConfig,
    include_full: bool,
    req_chars: impl Fn(&Vec<serde_json::Value>) -> usize,
) -> Result<Vec<serde_json::Value>, Vec<serde_json::Value>> {
    let SearchConfig { base, min, keep_events, clip_chars } = cfg;
    let no_pairs: HashSet<String> = HashSet::new();
    if include_full {
        let full = build_items(events, events.len(), &Caps { result: 0, text: 0 }, clip_chars, &no_pairs);
        if req_chars(&full) <= budget {
            return Ok(full);
        }
    }

    let keeps = next_keeps(keep_events);
    let mut caps = base;
    loop {
        for keep in &keeps {
            // Old schema-error pairs go out of the compacted region;
            // the keep window still carries its pairs (FT-008).
            let drop = schema_error_pair_ids(events, events.len().saturating_sub(*keep));
            let items = build_items(events, *keep, &caps, clip_chars, &drop);
            if req_chars(&items) <= budget {
                return Ok(items);
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
    // The candidate the summary request is built from: the floored
    // caps at the smallest keep window, no drops yet. Old schema-error
    // pairs are dropped from the compacted region (FT-008).
    let drop0 = schema_error_pair_ids(events, events.len().saturating_sub(keep));
    let mut last: Vec<serde_json::Value> =
        build_items(events, keep, &caps, clip_chars, &drop0);
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
        let drop = schema_error_pair_ids(&sel, sel.len().saturating_sub(keep));
        let items = build_items(&sel, keep, &caps, clip_chars, &drop);
        if req_chars(&items) <= budget {
            return Ok(items);
        }
        last = items;
    }
    Err(last)
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
    // The output cap of the one handoff summary call (correction 57).
    let handoff_summary_max_tokens: u64 =
        val_int(&limits, "handoff_summary_max_tokens").unwrap_or(4096) as u64;

    let tools_root = config
        .get("paths")
        .and_then(|p| p.get("tools_root"))
        .and_then(|t| t.as_str())
        .unwrap_or("tools");

    // Resolve the active model. The context budget is in tokens
    // (work item B): the user knob is `context_budget_tokens`.
    // A legacy `context_budget_chars` converts through the
    // chars-per-token fallback. The default is the model window
    // minus the output reservation.
    let active_model = resolve_active_model(&config);
    let model_settings = resolve_model_settings(&config, &active_model);
    let cprt = model_settings.chars_per_token.max(1);
    let window_input_tokens = model_settings
        .context_tokens
        .saturating_sub(model_settings.max_output_tokens as usize);
    let budget_tokens = val_int(&limits, "context_budget_tokens")
        .map(|v| v.max(1) as usize)
        .or_else(|| {
            val_int(&limits, "context_budget_chars").map(|c| div_ceil(c as usize, cprt))
        })
        .unwrap_or(window_input_tokens)
        .min(window_input_tokens.max(1));
    // The char budget the compact candidates are checked against:
    // the token budget times the chars-per-token fallback. The
    // compacted content has no token measurement by construction;
    // the char heuristic estimates it (the pre-measurement fallback).
    let budget_chars = budget_tokens.saturating_mul(cprt);

    // Read events
    let log_path = PathBuf::from(&args.session).join("events.jsonl");
    let lines = match fs::read_to_string(&log_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: cannot read log: {e}");
            std::process::exit(1);
        }
    };

    // The per-session tool log holds the full tool bodies. The event
    // log's slim tool_result index points into it by call id
    // (docs/tool-log-design_from_human.md). Legacy logs have no tool
    // log: the index text stands in for the body.
    let tool_texts = tool_log_texts(&PathBuf::from(&args.session));

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
                // The measured input tokens of the request that
                // produced this turn (work item B). They drive the
                // token-based context budget.
                let usage_input = event
                    .get("usage")
                    .and_then(|u| u.get("input_tokens"))
                    .and_then(|t| t.as_u64())
                    .map(|n| n as usize);
                events.push(Ev::Assistant { text, calls, reasoning, usage_input });
            }
            "tool_result" => {
                let id = event.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
                let index_text = event
                    .get("value")
                    .and_then(|v| v.get("text"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                // The full body comes from the tool log when the
                // session has one; the index text is the legacy
                // inline body or the short preview.
                let text = tool_texts
                    .get(&id)
                    .cloned()
                    .unwrap_or(index_text);
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

    // Auto-compact, token-driven (work item B). The full log goes
    // out when the measured estimate fits the token budget: the last
    // measured `usage.input_tokens` plus the projected growth since.
    // With no measurement yet, the char fallback decides. Otherwise
    // the compact search shrinks old events; its candidates check
    // against the token budget converted to chars. When nothing
    // fits, the last candidate becomes the handoff summary request
    // and the `context_exhausted` event ends the turn (correction 57).
    let req_chars = |items: &Vec<serde_json::Value>| {
        let r = make_request(items);
        serde_json::to_string(&r).unwrap().len()
    };
    let events_refs: Vec<&Ev> = events.iter().collect();

    // The last measured input token count, with the event index it
    // belongs to. Growth after that event converts at the fallback.
    let measurements: Vec<(usize, usize)> = events_refs
        .iter()
        .enumerate()
        .filter_map(|(i, e)| match e {
            Ev::Assistant { usage_input: Some(t), .. } => Some((i, *t)),
            _ => None,
        })
        .collect();

    let (full_items, full_items_off) =
        build_items_off(&events_refs, events_refs.len(), &Caps { result: 0, text: 0 }, tool_result_max_chars, &HashSet::new());
    let full_request_chars = req_chars(&full_items);
    let growth_chars = match measurements.last() {
        Some((last, _)) => items_chars(&full_items[full_items_off[*last + 1]..]),
        None => 0,
    };

    // The measured decision sends the full log when it fits. When it
    // does not, the search skips its full candidate (the measured
    // estimate already ruled it out) and shrinks the caps. Without a
    // measurement, the char fallback decision is the same check the
    // search would run, so it too skips the full candidate here.
    if full_log_fits(&measurements, full_request_chars, growth_chars, cprt, budget_tokens) {
        println!("{}", make_request(&full_items));
    } else {
        match compact_search(
            &events_refs,
            budget_chars,
            SearchConfig {
                base: Caps {
                    result: compact_result_chars,
                    text: compact_text_chars,
                },
                min: Caps {
                    result: compact_min_result_chars,
                    text: compact_min_text_chars,
                },
                keep_events: compact_keep_events,
                clip_chars: tool_result_max_chars,
            },
            false,
            req_chars,
        ) {
            Ok(items) => println!("{}", make_request(&items)),
            Err(last_items) => {
                let exhausted = context_exhausted_event(
                    &last_items,
                    &model_settings.model_id,
                    handoff_summary_max_tokens,
                );
                println!("{}", exhausted);
            }
        }
    }
}

fn chrono_utc_now() -> String {
    chrono::Utc::now()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// The instructions of the handoff summary call. One cheap
/// summarization call on the compacted log seeds a new session with
/// the task, the state, and the next steps (correction 57).
const HANDOFF_INSTRUCTIONS: &str = "You are writing a handoff summary for a new agent session that will continue this task. The summary is the only context the new session receives about this session. Cover, in order: 1) the task and its success criteria; 2) the work completed so far and its results; 3) the current state: in-progress steps, open questions, known pitfalls; 4) the immediate next steps. Be concrete: name the files, commands, and decisions. Keep it under 3000 words.";

/// The user message that asks for the summary, appended after the
/// compacted log in the summary request.
const HANDOFF_ASK: &str = "Write the handoff summary now, using the conversation above.";

/// The summary request of the automatic handoff: the largest compact
/// candidate that the search tried, plus the summary ask, with a
/// cheap output cap. No tools: the summary is plain text.
fn handoff_summary_request(
    model: &str,
    items: &[serde_json::Value],
    max_tokens: u64,
) -> serde_json::Value {
    let mut input = items.to_vec();
    input.push(serde_json::json!({
        "type": "message",
        "role": "user",
        "content": HANDOFF_ASK
    }));
    serde_json::json!({
        "model": model,
        "instructions": HANDOFF_INSTRUCTIONS,
        "input": input,
        "tools": [],
        "max_output_tokens": max_tokens
    })
}

/// The `context_exhausted` event: the terminal case of the compact
/// search. It carries the summary request so the loop layer can run
/// the handoff (correction 57). The loop logs the event with the
/// seeded session name added; this builder holds no session name.
fn context_exhausted_event(
    items: &[serde_json::Value],
    model: &str,
    max_tokens: u64,
) -> serde_json::Value {
    serde_json::json!({
        "v": 1,
        "type": "context_exhausted",
        "ts": chrono_utc_now(),
        "message": "Context budget exhausted after compaction. Run the handoff: summarize this session, seed a new session with the summary, and continue the task there.",
        "summary_request": handoff_summary_request(model, items, max_tokens)
    })
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

    /// A schema-validation failure result, the FT-008 error shape.
    fn ev_schema_error(id: &str) -> Ev {
        ev_res(id, "Tool arguments failed schema validation: command.")
    }

    /// The tool log map is call id -> full display text. A missing
    /// file is an empty map: legacy logs need no tool log.
    #[test]
    fn tool_log_texts_single_record_repro() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("tools.jsonl");
        std::fs::write(
            &p,
            "{\"v\":1,\"ts\":\"t\",\"id\":\"a\",\"exit\":0,\"stdout\":\"body-a\",\"stderr\":\"\",\"text\":\"body-a\"}\n",
        )
        .unwrap();
        let map = tool_log_texts(dir.path());
        eprintln!("REPRO map: {map:?}");
        assert_eq!(map.get("a").map(|s| s.as_str()), Some("body-a"));
    }

    #[test]
    fn tool_log_texts_reads_the_session_tool_log() {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.path();
        // No file: empty map, no error.
        assert!(tool_log_texts(dir_path).is_empty());
        std::fs::write(
            dir_path.join("tools.jsonl"),
            r#"{"v":1,"ts":"t","id":"a","exit":0,"stdout":"body-a","stderr":"","text":"body-a"}
not json at all
{"v":1,"ts":"t","id":"b","exit":null,"stdout":"","stderr":"","is_error":true,"error":"Unknown tool nope."}
{"v":1,"ts":"t","id":"b","exit":1,"stdout":"","stderr":"boom","text":"boom"}
"#,
        )
        .unwrap();
        let map = tool_log_texts(dir_path);
        assert_eq!(map.get("a").map(|s| s.as_str()), Some("body-a"));
        // The later record for the same id wins: the re-run body.
        assert_eq!(map.get("b").map(|s| s.as_str()), Some("boom"));
        assert_eq!(map.len(), 2, "the malformed line drops out");
    }

    /// A record without a `text` field rebuilds the display text from
    /// the raw bodies.
    #[test]
    fn record_text_rebuilds_the_display_form() {
        let clean: serde_json::Value =
            serde_json::json!({"exit": 0, "stdout": "out", "stderr": ""});
        assert_eq!(record_text(&clean).as_deref(), Some("out"));
        let err: serde_json::Value =
            serde_json::json!({"exit": 3, "stdout": "", "stderr": "boom"});
        assert_eq!(record_text(&err).as_deref(), Some("boom"));
        let code_only: serde_json::Value =
            serde_json::json!({"exit": 3, "stdout": "", "stderr": ""});
        assert_eq!(record_text(&code_only).as_deref(), Some("Tool exited with code 3."));
        let not_run: serde_json::Value =
            serde_json::json!({"error": "Unknown tool nope."});
        assert_eq!(record_text(&not_run).as_deref(), Some("Unknown tool nope."));
        assert!(record_text(&serde_json::json!({})).is_none());
    }

    /// The schema-error text check is on the result text prefix.
    #[test]
    fn schema_error_text_check() {
        assert!(is_schema_error_text(
            "Tool arguments failed schema validation: command."
        ));
        assert!(!is_schema_error_text("plain tool output"));
    }

    /// The full form drops a failed pair when its id is in the set.
    /// The event log keeps the pair; only the request loses it.
    #[test]
    fn full_items_drop_a_schema_error_pair() {
        let calls = vec![
            Call {
                id: "ok".to_string(),
                name: "read".to_string(),
                args_str: "{}".to_string(),
            },
            Call {
                id: "bad".to_string(),
                name: "bash".to_string(),
                args_str: "{}".to_string(),
            },
        ];
        let ev = Ev::Assistant {
            text: "trying".to_string(),
            calls,
            reasoning: vec![],
            usage_input: None,
        };
        let mut drop: HashSet<String> = HashSet::new();
        drop.insert("bad".to_string());
        let items = full_items(&ev, 20000, &drop);
        // The dropped call is gone; its call id appears nowhere.
        let s = items.iter().map(|i| i.to_string()).collect::<String>();
        assert!(!s.contains("\"bad\""), "the failed call must be out: {s}");
        assert!(s.contains("\"ok\""), "the clean call stays: {s}");
        // Its result goes out too.
        let res = full_items(&ev_schema_error("bad"), 20000, &drop);
        assert!(res.is_empty(), "the failed result must be out");
    }

    /// Pairs inside the keep window are not dropped: the model may
    /// still be recovering from the failure.
    #[test]
    fn compact_items_keep_pairs_inside_the_keep_window() {
        let ev = Ev::Assistant {
            text: "a".to_string(),
            calls: vec![Call {
                id: "bad".to_string(),
                name: "bash".to_string(),
                args_str: "{}".to_string(),
            }],
            reasoning: vec![],
            usage_input: None,
        };
        // An empty drop set keeps the pair, even in the compact form.
        let items = compact_items(&ev, &Caps { result: 50, text: 20 }, &HashSet::new());
        let s = items.iter().map(|i| i.to_string()).collect::<String>();
        assert!(s.contains("\"bad\""), "the pair stays in the keep window: {s}");
    }

    /// The id set is the schema-error results before the split point
    /// only. Results at or after the split are out of scope.
    #[test]
    fn schema_error_pair_ids_cover_the_compacted_region() {
        let evs: Vec<Ev> = vec![
            ev_schema_error("r1"),
            ev_res("r2", "fine"),
            ev_schema_error("r3"),
        ];
        let refs: Vec<&Ev> = evs.iter().collect();
        // Split after two events: r1 is old, r3 sits in the keep window.
        let ids = schema_error_pair_ids(&refs, 2);
        assert!(ids.contains("r1"), "the old failure is in the set");
        assert!(!ids.contains("r3"), "the recent failure stays out of the set");
        // A clean result is never in the set.
        assert!(!ids.contains("r2"));
        // No split: nothing is compacted, nothing drops.
        assert!(schema_error_pair_ids(&refs, 0).is_empty());
    }

    /// A compact search drops old schema-error pairs out of the
    /// request while the keep window keeps its pair and the task
    /// statement survives.
    #[test]
    fn compact_search_drops_old_schema_error_pairs() {
        let mut events: Vec<Ev> = vec![Ev::User {
            text: "the task".to_string(),
        }];
        for i in 0..30 {
            events.push(Ev::Assistant {
                text: "x".to_string(),
                calls: vec![Call {
                    id: format!("c{i}"),
                    name: "bash".to_string(),
                    args_str: "{}".to_string(),
                }],
                reasoning: vec![],
                usage_input: None,
            });
            // Half the steps fail schema validation. They are the
            // self-priming pairs (FT-008).
            if i % 2 == 0 {
                events.push(ev_schema_error(&format!("r{i}")));
            } else {
                events.push(ev_res(&format!("r{i}"), &"R".repeat(3000)));
            }
        }
        let refs: Vec<&Ev> = events.iter().collect();
        let req_chars = |items: &Vec<serde_json::Value>| {
            serde_json::to_string(items).unwrap().len()
        };
        // A budget that fits only with caps: the full form misses it.
        let out = compact_search(
            &refs,
            100_000,
            SearchConfig {
                base: Caps { result: 8000, text: 2000 },
                min: Caps { result: 128, text: 64 },
                keep_events: 4,
                clip_chars: 20000,
            },
            false,
            &req_chars,
        )
        .expect("the compacted request must fit");
        let s = serde_json::to_string(&out).unwrap();
        assert!(s.contains("the task"), "the task statement must survive");
        // The keep tail keeps its failed pair: it is the recovery
        // context. The oldest step is step 0, an even index, so r0 is
        // the oldest failed pair.
        assert!(!s.contains("\"call_id\":\"r0\""), "the old failed pair must be out");
        let last_even = 28;
        assert!(
            s.contains(&format!("\"call_id\":\"r{last_even}\"")),
            "the keep tail keeps its failed pair"
        );
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
        let items = build_items(&refs, 10, &caps(), 20000, &HashSet::new());
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
        let items = build_items(&refs, 4, &Caps { result: 50, text: 20 }, 20000, &HashSet::new());
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
        let items = build_items(&refs, 0, &caps(), 20000, &HashSet::new());
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
            usage_input: None,
        };
        let items = compact_items(&ev, &Caps { result: 50, text: 20 }, &HashSet::new());
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
                usage_input: None,
            },
            20000,
            &HashSet::new(),
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
                usage_input: None,
            },
            &Caps { result: 50, text: 20 },
            &HashSet::new(),
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
                usage_input: None,
            });
        }
        let refs: Vec<&Ev> = events.iter().collect();
        let req_chars = |items: &Vec<serde_json::Value>| {
            serde_json::to_string(items).unwrap().len()
        };
        let full = build_items(&refs, refs.len(), &Caps { result: 0, text: 0 }, 20000, &HashSet::new());
        assert!(req_chars(&full) > 12000, "the full form must outgrow the budget");
        let out = compact_search(
            &refs,
            12000,
            SearchConfig {
                base: Caps { result: 8000, text: 2000 },
                min: Caps { result: 128, text: 64 },
                keep_events: 24,
                clip_chars: 20000,
            },
            true,
            req_chars,
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
            Ev::Assistant { text: "a".into(), calls: vec![], reasoning: vec![], usage_input: None },
            ev_res("2", "r2"),
            ev_res("3", "r3"),
            Ev::User { text: "more".into() },
            Ev::Assistant { text: "b".into(), calls: vec![], reasoning: vec![], usage_input: None },
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
                usage_input: None,
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
            SearchConfig {
                base: Caps { result: 8000, text: 2000 },
                min: Caps { result: 128, text: 64 },
                keep_events: 24,
                clip_chars: 20000,
            },
            true,
            req_chars,
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
                usage_input: None,
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
            SearchConfig {
                base: Caps { result: 8000, text: 2000 },
                min: Caps { result: 128, text: 64 },
                keep_events: 24,
                clip_chars: 20000,
            },
            true,
            req_chars,
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
        // the budget. The search yields the last candidate: the caller
        // turns it into the handoff (correction 57).
        let out = compact_search(
            &refs,
            1000,
            SearchConfig {
                base: Caps { result: 8000, text: 2000 },
                min: Caps { result: 128, text: 64 },
                keep_events: 24,
                clip_chars: 20000,
            },
            true,
            req_chars,
        );
        let last = out.err().expect("nothing fits; the last candidate is carried");
        // The last candidate is the floored form, still too big.
        assert!(req_chars(&last) > 1000);
        let s = serde_json::to_string(&last).unwrap();
        assert!(s.contains("the task"), "the task statement must survive");
    }

    /// The measured estimate: last measured input tokens plus the
    /// projected growth chars at the chars-per-token fallback.
    #[test]
    fn estimate_full_log_tokens_uses_last_measurement() {
        let est = estimate_full_log_tokens(&[(10, 50_000)], 200_000, 40_000, 8);
        assert_eq!(est, 55_000, "50k measured + 40k/8 growth");
        // The growth divides up: a partial token still counts.
        let est = estimate_full_log_tokens(&[(10, 50_000)], 200_000, 7, 8);
        assert_eq!(est, 50_001);
    }

    /// Without any measurement, the pre-measurement fallback divides
    /// the whole request's char count by the fallback rate.
    #[test]
    fn estimate_full_log_tokens_falls_back_to_chars() {
        let est = estimate_full_log_tokens(&[], 200_000, 0, 4);
        assert_eq!(est, 50_000);
    }

    /// The two-fold error case: the char estimate says the full log
    /// fits, the measured tokens say it does not. The measured
    /// estimate drives the decision (work item B).
    #[test]
    fn full_log_fits_is_driven_by_measured_tokens() {
        // Char fallback: 440k chars at 4 chars/token = 110k tokens,
        // under the 131k budget: the old char-based check fits.
        assert!(full_log_fits(&[], 440_000, 0, 4, 131_000));
        // The same request measured at 110k input tokens. A 55k
        // budget (the FT-008 degradation zone) rejects it, while the
        // char fallback against 131k would have let it through.
        assert!(!full_log_fits(&[(3, 110_000)], 440_000, 0, 4, 55_000));
        assert!(full_log_fits(&[(3, 110_000)], 440_000, 0, 4, 131_000));
    }

    /// With the full candidate skipped, the search starts at the base
    /// caps. A log whose full form fits the char budget still compacts
    /// when the caller measured it out.
    #[test]
    fn compact_search_skips_full_when_told_to() {
        // 40 results longer than the base cap; the keep window leaves
        // the first 8 to compact. The full form and the base-caps
        // form both fit the budget, so the only difference between
        // the two searches is the full candidate.
        let mut events = vec![Ev::User { text: "the task".into() }];
        for i in 0..40 {
            events.push(ev_res(&format!("r{i}"), &"R".repeat(9000)));
        }
        let refs: Vec<&Ev> = events.iter().collect();
        let req_chars = |items: &Vec<serde_json::Value>| {
            serde_json::to_string(items).unwrap().len()
        };
        // The full form fits this budget: with `include_full` the
        // search returns it untouched.
        let full = compact_search(
            &refs,
            400_000,
            SearchConfig {
                base: Caps { result: 8000, text: 2000 },
                min: Caps { result: 128, text: 64 },
                keep_events: 32,
                clip_chars: 20000,
            },
            true,
            req_chars,
        )
        .expect("the full form fits");
        assert!(!full.iter().any(|i| i.to_string().contains("compacted")));
        // Skipped: the base caps fit the budget and carry the marker.
        let compact = compact_search(
            &refs,
            400_000,
            SearchConfig {
                base: Caps { result: 8000, text: 2000 },
                min: Caps { result: 128, text: 64 },
                keep_events: 32,
                clip_chars: 20000,
            },
            false,
            req_chars,
        )
        .expect("the capped form fits");
        let s = serde_json::to_string(&compact).unwrap();
        assert!(s.contains("[compacted:"), "skipped full starts at the base caps");
    }

    /// Per-event item offsets: `offsets[i + 1]` starts the event
    /// after `i`; the final entry is the list end.
    #[test]
    fn build_items_off_carries_event_boundaries() {
        let events = vec![
            Ev::User { text: "t".into() },
            Ev::Assistant {
                text: "a".into(),
                calls: vec![Call {
                    id: "c1".into(),
                    name: "bash".into(),
                    args_str: "{}".into(),
                }],
                reasoning: vec![serde_json::json!({
                    "type": "reasoning",
                    "id": "rs_1"
                })],
                usage_input: None,
            },
            ev_res("1", "r"),
        ];
        let refs: Vec<&Ev> = events.iter().collect();
        let (items, offsets) = build_items_off(&refs, 10, &Caps { result: 50, text: 20 }, 20000, &HashSet::new());
        assert_eq!(offsets.len(), 4, "one offset per event plus the end");
        assert_eq!(items.len(), offsets[3]);
        // The assistant event projects to three items: reasoning,
        // message, call. Its boundary spans them.
        assert_eq!(offsets[2] - offsets[1], 3);
        assert_eq!(offsets[3] - offsets[2], 1, "the result projects to one item");
    }

    /// The exhausted event carries the summary request: the compacted
    /// items, the summary ask, a cheap output cap, and no tools.
    #[test]
    fn exhausted_event_carries_the_summary_request() {
        let items = vec![
            serde_json::json!({"type": "message", "role": "user", "content": "the task"}),
            serde_json::json!({"type": "function_call_output", "call_id": "c1", "output": "out"}),
        ];
        let ev: serde_json::Value = context_exhausted_event(&items, "m", 4096);
        assert_eq!(ev["type"], "context_exhausted");
        assert_eq!(ev["v"], 1);
        assert!(ev["message"].as_str().unwrap().contains("handoff"));
        let sr = &ev["summary_request"];
        assert_eq!(sr["model"], "m");
        assert_eq!(sr["max_output_tokens"], 4096);
        assert_eq!(sr["tools"], serde_json::json!([]));
        assert!(sr["instructions"].as_str().unwrap().contains("handoff summary"));
        let input = sr["input"].as_array().expect("the input holds the items");
        assert_eq!(input.len(), 3, "the two items plus the summary ask");
        assert_eq!(input[0], items[0]);
        assert_eq!(input[2]["role"], "user");
        assert_eq!(input[2]["content"], HANDOFF_ASK);
    }
}
