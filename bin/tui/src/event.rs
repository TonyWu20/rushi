//! Versioned event vocabulary and safe accessors (docs/tui.md section 2.2).
//!
//! The TUI models *rendering fallbacks*, not every event. Every line of a
//! session log becomes an [`Event`] through [`Event::parse_line`]:
//!
//! - known `type` + supported `v`  -> semantic [`EventKind`]
//! - unknown `type`                -> [`EventKind::UnknownType`] (render raw)
//! - unknown `v`                   -> [`EventKind::UnsupportedVersion`] (render raw + hint)
//! - not valid JSON / not an object-> [`EventKind::BadLine`] (render raw + hint)
//!
//! Nothing in this module ever panics on log content: accessors return
//! `Option`s, so missing fields in a known type degrade to placeholders
//! instead of crashes (refinement policy G5).

use serde_json::{json, Value};

/// Log envelope versions this TUI renders semantically.
pub const SUPPORTED_VERSIONS: &[i64] = &[1];

/// How the renderer should treat an [`Event`].
///
/// The first nine variants are the known event types of the log's
/// vocabulary, one variant per wire `type` name; the enum itself is the
/// name registry, so no string constants are needed elsewhere.
/// The last three are fallback categories: they never crash the TUI,
/// they only change how the raw line is displayed (refinement policy G5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventKind {
    UserMessage,
    AssistantMessage,
    ToolCall,
    ToolResult,
    ApprovalRequest,
    Approval,
    Cancel,
    Error,
    /// Shared UI state published by extensions (`{id, value}`).
    /// The transcript suppresses the event by default. The log keeps
    /// the event (docs/ui-extension.md section 5).
    ExtStatus,
    /// The context budget ran out after compaction. The loop ran the
    /// automatic handoff: it summarized the session and seeded a new
    /// session with the summary. The event's `new_session` names the
    /// seeded session; an empty value means the summary call failed
    /// and no session was seeded (correction 57). The marker is
    /// legacy: the in-session compact (correction 63) no longer
    /// records it.
    ContextExhausted,
    /// The in-session auto-compact started. The summary call runs
    /// while the marker is open. The renderer closes the marker at
    /// the matching summary or failed event, or at the loop death
    /// (docs/auto-compact-plan.md section 4.6).
    CompactionStarted,
    /// The in-session auto-compact succeeded. The summary and the
    /// `first_kept_seq` boundary ride with the event; the log
    /// projects through the boundary from here.
    CompactionSummary,
    /// The in-session auto-compact failed. The `last_user_seq` of
    /// the marker anchors the threshold trigger cooldown.
    CompactionFailed,
    /// A user message was retracted by an edit (the user edited a
    /// pending message; docs/user-message-editing.md). `target` is
    /// the retracted message's `id`; `reason` is optional (v1:
    /// `"user_edit"`).
    UserMessageRetract,
    /// The user forked the session (docs/rewind-fork-design.md).
    /// `target_seq` is the 1-based log seq of the target event;
    /// `mode` is `before` (the target — a user message — is restored
    /// to the input box, unsent) or `on` (the target is included,
    /// the finished step prepares its next move). The marker
    /// projects to nothing in the model context: it masks the
    /// abandoned branch through the active path.
    Rewind,
    /// `type` value outside the known vocabulary. Render raw JSON.
    UnknownType,
    /// Known `type` but `v` outside [`SUPPORTED_VERSIONS`] (or missing).
    /// Render raw JSON with a "newer log version" hint.
    UnsupportedVersion,
    /// The log line is not valid JSON, or not a JSON object.
    BadLine,
}

impl EventKind {
    /// All semantic (known-vocabulary) kinds. Used by the test suite.
    #[allow(dead_code)]
    pub const ALL: &[EventKind] = &[
        EventKind::UserMessage,
        EventKind::AssistantMessage,
        EventKind::ToolCall,
        EventKind::ToolResult,
        EventKind::ApprovalRequest,
        EventKind::Approval,
        EventKind::Cancel,
        EventKind::Error,
        EventKind::ExtStatus,
        EventKind::ContextExhausted,
        EventKind::CompactionStarted,
        EventKind::CompactionSummary,
        EventKind::CompactionFailed,
        EventKind::UserMessageRetract,
        EventKind::Rewind,
    ];

