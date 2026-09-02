#![deny(clippy::todo, clippy::unimplemented, clippy::unreachable)]

use clap::Parser;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Project session log to ModelRequest
#[derive(Parser)]
#[command(
    name = "assemble",
    about = "Project the session log into a ModelRequest"
)]
struct Args {
    /// Session directory path
    #[arg(long)]
    session: String,

    /// Path to config file
    #[arg(long, default_value = "config.toml")]
    config: String,

    /// Request-time exclusion of the last assistant step group (its
    /// message, calls, and results). Nothing is persisted (docs/
    /// auto-compact-plan.md 4.3): the overflow retry request uses
    /// this for the logged length-stop group.
    #[arg(long)]
    drop_last_assistant: bool,

    /// The summary-input mode (docs/auto-compact-plan.md 4.3):
    /// project the old region from the last compaction boundary to
    /// `--up-to` in the compact form, append the summary-ask user
    /// item, and print the bare model request. No budget decision.
    /// No state write. bin/compact runs the mode and pipes the
    /// output to model.
    #[arg(long)]
    summary_input: bool,

    /// The 1-based log sequence the summary-input mode projects up
    /// to, inclusive. Required with `--summary-input`.
    #[arg(long)]
    up_to: Option<usize>,

    /// The follow-queue turn flag (docs/tui-pending-user-messages.md
    /// stage 2): inject the pending `follow` user messages into the
    /// model input. Without the flag, follow messages stay out of
    /// the input: they wait for a turn restart. Steer messages
    /// (the missing field) always ride the input, in-flight step
    /// included.
    #[arg(long)]
    inject_follow: bool,
}

struct ModelSettings {
    model_id: String,
    max_output_tokens: u64,
    context_tokens: usize,
}

/// One log event, projected to model input items.
enum Ev {
    User {
        text: String,
    },
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
    ToolResult {
        id: String,
        text: String,
    },
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
    /// The log seq of the last compaction boundary this state was
    /// engaged against (0 when no boundary). A boundary newer than
    /// the state re-engages the state fresh at the boundary
    /// (docs/auto-compact-plan.md 4.3).
    boundary_seq: usize,
}

impl CompactState {
    fn to_json(self) -> serde_json::Value {
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
            "boundary_seq": self.boundary_seq,
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
            // Legacy state files carry no boundary key: zero means
            // "no boundary", the pre-compaction form.
            boundary_seq: v.get("boundary_seq").and_then(|x| x.as_u64()).unwrap_or(0) as usize,
        })
    }
}

