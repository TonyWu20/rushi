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
#[derive(Clone, Copy, PartialEq, Debug)]
struct Caps {
    result: usize,
    text: usize,
}

/// The compact state, persisted per session as `compact.json`.
///
/// The compact form is sticky (correction 62). Once a session
/// engages compaction, every later request uses the compact form.
/// The caps freeze at engagement. The keep window halves down to
/// two across the trigger steps. The drop count only grows. The
/// compact region re-renders only when one of those moves. That
/// keeps the request prefix byte-stable between moves, which is
/// what the provider prefix KV cache needs.
///
/// `last_tokens` and `last_at` are the measured `usage.input_tokens`
/// of the last compact request, with the event index it belongs to.
/// `drops_at_last` is the drop count at that measurement. The
/// difference gives the measured token savings per dropped group
/// (`per_group`): the drop jump of the next over-budget reading.
#[derive(Clone, Copy, PartialEq, Debug)]
struct CompactState {
    caps: Caps,
    /// The current keep window. It halves down to two. It never
    /// grows back.
    keep: usize,
    /// Dropped step groups. It only grows.
    drops: usize,
    /// The event index at engagement. The measurements at or after
    /// it belong to compact-form requests.
    engaged_at: usize,
    /// The measured input tokens of the last compact request.
    last_tokens: usize,
    /// The event index of that measurement.
    last_at: usize,
    /// The drop count at that measurement.
    drops_at_last: usize,
    /// Measured token savings per dropped group. Zero until the
    /// first drop is measured.
    per_group: usize,
}

impl CompactState {
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "v": 2,
            "caps": { "result": self.caps.result, "text": self.caps.text },
            "keep": self.keep,
            "drops": self.drops,
            "engaged_at": self.engaged_at,
            "last_tokens": self.last_tokens,
            "last_at": self.last_at,
            "drops_at_last": self.drops_at_last,
            "per_group": self.per_group,
        })
    }

    fn from_json(v: &serde_json::Value) -> Option<Self> {
        Some(CompactState {
            caps: Caps {
                result: v.get("caps")?.get("result")?.as_u64()? as usize,
                text: v.get("caps")?.get("text")?.as_u64()? as usize,
            },
            keep: v.get("keep")?.as_u64()? as usize,
            drops: v.get("drops")?.as_u64()? as usize,
            engaged_at: v.get("engaged_at")?.as_u64()? as usize,
            last_tokens: v.get("last_tokens").and_then(|x| x.as_u64())? as usize,
            last_at: v.get("last_at").and_then(|x| x.as_u64())? as usize,
            drops_at_last: v.get("drops_at_last").and_then(|x| x.as_u64())? as usize,
            per_group: v.get("per_group").and_then(|x| x.as_u64())? as usize,
        })
    }
}

/// The load result of the compact state file.
///
/// Absent is a fresh full-form session. Corrupt re-engages at stage
/// zero with the base caps: the safe side.
enum StateLoad {
    Absent,
    Corrupt,
    Found(CompactState),
}

/// Read the compact state file of a session directory.
fn read_compact_state(session_dir: &Path) -> StateLoad {
    let raw = match fs::read_to_string(session_dir.join("compact.json")) {
        Ok(raw) => raw,
        Err(_) => return StateLoad::Absent,
    };
    let value: serde_json::Value = match serde_json::from_str(raw.trim()) {
        Ok(v) => v,
        Err(_) => return StateLoad::Corrupt,
    };
    match CompactState::from_json(&value) {
        Some(s) => StateLoad::Found(s),
        None => StateLoad::Corrupt,
    }
}

/// Write the compact state file atomically. One writer: assemble
/// runs in the step loop. A failed write drops the state: the next
/// run re-engages from the log measurements.
fn write_compact_state(session_dir: &Path, state: &CompactState) {
    let path = session_dir.join("compact.json");
    let tmp = session_dir.join("compact.json.tmp");
    if let Err(e) = fs::write(&tmp, serde_json::to_string(&state.to_json()).unwrap_or_else(|_| panic!("state json")))
        .and_then(|_| fs::rename(&tmp, path))
    {
        eprintln!("Warning: compact state write failed: {e}");
    }
}

/// The request form of one assemble run, decided in token space.
///
/// No char mechanism (correction 62). The token budget compares
/// against measured `usage.input_tokens` of the log. The growth of
/// appended events converts at the measured per-event token growth,
/// not at a chars-per-token rate.
#[derive(Debug, PartialEq)]
enum RequestForm {
    Full,
    /// The sticky compact form: the frozen caps, the current keep
    /// window, the current drop count. The compact region re-renders
    /// only when one of those moves.
    Compact { caps: Caps, keep: usize, drops: usize },
    /// The drop count reached its max and the estimate still
    /// outgrows the budget: the handoff.
    Exhausted { caps: Caps, keep: usize, drops: usize },
}