    /// The wire `type` value for a semantic kind; `None` for fallback kinds.
    pub fn as_wire(self) -> Option<&'static str> {
        Some(match self {
            EventKind::UserMessage => "user_message",
            EventKind::AssistantMessage => "assistant_message",
            EventKind::ToolCall => "tool_call",
            EventKind::ToolResult => "tool_result",
            EventKind::ApprovalRequest => "approval_request",
            EventKind::Approval => "approval",
            EventKind::Cancel => "cancel",
            EventKind::Error => "error",
            EventKind::ExtStatus => "ext_status",
            EventKind::ContextExhausted => "context_exhausted",
            EventKind::CompactionStarted => "compaction_started",
            EventKind::CompactionSummary => "compaction_summary",
            EventKind::CompactionFailed => "compaction_failed",
            EventKind::UserMessageRetract => "user_message_retract",
            EventKind::Rewind => "rewind",
            EventKind::UnknownType | EventKind::UnsupportedVersion | EventKind::BadLine => {
                return None
            }
        })
    }

    /// Parse a wire `type` value into a semantic kind. `None` for
    /// anything outside the known vocabulary (render via fallback).
    pub fn from_wire(s: &str) -> Option<Self> {
        Some(match s {
            "user_message" => EventKind::UserMessage,
            "assistant_message" => EventKind::AssistantMessage,
            "tool_call" => EventKind::ToolCall,
            "tool_result" => EventKind::ToolResult,
            "approval_request" => EventKind::ApprovalRequest,
            "approval" => EventKind::Approval,
            "cancel" => EventKind::Cancel,
            "error" => EventKind::Error,
            "ext_status" => EventKind::ExtStatus,
            "context_exhausted" => EventKind::ContextExhausted,
            "compaction_started" => EventKind::CompactionStarted,
            "compaction_summary" => EventKind::CompactionSummary,
            "compaction_failed" => EventKind::CompactionFailed,
            "user_message_retract" => EventKind::UserMessageRetract,
            "rewind" => EventKind::Rewind,
            _ => return None,
        })
    }
}

/// One line of a session log.
///
/// [`Event::Json`] carries the parsed object (possibly with an unknown type
/// or version). [`Event::MalformedLine`] carries the raw text of a line that
/// did not parse; the TUI shows it with a hint and stays responsive.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A well-formed JSON object line.
    Json { obj: Value },
    /// A line that is not valid JSON (or is empty/whitespace-collapsed).
    MalformedLine { line: String },
}

/// Constructors for events the TUI itself produces (G3: producers build
/// typed envelopes, not free-form JSON).
pub mod produce {
    use super::*;

    fn now_ts() -> String {
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    }

    /// `user_message` event composed by the human in the input line.
    ///
    /// The delivery queue is `steer` (docs/tui-pending-user-messages.md
    /// stage 2): the message injects at the next step of the running
    /// loop. The queue field is absent: a missing field means
    /// `steer`, and old logs stay valid. The `follow` queue uses
    /// [`user_message_follow`].
    /// An optional `id` (UUID v4) enables later retraction via
    /// `user_message_retract` (docs/user-message-editing.md).
    pub fn user_message(content: &str) -> Event {
        Event::Json {
            obj: json!({
                "v": 1,
                "type": EventKind::UserMessage.as_wire().expect("semantic kind has a wire name"),
                "ts": now_ts(),
                "id": uuid::Uuid::new_v4().to_string(),
                "content": content,
            }),
        }
    }

    /// `user_message` event in the `follow` queue (docs/tui-pending-
    /// user-messages.md stage 2): the message runs only after the
    /// loop would stop, as a new turn. The `queue` field is written
    /// for the follow queue only; the steer line keeps the stage-1
    /// shape.
    pub fn user_message_follow(content: &str) -> Event {
        Event::Json {
            obj: json!({
                "v": 1,
                "type": EventKind::UserMessage.as_wire().expect("semantic kind has a wire name"),
                "ts": now_ts(),
                "id": uuid::Uuid::new_v4().to_string(),
                "content": content,
                "queue": "follow",
            }),
        }
    }