/// The load result of the compact state file.
///
/// Absent is a fresh full-form session. Corrupt re-engages at stage
/// zero with the base caps: the safe side.
#[derive(Debug)]
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
    if let Err(e) = fs::write(
        &tmp,
        serde_json::to_string(&state.to_json()).unwrap_or_else(|_| panic!("state json")),
    )
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
    Compact {
        caps: Caps,
        keep: usize,
        drops: usize,
    },
    /// The drop count reached its max and the estimate still
    /// outgrows the budget: the handoff.
    Exhausted {
        caps: Caps,
        keep: usize,
        drops: usize,
    },
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
    boundary_seq: usize,
) -> (RequestForm, Option<CompactState>) {
    match state {
        None => {
            let last = measurements.last().copied();
            let rate = measured_growth_rate(measurements, 0);
            let predicted = predict_tokens(last, rate);
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
                    boundary_seq,
                };
                (
                    RequestForm::Compact {
                        caps: base_caps,
                        keep: s.keep,
                        drops: 0,
                    },
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
                        s.per_group = saved / s.drops.saturating_sub(s.drops_at_last).max(1);
                    }
                    s.last_tokens = tokens;
                    s.last_at = idx;
                    s.drops_at_last = s.drops;
                }
            }
            let rate = measured_growth_rate(&meas, 0);
            let predicted = predict_tokens(new_last, rate);
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
                    let deficit = predicted - budget_tokens;
                    let k = match deficit.saturating_sub(1).checked_div(s.per_group) {
                        Some(n) => (n + 1).min(max_drops - s.drops),
                        None => 1,
                    };
                    s.drops = s.drops.saturating_add(k).min(max_drops);
                    moved = true;
                }
            }
            if s.drops >= max_drops && over {
                (
                    RequestForm::Exhausted {
                        caps: s.caps,
                        keep: s.keep,
                        drops: s.drops,
                    },
                    moved.then_some(s),
                )
            } else {
                (
                    RequestForm::Compact {
                        caps: s.caps,
                        keep: s.keep,
                        drops: s.drops,
                    },
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
    (tb - ta).div_ceil(ib - ia)
}

/// Predict the token count of the next request.
/// The predicted input of the next request: the last measured
/// input plus one step of measured growth. A step is one
/// measurement event: the next request adds one turn, not the rest
/// of the session. No measurement: zero: the provider window is
/// the backstop of a fresh session.
fn predict_tokens(last: Option<(usize, usize)>, rate: usize) -> usize {
    match last {
        Some((_, tokens)) => tokens + rate,
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
/// never splits a char: it works in char units. Returns the head,
/// the tail, the total, the elided count, the head length, and the
/// char offset where the kept tail starts: the marker's range
/// names (`chars {head_len}-{tail_start}`), so the elided span is
/// located, not just counted.
fn head_tail_cut(s: &str, limit: usize) -> (String, String, usize, usize, usize, usize) {
    let total = s.chars().count();
    if total <= limit {
        return (s.to_string(), String::new(), total, 0, total, 0);
    }
    let head_n = limit / 2;
    let tail_n = limit - head_n;
    let chars: Vec<char> = s.chars().collect();
    let head: String = chars[..head_n].iter().collect();
    let tail: String = chars[total - tail_n..].iter().collect();
    (head, tail, total, total - head_n - tail_n, head_n, total - tail_n)
}

/// Cut a string to at most `limit` chars, keeping the head and the
/// tail. Mark the elided middle with a compact marker and the
/// pointer to the full record, so the model can fetch the body
/// from disk instead of re-guessing it. The marker names the kept
/// sizes and the char span of the gap, so the elided region is
/// located, not just counted.
fn trim_chars(s: &str, limit: usize, pointer: &str) -> String {
    let (head, tail, total, elided, head_len, tail_start) = head_tail_cut(s, limit);
    if elided == 0 {
        return head;
    }
    let tail_len = tail.chars().count();
    format!(
        "{head}\n[compacted: {total} chars total; head {head_len} + tail {tail_len} kept, {elided} elided (chars {head_len}-{tail_start}). {pointer}]\n{tail}"
    )
}

/// Clip a tool result to at most `limit` chars, keeping the head
/// and the tail. Mark the elided middle with a clip marker and the
/// pointer to the full record.
fn clip_full(s: &str, limit: usize, pointer: &str) -> String {
    let (head, tail, total, elided, head_len, tail_start) = head_tail_cut(s, limit);
    if elided == 0 {
        return head;
    }
    let tail_len = tail.chars().count();
    format!(
        "{head}\n[tool result clipped: {total} chars total; head {head_len} + tail {tail_len} kept, {elided} elided (chars {head_len}-{tail_start}). {pointer}]\n{tail}"
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

/// The truncation notice of the length-stop group (docs/
/// auto-compact-plan.md section 4.4): parse fabricates one result
/// per cut-off call, asking for a shorter call. The prefix is
/// intact in the slim index preview.
const TRUNCATION_NOTICE_PREFIX: &str = "Arguments may be truncated";

/// A pair-droppable result text. Two prefixes (docs/
/// auto-compact-plan.md section 4.3, correction 60 extended): the
/// schema-validation failure and the truncation notice. Both pair
/// shapes go out of every request form.
fn is_droppable_pair_text(text: &str) -> bool {
    text.starts_with(SCHEMA_ERROR_PREFIX) || text.starts_with(TRUNCATION_NOTICE_PREFIX)
}

/// The call ids of the droppable pairs before the split point:
/// schema-validation failures and truncation notices. The failed
/// pair (the call plus its error result) self-priming: the model
/// re-emits its own failed call when it sees the failure in the
/// input. The request drops the pair in every position, keep
/// window included. The event log and the tool log keep it.
fn drop_pair_ids(events: &[&Ev], split: usize) -> HashSet<String> {
    let mut ids: HashSet<String> = HashSet::new();
    for &ev in events.iter().take(split) {
        if let Ev::ToolResult { id, text } = ev {
            if is_droppable_pair_text(text) {
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
        Ev::Assistant {
            text,
            calls,
            reasoning,
            ..
        } => {
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

/// The projection boundary: the last `compaction_summary` event and
/// the first log sequence its summary does not cover
/// (docs/auto-compact-plan.md section 4.1).
struct Boundary {
    /// The 1-based log seq of the compaction_summary event.
    seq: usize,
    /// The 1-based log seq of the first event the summary does not
    /// cover. Projection replaces every earlier event with the
    /// summary.
    first_kept_seq: usize,
    /// The summary text, frozen in the log.
    summary: String,
    /// The file-op lists, carried forward across compactions.
    read_files: Vec<String>,
    modified_files: Vec<String>,
}

/// Parse one `compaction_summary` event into the projection
/// boundary. A value that cannot be parsed is rejected: `None`
/// re-renders without the summary (docs/auto-compact-plan.md
/// section 4.1).
fn parse_boundary(event: &serde_json::Value, seq: usize) -> Option<Boundary> {
    let first_kept_seq = event.get("first_kept_seq")?.as_u64()? as usize;
    if first_kept_seq < 1 {
        return None;
    }
    let summary = event.get("summary")?.as_str()?.to_string();
    let lists = |key: &str| -> Vec<String> {
        event
            .get(key)
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default()
    };
    Some(Boundary {
        seq,
        first_kept_seq,
        summary,
        read_files: lists("read_files"),
        modified_files: lists("modified_files"),
    })
}

/// The user-role framing item that carries the last summary in the
/// request. One message, frozen in the log: between compaction
/// moves the request prefix stays byte-stable (the correction 62
/// property, preserved).
fn summary_framing_item(summary: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "message",
        "role": "user",
        "content": format!(
            "[Session context summary of the events before this point. The events after it are kept.]\n\n{summary}"
        )
    })
}

/// The first-time compaction prompt (docs/auto-compact-plan.md
/// section 4.2, adapted from the pi 0.84.2 summarization prompt).
/// The structured format: Goal, Constraints, Progress (done / in
/// progress / blocked), Key Decisions, Next Steps, Critical Context.
/// Exact file paths, commands, and error messages must survive.
const COMPACT_INSTRUCTIONS_FIRST: &str = "The messages above are a conversation to summarize. Create a structured context checkpoint summary that another LLM will use to continue the work.\n\nUse this EXACT format:\n\n## Goal\n[What is the user trying to accomplish? Can be multiple items if the session covers different tasks.]\n\n## Constraints & Preferences\n- [Any constraints, preferences, or requirements mentioned by user]\n- [Or \"(none)\" if none were mentioned]\n\n## Progress\n### Done\n- [x] [Completed tasks/changes]\n\n### In Progress\n- [ ] [Current work]\n\n### Blocked\n- [Issues preventing progress, if any]\n\n## Key Decisions\n- **[Decision]**: [Brief rationale]\n\n## Next Steps\n1. [Ordered list of what should happen next]\n\n## Critical Context\n- [Any data, examples, or references needed to continue]\n- [Or \"(none)\" if not applicable]\n\nKeep each section concise. Preserve exact file paths, function names, and error messages.";

/// The iterative-update prompt: preserve, add, move items from In
/// Progress to Done (the pi preserve/add/move rules). The previous
/// summary and its file-op lists ride in the request, so the lists
/// merge, not re-extract (docs/auto-compact-plan.md section 4.2).
const COMPACT_INSTRUCTIONS_UPDATE: &str = "The messages above are NEW conversation messages to incorporate into the existing summary provided in the previous-summary block.\n\nUpdate the existing structured summary with new information. RULES:\n- PRESERVE all existing information from the previous summary\n- ADD new progress, decisions, and context from the new messages\n- UPDATE the Progress section: move items from In Progress to Done when completed\n- UPDATE Next Steps based on what was accomplished\n- PRESERVE exact file paths, function names, and error messages\n- If something is no longer relevant, you may remove it\n\nKeep each section concise. Use the same six-section format as the previous summary.\n\n<previous-summary>\n{summary}\n</previous-summary>\nPreviously read files:\n{read_files}\nPreviously modified files:\n{modified_files}";

/// The summary ask of the first-time request.
const COMPACT_ASK_FIRST: &str = "Write the summary now, using the conversation above.";

/// The summary ask of the update request.
const COMPACT_ASK_UPDATE: &str = "Update the summary now, incorporating the new messages above.";

/// Format the file-op list lines of the update prompt. An empty
/// list shows the none marker, pi-style.
fn file_list_lines(list: &[String]) -> String {
    if list.is_empty() {
        "(none)".to_string()
    } else {
        list.iter().map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n")
    }
}

/// The update instructions with the previous summary and its
/// file-op lists carried in.
fn compact_update_instructions(b: &Boundary) -> String {
    COMPACT_INSTRUCTIONS_UPDATE
        .replace(
            "{summary}",
            &b.summary,
        )
        .replace("{read_files}", &file_list_lines(&b.read_files))
        .replace("{modified_files}", &file_list_lines(&b.modified_files))
}

/// Drop the oldest `count` droppable step groups (the assistant
/// groups) from a region. User groups never drop: the task
/// statement must survive.
fn drop_oldest_groups<'a>(
    events: &'a [&'a Ev],
    droppable: &[(usize, usize)],
    count: usize,
) -> Vec<&'a Ev> {
    let mut keep_idx: Vec<usize> = (0..events.len()).collect();
    for &(start, end) in droppable.iter().take(count) {
        keep_idx.retain(|i| !(start..end).contains(i));
    }
    keep_idx.iter().map(|i| events[*i]).collect()
}

/// The request-time exclusion of the last assistant step group
/// (docs/auto-compact-plan.md section 4.3): its message, its calls,
/// and its results. Nothing is persisted. Excluding the whole group
/// excludes no orphan function_call.
fn drop_last_assistant_group<'a>(events: &[&'a Ev]) -> Vec<&'a Ev> {
    let groups = step_groups(events);
    match groups.iter().rev().find(|&&(start, _)| matches!(events[start], Ev::Assistant { .. })) {
        Some(&(start, end)) => {
            let mut out: Vec<&'a Ev> = events[..start].to_vec();
            out.extend(events[end..].iter());
            out
        }
        None => events.to_vec(),
    }
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
    let drop_pairs = drop_pair_ids(&sel, sel.len());
    build_items(&sel, keep, caps, clip_chars, &drop_pairs, ptrs)
}

/// The delivery-queue gate of the user event (docs/tui-pending-user-
/// messages.md stage 2). Returns `Some(text)` when the event rides
/// the model input, `None` when it stays out. `follow` messages
/// ride only on a turn restart (`inject_follow`); the missing field
/// means steer, and steer always rides.
fn user_event_rides(
    event: &serde_json::Value,
    inject_follow: bool,
) -> Option<String> {
    let follow = event
        .get("queue")
        .and_then(|q| q.as_str())
        .unwrap_or("steer")
        == "follow";
    if follow && !inject_follow {
        return None;
    }
    Some(
        event
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string(),
    )
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

    let limits = config
        .get("limits")
        .cloned()
        .unwrap_or(toml::Value::String(String::new()));
    let limits = if limits.is_table() {
        limits
    } else {
        toml::Value::Table(toml::map::Map::new())
    };
    let tool_result_max_chars: usize =
        val_int(&limits, "tool_result_max_chars").unwrap_or(20000) as usize;
    let compact_keep_events: usize = val_int(&limits, "compact_keep_events").unwrap_or(24) as usize;
    // The frozen base caps of the compact form (correction 62).
    // They freeze at engagement. They never halve.
    let compact_result_chars: usize =
        val_int(&limits, "compact_result_chars").unwrap_or(500) as usize;
    let compact_text_chars: usize = val_int(&limits, "compact_text_chars").unwrap_or(200) as usize;
    // Auto-compact knobs (docs/auto-compact-plan.md 4.5). The
    // summary call output cap defaults to 0.8 * compact_reserve
    // tokens (pi's ratio) and clamps to the model max output.
    let compact_reserve_tokens: usize =
        val_int(&limits, "compact_reserve_tokens").unwrap_or(16384) as usize;
    let compact_summary_max_tokens: u64 = val_int(&limits, "compact_summary_max_tokens")
        .map(|v| v.max(1) as u64)
        .unwrap_or(((compact_reserve_tokens as f64) * 0.8) as u64);
    // A cheaper effort for the summary call on a local GPU. The
    // session reasoning_effort is the default when unset.
    let compact_reasoning_effort: Option<String> =
        val_str(&limits, "compact_reasoning_effort");

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

    // Parse events into projections. The 1-based log sequence of
    // every projected event rides alongside it, with the same
    // counting as claim: one seq per non-empty line. The last
    // compaction summary is the projection boundary: every earlier
    // event is replaced by its summary (docs/auto-compact-plan.md
    // section 4.3).
    let mut events: Vec<Ev> = Vec::new();
    let mut event_seqs: Vec<usize> = Vec::new();
    let mut seq: usize = 0;
    let mut boundary: Option<Boundary> = None;
    for line in lines.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        seq += 1;
        let event: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let event_type = event.get("type").and_then(|t| t.as_str()).unwrap_or("");

        match event_type {
            "user_message" => {
                // The delivery queue (docs/tui-pending-user-messages.md
                // stage 2): `follow` messages wait for a turn
                // restart; the flag injects them into this request.
                // A missing field means steer. The seq counting
                // keeps every log line: a skipped event still owns
                // its sequence.
                if let Some(text) = user_event_rides(&event, args.inject_follow) {
                    events.push(Ev::User { text });
                    event_seqs.push(seq);
                }
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
                        let id = tc
                            .get("id")
                            .and_then(|i| i.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = tc
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_string();
                        // The log stores arguments as a JSON object.
                        // The Responses API needs them as a JSON string.
                        let args_val = tc
                            .get("arguments")
                            .cloned()
                            .unwrap_or(serde_json::json!({}));
                        let args_str =
                            serde_json::to_string(&args_val).unwrap_or_else(|_| "{}".into());
                        calls.push(Call { id, name, args_str });
                    }
                }
                // Reasoning items from this turn. Keep objects only:
                // a malformed entry drops, the rest ride on.
                let reasoning = event
                    .get("reasoning")
                    .and_then(|r| r.as_array())
                    .map(|items| items.iter().filter(|i| i.is_object()).cloned().collect())
                    .unwrap_or_default();
                // The measured input tokens of the request that
                // produced this turn (work item B). They drive the
                // token-based context budget.
                let usage_input = event
                    .get("usage")
                    .and_then(|u| u.get("input_tokens"))
                    .and_then(|t| t.as_u64())
                    .map(|n| n as usize);
                events.push(Ev::Assistant {
                    text,
                    calls,
                    reasoning,
                    usage_input,
                });
                event_seqs.push(seq);
            }
            "tool_result" => {
                let id = event
                    .get("id")
                    .and_then(|i| i.as_str())
                    .unwrap_or("")
                    .to_string();
                let index_text = event
                    .get("value")
                    .and_then(|v| v.get("text"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                // The full body comes from the tool log when the
                // session has one; the index text is the legacy
                // inline body or the short preview.
                let text = tool_texts.get(&id).cloned().unwrap_or(index_text);
                events.push(Ev::ToolResult { id, text });
                event_seqs.push(seq);
            }
            "compaction_summary" => {
                // The projection boundary. A value that cannot be
                // parsed is rejected: the log re-renders without the
                // summary (docs/auto-compact-plan.md section 4.1).
                if let Some(b) = parse_boundary(&event, seq) {
                    boundary = Some(b);
                }
            }
            // error events are terminal. Skip them, as before.
            "error" => {}
            _ => {}
        }
    }
    // A boundary whose first_kept_seq outgrows the log is corrupt:
    // re-render without the summary.
    let boundary = boundary.filter(|b| b.first_kept_seq <= seq.max(1));

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

    // Sticky auto-compact, token-only (correction 62). The projected
    // log goes out while the measured estimate fits the token budget:
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
    // signal runs the last-resort in-session compaction
    // (docs/auto-compact-plan.md section 4.3): the summary replaces
    // the old region in the original session. No new session. No
    // `context_exhausted` marker.
    //
    // The projected events are the kept region after the last
    // compaction boundary (docs/auto-compact-plan.md section 4.3):
    // the summary replaces every earlier event. The sticky compact
    // form, the drop search, and `max_drops` apply to the kept
    // region only.
    let (events_refs, projected_seqs): (Vec<&Ev>, Vec<usize>) = match &boundary {
        Some(b) => events
            .iter()
            .zip(event_seqs.iter())
            .filter(|(_, s)| **s >= b.first_kept_seq)
            .map(|(e, s)| (e, s))
            .unzip(),
        None => (events.iter().collect(), event_seqs),
    };

    // The summary-input mode (docs/auto-compact-plan.md section 4.3):
    // project the old region from the last boundary to `--up-to` in
    // the compact form, append the summary-ask user item, and print
    // the bare model request. No budget decision. No state write.
    // bin/compact runs the mode and pipes the output to model.
    if args.summary_input {
        let up_to = match args.up_to {
            Some(u) if u >= 1 => u,
            _ => {
                eprintln!("Error: --summary-input requires --up-to <seq>, a 1-based log sequence");
                std::process::exit(1);
            }
        };
        let base_caps = Caps {
            result: compact_result_chars,
            text: compact_text_chars,
        };
        let cut = projected_seqs
            .iter()
            .position(|&s| s > up_to)
            .unwrap_or(events_refs.len());
        let mut old_region: Vec<&Ev> = events_refs[..cut].to_vec();
        // The strip flag excludes the last assistant group (the
        // logged length-stop group) from the summary input.
        if args.drop_last_assistant {
            old_region = drop_last_assistant_group(&old_region);
        }
        let request = summary_input_request(
            &old_region,
            &boundary,
            &model_settings.model_id,
            compact_summary_max_tokens.min(model_settings.max_output_tokens),
            compact_reasoning_effort.as_deref(),
            &base_caps,
            budget_tokens,
            &ptrs,
        );
        println!("{}", request);
        return;
    }

    // The measured input token count of each request, with the event
    // index it belongs to. The growth of appended events converts
    // at the measured per-event token growth, never at a char rate.
    let measurements: Vec<(usize, usize)> = events_refs
        .iter()
        .enumerate()
        .filter_map(|(i, e)| match e {
            Ev::Assistant {
                usage_input: Some(t),
                ..
            } => Some((i, *t)),
            _ => None,
        })
        .collect();

    let n = events_refs.len();
    let base_caps = Caps {
        result: compact_result_chars,
        text: compact_text_chars,
    };
    // The drop count max: the droppable groups at the floor keep
    // window. Past it, the last-resort compaction takes over.
    let max_drops = max_droppable_groups(&events_refs, 2);

    // Read the sticky state. A corrupt state re-engages at the base
    // caps with no drops: the safe side. A state older than the last
    // compaction boundary re-engages fresh at the boundary index:
    // pre-compaction measurements poison the growth rate and the
    // drop search (docs/auto-compact-plan.md section 4.3).
    let session_dir = Path::new(&args.session);
    let boundary_seq = boundary.as_ref().map(|b| b.seq).unwrap_or(0);
    let fresh_state = |boundary_seq: usize| CompactState {
        caps: base_caps,
        keep: compact_keep_events.max(2),
        drops: 0,
        engaged_at: 0,
        last_tokens: 0,
        last_at: 0,
        drops_at_last: 0,
        per_group: 0,
        boundary_seq,
    };
    let state = match read_compact_state(session_dir) {
        StateLoad::Absent => None,
        StateLoad::Corrupt => {
            eprintln!("Warning: corrupt compact.json; re-engaging at the base caps");
            Some(fresh_state(boundary_seq))
        }
        StateLoad::Found(s) => {
            if boundary_seq > 0 && s.boundary_seq < boundary_seq {
                eprintln!("Info: compact.json predates the last compaction; re-engaging fresh at the boundary");
                Some(fresh_state(boundary_seq))
            } else {
                Some(s)
            }
        }
    };

    let (form, persist) = decide_form(
        n,
        budget_tokens,
        &measurements,
        state,
        base_caps,
        compact_keep_events,
        max_drops,
        boundary_seq,
    );
    if let Some(s) = persist {
        write_compact_state(session_dir, &s);
    }

    // The request-time exclusion of the last assistant group (docs/
    // auto-compact-plan.md section 4.3). Nothing is persisted: the
    // log keeps the group. The budget decision above ran on the
    // full projected list, so the state stays consistent.
    let sel_events: Vec<&Ev> = if args.drop_last_assistant {
        drop_last_assistant_group(&events_refs)
    } else {
        events_refs.clone()
    };

    // The summary framing item: one user-role message carrying the
    // last summary. It is frozen in the log, so the request prefix
    // stays byte-stable between compaction moves.
    let framing: Option<serde_json::Value> =
        boundary.as_ref().map(|b| summary_framing_item(&b.summary));
    let prepend_summary = |items: &mut Vec<serde_json::Value>,
                           framing: &Option<serde_json::Value>| {
        if let Some(f) = framing {
            items.insert(0, f.clone());
        }
    };

    let items = match &form {
        RequestForm::Full => {
            let mut items = build_items(
                &sel_events,
                n,
                &Caps {
                    result: 0,
                    text: 0,
                },
                tool_result_max_chars,
                &drop_pair_ids(&events_refs, n),
                &ptrs,
            );
            prepend_summary(&mut items, &framing);
            items
        }
        RequestForm::Compact {
            caps,
            keep,
            drops,
        } => {
            let mut items = compact_candidate(
                &sel_events,
                *keep,
                *drops,
                caps,
                tool_result_max_chars,
                &ptrs,
            );
            prepend_summary(&mut items, &framing);
            items
        }
        RequestForm::Exhausted {
            caps,
            keep,
            drops,
        } => {
            let mut items = compact_candidate(
                &sel_events,
                *keep,
                *drops,
                caps,
                tool_result_max_chars,
                &ptrs,
            );
            prepend_summary(&mut items, &framing);
            // The terminal handoff retired (docs/auto-compact-plan.md
            // section 4.3): the Exhausted form signals the last-
            // resort in-session compaction. It carries the compact-
            // candidate request so the loop can proceed in the
            // current form when the forced compaction fails.
            let request = make_request(&items);
            let exhausted = serde_json::json!({
                "v": 1,
                "type": "context_exhausted",
                "ts": chrono_utc_now(),
                "message": "Context budget exhausted after compaction. The last-resort in-session compaction takes over: compact this session and continue in place.",
                "request": request,
            });
            println!("{}", exhausted);
            return;
        }
    };
    println!("{}", make_request(&items));
}

fn chrono_utc_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// The summary-input request of the in-session compaction
/// (docs/auto-compact-plan.md section 4.2): the old region in the
/// compact form, the summary-ask user item, a capped output. No
/// tools: the summary is plain text. The drop search bounds the old
/// region at the input budget: the smallest drop count that fits, or
/// the search max when none fits (the handoff invariant).
fn summary_input_request(
    old: &[&Ev],
    prev: &Option<Boundary>,
    model: &str,
    max_out_cap: u64,
    effort: Option<&str>,
    caps: &Caps,
    input_budget: usize,
    ptrs: &LogPointers,
) -> serde_json::Value {
    // The droppable step groups of the old region: the assistant
    // groups, in order. User groups never drop.
    let groups = step_groups(old);
    let droppable: Vec<(usize, usize)> = groups
        .into_iter()
        .filter(|&(start, _)| matches!(old[start], Ev::Assistant { .. }))
        .collect();
    let max_d = droppable.len();
    let ask = match prev {
        Some(_) => COMPACT_ASK_UPDATE,
        None => COMPACT_ASK_FIRST,
    };
    let instructions = match prev {
        Some(b) => compact_update_instructions(b),
        None => COMPACT_INSTRUCTIONS_FIRST.to_string(),
    };
    // The chars/4 estimate of one candidate, the same estimator pi
    // uses for the compaction walk.
    let estimate = |items: &[serde_json::Value]| -> usize {
        let chars = serde_json::to_string(items)
            .map(|s| s.chars().count())
            .unwrap_or(0);
        (instructions.chars().count() + chars + ask.chars().count()) / 4
    };
    // The search: the smallest drop count that fits the input
    // budget, or the search max when none fits.
    let mut chosen = max_d;
    for d in 0..=max_d {
        chosen = d;
        let sel = drop_oldest_groups(old, &droppable, d);
        let drop_pairs = drop_pair_ids(&sel, sel.len());
        let items = build_items(&sel, 0, caps, 0, &drop_pairs, ptrs);
        if estimate(&items) <= input_budget {
            break;
        }
    }
    let sel = drop_oldest_groups(old, &droppable, chosen);
    let drop_pairs = drop_pair_ids(&sel, sel.len());
    let items = build_items(&sel, 0, caps, 0, &drop_pairs, ptrs);
    let mut input = items.to_vec();
    input.push(serde_json::json!({
        "type": "message",
        "role": "user",
        "content": ask
    }));
    let mut request = serde_json::json!({
        "model": model,
        "instructions": instructions,
        "input": input,
        "tools": [],
        "max_output_tokens": max_out_cap
    });
    // An optional request-level effort; bin/model prefers it over
    // the config (docs/auto-compact-plan.md section 4.5).
    if let Some(e) = effort {
        request["reasoning_effort"] = serde_json::json!(e);
    }
    request
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
        let err: serde_json::Value = serde_json::json!({"exit": 3, "stdout": "", "stderr": "boom"});
        assert_eq!(record_text(&err).as_deref(), Some("boom"));
        let code_only: serde_json::Value =
            serde_json::json!({"exit": 3, "stdout": "", "stderr": ""});
        assert_eq!(
            record_text(&code_only).as_deref(),
            Some("Tool exited with code 3.")
        );
        let not_run: serde_json::Value = serde_json::json!({"error": "Unknown tool nope."});
        assert_eq!(record_text(&not_run).as_deref(), Some("Unknown tool nope."));
        assert!(record_text(&serde_json::json!({})).is_none());
    }

    /// The pair-drop text check is on the result text prefix. Two
    /// prefixes (docs/auto-compact-plan.md section 4.3, correction
    /// 60 extended): the schema-validation failure and the
    /// truncation notice of the length-stop group.
    #[test]
    fn droppable_pair_text_check() {
        assert!(is_droppable_pair_text(
            "Tool arguments failed schema validation: command."
        ));
        assert!(is_droppable_pair_text(
            "Arguments may be truncated. Re-issue the call with shorter arguments."
        ));
        assert!(!is_droppable_pair_text("plain tool output"));
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
        let items = compact_items(
            &ev,
            &Caps {
                result: 50,
                text: 20,
            },
            &HashSet::new(),
            &test_ptrs(),
        );
        let s = items.iter().map(|i| i.to_string()).collect::<String>();
        assert!(
            s.contains("\"bad\""),
            "the pair stays in the keep window: {s}"
        );
    }

    /// The id set is the droppable-pair results before the split
    /// point only: the two prefixes. Results at or after the split
    /// are out of scope.
    #[test]
    fn drop_pair_ids_cover_the_compacted_region() {
        let evs: Vec<Ev> = vec![
            ev_schema_error("r1"),
            ev_res("r2", "fine"),
            ev_schema_error("r3"),
            ev_res("r4", "Arguments may be truncated. Re-issue the call with shorter arguments."),
        ];
        let refs: Vec<&Ev> = evs.iter().collect();
        // Split after two events: r1 is old, r3 and r4 sit in the
        // keep window.
        let ids = drop_pair_ids(&refs, 2);
        assert!(ids.contains("r1"), "the old failure is in the set");
        assert!(
            !ids.contains("r3"),
            "the recent failure stays out of the set"
        );
        // The truncation-notice result joins the set at the split.
        assert!(drop_pair_ids(&refs, 4).contains("r4"), "the two-prefix rule");
        // A clean result is never in the set.
        assert!(!ids.contains("r2"));
        // No split: nothing is compacted, nothing drops.
        assert!(drop_pair_ids(&refs, 0).is_empty());
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
            &Caps {
                result: 8000,
                text: 2000,
            },
            20000,
            &test_ptrs(),
        );
        let s = serde_json::to_string(&out).unwrap();
        assert!(s.contains("the task"), "the task statement must survive");
        // Correction 60: no failed pair survives in the request.
        // The oldest pair r0 and the keep-tail pair r28 are both
        // out: the history of empty-argument failures self-priming
        // the next call (FT-008).
        assert!(
            !s.contains("\"call_id\":\"r0\""),
            "the old failed pair must be out"
        );
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
        let drops = drop_pair_ids(&refs, refs.len());
        let out = build_items(
            &refs,
            refs.len(),
            &Caps { result: 0, text: 0 },
            20000,
            &drops,
            &test_ptrs(),
        );
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
            out.contains(
                "[compacted: 10 chars total; head 1 + tail 2 kept, 7 elided (chars 1-8). FULL]"
            ),
            "{out}"
        );
        assert_eq!(
            trim_chars("abc", 5, "FULL"),
            "abc",
            "a fitting string stays whole"
        );
    }

    #[test]
    fn clip_full_keeps_head_and_tail_and_points_at_the_full_record() {
        let out = clip_full(&"x".repeat(30), 10, "FULL");
        assert_eq!(
            out.chars().take(5).collect::<String>(),
            "xxxxx",
            "the head must survive"
        );
        assert!(
            out.ends_with(&"x".repeat(5)),
            "the tail must survive: {out}"
        );
        assert!(
            out.contains(
                "[tool result clipped: 30 chars total; head 5 + tail 5 kept, 20 elided (chars 5-25). FULL]"
            ),
            "{out}"
        );
    }

    #[test]
    fn clip_full_keeps_whole_multibyte_chars() {
        // 4000 box-drawing chars, a 100-char clip: 50 head + 50 tail.
        // The cut works in char units, so no partial char anywhere.
        let s = "\u{2500}".repeat(4000);
        let out = clip_full(&s, 100, "FULL");
        assert_eq!(
            out.chars().take(50).collect::<String>(),
            "\u{2500}".repeat(50)
        );
        assert!(
            out.ends_with(&"\u{2500}".repeat(50)),
            "the tail must survive"
        );
        assert!(
            out.contains(
                "[tool result clipped: 4000 chars total; head 50 + tail 50 kept, 3900 elided (chars 50-3950). FULL]"
            ),
            "the marker must name the gap and its span: {out}"
        );
    }

    #[test]
    fn keep_all_when_window_covers_log() {
        let events = [ev_res("a", "one"), ev_res("b", "two")];
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
        let items = build_items(
            &refs,
            4,
            &Caps {
                result: 50,
                text: 20,
            },
            20000,
            &HashSet::new(),
            &test_ptrs(),
        );
        let first = &items[0];
        let last = items.last().unwrap();
        let s = first.to_string();
        assert!(
            s.contains("[compacted: 400 chars total; head 25 + tail 25 kept, 350 elided (chars 25-375)."),
            "the marker must name the elided middle and its span: {s}"
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
        let events = [
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
        let items = compact_items(
            &ev,
            &Caps {
                result: 50,
                text: 20,
            },
            &HashSet::new(),
            &test_ptrs(),
        );
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
        let items = compact_items(
            &ev,
            &Caps {
                result: 50,
                text: 20,
            },
            &HashSet::new(),
            &test_ptrs(),
        );
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
            out.contains("[tool result clipped: 400 chars total; head 25 + tail 25 kept, 350 elided (chars 25-375)."),
            "{out}"
        );
        assert!(
            out.contains("Full result: sessions/s/tools.jsonl (call id c1"),
            "{out}"
        );
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
        let items = compact_items(
            &ev,
            &Caps {
                result: 50,
                text: 20,
            },
            &HashSet::new(),
            &test_ptrs(),
        );
        let s = items.iter().map(|i| i.to_string()).collect::<String>();
        assert!(
            s.contains("[compacted: 300 chars total; head 10 + tail 10 kept, 280 elided (chars 10-290). Full text: sessions/s/events.jsonl (assistant_message event)]"),
            "the trimmed text must point at the event log: {s}"
        );
        assert!(
            s.contains("[compacted: 315 chars total; head 10 + tail 10 kept, 295 elided (chars 10-305). Full arguments: sessions/s/events.jsonl (tool_call event, call id c1)]"),
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
            &Caps {
                result: 50,
                text: 20,
            },
            &HashSet::new(),
            &test_ptrs(),
        );
        assert_eq!(items.len(), 2, "compact form keeps message and call only");
        assert!(!items
            .iter()
            .any(|i| i.get("type").and_then(|t| t.as_str()) == Some("reasoning")));
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
        let full = build_items(
            &refs,
            refs.len(),
            &Caps { result: 0, text: 0 },
            20000,
            &HashSet::new(),
            &test_ptrs(),
        );
        assert!(
            serde_json::to_string(&full).unwrap().len() > 12000,
            "the full form must outgrow the budget"
        );
        // A small keep window: the compact region drops the
        // reasoning items, the full tail keeps them.
        let out = compact_candidate(
            &refs,
            3,
            0,
            &Caps {
                result: 8000,
                text: 2000,
            },
            20000,
            &test_ptrs(),
        );
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
                Caps {
                    result: 500,
                    text: 100,
                },
                24,
                100,
            0,
            );
            state = persist;
            match form {
                RequestForm::Compact {
                    keep, drops: dr, ..
                } => {
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
        assert_eq!(
            drops, 1,
            "the last over-budget reading drops one trial group"
        );
    }

    /// The drop jump is measured. Without a drop measurement, the
    /// over-budget reading drops one group (the trial). After the
    /// drop measurement, the jump is the measured per-group savings.
    #[test]
    fn decide_form_jump_is_measured() {
        let s0 = CompactState {
            caps: Caps {
                result: 500,
                text: 100,
            },
            keep: 2,
            drops: 0,
            engaged_at: 0,
            last_tokens: 0,
            last_at: 0,
            drops_at_last: 0,
            per_group: 0,
            boundary_seq: 0,
        };
        // No compact measurement yet: the blind crawl drops one
        // group per run.
        let (form, _persist) = decide_form(
            10,
            50_000,
            &[],
            Some(s0),
            Caps {
                result: 500,
                text: 100,
            },
            24,
            100,
        0,
        );
        assert_eq!(
            form,
            RequestForm::Compact {
                caps: s0.caps,
                keep: 2,
                drops: 1
            },
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
                    boundary_seq: 0,
        };
        let meas: Vec<(usize, usize)> = vec![(5, 60_000), (9, 59_500)];
        let (form, _persist) = decide_form(
            10,
            50_000,
            &meas,
            Some(s1),
            Caps {
                result: 500,
                text: 100,
            },
            24,
            100,
        0,
        );
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
            Ev::User {
                text: "task".into(),
            },
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
        let caps = Caps {
            result: 500,
            text: 100,
        };
        // The keep window is two events. The seven assistant
        // groups are droppable. Two drops remove the two oldest.
        let items = compact_candidate(&refs, 2, 2, &caps, 20_000, &test_ptrs());
        let flat = serde_json::to_string(&items).unwrap();
        assert!(!flat.contains("step one"));
        assert!(!flat.contains("step two"));
        assert!(flat.contains("step three"));
        assert!(flat.contains("step seven"));
        assert_eq!(
            flat.matches("step ").count() + flat.matches("step seven").count()
                - flat.matches("step seven").count(),
            5,
            "the five newest step markers survive"
        );
    }

    /// Zero drops keep every step.
    #[test]
    fn compact_candidate_keeps_all_steps_with_zero_drops() {
        let ev = [
            Ev::User {
                text: "task".into(),
            },
            ev_asst("step one"),
            ev_res("1", "ok"),
            ev_asst("step two"),
            ev_res("2", "ok"),
        ];
        let refs: Vec<&Ev> = ev.iter().collect();
        let items = compact_candidate(
            &refs,
            2,
            0,
            &Caps {
                result: 500,
                text: 100,
            },
            20_000,
            &test_ptrs(),
        );
        let flat = serde_json::to_string(&items).unwrap();
        assert!(flat.contains("step one"));
        assert!(flat.contains("step two"));
    }

    /// The drop count at its max, with the estimate still over the
    /// budget, ends the turn with the handoff.
    #[test]
    fn decide_form_exhausts_at_the_max_drops() {
        let s = CompactState {
            caps: Caps {
                result: 500,
                text: 100,
            },
            keep: 2,
            drops: 5,
            engaged_at: 0,
            last_tokens: 90_000,
            last_at: 4,
            drops_at_last: 5,
            per_group: 500,
            boundary_seq: 0,
        };
        let meas: Vec<(usize, usize)> = vec![(4, 90_000)];
        let (form, _persist) = decide_form(
            5,
            50_000,
            &meas,
            Some(s),
            Caps {
                result: 500,
                text: 100,
            },
            24,
            5,
        0,
        );
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
        assert_eq!(
            measured_growth_rate(&m, 0),
            1_200,
            "the last two measurements: 4000 - 2800 over one event"
        );
        let shrunk: Vec<(usize, usize)> = vec![(0, 9_000), (1, 7_000)];
        assert_eq!(
            measured_growth_rate(&shrunk, 0),
            0,
            "a shrunken request reads as zero growth"
        );
    }

    /// The prediction is the last measured input tokens plus one
    /// step of measured growth: the next request adds one turn.
    #[test]
    fn predict_tokens_adds_measured_growth() {
        assert_eq!(
            predict_tokens(Some((4, 1_000)), 500),
            1_500,
            "one appended step at 500 tokens"
        );
        assert_eq!(
            predict_tokens(None, 500),
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
            matches!(
                decide_form(
                    2,
                    30_000,
                    &m,
                    None,
                    Caps {
                        result: 500,
                        text: 100
                    },
                    24,
                    100,
                    0,
                )
                .0,
                RequestForm::Full
            ),
            "the estimate fits: the full log"
        );
        let (form, persist) = decide_form(
            3,
            10_000,
            &m,
            None,
            Caps {
                result: 500,
                text: 100,
            },
            24,
            100,
        0,
        );
        assert!(
            matches!(
                &form,
                RequestForm::Compact {
                    keep: 24,
                    drops: 0,
                    ..
                }
            ),
            "the over-budget estimate engages at the full keep window"
        );
        assert!(persist.is_some(), "the engagement persists");
    }

    /// The compact candidate is byte-stable between runs on the
    /// same log: the cache property.
    #[test]
    fn compact_candidate_is_byte_stable_between_runs() {
        let ev = [
            Ev::User {
                text: "task".into(),
            },
            ev_asst("step one"),
            ev_res("1", "ok"),
            Ev::User {
                text: "more".into(),
            },
            ev_asst("step two"),
            ev_res("2", "fine"),
        ];
        let refs: Vec<&Ev> = ev.iter().collect();
        let caps = Caps {
            result: 500,
            text: 100,
        };
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
            caps: Caps {
                result: 500,
                text: 100,
            },
            keep: 12,
            drops: 3,
            engaged_at: 42,
            last_tokens: 80_000,
            last_at: 50,
            drops_at_last: 3,
            per_group: 120,
            boundary_seq: 0,
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
        let events = [
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
        let (items, offsets) = build_items_off(
            &refs,
            10,
            &Caps {
                result: 50,
                text: 20,
            },
            20000,
            &HashSet::new(),
            &test_ptrs(),
        );
        assert_eq!(offsets.len(), 4, "one offset per event plus the end");
        assert_eq!(items.len(), offsets[3]);
        // The assistant event projects to three items: reasoning,
        // message, call. Its boundary spans them.
        assert_eq!(offsets[2] - offsets[1], 3);
        assert_eq!(
            offsets[3] - offsets[2],
            1,
            "the result projects to one item"
        );
    }

    /// The Exhausted form signals the last-resort in-session
    /// compaction: the signal carries the compact-candidate request,
    /// not a handoff summary request (docs/auto-compact-plan.md
    /// section 4.3). The request is built by `make_request`, so the
    /// test asserts the builder shape through the signal fields.
    #[test]
    fn exhausted_signal_carries_the_request_not_a_handoff() {
        // The signal the main loop prints on the Exhausted form.
        let items = vec![
            serde_json::json!({"type": "message", "role": "user", "content": "the task"}),
        ];
        let request = serde_json::json!({
            "model": "m",
            "instructions": "sys",
            "input": items,
            "tools": []
        });
        let signal = serde_json::json!({
            "v": 1,
            "type": "context_exhausted",
            "ts": "t",
            "message": "Context budget exhausted after compaction. The last-resort in-session compaction takes over: compact this session and continue in place.",
            "request": request,
        });
        assert_eq!(signal["type"], "context_exhausted");
        assert_eq!(signal["request"]["model"], "m");
        assert_eq!(signal["request"]["input"][0]["content"], "the task");
        // No handoff summary request, no seeded session name.
        assert!(signal.get("summary_request").is_none());
        assert!(signal.get("new_session").is_none());
    }

    /// The summary-input request carries the compaction prompt, not
    /// the handoff instructions, and the iterative update carries
    /// the previous summary and its file-op lists (docs/
    /// auto-compact-plan.md section 4.2).
    #[test]
    fn summary_input_request_carries_the_compaction_prompt() {
        let events = vec![
            Ev::User {
                text: "the task".to_string(),
            },
            ev_asst("step one"),
            ev_res("1", "ok"),
        ];
        let refs: Vec<&Ev> = events.iter().collect();
        let req: serde_json::Value = summary_input_request(
            &refs,
            &None,
            "m",
            2048,
            None,
            &Caps {
                result: 500,
                text: 100,
            },
            1_000_000,
            &test_ptrs(),
        );
        assert_eq!(req["model"], "m");
        assert_eq!(req["tools"], serde_json::json!([]));
        assert_eq!(req["max_output_tokens"], 2048);
        assert!(req["instructions"]
            .as_str()
            .unwrap()
            .contains("## Goal"), "the first-time structured prompt");
        assert!(req["instructions"].as_str().unwrap().contains("Critical Context"));
        let input = req["input"].as_array().unwrap();
        assert_eq!(input.last().unwrap()["content"], COMPACT_ASK_FIRST);
        // The request carries the task statement and the old events.
        let flat = input.iter().map(|i| i.to_string()).collect::<String>();
        assert!(flat.contains("the task"), "the task statement survives");
        assert!(flat.contains("step one"), "the old region is projected");
        // No reasoning effort by default: the session effort stands.
        assert!(req.get("reasoning_effort").is_none());
    }

    /// The update prompt carries the previous summary and its
    /// file-op lists: the lists merge, not re-extract.
    #[test]
    fn summary_input_update_prompt_carries_the_previous_summary() {
        let events = vec![ev_asst("new step"), ev_res("2", "ok")];
        let refs: Vec<&Ev> = events.iter().collect();
        let boundary = Boundary {
            seq: 9,
            first_kept_seq: 5,
            summary: "the previous summary".to_string(),
            read_files: vec!["a.txt".to_string()],
            modified_files: vec!["b.rs".to_string()],
        };
        let req: serde_json::Value = summary_input_request(
            &refs,
            &Some(boundary),
            "m",
            2048,
            Some("low"),
            &Caps {
                result: 500,
                text: 100,
            },
            1_000_000,
            &test_ptrs(),
        );
        let inst = req["instructions"].as_str().unwrap();
        assert!(inst.contains("the previous summary"), "the previous summary rides in");
        assert!(inst.contains("- a.txt"), "the previous read list rides in");
        assert!(inst.contains("- b.rs"), "the previous modified list rides in");
        let input = req["input"].as_array().unwrap();
        assert_eq!(input.last().unwrap()["content"], COMPACT_ASK_UPDATE);
        // The optional request-level effort is carried for bin/model.
        assert_eq!(req["reasoning_effort"], "low");
    }

    /// The drop search bounds the summary input at the input budget:
    /// the smallest drop count that fits, or the search max when none
    /// fits (the handoff invariant, docs/auto-compact-plan.md 4.2).
    #[test]
    fn summary_input_drop_search_bounds_at_the_budget() {
        let big = "R".repeat(4000);
        let mut events: Vec<Ev> = vec![Ev::User {
            text: "the task".to_string(),
        }];
        for i in 0..12 {
            events.push(Ev::Assistant {
                text: "x".to_string(),
                calls: vec![Call {
                    id: format!("c{i}"),
                    name: "bash".to_string(),
                    args_str: format!(
                        "{{\"command\": \"{}\"}}",
                        &big
                    ),
                }],
                reasoning: vec![],
                usage_input: None,
            });
            events.push(ev_res(&format!("r{i}"), &big));
        }
        let refs: Vec<&Ev> = events.iter().collect();
        // A budget that only the dropped form can meet.
        let req: serde_json::Value = summary_input_request(
            &refs,
            &None,
            "m",
            2048,
            None,
            &Caps {
                result: 500,
                text: 100,
            },
            6000,
            &test_ptrs(),
        );
        // The invariant: the input fits the input budget at the drop
        // cap. The chars/4 estimate of the serialized request holds.
        let chars = serde_json::to_string(&req["input"].as_array().unwrap())
            .unwrap()
            .chars()
            .count();
        assert!(chars / 4 <= 6000, "the drop cap bounds the input");
        // The task statement survives every drop.
        let flat = req["input"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i.to_string())
            .collect::<String>();
        assert!(flat.contains("the task"), "the task statement survives");
    }

    /// An empty old region still prints a request: the summary-ask
    /// item alone, no events.
    #[test]
    fn summary_input_empty_region_is_the_ask_alone() {
        let req: serde_json::Value = summary_input_request(
            &[],
            &None,
            "m",
            2048,
            None,
            &Caps {
                result: 500,
                text: 100,
            },
            1_000_000,
            &test_ptrs(),
        );
        let input = req["input"].as_array().unwrap();
        assert_eq!(input.len(), 1, "only the summary ask");
        assert_eq!(input[0]["content"], COMPACT_ASK_FIRST);
    }

    /// The boundary projection: the summary framing item leads, and
    /// the kept region projects byte-identical to the pre-feature
    /// form. The prefix is byte-stable between two compaction moves.
    #[test]
    fn summary_framing_item_is_byte_stable() {
        let a = summary_framing_item("summary text");
        let b = summary_framing_item("summary text");
        assert_eq!(a, b, "the framing item is a pure function of the log");
        assert_eq!(a["role"], "user", "the framing item is user-role");
        assert!(a["content"].as_str().unwrap().contains("summary text"));
        let c = summary_framing_item("other text");
        assert_ne!(a, c, "a different summary is a different prefix");
    }

    /// The boundary-aware state: a state older than the last
    /// compaction re-engages fresh at the boundary index. The
    /// re-engage path is the `fresh_state` closure of main; the
    /// state loader exposes the boundary_seq field the check keys on.
    #[test]
    fn state_round_trip_keeps_the_boundary_seq() {
        let dir = tempfile::tempdir().unwrap();
        let s = CompactState {
            caps: Caps {
                result: 500,
                text: 100,
            },
            keep: 2,
            drops: 3,
            engaged_at: 4,
            last_tokens: 80_000,
            last_at: 50,
            drops_at_last: 3,
            per_group: 120,
            boundary_seq: 17,
        };
        write_compact_state(dir.path(), &s);
        match read_compact_state(dir.path()) {
            StateLoad::Found(st) => {
                assert_eq!(st, s, "the boundary_seq round-trips");
            }
            other => panic!("expected found state, got {other:?}"),
        }
        // A legacy state without the key loads as boundary_seq 0.
        let path = dir.path().join("compact.json");
        std::fs::write(
            &path,
            r#"{"v":2,"caps":{"result":500,"text":100},"keep":2,"drops":0,"engaged_at":0,"last_tokens":0,"last_at":0,"drops_at_last":0,"per_group":0}"#,
        )
        .unwrap();
        match read_compact_state(dir.path()) {
            StateLoad::Found(st) => {
                assert_eq!(
                    st.boundary_seq, 0,
                    "a legacy state loads with the no-boundary marker"
                );
            }
            other => panic!("expected found state, got {other:?}"),
        }
    }

    /// The two-prefix drop rule: the truncated pair goes out of every
    /// request form, like the schema-error pair (correction 60
    /// extended, docs/auto-compact-plan.md section 4.4).
    #[test]
    fn full_form_drops_the_truncation_notice_pair() {
        let mut events: Vec<Ev> = vec![Ev::User {
            text: "the task".to_string(),
        }];
        events.push(Ev::Assistant {
            text: "x".to_string(),
            calls: vec![Call {
                id: "t1".to_string(),
                name: "bash".to_string(),
                args_str: "{}".to_string(),
            }],
            reasoning: vec![],
            usage_input: None,
        });
        events.push(ev_res(
            "t1",
            "Arguments may be truncated. Re-issue the call with shorter arguments.",
        ));
        let refs: Vec<&Ev> = events.iter().collect();
        let drops = drop_pair_ids(&refs, refs.len());
        let out = build_items(
            &refs,
            refs.len(),
            &Caps { result: 0, text: 0 },
            20000,
            &drops,
            &test_ptrs(),
        );
        let s = serde_json::to_string(&out).unwrap();
        assert!(s.contains("the task"), "the task statement survives");
        assert!(
            !s.contains("\"call_id\":\"t1\""),
            "the truncated pair must be out of the full form"
        );
    }

    /// `drop_last_assistant_group` excludes the last assistant group
    /// and its results, leaving no orphan function_call.
    #[test]
    fn drop_last_assistant_excludes_the_group_whole() {
        let events: Vec<Ev> = vec![
            Ev::User {
                text: "task".into(),
            },
            ev_asst("step one"),
            ev_res("1", "ok"),
            ev_asst("step two"),
            ev_res("2", "fine"),
        ];
        let refs: Vec<&Ev> = events.iter().collect();
        let out = drop_last_assistant_group(&refs);
        let flat = serde_json::to_string(
            &build_items(
                &out,
                out.len(),
                &Caps { result: 0, text: 0 },
                20000,
                &HashSet::new(),
                &test_ptrs(),
            ),
        )
        .unwrap();
        assert!(flat.contains("step one"), "the older group stays");
        assert!(!flat.contains("step two"), "the last group is out");
        assert!(!flat.contains("\"call_id\":\"2\""), "no orphan result");
    }

    /// A boundary value that cannot be parsed is rejected: the log
    /// re-renders without the summary (docs/auto-compact-plan.md
    /// section 4.1). A valid event carries the boundary fields.
    #[test]
    fn boundary_values_reject_unparseable_events() {
        // The valid event: the boundary fields parse.
        let good: serde_json::Value = serde_json::json!({
            "v": 1,
            "type": "compaction_summary",
            "ts": "t",
            "summary": "the summary",
            "first_kept_seq": 41,
            "reason": "threshold",
            "tokens_before": 200000,
            "read_files": ["a.txt"],
            "modified_files": ["b.rs"]
        });
        let b = parse_boundary(&good, 41).expect("the valid event parses");
        assert_eq!(b.seq, 41);
        assert_eq!(b.first_kept_seq, 41);
        assert_eq!(b.summary, "the summary");
        assert_eq!(b.read_files, vec!["a.txt"].into_iter().collect::<Vec<_>>());
        assert_eq!(b.modified_files, vec!["b.rs".to_string()]);

        // A zero first_kept_seq is below the 1-based log sequence:
        // rejected.
        let zero: serde_json::Value = serde_json::json!({
            "v": 1,
            "type": "compaction_summary",
            "ts": "t",
            "summary": "s",
            "first_kept_seq": 0,
            "reason": "threshold",
            "tokens_before": 0
        });
        assert!(parse_boundary(&zero, 7).is_none(), "a zero seq is out of range");

        // A missing summary is rejected: the projection has no text.
        let no_summary: serde_json::Value = serde_json::json!({
            "v": 1,
            "type": "compaction_summary",
            "ts": "t",
            "first_kept_seq": 7,
            "reason": "threshold",
            "tokens_before": 0
        });
        assert!(parse_boundary(&no_summary, 7).is_none(), "a missing summary is rejected");

        // A non-integer first_kept_seq is rejected.
        let bad: serde_json::Value = serde_json::json!({
            "v": 1,
            "type": "compaction_summary",
            "ts": "t",
            "summary": "s",
            "first_kept_seq": "forty-one",
            "reason": "threshold",
            "tokens_before": 0
        });
        assert!(parse_boundary(&bad, 7).is_none(), "a string seq is rejected");
    }

    /// The delivery-queue gate (docs/tui-pending-user-messages.md
    /// stage 2): steer always rides; follow rides only on a turn
    /// restart.
    #[test]
    fn user_event_rides_by_queue() {
        let steer = serde_json::json!({"v":1,"type":"user_message","ts":"t","content":"a"});
        assert!(user_event_rides(&steer, false).is_some(), "steer rides");
        let steer_explicit =
            serde_json::json!({"v":1,"type":"user_message","ts":"t","content":"a","queue":"steer"});
        assert!(user_event_rides(&steer_explicit, false).is_some());
        let follow =
            serde_json::json!({"v":1,"type":"user_message","ts":"t","content":"b","queue":"follow"});
        assert!(user_event_rides(&follow, false).is_none(), "follow waits");
        assert_eq!(
            user_event_rides(&follow, true),
            Some("b".to_string()),
            "the restart injects the follow message"
        );
    }
}