/// Decide the request form of one run, in token space.
///
/// Absent state: the full log when the measured estimate fits the
/// budget, else engage at the base caps, the full keep window, no
/// drops. Present state: the compact form. An over-budget reading
/// moves one lever per run. The keep window halves down to two.
/// Then the drop count jumps by the measured per-group savings, or
/// by one group until the first drop is measured. No post-engagement
/// measurement: the blind crawl drops one group per run until the
/// handoff. The drop count and the keep window never move back.
///
/// The second return is the state to persist. None persists nothing.
fn decide_form(
    n_events: usize,
    budget_tokens: usize,
    measurements: &[(usize, usize)],
    state: Option<CompactState>,
    base_caps: Caps,
    keep_events: usize,
    max_drops: usize,
) -> (RequestForm, Option<CompactState>) {
    match state {
        None => {
            let last = measurements.last().copied();
            let rate = measured_growth_rate(measurements, 0);
            let predicted = predict_tokens(last, rate, n_events);
            if predicted <= budget_tokens {
                (RequestForm::Full, None)
            } else {
                let s = CompactState {
                    caps: base_caps,
                    keep: keep_events.max(2),
                    drops: 0,
                    engaged_at: n_events,
                    last_tokens: 0,
                    last_at: 0,
                    drops_at_last: 0,
                    per_group: 0,
                };
                (
                    RequestForm::Compact { caps: base_caps, keep: s.keep, drops: 0 },
                    Some(s),
                )
            }
        }
        Some(mut s) => {
            // The log measurements at or after engagement are the
            // compact-form measurements. A new one recalibrates the
            // per-group drop savings, then replaces the last point.
            let meas: Vec<(usize, usize)> = measurements
                .iter()
                .copied()
                .filter(|(i, _)| *i >= s.engaged_at)
                .collect();
            let new_last = meas.last().copied();
            if let Some((idx, tokens)) = new_last {
                if idx > s.last_at {
                    if s.last_tokens > 0 && s.drops > s.drops_at_last {
                        let saved = s.last_tokens.saturating_sub(tokens);
                        s.per_group =
                            saved / s.drops.saturating_sub(s.drops_at_last).max(1);
                    }
                    s.last_tokens = tokens;
                    s.last_at = idx;
                    s.drops_at_last = s.drops;
                }
            }
            let rate = measured_growth_rate(&meas, 0);
            let predicted = predict_tokens(new_last, rate, n_events);
            let over = predicted > budget_tokens;
            let mut moved = false;
            // Blind crawl: no compact measurement yet, the keep
            // window is at the floor. One drop group per run. The
            // provider window is the backstop until the first
            // measurement recalibrates the drop jump.
            if new_last.is_none() && s.keep <= 2 && s.drops < max_drops {
                s.drops += 1;
                moved = true;
            }
            if over {
                if s.keep > 2 {
                    let keeps = next_keeps(keep_events);
                    let next = keeps.iter().find(|&&k| k < s.keep).copied().unwrap_or(2);
                    s.keep = next;
                    moved = true;
                } else if s.drops < max_drops {
                    let k = if s.per_group > 0 {
                        let deficit = predicted - budget_tokens;
                        let n = deficit.saturating_sub(1) / s.per_group + 1;
                        n.min(max_drops - s.drops)
                    } else {
                        1
                    };
                    s.drops = s.drops.saturating_add(k).min(max_drops);
                    moved = true;
                }
            }
            if s.drops >= max_drops && over {
                (
                    RequestForm::Exhausted { caps: s.caps, keep: s.keep, drops: s.drops },
                    moved.then_some(s),
                )
            } else {
                (
                    RequestForm::Compact { caps: s.caps, keep: s.keep, drops: s.drops },
                    moved.then_some(s),
                )
            }
        }
    }
}

/// Per-event token growth, measured in token space.
///
/// The last two measurements at or after `engaged_at` give the
/// growth: the token delta over the event delta, ceiled. A shrunken
/// request (a stage move) reads as zero growth: the clamp is the
/// conservative side. Fewer than two measurements: zero.
fn measured_growth_rate(measurements: &[(usize, usize)], engaged_at: usize) -> usize {
    let v: Vec<(usize, usize)> = measurements
        .iter()
        .copied()
        .filter(|(i, _)| *i >= engaged_at)
        .collect();
    if v.len() < 2 {
        return 0;
    }
    let (ia, ta) = v[v.len() - 2];
    let (ib, tb) = v[v.len() - 1];
    if ib <= ia || tb <= ta {
        return 0;
    }
    (tb - ta + (ib - ia) - 1) / (ib - ia)
}

/// Predict the token count of the next request.
///
/// The last measured input tokens plus the projected growth of the
/// events appended after that measurement. No measurement: zero:
/// the provider window is the backstop of a fresh session.
fn predict_tokens(last: Option<(usize, usize)>, rate: usize, n_events: usize) -> usize {
    match last {
        Some((idx, tokens)) => tokens + rate * n_events.saturating_sub(idx),
        None => 0,
    }
}

/// The event selection of a keep window and a drop count: the log
/// minus the first `drop_count` droppable step groups. The
/// droppable groups are the leading assistant groups strictly
/// before the keep tail. User groups are never dropped. The keep
/// tail is never dropped.
fn stage_selection<'a>(events: &'a [&'a Ev], keep: usize, drop_count: usize) -> Vec<&'a Ev> {
    let groups = step_groups(events);
    let tail_start = events.len().saturating_sub(keep);
    let mut keep_idx: Vec<usize> = (0..events.len()).collect();
    let mut dropped = 0usize;
    for &(start, end) in &groups {
        if !matches!(events[start], Ev::Assistant { .. }) {
            continue;
        }
        if start >= tail_start || end > tail_start {
            break;
        }
        if dropped >= drop_count {
            break;
        }
        keep_idx.retain(|i| !(start..end).contains(i));
        dropped += 1;
    }
    keep_idx.iter().map(|i| events[*i]).collect()
}

/// The droppable step group count at the final keep window. It sets
/// the max stage: halving count plus this count.
fn max_droppable_groups(events: &[&Ev], keep: usize) -> usize {
    let groups = step_groups(events);
    let tail_start = events.len().saturating_sub(keep);
    let mut count = 0;
    for &(start, end) in &groups {
        if !matches!(events[start], Ev::Assistant { .. }) {
            continue;
        }
        if start >= tail_start || end > tail_start {
            break;
        }
        count += 1;
    }
    count
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
    }
}