    /// `user_message_retract` event marking a previously sent
    /// user message as retracted (docs/user-message-editing.md).
    ///
    /// `target` is the `id` of the user message to retract.
    /// `reason` is optional; v1 uses `"user_edit"`.
    pub fn user_message_retract(target: &str) -> Event {
        Event::Json {
            obj: json!({
                "v": 1,
                "type": EventKind::UserMessageRetract.as_wire().expect("semantic kind has a wire name"),
                "ts": now_ts(),
                "target": target,
                "reason": "user_edit",
            }),
        }
    }

    /// `rewind` event forking the session at the target
    /// (docs/rewind-fork-design.md).
    ///
    /// `target_seq` is the 1-based log seq of the target event.
    /// `mode` is `"before"` (the target — a user message — is
    /// restored to the input box, unsent) or `"on"` (the target is
    /// included in the context; a finished step prepares its next
    /// move). `reason` is optional; v1 uses `"tui_pick"`.
    #[allow(dead_code)]
    pub fn rewind(target_seq: u64, mode: &str, reason: Option<&str>) -> Event {
        let mut obj = json!({
            "v": 1,
            "type": EventKind::Rewind.as_wire().expect("semantic kind has a wire name"),
            "ts": now_ts(),
            "target_seq": target_seq,
            "mode": mode,
        });
        if let Some(reason) = reason {
            obj["reason"] = json!(reason);
        }
        Event::Json { obj }
    }

    /// `approval` event answering an `approval_request`.
    ///
    /// `decision` is `"allow"` or `"deny"`. For an edit-then-allow the
    /// TUI passes the edited `arguments` object (additive field, P1b:
    /// no `v` bump); a plain allow/deny omits it.
    pub fn approval(request_id: &str, decision: &str, edited_arguments: Option<Value>) -> Event {
        let mut obj = json!({
            "v": 1,
            "type": EventKind::Approval.as_wire().expect("semantic kind has a wire name"),
            "ts": now_ts(),
            "id": request_id,
            "decision": decision,
        });
        if let Some(args) = edited_arguments {
            obj["arguments"] = args;
        }
        Event::Json { obj }
    }

    /// `cancel` event. `target` names the cancelled unit, `"turn"` in
    /// phase 1 (docs/tui.md section 2.2 example).
    pub fn cancel(target: &str) -> Event {
        Event::Json {
            obj: json!({
                "v": 1,
                "type": EventKind::Cancel.as_wire().expect("semantic kind has a wire name"),
                "ts": now_ts(),
                "target": target,
            }),
        }
    }

    /// `ext_status` event: shared UI state `{id, value}` (docs/ui-extension.md
    /// section 5). Extensions publish the state; the statusline consumes
    /// the state. `value` stays open-typed: a string like a vim mode, or
    /// an object like a team status. G3: producers build the typed
    /// envelope, not free-form JSON. No TUI call site yet: the extension
    /// host (ui-extension-plan stage 1) appends these events. Tests use
    /// the constructor directly.
    #[allow(dead_code)]
    pub fn ext_status(id: &str, value: Value) -> Event {
        Event::Json {
            obj: json!({
                "v": 1,
                "type": EventKind::ExtStatus.as_wire().expect("semantic kind has a wire name"),
                "ts": now_ts(),
                "id": id,
                "value": value,
            }),
        }
    }
}

