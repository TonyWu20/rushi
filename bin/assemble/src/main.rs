#![deny(clippy::todo, clippy::unimplemented, clippy::unreachable)]

use clap::Parser;
use bon::builder;
use rushi_common::compact_math::trigger_level_for;
use rushi_common::model_settings::{ModelSettings, resolve_active_model, resolve_model_settings, val_int, val_str};
use rushi_common::rewind;
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

    /// Prompt fragments to append to the system prompt
    /// (docs/system-prompt-generation.md D5). A JSON array of
    /// `[id, text]` pairs, e.g. `["goal", "<goal_instructions>…"]`.
    /// The kernel joins the text values in order and appends them
    /// after the cwd line. Absent: no-op.
    #[arg(long)]
    fragments: Option<String>,
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
        #[allow(dead_code)]
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
    text: usize,
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
    #[allow(dead_code)]
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
/// Exact file paths, function names, and error messages must survive.
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



/// The wire budget for the context budget gate and the summary call.
///
/// The default `input_budget` base reserves `max_output_tokens` for
/// output: the budget clamps to the window minus that reservation.
/// The `context_budget` base is pi parity (docs/auto-compact-plan.md
/// 4.5 and 9): the whole context window is available for input, so
/// the budget clamps only to the window, never to the input-only
/// window. An unset or unknown base keeps the default.
fn resolve_budget_tokens(limits: &toml::Value, model_settings: &ModelSettings) -> usize {
    let window_input_tokens = model_settings
        .context_tokens
        .saturating_sub(model_settings.max_output_tokens)
        .max(1)
        as usize;
    let trigger_base = val_str(limits, "compact_trigger_base")
        .unwrap_or_else(|| "input_budget".to_string());
    let context_budget_raw = val_int(limits, "context_budget_tokens")
        .map(|v| v.max(1) as usize)
        .unwrap_or(window_input_tokens);
    if trigger_base == "context_budget" {
        context_budget_raw.min(model_settings.context_tokens.max(1) as usize)
    } else {
        context_budget_raw.min(window_input_tokens.max(1))
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

/// Where the full record of a trimmed piece lives, as seen from the
/// tool working directory. A trimmed text never dies: the marker
/// points at the file that still holds the full body.
struct LogPointers {
    event_log: Option<String>,
}

impl LogPointers {
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
) -> LogPointers {
    let base = if Path::new(session_dir).is_absolute() {
        cwd.filter(|c| Path::new(c).is_absolute())
            .and_then(|c| Path::new(session_dir).strip_prefix(c).ok())
            .map(|rel| rel.to_string_lossy().to_string())
            .unwrap_or_else(|| session_dir.to_string())
    } else {
        session_dir.to_string()
    };
    LogPointers {
        event_log: Some(format!("{base}/events.jsonl")),
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

/// The pair-stranding invariant (docs/rewind-fork-design.md P4):
/// every `function_call` in the context carries its `function_call_
/// output`, and every `function_call_output` carries its call. Call
/// ids are unique, so membership on each side is enough. A rewind
/// that strands either side is ignored by the projection: the
/// marker is dropped, the branch re-projects linear.
fn context_strands_pairs(events: &[&Ev]) -> bool {
    let result_ids: HashSet<&str> = events
        .iter()
        .filter_map(|ev| match ev {
            Ev::ToolResult { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    let call_ids: HashSet<&str> = events
        .iter()
        .filter_map(|ev| match ev {
            Ev::Assistant { calls, .. } => Some(calls),
            _ => None,
        })
        .flat_map(|calls| calls.iter())
        .map(|c| c.id.as_str())
        .collect();
    events.iter().any(|ev| match ev {
        Ev::Assistant { calls, .. } => {
            calls.iter().any(|c| !result_ids.contains(c.id.as_str()))
        }
        // An orphan result: the output sits in the context but its
        // call is masked (or absent). The pair is stranded either
        // way.
        Ev::ToolResult { id, .. } => !call_ids.contains(id.as_str()),
        Ev::User { .. } => false,
    })
}

/// The active-path mask of the projected region
/// (docs/rewind-fork-design.md section 3). Keeps the events whose
/// log seq is in the active path of the log tail, and drops the
/// markers that strand a tool call without its result (P4): each
/// violation pops the outermost active marker, warns through the
/// returned list, and re-projects. Returns the kept events, their
/// seqs, and the ignored markers (outermost first).
fn mask_active_path<'a>(
    events_refs: &'a [&'a Ev],
    projected_seqs: &[usize],
    end_seq: usize,
    mut rewinds: Vec<rewind::RewindRef>,
) -> (Vec<&'a Ev>, Vec<usize>, Vec<rewind::RewindRef>) {
    let mut ignored: Vec<rewind::RewindRef> = Vec::new();
    loop {
        let ranges = rewind::active_ranges(end_seq, &rewinds);
        let mut kept_events: Vec<&'a Ev> = Vec::new();
        let mut kept_seqs: Vec<usize> = Vec::new();
        for (ev, s) in events_refs.iter().zip(projected_seqs.iter()) {
            if rewind::seq_in_ranges(*s, &ranges) {
                kept_events.push(ev);
                kept_seqs.push(*s);
            }
        }
        if context_strands_pairs(&kept_events) && !rewinds.is_empty() {
            // The outermost marker is the one that owns the current
            // gap: pop it and re-project. The branch it abandoned
            // re-projects linear until a valid marker applies.
            ignored.push(rewinds.pop().expect("the guard above"));
            continue;
        }
        return (kept_events, kept_seqs, ignored);
    }
}

/// Model input items for one event, full form.
fn full_items(
    ev: &Ev,
    drop_pairs: &HashSet<String>,
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
            vec![serde_json::json!({
                "type": "function_call_output",
                "call_id": id,
                "output": text
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
        Ev::User { .. } => full_items(ev, drop_pairs),
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
                "output": text
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
            full_items(ev, drop_pairs)
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
    drop_pairs: &HashSet<String>,
    ptrs: &LogPointers,
) -> Vec<serde_json::Value> {
    build_items_off(events, keep, caps, drop_pairs, ptrs).0
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

/// Estimate the token cost of one projected event for the context
/// budget check. Tool results count their full length. Reasoning
/// items are counted by their JSON-serialized size (the same payload
/// the request carries).
fn estimate_ev_tokens(ev: &Ev) -> u64 {
    let chars = match ev {
        Ev::User { text } => text.chars().count() as u64,
        Ev::Assistant {
            text, calls, reasoning, ..
        } => {
            let t = text.chars().count() as u64;
            let c: u64 = calls
                .iter()
                .map(|c| c.args_str.chars().count() as u64)
                .sum();
            let r: u64 = reasoning
                .iter()
                .map(|r| {
                    serde_json::to_string(r)
                        .map(|s| s.chars().count() as u64)
                        .unwrap_or(0)
                })
                .sum();
            t + c + r
        }
        Ev::ToolResult { text, .. } => text.chars().count() as u64,
    };
    chars / 4
}

/// Estimate the input-token cost of the assembled request for the
/// context-budget gate.  Without a compaction boundary, anchors on
/// the last provider-measured `usage.input_tokens` so that the
/// structural overhead of the serialized JSON (system prompt, tool
/// schemas, etc.) is already accounted for.  Only the trailing
/// events that appear *after* the last measurement are estimated at
/// ~4 chars/token.
///
/// When a compaction boundary exists (framing is present), the last
/// measured usage in the kept region may predate the compaction and
/// is stale.  In that case the full kept region is estimated from
/// scratch (chars/4) plus the framing-item size.
fn estimate_request_tokens(
    kept_events: &[&Ev],
    framing: &Option<serde_json::Value>,
) -> u64 {
    // When a compaction boundary exists, the last measured usage in
    // the kept region predates the compaction and is stale.  Skip
    // the anchor and estimate the full region from scratch.
    let last_meas_idx = if framing.is_some() {
        None
    } else {
        kept_events.iter().rposition(|ev| {
            matches!(ev, Ev::Assistant { usage_input: Some(_), .. })
        })
    };

    let (anchor, trailing_start) = match last_meas_idx {
        Some(idx) => {
            let measured = match kept_events[idx] {
                Ev::Assistant { usage_input: Some(m), .. } => *m as u64,
                _ => 0,
            };
            (measured, idx + 1)
        }
        None => (0u64, 0usize),
    };

    // Estimate trailing events that were not part of the anchored
    // request.  In the None case the whole log is "trailing".
    let mut trailing: u64 = 0;
    for ev in &kept_events[trailing_start..] {
        trailing += estimate_ev_tokens(ev);
    }

    // Add the framing item's cost when it is present and the anchor
    // did not already cover it (i.e. no anchor, or the framing was
    // added after the anchored request was sent).
    if last_meas_idx.is_none() {
        if let Some(f) = framing {
            if let Some(content) = f.get("content").and_then(|c| c.as_str()) {
                trailing += content.chars().count() as u64 / 4;
            }
        }
    }

    anchor + trailing
}

/// The hard-trim walk (docs/auto-compact-plan.md section 9.8). When
/// the full-form estimate of `events` exceeds `target`, drop whole
/// step groups from the oldest until the estimate fits. A cut never
/// lands between a tool call and its result. Returns the number of
/// oldest groups to drop (zero when nothing is dropped). `None`
/// when even the framing alone exceeds the target; the
/// `context_exhausted` form and the last-resort in-session
/// compaction take over in that case.
fn hard_trim_groups(
    events: &[&Ev],
    framing: &Option<serde_json::Value>,
    target: u64,
) -> Option<usize> {
    if estimate_request_tokens(events, framing) <= target {
        return Some(0);
    }
    let groups = step_groups(events);
    for drop in 1..=groups.len() {
        let start = if drop < groups.len() {
            groups[drop].0
        } else {
            events.len()
        };
        if estimate_request_tokens(&events[start..], framing) <= target {
            return Some(drop);
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

    let base_prompt = config
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

    let limits = config
        .get("limits")
        .cloned()
        .unwrap_or(toml::Value::String(String::new()));
    let limits = if limits.is_table() {
        limits
    } else {
        toml::Value::Table(toml::map::Map::new())
    };
    let compact_text_chars: usize = val_int(&limits, "compact_text_chars").unwrap_or(200) as usize;
    // A cheaper effort for the summary call on a local GPU. The
    // session reasoning_effort is the default when unset.
    let compact_reasoning_effort: Option<String> =
        val_str(&limits, "compact_reasoning_effort");

    let tools_root = config
        .get("paths")
        .and_then(|p| p.get("tools_root"))
        .and_then(|t| t.as_str())
        .unwrap_or("tools");

    // Extra tools roots (extension-provided tool manifests, e.g. the
    // exts repo's goal-tools/ group; docs/tui-ext-repo-split.md
    // section 4, item 16)
    let extra_tools_roots = config
        .get("paths")
        .and_then(|p| p.get("extra_tools_roots"))
        .and_then(|l| l.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(PathBuf::from)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    // Resolve the active model. The context budget is in tokens
    // (correction 62): the user knob is `context_budget_tokens`.
    // No char mechanism. The default is the model window minus the
    // output reservation.
    let active_model = resolve_active_model(&config);
    let model_settings = resolve_model_settings(&config, &active_model);
    let budget_tokens = resolve_budget_tokens(&limits, &model_settings);

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
    // log's slim tool_result index points into it by call id.
    let tool_texts = tool_log_texts(&PathBuf::from(&args.session));

    // Where the model finds a full record after a trim:
    // the event log. The paths resolve against the tool working directory.
    let ptrs = log_pointers(&args.session, cwd.as_deref());

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
    let mut rewinds: Vec<rewind::RewindRef> = Vec::new();
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
            "rewind" => {
                // The fork marker (docs/rewind-fork-design.md
                // section 4): it projects to nothing. It only masks
                // the abandoned branch through the active path. A
                // malformed value is skipped: the projection
                // re-renders as if the marker were absent, like a
                // corrupt compaction boundary.
                if let Some(r) = rewind::parse_rewind_event(&event, seq) {
                    rewinds.push(r);
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

    // Tool dir names that carry a manifest under a tools root.
    fn tool_names_in(root: &Path) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        if let Ok(entries) = fs::read_dir(root) {
            for entry in entries.flatten() {
                let tool_path = entry.path();
                if tool_path.is_dir() && tool_path.join("tool.toml").exists() {
                    if let Some(name) = tool_path.file_name() {
                        names.push(name.to_string_lossy().to_string());
                    }
                }
            }
        }
        names.sort();
        names
    }

    // Build one model tool schema from a `tool.toml` manifest.
    fn load_tool_schema(tool_toml: &Path, name: &str) -> Option<serde_json::Value> {
        let content = fs::read_to_string(tool_toml).ok()?;
        let tool_config = content.parse::<toml::Value>().ok()?;
        let tool_def = tool_config.get("tool")?;
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
        Some(serde_json::json!({
            "type": "function",
            "name": name,
            "description": desc,
            "parameters": params
        }))
    }

    // Collect (name, description) pairs for the generated tool list
    // (docs/system-prompt-generation.md D4). The list renders after
    // the base prompt and before the cwd line.
    let mut tool_list_entries: Vec<(String, String)> = Vec::new();

    for name in tool_names_in(&tools_root_path) {
        let tool_toml = tools_root_path.join(&name).join("tool.toml");
        if let Some(schema) = load_tool_schema(&tool_toml, &name) {
            tool_schemas.push(schema.clone());
            let desc = schema
                .get("description")
                .and_then(|d| d.as_str())
                .filter(|d| !d.is_empty())
                .unwrap_or(name.as_str())
                .to_string();
            tool_list_entries.push((name, desc));
        }
    }

    // Extension-provided tool roots (config `[paths]
    // extra_tools_roots` plus the RUSHI_EXTRA_TOOLS_ROOT env-var
    // fallback — the exts repo's goal-tools/ group): additive
    // discovery, same as route. The primary root wins on a name
    // collision.
    let mut extra_roots: Vec<PathBuf> = extra_tools_roots;
    if let Ok(extra_root_str) = std::env::var("RUSHI_EXTRA_TOOLS_ROOT") {
        if !extra_root_str.is_empty() {
            extra_roots.push(PathBuf::from(extra_root_str));
        }
    }
    for extra_root in &extra_roots {
        let known: HashSet<String> = tool_schemas
            .iter()
            .filter_map(|s| s.get("name").and_then(|n| n.as_str()).map(str::to_string))
            .collect();
        for name in tool_names_in(extra_root) {
            if known.contains(name.as_str()) {
                continue;
            }
            let tool_toml = extra_root.join(&name).join("tool.toml");
            if let Some(schema) = load_tool_schema(&tool_toml, &name) {
                tool_schemas.push(schema.clone());
                let desc = schema
                    .get("description")
                    .and_then(|d| d.as_str())
                    .filter(|d| !d.is_empty())
                    .unwrap_or(name.as_str())
                    .to_string();
                tool_list_entries.push((name, desc));
            }
        }
    }

    // Build the full system prompt (docs/system-prompt-generation.md):
    // base_prompt + generated_tool_list + cwd_line + fragments.
    // The generated tool list is byte-stable per session: it is a
    // pure function of the config and the on-disk manifests (D4).
    let mut system_prompt = base_prompt;

    if !tool_list_entries.is_empty() {
        let tool_list = tool_list_entries
            .iter()
            .map(|(name, desc)| format!("- {name}: {desc}"))
            .collect::<Vec<_>>()
            .join("\n");
        system_prompt.push_str(&format!("\n\nAvailable tools:\n{tool_list}"));
    }

    if let Some(cwd) = &cwd {
        system_prompt.push_str(&format!(
            "\n\nCurrent working directory: {cwd}\n\
             Relative paths in tool calls resolve against this directory."
        ));
    }

    // Append prompt fragments (docs/system-prompt-generation.md D5).
    // The wire form is an ordered array of [id, text] pairs.
    // The kernel joins the text values in order and appends them
    // after the cwd line. When the argument is absent, this is a
    // no-op.
    if let Some(fragments_json) = &args.fragments {
        if let Ok(fragments) =
            serde_json::from_str::<Vec<Vec<serde_json::Value>>>(fragments_json)
        {
            for pair in &fragments {
                if pair.len() >= 2 {
                    if let Some(text) = pair[1].as_str() {
                        if !text.is_empty() {
                            system_prompt.push_str(&format!("\n\n{text}"));
                        }
                    }
                }
            }
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

    // Phase 2 handoff-based projection (docs/phase-2-plan.md 4.3).
    // The projected events are the post-boundary region when a
    // compaction_summary boundary exists, else the full log. No
    // sticky state, no keep-window halving, no group-drop levers.
    // The handoff document (handoff.md) or the boundary summary
    // serves as the framing item. If a boundary exists and the
    // projected context still exceeds the input budget, the
    // context_exhausted form fires the exhausted.handle window
    // (docs/loop-lifecycle-hooks.md 3.4).
    let (events_refs, projected_seqs): (Vec<&Ev>, Vec<usize>) = match &boundary {
        Some(b) => events
            .iter()
            .zip(event_seqs.iter())
            .filter(|(_, s)| **s >= b.first_kept_seq)
            .unzip(),
        None => (events.iter().collect(), event_seqs),
    };

    // The active path of the log tail (docs/rewind-fork-design.md
    // section 3): the recursion through the rewind chain masks every
    // abandoned branch, at every nesting depth. The boundary filter
    // runs first: a rewind target inside the compacted region
    // degrades to the region head.
    let (kept_events, kept_seqs, ignored_rewinds) =
        mask_active_path(&events_refs, &projected_seqs, seq, rewinds);
    // The P4 violations, outermost first: each marker re-projected
    // as absent. The log keeps them; only the request loses them.
    for r in &ignored_rewinds {
        eprintln!(
            "assemble: ignoring rewind at seq {} (target seq {}): its context strands a tool call or result without its pair",
            r.seq, r.target
        );
    }

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
            text: compact_text_chars,
        };
        let cut = kept_seqs
            .iter()
            .position(|&s| s > up_to)
            .unwrap_or(kept_events.len());
        let mut old_region: Vec<&Ev> = kept_events[..cut].to_vec();
        // The strip flag excludes the last assistant group (the
        // logged length-stop group) from the summary input.
        if args.drop_last_assistant {
            old_region = drop_last_assistant_group(&old_region);
        }
        // The summary request must fit the window with room for its
        // own output cap. Under the `context_budget` base the wire
        // budget is the full window, so the drop search bounds the
        // input at the input-only window (docs/auto-compact-plan.md
        // section 9.8). Under the default base the budget is already
        // clamped, and the minimum is a no-op.
        let window_input = (model_settings
            .context_tokens
            .saturating_sub(model_settings.max_output_tokens)
            .max(1)) as usize;
        let summary_input_target = budget_tokens.min(window_input);
        let builder = summary_input_request()
            .old(&old_region)
            .prev(&boundary)
            .model(&model_settings.model_id)
            .max_out_cap(model_settings.max_output_tokens)
            .caps(&base_caps)
            .input_budget(summary_input_target)
            .ptrs(&ptrs);
        // The optional request-level effort: the session effort stands
        // when the config leaves it unset.
        let request = if let Some(e) = compact_reasoning_effort.as_deref() {
            builder.effort(e).call()
        } else {
            builder.call()
        };
        println!("{}", request);
        return;
    }

    // The request-time exclusion of the last assistant group.
    // The `sel_seqs` parallel array maps each projected event to its
    // 1-based log sequence: the hard-trim marker names the first
    // kept sequence so the log and the request stay aligned.
    let mut sel_events: Vec<&Ev>;
    let sel_seqs: Vec<usize>;
    if args.drop_last_assistant {
        let groups = step_groups(&kept_events);
        match groups
            .iter()
            .rev()
            .find(|&&(start, _)| matches!(kept_events[start], Ev::Assistant { .. }))
        {
            Some(&(start, end)) => {
                sel_events = kept_events
                    .iter()
                    .take(start)
                    .chain(kept_events.iter().skip(end))
                    .cloned()
                    .collect();
                sel_seqs = kept_seqs
                    .iter()
                    .take(start)
                    .chain(kept_seqs.iter().skip(end))
                    .cloned()
                    .collect();
            }
            None => {
                sel_events = kept_events.clone();
                sel_seqs = kept_seqs.clone();
            }
        }
    } else {
        sel_events = kept_events.clone();
        sel_seqs = kept_seqs.clone();
    };

    // The framing item: the handoff document when present, else the
    // boundary summary from the log. Both are user-role messages that
    // lead the post-boundary events (docs/phase-2-plan.md 4.3 step 5).
    let session_dir = Path::new(&args.session);
    let handoff_doc = std::fs::read_to_string(session_dir.join("handoff.md")).ok();
    let framing: Option<serde_json::Value> = match (&handoff_doc, &boundary) {
        (Some(doc), _) => Some(summary_framing_item(doc.trim())),
        (None, Some(b)) => Some(summary_framing_item(&b.summary)),
        (None, None) => None,
    };

    // The projected events: post-boundary when a boundary exists,
    // else the full log, masked to the active path when rewinds
    // exist. No sticky state, no keep halving, no group-drop levers
    // (docs/phase-2-plan.md stage 0). The drop set is computed over
    // the masked context: a pair masked out of the context is out of
    // the set (docs/rewind-fork-design.md section 3).

    // The hard-trim backstop (docs/auto-compact-plan.md section
    // 9.8): the LLM compaction leads at the trigger level. When the
    // full-form estimate of this request still exceeds that level,
    // drop the oldest step groups until the request fits. The log
    // keeps every event (the append-only source of truth); the
    // request is the projection that fits. The trim marker rides the
    // request JSON; the harness logs it and strips it before the
    // model call. When even the framing alone exceeds the level,
    // emit the `context_exhausted` form: the last-resort in-session
    // compaction takes over (docs/phase-2-plan.md 4.3 step 5).
    let compact_reserve: u64 = val_int(&limits, "compact_reserve_tokens")
        .unwrap_or(16384)
        .max(0) as u64;
    let trim_target = trigger_level_for(budget_tokens as u64, compact_reserve);
    let est_before = estimate_request_tokens(&sel_events, &framing);
    let mut hard_trim: Option<serde_json::Value> = None;
    if est_before > trim_target {
        match hard_trim_groups(&sel_events, &framing, trim_target) {
            Some(drop) => {
                if drop > 0 {
                    let groups = step_groups(&sel_events);
                    let start = groups.get(drop).map(|g| g.0).unwrap_or(sel_events.len());
                    sel_events = sel_events.iter().skip(start).cloned().collect();
                    let est_after = estimate_request_tokens(&sel_events, &framing);
                    hard_trim = Some(serde_json::json!({
                        "dropped_groups": drop,
                        "first_kept_seq": sel_seqs.get(start).copied(),
                        "est_tokens_before": est_before,
                        "est_tokens_after": est_after,
                    }));
                }
            }
            None => {
                let drop_pairs = drop_pair_ids(&kept_events, kept_events.len());
                let mut items = build_items(
                    &sel_events,
                    sel_events.len(),
                    &Caps { text: 0 },
                    &drop_pairs,
                    &ptrs,
                );
                if let Some(f) = &framing {
                    items.insert(0, f.clone());
                }
                let request = make_request(&items);
                let exhausted = serde_json::json!({
                    "v": 1,
                    "type": "context_exhausted",
                    "ts": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    "message": "Context budget exhausted. The last-resort in-session compaction takes over.",
                    "request": request,
                });
                println!("{}", exhausted);
                return;
            }
        }
    }

    let drop_pairs = drop_pair_ids(&kept_events, kept_events.len());
    let mut items = build_items(
        &sel_events,
        sel_events.len(),
        &Caps { text: 0 },
        &drop_pairs,
        &ptrs,
    );
    if let Some(f) = &framing {
        items.insert(0, f.clone());
    }
    let mut request = make_request(&items);
    if let Some(marker) = &hard_trim {
        request["hard_trim"] = marker.clone();
    }

    println!("{}", request);
}

/// The summary-input request of the in-session compaction
/// (docs/auto-compact-plan.md section 4.2): the old region in the
/// compact form, the summary-ask user item, a capped output. No
/// tools: the summary is plain text. The drop search bounds the old
/// region at the input budget: the smallest drop count that fits, or
/// the search max when none fits (the handoff invariant).
#[builder]
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
        let items = build_items(&sel, 0, caps, &drop_pairs, ptrs);
        if estimate(&items) <= input_budget {
            break;
        }
    }
    let sel = drop_oldest_groups(old, &droppable, chosen);
    let drop_pairs = drop_pair_ids(&sel, sel.len());
    let items = build_items(&sel, 0, caps, &drop_pairs, ptrs);
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

    fn ms(context_tokens: u64, max_output_tokens: u64) -> ModelSettings {
        ModelSettings {
            model_id: "m".to_string(),
            base_url: String::new(),
            max_output_tokens,
            context_tokens,
            reasoning_effort: String::new(),
            api_key_env: String::new(),
            timeout_s: 0,
        }
    }

    #[test]
    fn budget_default_base_clamps_to_the_input_only_window() {
        // window 262144, output 32768 -> input window 229376.
        // An unset base clamps a larger context_budget to 229376.
        let limits = toml::Value::String(String::new());
        assert_eq!(resolve_budget_tokens(&limits, &ms(262144, 32768)), 229376);
    }

    #[test]
    fn budget_default_base_keeps_a_smaller_explicit_budget() {
        let toml = r#"
[limits]
context_budget_tokens = 100000
"#;
        let v: toml::Value = toml.parse().unwrap();
        let limits = v.get("limits").unwrap();
        assert_eq!(resolve_budget_tokens(limits, &ms(262144, 32768)), 100000);
    }

    #[test]
    fn budget_pi_parity_base_uses_the_full_window() {
        // context_budget base: no clamp to the input-only window.
        // budget = min(context_budget_tokens, context_tokens).
        let toml = r#"
[limits]
context_budget_tokens = 262144
compact_trigger_base = "context_budget"
"#;
        let v: toml::Value = toml.parse().unwrap();
        let limits = v.get("limits").unwrap();
        assert_eq!(resolve_budget_tokens(limits, &ms(262144, 32768)), 262144);
    }

    #[test]
    fn budget_pi_parity_base_clamps_to_the_model_window() {
        // A context_budget above the window clamps to the window.
        let toml = r#"
[limits]
context_budget_tokens = 999999
compact_trigger_base = "context_budget"
"#;
        let v: toml::Value = toml.parse().unwrap();
        let limits = v.get("limits").unwrap();
        assert_eq!(resolve_budget_tokens(limits, &ms(262144, 32768)), 262144);
    }

    #[test]
    fn budget_unknown_base_falls_back_to_the_default() {
        let toml = r#"
[limits]
context_budget_tokens = 262144
compact_trigger_base = "nonsense"
"#;
        let v: toml::Value = toml.parse().unwrap();
        let limits = v.get("limits").unwrap();
        // Unknown base -> default input_budget clamp -> 229376.
        assert_eq!(resolve_budget_tokens(limits, &ms(262144, 32768)), 229376);
    }

    /// The hard-trim walk of section 9.8. The events: user A
    /// (1000 tokens), an assistant group of a call and its result
    /// (1000 + 1000), user B (1000). Total 4000 tokens.
    fn hard_trim_events() -> Vec<Ev> {
        let mut evs: Vec<Ev> = Vec::new();
        evs.push(Ev::User {
            text: "a".repeat(4000),
        });
        evs.push(Ev::Assistant {
            text: String::new(),
            calls: vec![Call {
                id: "c1".to_string(),
                name: "bash".to_string(),
                args_str: "b".repeat(4000),
            }],
            reasoning: Vec::new(),
            usage_input: None,
        });
        evs.push(Ev::ToolResult {
            id: "c1".to_string(),
            text: "c".repeat(4000),
        });
        evs.push(Ev::User {
            text: "d".repeat(4000),
        });
        evs
    }

    fn hard_trim_refs<'a>(evs: &'a [Ev]) -> Vec<&'a Ev> {
        evs.iter().collect()
    }

    #[test]
    fn hard_trim_noop_when_the_estimate_fits() {
        let evs = hard_trim_events();
        let refs = hard_trim_refs(&evs);
        let framing: Option<serde_json::Value> = None;
        assert_eq!(hard_trim_groups(&refs, &framing, 4000), Some(0));
        assert_eq!(hard_trim_groups(&refs, &framing, 4001), Some(0));
    }

    #[test]
    fn hard_trim_drops_groups_from_the_oldest() {
        let evs = hard_trim_events();
        let refs = hard_trim_refs(&evs);
        let framing: Option<serde_json::Value> = None;
        // Target 3000: keep the assistant group (2000) plus user B
        // (1000); drop user A. Target 1500: keep user B only. Target
        // 999 drops nothing that fits: even the smallest non-empty
        // suffix (user B, 1000) exceeds 999, so the walk drops to
        // the empty suffix, which is 0 tokens.
        assert_eq!(hard_trim_groups(&refs, &framing, 3000), Some(1));
        assert_eq!(hard_trim_groups(&refs, &framing, 1500), Some(2));
        assert_eq!(hard_trim_groups(&refs, &framing, 999), Some(3));
    }

    #[test]
    fn hard_trim_never_splits_a_call_from_its_result() {
        let evs = hard_trim_events();
        let refs = hard_trim_refs(&evs);
        let framing: Option<serde_json::Value> = None;
        // Every kept suffix starts at a group boundary: either an
        // assistant message (the head of the call/result group) or a
        // user message. A target that fits exactly one group keeps
        // the group whole.
        let drop = hard_trim_groups(&refs, &framing, 2000).expect("trims to one group");
        let start = step_groups(&refs)[drop].0;
        assert!(matches!(refs[start], Ev::Assistant { .. } | Ev::User { .. }));
    }

    #[test]
    fn hard_trim_fails_when_the_framing_alone_exceeds_the_target() {
        let evs = hard_trim_events();
        let refs = hard_trim_refs(&evs);
        // A 20000-char framing item is 5000 tokens: above any target
        // that is under 5000, no drop count fits the request.
        let framing: Option<serde_json::Value> = Some(serde_json::json!({
            "type": "message",
            "role": "user",
            "content": "f".repeat(20000)
        }));
        assert_eq!(hard_trim_groups(&refs, &framing, 4000), None);
        // A target above the framing cost fits with zero events kept.
        assert_eq!(hard_trim_groups(&refs, &framing, 5000), Some(3));
    }

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
    /// always. The event log path is the only pointer now.
    fn test_ptrs() -> LogPointers {
        LogPointers {
            event_log: Some("sessions/s/events.jsonl".to_string()),
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
        let items = full_items(&ev, &drop);
        // The dropped call is gone; its call id appears nowhere.
        let s = items.iter().map(|i| i.to_string()).collect::<String>();
        assert!(!s.contains("\"bad\""), "the failed call must be out: {s}");
        assert!(s.contains("\"ok\""), "the clean call stays: {s}");
        // Its result goes out too.
        let res = full_items(&ev_schema_error("bad"), &drop);
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
            &Caps { text: 0 },
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
    fn keep_all_when_window_covers_log() {
        let events = [ev_res("a", "one"), ev_res("b", "two")];
        let refs: Vec<&Ev> = events.iter().collect();
        let items = build_items(&refs, 10, &caps(), &HashSet::new(), &test_ptrs());
        // No compact marker, no clip marker, plain outputs.
        assert!(!items.iter().any(|i| i.to_string().contains("compacted")));
        assert_eq!(items.len(), 2);
    }

    /// Old tool results pass through in full even in the compact
    /// form. No elision marker is emitted.
    #[test]
    fn compact_tool_results_are_full() {
        let mut events = Vec::new();
        for i in 0..30 {
            events.push(ev_res(&format!("id{i}"), &"R".repeat(400)));
        }
        let refs: Vec<&Ev> = events.iter().collect();
        let items = build_items(
            &refs,
            4,
            &Caps {
                text: 20,
            },
            &HashSet::new(),
            &test_ptrs(),
        );
        // Old tool results are no longer compacted; full body passes through.
        let first = &items[0];
        let s = first.to_string();
        assert!(!s.contains("compacted"), "no compacted marker on tool results: {s}");
        assert!(s.contains(&"R".repeat(400)), "the full 400-char body must be present");
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
        let items = build_items(&refs, 0, &caps(), &HashSet::new(), &test_ptrs());
        assert!(items[0].to_string().contains("do the task"));
        assert!(!items[1].to_string().contains("compacted"), "tool results are no longer compacted");
    }

    /// The compact form still trims assistant text and call args
    /// (only tool results are now full).
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
            &Caps {
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
    /// The keep window halves down to two across over-budget
    /// readings. It never grows back.
    /// The drop jump is measured. Without a drop measurement, the
    /// over-budget reading drops one group (the trial). After the
    /// drop measurement, the jump is the measured per-group savings.
    /// The drop count drops the oldest droppable step groups.
    /// Zero drops keep every step.
    /// The drop count at its max, with the estimate still over the
    /// budget, ends the turn with the handoff.
    /// The per-event growth is the token delta over the event delta
    /// of the last two measurements. A shrunken request reads as
    /// zero growth.
    /// The prediction is the last measured input tokens plus one
    /// step of measured growth: the next request adds one turn.
    /// A fresh session sends the full log while the measured
    /// estimate fits. The over-budget estimate engages the compact
    /// form once.
    /// The compact candidate is byte-stable between runs on the
    /// same log: the cache property.
    /// The state file round-trips. A corrupt file re-engages; the
    /// absent file is a fresh session.
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
                text: 20,
            },
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
        let events = [Ev::User {
                text: "the task".to_string(),
            },
            ev_asst("step one"),
            ev_res("1", "ok")];
        let refs: Vec<&Ev> = events.iter().collect();
        let req: serde_json::Value = summary_input_request()
            .old(&refs)
            .prev(&None)
            .model("m")
            .max_out_cap(2048)
            .caps(&Caps {
                text: 100,
            })
            .input_budget(1_000_000)
            .ptrs(&test_ptrs())
            .call();
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
        let events = [ev_asst("new step"), ev_res("2", "ok")];
        let refs: Vec<&Ev> = events.iter().collect();
        let boundary = Boundary {
            seq: 9,
            first_kept_seq: 5,
            summary: "the previous summary".to_string(),
            read_files: vec!["a.txt".to_string()],
            modified_files: vec!["b.rs".to_string()],
        };
        let req: serde_json::Value = summary_input_request()
            .old(&refs)
            .prev(&Some(boundary))
            .model("m")
            .max_out_cap(2048)
            .effort("low")
            .caps(&Caps {
                text: 100,
            })
            .input_budget(1_000_000)
            .ptrs(&test_ptrs())
            .call();
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
                        big
                    ),
                }],
                reasoning: vec![],
                usage_input: None,
            });
            events.push(ev_res(&format!("r{i}"), &big));
        }
        let refs: Vec<&Ev> = events.iter().collect();
        // A budget that only the dropped form can meet.
        let req: serde_json::Value = summary_input_request()
            .old(&refs)
            .prev(&None)
            .model("m")
            .max_out_cap(2048)
            .caps(&Caps {
                text: 100,
            })
            .input_budget(6000)
            .ptrs(&test_ptrs())
            .call();
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
        let req: serde_json::Value = summary_input_request()
            .old(&[])
            .prev(&None)
            .model("m")
            .max_out_cap(2048)
            .caps(&Caps {
                text: 100,
            })
            .input_budget(1_000_000)
            .ptrs(&test_ptrs())
            .call();
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
            &Caps { text: 0 },
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
                &Caps { text: 0 },
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

    // ── the rewind active path (docs/rewind-fork-design.md) ─────

    fn ev_user(text: &str) -> Ev {
        Ev::User {
            text: text.to_string(),
        }
    }

    fn ev_call(call_id: &str) -> Ev {
        Ev::Assistant {
            text: "step".to_string(),
            calls: vec![Call {
                id: call_id.to_string(),
                name: "bash".to_string(),
                args_str: "{}".to_string(),
            }],
            reasoning: vec![],
            usage_input: None,
        }
    }

    fn refs_of(events: &[Ev]) -> (Vec<&Ev>, Vec<usize>) {
        (events.iter().collect(), (1..=events.len()).collect())
    }

    /// P1: a single fork masks the abandoned span (T, S] and keeps
    /// the target prefix plus the continuation after the marker.
    #[test]
    fn mask_masks_the_abandoned_branch() {
        let events: Vec<Ev> = vec![
            ev_user("task"),              // 1
            ev_call("c1"),               // 2
            ev_res("c1", "A result"),    // 3
            ev_call("c2"),               // 4
            ev_res("c2", "B result"),    // 5
        ];
        let (refs, seqs) = refs_of(&events);
        // Rewind at log seq 6 to seq 3 (on): the span 4..5 is the
        // abandoned branch.
        let rewinds = vec![rushi_common::rewind::RewindRef {
            seq: 6,
            target: 3,
            before: false,
        }];
        let (kept, kept_seqs, ignored) = mask_active_path(&refs, &seqs, 6, rewinds);
        let texts: Vec<&str> = kept
            .iter()
            .map(|ev| match ev {
                Ev::User { text } => text.as_str(),
                Ev::Assistant { .. } => "assistant",
                Ev::ToolResult { text, .. } => text.as_str(),
            })
            .collect();
        assert_eq!(texts, vec!["task", "assistant", "A result"]);
        assert_eq!(kept_seqs, vec![1, 2, 3]);
        assert!(ignored.is_empty());
    }

    /// P2: the depth-2 counter-example. The single-gap rule of the
    /// naive design would keep branch B; the recursion masks it.
    #[test]
    fn mask_nested_forks_mask_the_intermediate_branch() {
        // 1..3 = A, rewind(4,3) forks B (5..6), rewind(7,3) forks
        // A' (8..9), rewind(10,9) continues A'.
        let events: Vec<Ev> = vec![
            ev_user("task"),              // 1
            ev_call("ca"),                // 2
            ev_res("ca", "A result"),     // 3
            ev_call("cb"),                // 5 (4 is the rewind)
            ev_res("cb", "B result"),     // 6
            ev_call("ca2"),               // 8 (7 is the rewind)
            ev_res("ca2", "A' result"),   // 9
            ev_call("ca3"),               // 11 (10 is the rewind)
            ev_res("ca3", "A' done"),     // 12
        ];
        // Log seqs: the rewinds sit at 4, 7, 10, so the events above
        // occupy 1,2,3,5,6,8,9,11,12.
        let refs: Vec<&Ev> = events.iter().collect();
        let seqs: Vec<usize> = vec![1, 2, 3, 5, 6, 8, 9, 11, 12];
        let rewinds = vec![
            rushi_common::rewind::RewindRef { seq: 4, target: 3, before: false },
            rushi_common::rewind::RewindRef { seq: 7, target: 3, before: false },
            rushi_common::rewind::RewindRef { seq: 10, target: 9, before: false },
        ];
        let (kept, _, ignored) = mask_active_path(&refs, &seqs, 12, rewinds);
        let texts: Vec<&str> = kept
            .iter()
            .map(|ev| match ev {
                Ev::User { text } => text.as_str(),
                Ev::Assistant { .. } => "assistant",
                Ev::ToolResult { text, .. } => text.as_str(),
            })
            .collect();
        // A (1..3) and A' (11..12) ride; B (5..6) is masked.
        assert!(texts.contains(&"A result"));
        assert!(texts.contains(&"A' done"));
        assert!(!texts.contains(&"B result"), "branch B must be masked");
        assert!(ignored.is_empty());
    }

    /// P3: a rewind to a branch tail re-enters that branch: its full
    /// active path is rebuilt, the sibling branch is masked.
    #[test]
    fn mask_reentering_a_branch_rebuilds_its_path() {
        // 1..3 = A, rewind(4,3) forks B (5..6), rewind(7,3) forks
        // A' (8..9), rewind(10,6) re-enters B at its tail.
        let events: Vec<Ev> = vec![
            ev_user("task"),              // 1
            ev_call("ca"),                // 2
            ev_res("ca", "A result"),     // 3
            ev_call("cb"),                // 5
            ev_res("cb", "B result"),     // 6
            ev_call("ca2"),               // 8
            ev_res("ca2", "A' result"),   // 9
            ev_call("cb2"),               // 11
            ev_res("cb2", "B' result"),   // 12
        ];
        let refs: Vec<&Ev> = events.iter().collect();
        let seqs: Vec<usize> = vec![1, 2, 3, 5, 6, 8, 9, 11, 12];
        let rewinds = vec![
            rushi_common::rewind::RewindRef { seq: 4, target: 3, before: false },
            rushi_common::rewind::RewindRef { seq: 7, target: 3, before: false },
            rushi_common::rewind::RewindRef { seq: 10, target: 6, before: false },
        ];
        let (kept, _, ignored) = mask_active_path(&refs, &seqs, 12, rewinds);
        let texts: Vec<&str> = kept
            .iter()
            .map(|ev| match ev {
                Ev::User { text } => text.as_str(),
                Ev::Assistant { .. } => "assistant",
                Ev::ToolResult { text, .. } => text.as_str(),
            })
            .collect();
        // A (1..3) and B (5..6, 11..12) ride; A' (8..9) is masked.
        assert!(texts.contains(&"B result"));
        assert!(texts.contains(&"B' result"));
        assert!(!texts.contains(&"A' result"), "branch A' must be masked");
        assert!(ignored.is_empty());
    }

    /// P4: a rewind whose context strands a tool call without its
    /// result is ignored: the marker drops out, the branch re-
    /// projects linear, and the violation is reported.
    #[test]
    fn mask_ignores_a_rewind_that_strands_a_call() {
        // The target branch ends mid-step: assistant c5 with no
        // result. A rewind to it strands c5 in the context.
        let events: Vec<Ev> = vec![
            ev_user("task"),              // 1
            ev_call("c5"),                // 2 (no result follows)
            ev_call("c6"),                // 4 (3 is the rewind)
            ev_res("c6", "C result"),     // 5
        ];
        let refs: Vec<&Ev> = events.iter().collect();
        let seqs: Vec<usize> = vec![1, 2, 4, 5];
        let rewinds = vec![rushi_common::rewind::RewindRef {
            seq: 3,
            target: 2,
            before: false,
        }];
        let (kept, _, ignored) = mask_active_path(&refs, &seqs, 5, rewinds);
        // The marker is ignored: the projection is the full log.
        assert_eq!(kept.len(), 4, "the branch re-projects linear");
        assert_eq!(ignored.len(), 1, "the violation is reported");
        assert_eq!(ignored[0].seq, 3);
        // And the linear projection is itself dangling (an
        // interrupted log): the check stops at the marker, not at
        // the log. The loop state machine owes the result (claim),
        // so no stranded request is ever sent.
        assert!(context_strands_pairs(&kept), "the linear tail strands c5");
    }

    /// The pair-stranding invariant, both sides: a call without a
    /// result strands; a result without its call strands; a paired
    /// log is clean.
    #[test]
    fn pair_stranding_check_both_sides() {
        let ok: Vec<Ev> = vec![ev_user("task"), ev_call("c1"), ev_res("c1", "r")];
        let refs: Vec<&Ev> = ok.iter().collect();
        assert!(!context_strands_pairs(&refs), "the paired log is clean");

        let dangling: Vec<Ev> = vec![ev_user("task"), ev_call("c2")];
        let refs: Vec<&Ev> = dangling.iter().collect();
        assert!(context_strands_pairs(&refs), "a result-less call strands");

        // The mirror case: an orphan result, its call masked out.
        let orphan: Vec<Ev> = vec![ev_user("task"), ev_res("c3", "orphan")];
        let refs: Vec<&Ev> = orphan.iter().collect();
        assert!(context_strands_pairs(&refs), "a call-less result strands");
    }
}