/// Split a string into a head and a tail that together hold at most
/// `limit` chars. The head takes the first half, the tail the rest.
/// A string that fits comes back whole with zero elided. The cut
/// never splits a char: it works in char units.
fn head_tail_cut(s: &str, limit: usize) -> (String, String, usize, usize) {
    let total = s.chars().count();
    if total <= limit {
        return (s.to_string(), String::new(), total, 0);
    }
    let head_n = limit / 2;
    let tail_n = limit - head_n;
    let chars: Vec<char> = s.chars().collect();
    let head: String = chars[..head_n].iter().collect();
    let tail: String = chars[total - tail_n..].iter().collect();
    (head, tail, total, total - head_n - tail_n)
}

/// Cut a string to at most `limit` chars, keeping the head and the
/// tail. Mark the elided middle with a compact marker and the
/// pointer to the full record, so the model can fetch the body
/// from disk instead of re-guessing it.
fn trim_chars(s: &str, limit: usize, pointer: &str) -> String {
    let (head, tail, total, elided) = head_tail_cut(s, limit);
    if elided == 0 {
        return head;
    }
    format!(
        "{head}\n[compacted: {total} chars total, {elided} elided from the middle. {pointer}]\n{tail}"
    )
}

/// Clip a tool result to at most `limit` chars, keeping the head
/// and the tail. Mark the elided middle with a clip marker and the
/// pointer to the full record.
fn clip_full(s: &str, limit: usize, pointer: &str) -> String {
    let (head, tail, total, elided) = head_tail_cut(s, limit);
    if elided == 0 {
        return head;
    }
    format!(
        "{head}\n[tool result clipped: {total} chars total, {elided} elided from the middle. {pointer}]\n{tail}"
    )
}

/// Where the full record of a trimmed piece lives, as seen from the
/// tool working directory. A trimmed result or text never dies: the
/// marker points at the file that still holds the full body.
struct LogPointers {
    tool_log: Option<String>,
    event_log: Option<String>,
    tool_log_ids: HashSet<String>,
}

impl LogPointers {
    /// The pointer of a trimmed tool result. A call id with a tool
    /// log record points at that record, with the fetch command. An
    /// id without a record (a legacy inline body) points at the
    /// event log.
    fn tool_result_pointer(&self, id: &str) -> String {
        if let Some(path) = &self.tool_log {
            if self.tool_log_ids.contains(id) {
                return format!(
                    "Full result: {path} (call id {id}; fetch: jq -c 'select(.id == \"{id}\")' {path})"
                );
            }
        }
        format!(
            "Full result: {} (tool_result event, call id {id})",
            self.event_label()
        )
    }
    /// The pointer of a trimmed call argument: the tool_call event.
    fn call_args_pointer(&self, id: &str) -> String {
        format!(
            "Full arguments: {} (tool_call event, call id {id})",
            self.event_label()
        )
    }
    /// The pointer of a trimmed assistant text: the assistant_message
    /// event. The head of the trimmed text locates the record.
    fn assistant_pointer(&self) -> String {
        format!(
            "Full text: {} (assistant_message event)",
            self.event_label()
        )
    }
    fn event_label(&self) -> &str {
        self.event_log.as_deref().unwrap_or("the session event log")
    }
}

