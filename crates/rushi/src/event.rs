//! Typed event vocabulary for the session log.
//!
//! Every line in `events.jsonl` is one `Event`. The enum is internally
//! tagged on `"type"` so the wire format matches the existing JSONL.
//! `parse_event`, i.e. `serde_json::from_str::<Event>(line)`, is the
//! single validation step. The old JSON-Schema `event_validation` module
//! is retired.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One line in `events.jsonl`.
///
/// Internally tagged: the `"type"` field in the JSON selects the
/// variant. All variant structs carry their own `v` and `ts` fields,
/// so the serialized form is a flat object — no wrapper.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    UserMessage(UserMessage),
    AssistantMessage(AssistantMessage),
    ToolCall(ToolCall),
    ToolResult(ToolResult),
    Error(ErrorEvent),
    ExtStatus(ExtStatus),
    CompactionStarted(CompactionStarted),
    CompactionSummary(CompactionSummary),
    CompactionFailed(CompactionFailed),
    ContextExhausted(ContextExhausted),
    ApprovalRequest(ApprovalRequest),
    Approval(Approval),
    Rewind(Rewind),
    UserMessageRetract(UserMessageRetract),
}

/// Parse one JSONL line into a typed `Event`.
///
/// Returns a `serde_json::Error` on malformed input (missing a needed
/// field, unknown tag, wrong type).
pub fn parse_event(line: &str) -> Result<Event, serde_json::Error> {
    serde_json::from_str(line)
}

/// The 14 known event-type tags, in the order the schema files are
/// named. Useful for diagnostics and for `route` / `log` CLI output.
pub const EVENT_TYPES: &[&str] = &[
    "user_message",
    "assistant_message",
    "tool_call",
    "tool_result",
    "error",
    "ext_status",
    "compaction_started",
    "compaction_summary",
    "compaction_failed",
    "context_exhausted",
    "approval_request",
    "approval",
    "rewind",
    "user_message_retract",
];

// ----------------------------------------------------------------------
// user-facing
// ----------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserMessage {
    pub v: u8,
    pub ts: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue: Option<Queue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Queue {
    Steer,
    Follow,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserMessageRetract {
    pub v: u8,
    pub ts: String,
    /// The `id` of the `user_message` being retracted.
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

// ----------------------------------------------------------------------
// assistant
// ----------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantMessage {
    pub v: u8,
    pub ts: String,
    pub content: String,
    pub tool_calls: Vec<InlineToolCall>,
    pub stop_reason: StopReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Vec<ReasoningItem>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

/// One tool call inside an `assistant_message`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InlineToolCall {
    pub id: String,
    pub name: String,
    /// Free-form JSON object.
    pub arguments: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    Stop,
    ToolCalls,
    Length,
    Error,
    Aborted,
}

/// One reasoning block inside `AssistantMessage.reasoning`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningItem {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encrypted_content: Option<Value>,
}

/// Token usage from the model provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u64>,
}

// ----------------------------------------------------------------------
// tool I/O
// ----------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub v: u8,
    pub ts: String,
    pub id: String,
    pub name: String,
    /// Free-form JSON object.
    pub arguments: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub v: u8,
    pub ts: String,
    pub id: String,
    /// The tool's output. A JSON object with optional `text` /
    /// `details` keys, or the tool's stdout parsed as JSON.
    pub value: Value,
    pub is_error: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_log: Option<String>,
}

// ----------------------------------------------------------------------
// errors and status
// ----------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorEvent {
    pub v: u8,
    pub ts: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtStatus {
    pub v: u8,
    pub ts: String,
    pub id: String,
    /// Shared UI state. Any JSON value is allowed.
    pub value: Value,
}