impl Event {
    /// Parse one log line. Empty or whitespace-only lines yield `None`
    /// (they carry no event; the log writer never emits them).
    pub fn parse_line(line: &str) -> Option<Event> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return None;
        }
        match serde_json::from_str(trimmed) {
            Ok(Value::Object(map)) => Some(Event::Json {
                obj: Value::Object(map),
            }),
            Ok(scalar) => {
                // Valid JSON but not an object: an event must be an object
                // with a versioned envelope. Treat as a bad line.
                Some(Event::MalformedLine {
                    line: scalar.to_string(),
                })
            }
            Err(_) => Some(Event::MalformedLine {
                line: trimmed.to_string(),
            }),
        }
    }

    /// The rendering category of this event.
    pub fn kind(&self) -> EventKind {
        match self {
            Event::MalformedLine { .. } => EventKind::BadLine,
            Event::Json { obj } => {
                let semantic = obj
                    .get("type")
                    .and_then(|v| v.as_str())
                    .and_then(EventKind::from_wire);
                let v_ok = obj
                    .get("v")
                    .and_then(|v| v.as_i64())
                    .is_some_and(|v| SUPPORTED_VERSIONS.contains(&v));
                match (semantic, v_ok) {
                    (Some(kind), true) => kind,
                    // Known type, unsupported version: hint at the raw line.
                    (Some(_), false) => EventKind::UnsupportedVersion,
                    // Unknown or missing `type`: never crash, render raw.
                    (None, _) => EventKind::UnknownType,
                }
            }
        }
    }

    /// The raw JSON object, if this event parsed to one.
    pub fn obj(&self) -> Option<&Value> {
        match self {
            Event::Json { obj } => Some(obj),
            Event::MalformedLine { .. } => None,
        }
    }

    /// The raw text of a malformed line, if this is one.
    pub fn raw_line(&self) -> Option<&str> {
        match self {
            Event::MalformedLine { line } => Some(line),
            Event::Json { .. } => None,
        }
    }

    /// Look up a field by name. Safe on any event: missing keys, wrong
    /// shapes, and malformed lines all yield `None`.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.obj()?.get(key)
    }

    /// A string field.
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(|v| v.as_str())
    }

    /// An integer field.
    pub fn get_i64(&self, key: &str) -> Option<i64> {
        self.get(key).and_then(|v| v.as_i64())
    }

    /// A boolean field.
    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.get(key).and_then(|v| v.as_bool())
    }

    /// The envelope version `v`, if present.
    pub fn version(&self) -> Option<i64> {
        self.get_i64("v")
    }

    /// The event type name, if present and a string.
    pub fn type_name(&self) -> Option<&str> {
        self.get_str("type")
    }

    /// Compact single-line JSON of this event (fallback rendering and
    /// test assertions).
    #[allow(dead_code)]
    pub fn compact(&self) -> String {
        match self {
            Event::Json { obj } => obj.to_string(),
            Event::MalformedLine { line } => line.clone(),
        }
    }

    /// Prettified (2-space) JSON of this event, capped at `max_lines`
    /// lines for terminal readability. Fallback rendering only.
    pub fn pretty_capped(&self, max_lines: usize) -> String {
        let pretty = match self {
            Event::Json { obj } => {
                serde_json::to_string_pretty(obj).unwrap_or_else(|_| obj.to_string())
            }
            Event::MalformedLine { line } => line.clone(),
        };
        let mut out = String::new();
        for (i, l) in pretty.lines().take(max_lines).enumerate() {
            if i > 0 {
                out.push('\n');
            }
            out.push_str(l);
        }
        if pretty.lines().count() > max_lines {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str("... (truncated)");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_line_valid_object() {
        let e = Event::parse_line(r#"{"v":1,"type":"user_message","ts":"t","content":"hi"}"#);
        let e = e.expect("line must parse");
        assert_eq!(e.kind(), EventKind::UserMessage);
        assert_eq!(e.get_str("content"), Some("hi"));
        assert_eq!(e.version(), Some(1));
    }

    #[test]
    fn parse_line_empty_is_none() {
        assert!(Event::parse_line("").is_none());
        assert!(Event::parse_line("   ").is_none());
    }

    #[test]
    fn parse_line_bad_json_is_bad_line() {
        let e = Event::parse_line("{not json").expect("present");
        assert_eq!(e.kind(), EventKind::BadLine);
        assert_eq!(e.raw_line(), Some("{not json"));
        assert_eq!(e.get_str("type"), None);
    }

    #[test]
    fn parse_line_json_scalar_is_bad_line() {
        let e = Event::parse_line("42").expect("present");
        assert_eq!(e.kind(), EventKind::BadLine);
    }

    #[test]
    fn all_known_types_render_semantically() {
        for kind in EventKind::ALL {
            let wire = kind.as_wire().expect("semantic kind has a wire name");
            let e = Event::parse_line(&format!(r#"{{"v":1,"type":"{wire}"}}"#)).unwrap();
            assert_eq!(e.kind(), *kind, "type {wire} must be semantic");
            assert!(
                matches!(
                    e.kind(),
                    EventKind::UserMessage
                        | EventKind::AssistantMessage
                        | EventKind::ToolCall
                        | EventKind::ToolResult
                        | EventKind::ApprovalRequest
                        | EventKind::Approval
                        | EventKind::Cancel
                        | EventKind::Error
                        | EventKind::ExtStatus
                        | EventKind::ContextExhausted
                        | EventKind::CompactionStarted
                        | EventKind::CompactionSummary
                        | EventKind::CompactionFailed
                        | EventKind::UserMessageRetract
                        | EventKind::Rewind
                ),
                "type {wire} fell through to fallback"
            );
        }
    }

    #[test]
    fn kind_from_wire_round_trips_and_rejects_unknown() {
        for kind in EventKind::ALL {
            let wire = kind.as_wire().unwrap();
            assert_eq!(EventKind::from_wire(wire), Some(*kind));
        }
        assert_eq!(EventKind::from_wire("flux_capacitor"), None);
        assert_eq!(EventKind::from_wire(""), None);
    }

    #[test]
    fn unknown_type_event_falls_back() {
        let e = Event::parse_line(r#"{"v":1,"type":"flux_capacitor","ts":"t"}"#).unwrap();
        assert_eq!(e.kind(), EventKind::UnknownType);
    }

    #[test]
    fn unsupported_version_falls_back() {
        let e =
            Event::parse_line(r#"{"v":99,"type":"user_message","ts":"t","content":"x"}"#).unwrap();
        assert_eq!(e.kind(), EventKind::UnsupportedVersion);
    }

    #[test]
    fn missing_version_falls_back() {
        // A known type without `v` is not something this TUI can render
        // semantically: the envelope contract (architecture.md 5.2) is
        // versioned, so missing `v` must degrade to raw rendering.
        let e = Event::parse_line(r#"{"type":"user_message","ts":"t","content":"x"}"#).unwrap();
        assert_eq!(e.kind(), EventKind::UnsupportedVersion);
    }

    #[test]
    fn missing_fields_do_not_panic() {
        // G5 case (d): known type, missing fields -> safe Options.
        let e = Event::parse_line(r#"{"v":1,"type":"tool_call","ts":"t"}"#).unwrap();
        assert_eq!(e.kind(), EventKind::ToolCall);
        assert_eq!(e.get_str("id"), None);
        assert_eq!(e.get("arguments"), None);
        assert!(e.get_bool("is_error").is_none());
    }

    /// Regression: the assistant_message event carries a `reasoning`
    /// field with the server's reasoning items. The parser must stay
    /// semantic, and the field must be readable (handoff work item A).
    #[test]
    fn assistant_message_with_reasoning_stays_semantic() {
        let line = r#"{"v":1,"type":"assistant_message","ts":"t","content":"x","tool_calls":[],"stop_reason":"stop","usage":{"input_tokens":10,"output_tokens":2},"reasoning":[{"type":"reasoning","id":"rs_1","status":"completed","content":[{"type":"reasoning_text","text":"plan"}],"summary":[],"encrypted_content":null}]}"#;
        let e = Event::parse_line(line).expect("line must parse");
        assert_eq!(e.kind(), EventKind::AssistantMessage);
        let items = e.get("reasoning").expect("the reasoning field is present");
        let item = items.as_array().expect("reasoning is an array");
        assert_eq!(item[0]["type"], "reasoning");
        assert_eq!(item[0]["id"], "rs_1");
        assert_eq!(item[0]["content"][0]["text"], "plan");
        // The old fields still read.
        assert_eq!(e.get_str("content"), Some("x"));
        assert_eq!(
            e.get("usage").and_then(|u| u.get("input_tokens")),
            Some(&serde_json::json!(10))
        );
    }

    /// A reasoning entry with a malformed shape must not change the
    /// kind. The event stays semantic, the field degrades to raw.
    #[test]
    fn assistant_message_with_bad_reasoning_stays_semantic() {
        let line = r#"{"v":1,"type":"assistant_message","ts":"t","content":"x","tool_calls":[],"stop_reason":"stop","reasoning":["not-an-object"]}"#;
        let e = Event::parse_line(line).expect("line must parse");
        assert_eq!(e.kind(), EventKind::AssistantMessage);
        let items = e
            .get("reasoning")
            .and_then(|v| v.as_array())
            .expect("reasoning is an array");
        assert_eq!(items, &vec![serde_json::json!("not-an-object")]);
    }

    #[test]
    fn produced_events_carry_envelope() {
        let e = produce::user_message("hello");
        assert_eq!(e.kind(), EventKind::UserMessage);
        assert_eq!(e.get_str("type"), Some("user_message"));
        assert_eq!(e.get_str("content"), Some("hello"));
        assert_eq!(e.get_i64("v"), Some(1));
        assert!(e.get_str("ts").is_some());
    }

    #[test]
    fn produced_approval_plain_and_edited() {
        let a = produce::approval("appr-1", "allow", None);
        assert_eq!(a.get_str("decision"), Some("allow"));
        assert_eq!(a.get_str("id"), Some("appr-1"));
        assert!(a.get("arguments").is_none());

        let e = produce::approval("appr-1", "allow", Some(json!({"command": "ls"})));
        assert_eq!(e.get("arguments"), Some(&json!({"command": "ls"})));
    }

    #[test]
    fn produced_cancel_event() {
        let c = produce::cancel("turn");
        assert_eq!(c.kind(), EventKind::Cancel);
        assert_eq!(c.get_str("target"), Some("turn"));
    }

    #[test]
    fn produced_rewind_event() {
        let e = produce::rewind(41, "on", Some("tui_pick"));
        assert_eq!(e.kind(), EventKind::Rewind);
        assert_eq!(e.get_str("type"), Some("rewind"));
        assert_eq!(e.get_i64("v"), Some(1));
        assert_eq!(e.get_i64("target_seq"), Some(41));
        assert_eq!(e.get_str("mode"), Some("on"));
        assert_eq!(e.get_str("reason"), Some("tui_pick"));
        assert!(e.get_str("ts").is_some());
        // The optional reason degrades to absent, never to an error.
        let bare = produce::rewind(41, "before", None);
        assert_eq!(bare.get_str("mode"), Some("before"));
        assert!(bare.get("reason").is_none());
    }

    #[test]
    fn ext_status_line_is_semantic_not_fallback() {
        // Stage 0 acceptance (ui-extension-plan): an ext_status line
        // parses to the semantic kind, never a fallback category.
        let e = Event::parse_line(
            r#"{"v":1,"type":"ext_status","ts":"t","id":"vim_mode","value":"insert"}"#,
        )
        .unwrap();
        assert_eq!(e.kind(), EventKind::ExtStatus);
        assert_ne!(e.kind(), EventKind::UnknownType);
        assert_ne!(e.kind(), EventKind::UnsupportedVersion);
        assert_ne!(e.kind(), EventKind::BadLine);
        assert_eq!(e.get_str("id"), Some("vim_mode"));
        assert_eq!(e.get("value"), Some(&json!("insert")));
        // An object value parses the same way: the field is open-typed.
        let e = Event::parse_line(
            r#"{"v":1,"type":"ext_status","ts":"t","id":"team","value":{"on_call":"t"}}"#,
        )
        .unwrap();
        assert_eq!(e.kind(), EventKind::ExtStatus);
        assert_eq!(e.get("value"), Some(&json!({"on_call": "t"})));
    }

    #[test]
    fn produced_ext_status_event() {
        // G3 producer coverage: producers build the typed envelope
        // with every schema field.
        let e = produce::ext_status("vim_mode", json!("insert"));
        assert_eq!(e.kind(), EventKind::ExtStatus);
        assert_eq!(e.get_str("type"), Some("ext_status"));
        assert_eq!(e.get_i64("v"), Some(1));
        assert_eq!(e.get_str("id"), Some("vim_mode"));
        assert_eq!(e.get("value"), Some(&json!("insert")));
        assert!(e.get_str("ts").is_some());
    }

    /// The handoff marker parses semantically. Its fields degrade
    /// safely: a missing `new_session` reads as `None`, never crashes
    /// (G5). An empty `new_session` is the seeded-none form (the
    /// summary call failed).
    #[test]
    fn context_exhausted_line_is_semantic() {
        let e = Event::parse_line(
            r#"{"v":1,"type":"context_exhausted","ts":"t","message":"m","new_session":"s1_h1"}"#,
        )
        .expect("line must parse");
        assert_eq!(e.kind(), EventKind::ContextExhausted);
        assert_eq!(e.get_str("new_session"), Some("s1_h1"));

        let e = Event::parse_line(
            r#"{"v":1,"type":"context_exhausted","ts":"t","message":"m","new_session":""}"#,
        )
        .expect("line must parse");
        assert_eq!(e.kind(), EventKind::ContextExhausted);
        assert_eq!(e.get_str("new_session"), Some(""));

        // The marker without a seeded session field still parses.
        let e = Event::parse_line(r#"{"v":1,"type":"context_exhausted","ts":"t","message":"m"}"#)
            .expect("line must parse");
        assert_eq!(e.kind(), EventKind::ContextExhausted);
        assert_eq!(e.get_str("new_session"), None);
    }

    /// The compact markers parse semantically. Their fields degrade
    /// safely: missing numbers read as `None`, never crash (G5).
    #[test]
    fn compaction_marker_lines_are_semantic() {
        let e = Event::parse_line(
            r#"{"v":1,"type":"compaction_started","ts":"t","reason":"threshold","tokens_before":212992}"#,
        )
        .expect("line must parse");
        assert_eq!(e.kind(), EventKind::CompactionStarted);
        assert_eq!(e.get_str("reason"), Some("threshold"));
        assert_eq!(e.get_i64("tokens_before"), Some(212_992));

        let e = Event::parse_line(
            r#"{"v":1,"type":"compaction_summary","ts":"t","summary":"s","first_kept_seq":312,"reason":"threshold","tokens_before":212992,"tokens_after":33000,"read_files":["a.txt"],"modified_files":["b.rs"]}"#,
        )
        .expect("line must parse");
        assert_eq!(e.kind(), EventKind::CompactionSummary);
        assert_eq!(e.get_i64("first_kept_seq"), Some(312));
        assert_eq!(e.get_i64("tokens_after"), Some(33_000));

        let e = Event::parse_line(
            r#"{"v":1,"type":"compaction_failed","ts":"t","reason":"overflow","last_user_seq":41,"attempts":2,"detail":"the model returned an error stop"}"#,
        )
        .expect("line must parse");
        assert_eq!(e.kind(), EventKind::CompactionFailed);
        assert_eq!(e.get_i64("last_user_seq"), Some(41));

        // The degraded form: no optional field crashes the parse.
        let e = Event::parse_line(r#"{"v":1,"type":"compaction_started","ts":"t"}"#)
            .expect("line must parse");
        assert_eq!(e.kind(), EventKind::CompactionStarted);
        assert_eq!(e.get_str("reason"), None);
        assert_eq!(e.get_i64("tokens_before"), None);
    }

    #[test]
    fn compact_is_single_line() {
        let e = Event::parse_line(r#"{"v":1,"type":"error","ts":"t","message":"a\nb"}"#).unwrap();
        assert!(!e.compact().contains('\n'));
    }

    #[test]
    fn pretty_capped_limits_lines() {
        let e = Event::parse_line(
            r#"{"v":1,"type":"x","ts":"t","data":{"a":1,"b":2,"c":3,"d":4,"e":5}}"#,
        )
        .unwrap();
        let out = e.pretty_capped(2);
        assert_eq!(out.lines().count(), 3); // 2 lines + truncation marker
        assert!(out.ends_with("... (truncated)"));
    }
}