/// Build the log pointers of a session. The paths resolve against the
/// tool working directory: the session dir relative to the cwd when
/// the session dir is absolute and rooted in the cwd, else the path
/// as given (a relative session dir is already relative to the cwd).
fn log_pointers(
    session_dir: &str,
    cwd: Option<&str>,
    tool_texts: &HashMap<String, String>,
) -> LogPointers {
    let base = if Path::new(session_dir).is_absolute() {
        cwd.filter(|c| Path::new(c).is_absolute())
            .and_then(|c| Path::new(session_dir).strip_prefix(c).ok())
            .map(|rel| rel.to_string_lossy().to_string())
            .unwrap_or_else(|| session_dir.to_string())
    } else {
        session_dir.to_string()
    };
    let tool_log = if Path::new(session_dir).join("tools.jsonl").exists() {
        Some(format!("{base}/tools.jsonl"))
    } else {
        None
    };
    LogPointers {
        tool_log,
        event_log: Some(format!("{base}/events.jsonl")),
        tool_log_ids: tool_texts.keys().cloned().collect(),
    }
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

/// The call ids of schema-validation failures (FT-008).
///
/// The failed pair (the call plus its error result) self-priming:
/// the model re-emits its own empty-arguments call when it sees the
/// failure in the input. Correction 60: the request drops the pair
/// in every position, keep window included. The event log and the
/// tool log keep it.
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
    ptrs: &LogPointers,
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
            let out = clip_full(text, clip_chars, &ptrs.tool_result_pointer(id));
            vec![serde_json::json!({
                "type": "function_call_output",
                "call_id": id,
                "output": out
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
    ptrs: &LogPointers,
) -> Vec<serde_json::Value> {
    match ev {
        Ev::User { .. } => full_items(ev, 0, drop_pairs, ptrs),
        Ev::Assistant { text, calls, .. } => {
            let mut items = vec![serde_json::json!({
                "type": "message",
                "role": "assistant",
                "content": trim_chars(text, caps.text, &ptrs.assistant_pointer())
            })];
            for c in calls {
                if drop_pairs.contains(&c.id) {
                    continue;
                }
                items.push(serde_json::json!({
                    "type": "function_call",
                    "call_id": c.id,
                    "name": c.name,
                    "arguments": trim_chars(&c.args_str, caps.text, &ptrs.call_args_pointer(&c.id))
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
                "output": trim_chars(text, caps.result, &ptrs.tool_result_pointer(id))
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
    ptrs: &LogPointers,
) -> (Vec<serde_json::Value>, Vec<usize>) {
    let split = events.len().saturating_sub(keep);
    let mut items: Vec<serde_json::Value> = Vec::new();
    let mut offsets: Vec<usize> = Vec::with_capacity(events.len() + 1);
    for (i, ev) in events.iter().enumerate() {
        offsets.push(items.len());
        let ev_items = if i < split {
            compact_items(ev, caps, drop_pairs, ptrs)
        } else {
            full_items(ev, clip_chars, drop_pairs, ptrs)
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
    ptrs: &LogPointers,
) -> Vec<serde_json::Value> {
    build_items_off(events, keep, caps, clip_chars, drop_pairs, ptrs).0
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

/// The input items of one compact request. The compact region is
/// everything before the keep window minus the dropped step groups.
/// It re-renders only when the caps, the keep window, or the drop
/// count moves. Within those moves the items are byte-stable: that
/// is the cache property (correction 62). The provider prefix cache
/// holds until the next move.
///
/// The schema-error pairs go out of the request, keep window
/// included (correction 60). The drop count drops the oldest
/// droppable step groups (correction 55, monotone form).
fn compact_candidate(
    events: &[&Ev],
    keep: usize,
    drops: usize,
    caps: &Caps,
    clip_chars: usize,
    ptrs: &LogPointers,
) -> Vec<serde_json::Value> {
    let sel = stage_selection(events, keep, drops);
    // Every schema-error pair goes out of the request (correction
    // 60): old region and keep window alike.
    let drop_pairs = schema_error_pair_ids(&sel, sel.len());
    build_items(&sel, keep, caps, clip_chars, &drop_pairs, ptrs)
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

    // The session working directory, recorded at the entry point.
    let cwd_file = PathBuf::from(&args.session).join("cwd");
    let cwd = fs::read_to_string(&cwd_file)
        .ok()
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty());
    if let Some(cwd) = &cwd {
        system_prompt.push_str(&format!(
            "\n\nCurrent working directory: {cwd}\n\
             Relative paths in tool calls resolve against this directory."
        ));
    }

    let limits = config.get("limits").cloned().unwrap_or(toml::Value::String(String::new()));
    let limits = if limits.is_table() {
        limits
    } else {
        toml::Value::Table(toml::map::Map::new())
    };
    let tool_result_max_chars: usize = val_int(&limits, "tool_result_max_chars").unwrap_or(20000) as usize;
    let compact_keep_events: usize = val_int(&limits, "compact_keep_events").unwrap_or(24) as usize;
    // The frozen base caps of the compact form (correction 62).
    // They freeze at engagement. They never halve.
    let compact_result_chars: usize = val_int(&limits, "compact_result_chars").unwrap_or(500) as usize;
    let compact_text_chars: usize = val_int(&limits, "compact_text_chars").unwrap_or(200) as usize;
    // The output cap of the one handoff summary call (correction 57).
    let handoff_summary_max_tokens: u64 =
        val_int(&limits, "handoff_summary_max_tokens").unwrap_or(4096) as u64;

    let tools_root = config
        .get("paths")
        .and_then(|p| p.get("tools_root"))
        .and_then(|t| t.as_str())
        .unwrap_or("tools");

    // Resolve the active model. The context budget is in tokens
    // (correction 62): the user knob is `context_budget_tokens`.
    // No char mechanism. The default is the model window minus the
    // output reservation.
    let active_model = resolve_active_model(&config);
    let model_settings = resolve_model_settings(&config, &active_model);
    let window_input_tokens = model_settings
        .context_tokens
        .saturating_sub(model_settings.max_output_tokens as usize);
    let budget_tokens = val_int(&limits, "context_budget_tokens")
        .map(|v| v.max(1) as usize)
        .unwrap_or(window_input_tokens)
        .min(window_input_tokens.max(1));

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

    // Where the model finds a full record after a trim or a clip:
    // the per-session tool log when the session has one, else the
    // event log's inline body (legacy). The paths resolve against
    // the tool working directory.
    let ptrs = log_pointers(&args.session, cwd.as_deref(), &tool_texts);
    match &ptrs.tool_log {
        Some(path) => system_prompt.push_str(&format!(
            "\n\nFull tool records: the session tool log {path} holds the full output of every tool call, one JSON line per call id. Trimmed and clipped results in the input point back to it. Fetch one record with the bash tool: jq -c 'select(.id == \"<call id>\")' {path}"
        )),
        None => system_prompt.push_str(&format!(
            "\n\nFull tool records: the session event log {} inlines the full body of each tool_result event, keyed by call id. Trimmed results in the input point back to it.",
            ptrs.event_log.as_deref().unwrap_or("events.jsonl")
        )),
    }

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

    // Sticky auto-compact, token-only (correction 62). The full log
    // goes out while the measured estimate fits the token budget:
    // the last measured `usage.input_tokens` plus the projected
    // growth at the measured per-event token growth. A fresh session
    // without measurements sends the full log: the provider window
    // is its backstop. When the estimate outgrows the budget, the
    // session engages the sticky compact form once. After that the
    // compact form never gives the log back. Each over-budget
    // reading moves one lever: the keep window halves, or the drop
    // count jumps by the measured per-group savings. The levers
    // never move back. When the drop count reaches its max and the
    // estimate still outgrows the budget, the `context_exhausted`
    // event ends the turn with the handoff summary request
    // (correction 57).
    let events_refs: Vec<&Ev> = events.iter().collect();

    // The measured input token count of each request, with the event
    // index it belongs to. The growth of appended events converts
    // at the measured per-event token growth, never at a char rate.
    let measurements: Vec<(usize, usize)> = events_refs
        .iter()
        .enumerate()
        .filter_map(|(i, e)| match e {
            Ev::Assistant { usage_input: Some(t), .. } => Some((i, *t)),
            _ => None,
        })
        .collect();

    let n = events_refs.len();
    let base_caps = Caps {
        result: compact_result_chars,
        text: compact_text_chars,
    };
    // The drop count max: the droppable groups at the floor keep
    // window. Past it, the handoff takes over.
    let max_drops = max_droppable_groups(&events_refs, 2);

    // Read the sticky state. A corrupt state re-engages at the base
    // caps with no drops: the safe side.
    let session_dir = Path::new(&args.session);
    let state = match read_compact_state(session_dir) {
        StateLoad::Absent => None,
        StateLoad::Corrupt => {
            eprintln!("Warning: corrupt compact.json; re-engaging at the base caps");
            Some(CompactState {
                caps: base_caps,
                keep: compact_keep_events.max(2),
                drops: 0,
                engaged_at: 0,
                last_tokens: 0,
                last_at: 0,
                drops_at_last: 0,
                per_group: 0,
            })
        }
        StateLoad::Found(s) => Some(s),
    };

    let (form, persist) = decide_form(
        n,
        budget_tokens,
        &measurements,
        state,
        base_caps,
        compact_keep_events,
        max_drops,
    );
    if let Some(s) = persist {
        write_compact_state(session_dir, &s);
    }

    let items = match &form {
        RequestForm::Full => build_items(
            &events_refs,
            n,
            &Caps { result: 0, text: 0 },
            tool_result_max_chars,
            &schema_error_pair_ids(&events_refs, n),
            &ptrs,
        ),
        RequestForm::Compact { caps, keep, drops } => compact_candidate(
            &events_refs,
            *keep,
            *drops,
            caps,
            tool_result_max_chars,
            &ptrs,
        ),
        RequestForm::Exhausted { caps, keep, drops } => {
            let items = compact_candidate(&events_refs, *keep, *drops, caps, tool_result_max_chars, &ptrs);
            let exhausted = context_exhausted_event(
                &items,
                &model_settings.model_id,
                handoff_summary_max_tokens,
            );
            println!("{}", exhausted);
            return;
        }
    };
    println!("{}", make_request(&items));
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

    fn ev_asst(text: &str) -> Ev {
        Ev::Assistant {
            text: text.to_string(),
            calls: vec![],
            reasoning: vec![],
            usage_input: None,
        }
    }

    /// Test pointers: a tool log with one record, the event log
    /// always. Call id `c1` has a tool log record; the rest fall
    /// back to the inline event log.
    fn test_ptrs() -> LogPointers {
        LogPointers {
            tool_log: Some("sessions/s/tools.jsonl".to_string()),
            event_log: Some("sessions/s/events.jsonl".to_string()),
            tool_log_ids: HashSet::from(["c1".to_string()]),
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
        let items = full_items(&ev, 20000, &drop, &test_ptrs());
        // The dropped call is gone; its call id appears nowhere.
        let s = items.iter().map(|i| i.to_string()).collect::<String>();
        assert!(!s.contains("\"bad\""), "the failed call must be out: {s}");
        assert!(s.contains("\"ok\""), "the clean call stays: {s}");
        // Its result goes out too.
        let res = full_items(&ev_schema_error("bad"), 20000, &drop, &test_ptrs());
        assert!(res.is_empty(), "the failed result must be out");
    }

    /// An empty drop set keeps the pair at the mechanism level.
    /// The compact candidate supplies the full drop set (correction
    /// 60), so the keep window's pairs go out too.
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
        let items = compact_items(&ev, &Caps { result: 50, text: 20 }, &HashSet::new(), &test_ptrs());
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

    /// The stage candidate drops every schema-error pair from the
    /// request, keep window included (correction 60). The task
    /// statement survives.
    #[test]
    fn compact_candidate_drops_schema_error_pairs() {
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
        // The base caps, the full keep window, no drops.
        let out = compact_candidate(
            &refs,
            4,
            0,
            &Caps { result: 8000, text: 2000 },
            20000,
            &test_ptrs(),
        );
        let s = serde_json::to_string(&out).unwrap();
        assert!(s.contains("the task"), "the task statement must survive");
        // Correction 60: no failed pair survives in the request.
        // The oldest pair r0 and the keep-tail pair r28 are both
        // out: the history of empty-argument failures self-priming
        // the next call (FT-008).
        assert!(!s.contains("\"call_id\":\"r0\""), "the old failed pair must be out");
        let last_even = 28;
        assert!(
            !s.contains(&format!("\"call_id\":\"r{last_even}\"")),
            "the keep-tail failed pair must be out too"
        );
    }

    /// The full form drops the schema-error pairs even when the log
    /// fits the budget (correction 60). A fitting log is no reason
    /// to hand the model its own failures.
    #[test]
    fn full_form_drops_schema_error_pairs() {
        let mut events: Vec<Ev> = vec![Ev::User {
            text: "the task".to_string(),
        }];
        events.push(Ev::Assistant {
            text: "x".to_string(),
            calls: vec![Call {
                id: "f1".to_string(),
                name: "bash".to_string(),
                args_str: "{}".to_string(),
            }],
            reasoning: vec![],
            usage_input: None,
        });
        events.push(ev_schema_error("f1"));
        events.push(Ev::User {
            text: "continue".to_string(),
        });
        let refs: Vec<&Ev> = events.iter().collect();
        // The full form of the whole log, every schema-error pair
        // dropped.
        let drops = schema_error_pair_ids(&refs, refs.len());
        let out = build_items(&refs, refs.len(), &Caps { result: 0, text: 0 }, 20000, &drops, &test_ptrs());
        let s = serde_json::to_string(&out).unwrap();
        assert!(s.contains("the task"), "the task statement survives");
        assert!(
            !s.contains("\"call_id\":\"f1\""),
            "the failed pair must be out of the full form"
        );
    }

    fn caps() -> Caps {
        Caps {
            result: 50,
            text: 20,
        }
    }

    #[test]
    fn trim_keeps_head_and_tail_and_points_at_the_full_record() {
        let out = trim_chars("abcdefghij", 3, "FULL");
        assert!(out.starts_with('a'), "the head must survive: {out}");
        assert!(out.ends_with("ij"), "the tail must survive: {out}");
        assert!(
            out.contains("[compacted: 10 chars total, 7 elided from the middle. FULL]"),
            "{out}"
        );
        assert_eq!(trim_chars("abc", 5, "FULL"), "abc", "a fitting string stays whole");
    }

    #[test]
    fn clip_full_keeps_head_and_tail_and_points_at_the_full_record() {
        let out = clip_full(&"x".repeat(30), 10, "FULL");
        assert_eq!(out.chars().take(5).collect::<String>(), "xxxxx", "the head must survive");
        assert!(out.ends_with(&"x".repeat(5)), "the tail must survive: {out}");
        assert!(
            out.contains("[tool result clipped: 30 chars total, 20 elided from the middle. FULL]"),
            "{out}"
        );
    }

    #[test]
    fn clip_full_keeps_whole_multibyte_chars() {
        // 4000 box-drawing chars, a 100-char clip: 50 head + 50 tail.
        // The cut works in char units, so no partial char anywhere.
        let s = "\u{2500}".repeat(4000);
        let out = clip_full(&s, 100, "FULL");
        assert_eq!(out.chars().take(50).collect::<String>(), "\u{2500}".repeat(50));
        assert!(out.ends_with(&"\u{2500}".repeat(50)), "the tail must survive");
        assert!(
            out.contains("[tool result clipped: 4000 chars total, 3900 elided from the middle. FULL]"),
            "the marker must name the gap: {out}"
        );
    }

    #[test]
    fn keep_all_when_window_covers_log() {
        let events = vec![ev_res("a", "one"), ev_res("b", "two")];
        let refs: Vec<&Ev> = events.iter().collect();
        let items = build_items(&refs, 10, &caps(), 20000, &HashSet::new(), &test_ptrs());
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
        let items = build_items(&refs, 4, &Caps { result: 50, text: 20 }, 20000, &HashSet::new(), &test_ptrs());
        let first = &items[0];
        let last = items.last().unwrap();
        let s = first.to_string();
        assert!(
            s.contains("[compacted: 400 chars total, 350 elided from the middle."),
            "the marker must name the elided middle: {s}"
        );
        assert!(
            s.contains("Full result: sessions/s/events.jsonl (tool_result event, call id id0)"),
            "a legacy id points at the event log: {s}"
        );
        let r25 = "R".repeat(25);
        assert!(
            s.contains(&format!("\"output\":\"{r25}\\n[compacted:")),
            "the head and the tail must survive around the marker: {s}"
        );
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
        let items = build_items(&refs, 0, &caps(), 20000, &HashSet::new(), &test_ptrs());
        assert!(items[0].to_string().contains("do the task"));
        assert!(items[1].to_string().contains("compacted"));
    }

    /// A compacted tool result with a tool log record points at that
    /// record: the path, the call id, and the fetch command. The
    /// head and the tail of the result survive around the marker.
    #[test]
    fn compact_tool_result_points_at_the_tool_log_record() {
        let ev = ev_res("c1", &"R".repeat(400));
        let items = compact_items(&ev, &Caps { result: 50, text: 20 }, &HashSet::new(), &test_ptrs());
        let out = items[0]["output"].as_str().unwrap();
        let r25 = "R".repeat(25);
        assert!(out.starts_with(&r25), "the head must survive: {out}");
        assert!(out.ends_with(&r25), "the tail must survive: {out}");
        assert!(
            out.contains("Full result: sessions/s/tools.jsonl (call id c1; fetch: jq -c 'select(.id == \"c1\")' sessions/s/tools.jsonl)"),
            "the pointer must name the path, the id, and the fetch: {out}"
        );
    }

    /// A compacted tool result without a tool log record points at
    /// the inline event log (legacy shape).
    #[test]
    fn compact_tool_result_falls_back_to_the_event_log_pointer() {
        let ev = ev_res("z9", &"R".repeat(400));
        let items = compact_items(&ev, &Caps { result: 50, text: 20 }, &HashSet::new(), &test_ptrs());
        let s = items[0].to_string();
        assert!(
            s.contains("Full result: sessions/s/events.jsonl (tool_result event, call id z9)"),
            "{s}"
        );
    }

    /// The clip in the full pass keeps head and tail and points at
    /// the tool log record, like the compact form.
    #[test]
    fn clipped_full_result_points_at_the_tool_log_record() {
        let ev = ev_res("c1", &"R".repeat(400));
        let items = full_items(&ev, 50, &HashSet::new(), &test_ptrs());
        let out = items[0]["output"].as_str().unwrap();
        let r25 = "R".repeat(25);
        assert!(out.starts_with(&r25), "the head must survive: {out}");
        assert!(out.ends_with(&r25), "the tail must survive: {out}");
        assert!(
            out.contains("[tool result clipped: 400 chars total, 350 elided from the middle."),
            "{out}"
        );
        assert!(out.contains("Full result: sessions/s/tools.jsonl (call id c1"), "{out}");
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
        let items = compact_items(&ev, &Caps { result: 50, text: 20 }, &HashSet::new(), &test_ptrs());
        let s = items.iter().map(|i| i.to_string()).collect::<String>();
        assert!(
            s.contains("[compacted: 300 chars total, 280 elided from the middle. Full text: sessions/s/events.jsonl (assistant_message event)]"),
            "the trimmed text must point at the event log: {s}"
        );
        assert!(
            s.contains("[compacted: 315 chars total, 295 elided from the middle. Full arguments: sessions/s/events.jsonl (tool_call event, call id c1)]"),
            "the trimmed args must point at the tool_call event: {s}"
        );
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
            &test_ptrs(),
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
            &test_ptrs(),
        );
        assert_eq!(items.len(), 2, "compact form keeps message and call only");
        assert!(!items.iter().any(|i| i.get("type").and_then(|t| t.as_str()) == Some("reasoning")));
    }

    /// A session whose reasoning items outgrow the budget still fits
    /// after compaction. The compacted events drop the items. The
    /// full tail keeps them. The task statement survives.
    #[test]
    fn compact_candidate_drops_reasoning_from_compacted_events() {
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
        let full = build_items(&refs, refs.len(), &Caps { result: 0, text: 0 }, 20000, &HashSet::new(), &test_ptrs());
        assert!(
            serde_json::to_string(&full).unwrap().len() > 12000,
            "the full form must outgrow the budget"
        );
        // A small keep window: the compact region drops the
        // reasoning items, the full tail keeps them.
        let out = compact_candidate(&refs, 3, 0, &Caps { result: 8000, text: 2000 }, 20000, &test_ptrs());
        let s = serde_json::to_string(&out).unwrap();
        assert!(s.contains("the task"), "the task statement must survive");
        // The compacted events drop their items. The full tail keeps
        // its items.
        assert!(!s.contains("rs_0"), "compacted turn 0 must drop its item");
        assert!(!s.contains("rs_1"), "compacted turn 1 must drop its item");
        assert!(s.contains("rs_3"), "the full tail keeps its items");
        assert!(s.contains("rs_4"), "the full tail keeps its items");
    }

    /// The keep window halves down to two across over-budget
    /// readings. It never grows back.
    #[test]
    fn decide_form_halves_the_keep_window() {
        let mut state: Option<CompactState> = None;
        let mut keeps = Vec::new();
        let mut drops = 0;
        for i in 0..6 {
            let n = 6 + i * 2;
            let meas: Vec<(usize, usize)> = (0..=7)
                .map(|j| (j, 100_000 + 4_000 * j))
                .filter(|(idx, _)| *idx < n)
                .collect();
            let (form, persist) = decide_form(
                n,
                50_000,
                &meas,
                state,
                Caps { result: 500, text: 100 },
                24,
                100,
            );
            state = persist;
            match form {
                RequestForm::Compact { keep, drops: dr, .. } => {
                    keeps.push(keep);
                    drops = dr;
                }
                other => panic!("expected compact, got {other:?}"),
            }
        }
        assert_eq!(
            keeps,
            vec![24, 12, 6, 3, 2, 2],
            "halving to two, then stable at two"
        );
        assert_eq!(drops, 1, "the last over-budget reading drops one trial group");
    }

    /// The drop jump is measured. Without a drop measurement, the
    /// over-budget reading drops one group (the trial). After the
    /// drop measurement, the jump is the measured per-group savings.
    #[test]
    fn decide_form_jump_is_measured() {
        let s0 = CompactState {
            caps: Caps { result: 500, text: 100 },
            keep: 2,
            drops: 0,
            engaged_at: 0,
            last_tokens: 0,
            last_at: 0,
            drops_at_last: 0,
            per_group: 0,
        };
        // No compact measurement yet: the blind crawl drops one
        // group per run.
        let (form, _persist) =
            decide_form(10, 50_000, &[], Some(s0), Caps { result: 500, text: 100 }, 24, 100);
        assert_eq!(
            form,
            RequestForm::Compact { caps: s0.caps, keep: 2, drops: 1 },
            "the blind crawl drops one group"
        );
        // The measured jump. The trial drop saved 500 tokens for
        // one group. The predicted overage is 9,500 tokens: the
        // jump is 19 more groups, 20 total.
        let s1 = CompactState {
            caps: s0.caps,
            keep: 2,
            drops: 1,
            engaged_at: 0,
            last_tokens: 60_000,
            last_at: 5,
            drops_at_last: 0,
            per_group: 0,
        };
        let meas: Vec<(usize, usize)> = vec![(5, 60_000), (9, 59_500)];
        let (form, _persist) =
            decide_form(10, 50_000, &meas, Some(s1), Caps { result: 500, text: 100 }, 24, 100);
        match form {
            RequestForm::Compact { drops, .. } => {
                assert_eq!(drops, 20, "the measured jump: one trial plus nineteen");
            }
            other => panic!("expected compact, got {other:?}"),
        }
    }

    /// The drop count drops the oldest droppable step groups.
    #[test]
    fn compact_candidate_drops_oldest_steps() {
        let ev = vec![
            Ev::User { text: "task".into() },
            ev_asst("step one"),
            ev_res("1", "ok"),
            ev_res("2", "ok"),
            ev_asst("step two"),
            ev_res("3", "ok"),
            ev_asst("step three"),
            ev_res("4", "ok"),
            ev_asst("step four"),
            ev_res("5", "ok"),
            ev_asst("step five"),
            ev_res("6", "ok"),
            ev_asst("step six"),
            ev_res("7", "ok"),
            ev_asst("step seven"),
            ev_res("8", "ok"),
        ];
        let refs: Vec<&Ev> = ev.iter().collect();
        let caps = Caps { result: 500, text: 100 };
        // The keep window is two events. The seven assistant
        // groups are droppable. Two drops remove the two oldest.
        let items = compact_candidate(&refs, 2, 2, &caps, 20_000, &test_ptrs());
        let flat = serde_json::to_string(&items).unwrap();
        assert!(!flat.contains("step one"));
        assert!(!flat.contains("step two"));
        assert!(flat.contains("step three"));
        assert!(flat.contains("step seven"));
        assert_eq!(
            flat.matches("step ").count() + flat.matches("step seven").count() - flat.matches("step seven").count(),
            5,
            "the five newest step markers survive"
        );
    }

    /// Zero drops keep every step.
    #[test]
    fn compact_candidate_keeps_all_steps_with_zero_drops() {
        let ev = vec![
            Ev::User { text: "task".into() },
            ev_asst("step one"),
            ev_res("1", "ok"),
            ev_asst("step two"),
            ev_res("2", "ok"),
        ];
        let refs: Vec<&Ev> = ev.iter().collect();
        let items = compact_candidate(&refs, 2, 0, &Caps { result: 500, text: 100 }, 20_000, &test_ptrs());
        let flat = serde_json::to_string(&items).unwrap();
        assert!(flat.contains("step one"));
        assert!(flat.contains("step two"));
    }

    /// The drop count at its max, with the estimate still over the
    /// budget, ends the turn with the handoff.
    #[test]
    fn decide_form_exhausts_at_the_max_drops() {
        let s = CompactState {
            caps: Caps { result: 500, text: 100 },
            keep: 2,
            drops: 5,
            engaged_at: 0,
            last_tokens: 90_000,
            last_at: 4,
            drops_at_last: 5,
            per_group: 500,
        };
        let meas: Vec<(usize, usize)> = vec![(4, 90_000)];
        let (form, _persist) =
            decide_form(5, 50_000, &meas, Some(s), Caps { result: 500, text: 100 }, 24, 5);
        assert!(
            matches!(&form, RequestForm::Exhausted { .. }),
            "the max drop count with an over-budget estimate is the handoff"
        );
    }

    /// The per-event growth is the token delta over the event delta
    /// of the last two measurements. A shrunken request reads as
    /// zero growth.
    #[test]
    fn measured_growth_rate_reads_the_last_two_measurements() {
        let m: Vec<(usize, usize)> =
            vec![(0, 1_000), (1, 2_000), (2, 1_900), (3, 2_800), (4, 4_000)];
        assert_eq!(measured_growth_rate(&m, 0), 1_200, "the last two measurements: 4000 - 2800 over one event");
        let shrunk: Vec<(usize, usize)> = vec![(0, 9_000), (1, 7_000)];
        assert_eq!(measured_growth_rate(&shrunk, 0), 0, "a shrunken request reads as zero growth");
    }

    /// The prediction is the last measured input tokens plus the
    /// projected growth of the appended events.
    #[test]
    fn predict_tokens_adds_measured_growth() {
        assert_eq!(
            predict_tokens(Some((4, 1_000)), 500, 6),
            2_000,
            "two appended events at 500 tokens each"
        );
        assert_eq!(
            predict_tokens(None, 500, 10),
            0,
            "no measurement: the provider window is the backstop"
        );
    }

    /// A fresh session sends the full log while the measured
    /// estimate fits. The over-budget estimate engages the compact
    /// form once.
    #[test]
    fn decide_form_engages_on_over_budget_estimate() {
        let m: Vec<(usize, usize)> = vec![(0, 10_000), (2, 12_000)];
        assert!(
            matches!(decide_form(2, 30_000, &m, None, Caps { result: 500, text: 100 }, 24, 100).0, RequestForm::Full),
            "the estimate fits: the full log"
        );
        let (form, persist) = decide_form(3, 10_000, &m, None, Caps { result: 500, text: 100 }, 24, 100);
        assert!(
            matches!(&form, RequestForm::Compact { keep: 24, drops: 0, .. }),
            "the over-budget estimate engages at the full keep window"
        );
        assert!(persist.is_some(), "the engagement persists");
    }

    /// The compact candidate is byte-stable between runs on the
    /// same log: the cache property.
    #[test]
    fn compact_candidate_is_byte_stable_between_runs() {
        let ev = vec![
            Ev::User { text: "task".into() },
            ev_asst("step one"),
            ev_res("1", "ok"),
            Ev::User { text: "more".into() },
            ev_asst("step two"),
            ev_res("2", "fine"),
        ];
        let refs: Vec<&Ev> = ev.iter().collect();
        let caps = Caps { result: 500, text: 100 };
        let a = compact_candidate(&refs, 24, 0, &caps, 20_000, &test_ptrs());
        let b = compact_candidate(&refs, 24, 0, &caps, 20_000, &test_ptrs());
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap(),
            "the same form re-renders byte-identical: the cache property"
        );
    }

    /// The state file round-trips. A corrupt file re-engages; the
    /// absent file is a fresh session.
    #[test]
    fn compact_state_round_trips_and_corrupt_reengages() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        // Absent: no state.
        assert!(matches!(read_compact_state(path), StateLoad::Absent));
        let s = CompactState {
            caps: Caps { result: 500, text: 100 },
            keep: 12,
            drops: 3,
            engaged_at: 42,
            last_tokens: 80_000,
            last_at: 50,
            drops_at_last: 3,
            per_group: 120,
        };
        write_compact_state(path, &s);
        assert!(
            matches!(read_compact_state(path), StateLoad::Found(st) if st == s),
            "the state round-trips"
        );
        // Corrupt: the re-engage path.
        std::fs::write(path.join("compact.json"), "not json").unwrap();
        assert!(matches!(read_compact_state(path), StateLoad::Corrupt));
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
        let (items, offsets) = build_items_off(&refs, 10, &Caps { result: 50, text: 20 }, 20000, &HashSet::new(), &test_ptrs());
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