// ----------------------------------------------------------------------
// compaction
// ----------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompactReason {
    Threshold,
    Overflow,
    /// The context-exhausted last-resort compact.
    LastResort,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionStarted {
    pub v: u8,
    pub ts: String,
    pub reason: CompactReason,
    pub tokens_before: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionSummary {
    pub v: u8,
    pub ts: String,
    pub summary: String,
    pub first_kept_seq: u64,
    pub version: u64,
    pub parent_version: u64,
    pub diverge_seq: u64,
    pub reason: CompactReason,
    pub tokens_before: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_after: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_files: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_files: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionFailed {
    pub v: u8,
    pub ts: String,
    pub reason: CompactReason,
    pub detail: String,
    pub last_user_seq: u64,
}

// ----------------------------------------------------------------------
// context
// ----------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextExhausted {
    pub v: u8,
    pub ts: String,
    pub message: String,
    /// The handoff session seeded with the summary. Empty string when
    /// the summary call failed and no session was seeded.
    pub new_session: String,
}

// ----------------------------------------------------------------------
// approval
// ----------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub v: u8,
    pub ts: String,
    pub id: String,
    pub tool_call_id: String,
    pub prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Approval {
    pub v: u8,
    pub ts: String,
    pub id: String,
    pub decision: ApprovalDecision,
    /// Edited arguments on the edit-then-allow path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalDecision {
    Allow,
    Deny,
}

// ----------------------------------------------------------------------
// rewind
// ----------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rewind {
    pub v: u8,
    pub ts: String,
    pub target_seq: u64,
    pub mode: RewindMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RewindMode {
    Before,
    On,
}

// ======================================================================
// Tests
// ======================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn user_message_line() -> &'static str {
        r#"{"v":1,"type":"user_message","ts":"2025-01-01T00:00:00Z","content":"hello","queue":"follow","id":"abc"}"#
    }

    fn assistant_message_line() -> &'static str {
        r#"{"v":1,"type":"assistant_message","ts":"2025-01-01T00:00:01Z","content":"hi","tool_calls":[{"id":"tc1","name":"read","arguments":{"path":"/tmp/x"}}],"stop_reason":"tool_calls","usage":{"input_tokens":10,"output_tokens":5,"cached_tokens":0}}"#
    }

    fn tool_call_line() -> &'static str {
        r#"{"v":1,"type":"tool_call","ts":"2025-01-01T00:00:02Z","id":"tc1","name":"read","arguments":{"path":"/tmp/x"}}"#
    }

    fn tool_result_line() -> &'static str {
        r#"{"v":1,"type":"tool_result","ts":"2025-01-01T00:00:03Z","id":"tc1","value":{"text":"ok"},"is_error":false,"bytes":42,"tool_log":"sessions/1/tools.jsonl"}"#
    }

    fn error_line() -> &'static str {
        r#"{"v":1,"type":"error","ts":"2025-01-01T00:00:04Z","message":"something broke"}"#
    }

    fn ext_status_line() -> &'static str {
        r#"{"v":1,"type":"ext_status","ts":"2025-01-01T00:00:05Z","id":"idle","value":{"status":"idle"}}"#
    }

    fn compaction_started_line() -> &'static str {
        r#"{"v":1,"type":"compaction_started","ts":"2025-01-01T00:00:06Z","reason":"threshold","tokens_before":100000}"#
    }

    fn compaction_summary_line() -> &'static str {
        r#"{"v":1,"type":"compaction_summary","ts":"2025-01-01T00:00:07Z","summary":"did stuff","first_kept_seq":10,"version":1,"parent_version":0,"diverge_seq":0,"reason":"threshold","tokens_before":100000,"tokens_after":500,"read_files":["/a"],"modified_files":["/b"],"usage":{"input_tokens":90,"output_tokens":10}}"#
    }

    fn compaction_failed_line() -> &'static str {
        r#"{"v":1,"type":"compaction_failed","ts":"2025-01-01T00:00:08Z","reason":"overflow","detail":"context overflow","last_user_seq":5}"#
    }

    fn context_exhausted_line() -> &'static str {
        r#"{"v":1,"type":"context_exhausted","ts":"2025-01-01T00:00:09Z","message":"out of context","new_session":"sessions/2"}"#
    }

    fn approval_request_line() -> &'static str {
        r#"{"v":1,"type":"approval_request","ts":"2025-01-01T00:00:10Z","id":"ar1","tool_call_id":"tc2","prompt":"allow bash?"}"#
    }

    fn approval_line() -> &'static str {
        r#"{"v":1,"type":"approval","ts":"2025-01-01T00:00:11Z","id":"ar1","decision":"allow"}"#
    }

    fn rewind_line() -> &'static str {
        r#"{"v":1,"type":"rewind","ts":"2025-01-01T00:00:12Z","target_seq":3,"mode":"before","reason":"tui_pick"}"#
    }

    fn retract_line() -> &'static str {
        r#"{"v":1,"type":"user_message_retract","ts":"2025-01-01T00:00:13Z","target":"abc","reason":"user_edit"}"#
    }

    fn round_trip(line: &str) {
        let ev: Event = parse_event(line).unwrap_or_else(|e| panic!("parse: {e}"));
        let out = serde_json::to_string(&ev).unwrap();
        let ev2: Event = parse_event(&out).unwrap_or_else(|e| panic!("re-parse: {e}"));
        let out2 = serde_json::to_string(&ev2).unwrap();
        assert_eq!(out, out2, "round-trip must be stable");
    }

    #[test]
    fn round_trip_user_message() {
        round_trip(user_message_line());
    }

    #[test]
    fn round_trip_assistant_message() {
        round_trip(assistant_message_line());
    }

    #[test]
    fn round_trip_tool_call() {
        round_trip(tool_call_line());
    }

    #[test]
    fn round_trip_tool_result() {
        round_trip(tool_result_line());
    }

    #[test]
    fn round_trip_error() {
        round_trip(error_line());
    }

    #[test]
    fn round_trip_ext_status() {
        round_trip(ext_status_line());
    }

    /// The `compact.failed` marker carries extra `reason`/`detail` fields
    /// alongside the required `value`. Extra fields must be tolerated.
    #[test]
    fn ext_status_with_extra_fields_parses() {
        let line = r#"{"v":1,"type":"ext_status","ts":"t","id":"compact.failed","value":"summary call failed","reason":"overflow","detail":"summary call failed"}"#;
        let ev = parse_event(line).expect("compact.failed shape must parse");
        match ev {
            Event::ExtStatus(e) => assert_eq!(e.id, "compact.failed"),
            _ => panic!("expected ExtStatus"),
        }
    }

    /// `value` is required on ext_status (the schema's `required`).
    /// A marker that omits it must be rejected.
    #[test]
    fn ext_status_without_value_fails() {
        let line = r#"{"v":1,"type":"ext_status","ts":"t","id":"compact.failed","detail":"x"}"#;
        assert!(parse_event(line).is_err());
    }

    #[test]
    fn round_trip_compaction_started() {
        round_trip(compaction_started_line());
    }

    #[test]
    fn round_trip_compaction_summary() {
        round_trip(compaction_summary_line());
    }

    #[test]
    fn round_trip_compaction_failed() {
        round_trip(compaction_failed_line());
    }

    #[test]
    fn round_trip_context_exhausted() {
        round_trip(context_exhausted_line());
    }

    #[test]
    fn round_trip_approval_request() {
        round_trip(approval_request_line());
    }

    #[test]
    fn round_trip_approval() {
        round_trip(approval_line());
    }

    #[test]
    fn round_trip_rewind() {
        round_trip(rewind_line());
    }

    #[test]
    fn round_trip_user_message_retract() {
        round_trip(retract_line());
    }

    #[test]
    fn unknown_type_tag_fails() {
        let err = parse_event(r#"{"v":1,"type":"bogus","ts":"t"}"#).unwrap_err();
        assert!(err.to_string().contains("unknown variant"), "got: {err}");
    }

    #[test]
    fn missing_field_fails() {
        let err =
            parse_event(r#"{"v":1,"type":"user_message","ts":"t"}"#).unwrap_err();
        assert!(
            err.to_string().contains("content"),
            "expected 'content' in error, got: {err}"
        );
    }

    #[test]
    fn event_types_constant_has_14_entries() {
        assert_eq!(EVENT_TYPES.len(), 14);
    }

    #[test]
    fn event_types_match_schema_filenames() {
        let expected = [
            "approval",
            "approval_request",
            "assistant_message",
            "compaction_failed",
            "compaction_started",
            "compaction_summary",
            "context_exhausted",
            "error",
            "ext_status",
            "rewind",
            "tool_call",
            "tool_result",
            "user_message",
            "user_message_retract",
        ];
        let mut got: Vec<&str> = EVENT_TYPES.iter().copied().collect();
        let mut want: Vec<&str> = expected.to_vec();
        got.sort();
        want.sort();
        assert_eq!(got, want);
    }
}
