//! `stage` — the `StageRunner` trait and its payload types.
//!
//! The loop state machine depends on this trait, not on process
//! handles. Phase 2 ships one implementation (subprocess spawn of the
//! stage binaries). Phase 3 moves this trait to `crates/core` with
//! the state machine. Phase 4 adds in-process and wasm runners.
//!
//! Appends are not a stage: the loop appends in-process through the
//! shared `LogLine` and validator. The `log` binary stays available
//! to humans and e2e.

use serde_json::Value;
use std::path::PathBuf;

/// The session directory handle. Wraps the path to `sessions/<name>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionDir {
    pub path: PathBuf,
}

/// The claim result: what the loop owes this session.
#[derive(Clone, Debug)]
pub struct Claim {
    /// One of: `idle`, `awaiting_model`, `awaiting_tool_result`,
    /// `exhausted`, `awaiting_approval`.
    pub state: String,
    /// The 1-based log sequence of the last `user_message`.
    pub last_user_message_seq: usize,
    /// Unresolved `tool_call` events (crash recovery, G2).
    pub pending_tool_calls: Vec<Value>,
    /// Log sequences of pending `queue=follow` user messages.
    pub pending_follow_ups: Vec<usize>,
}

/// Options for the `assemble` stage.
#[derive(Clone, Debug, Default)]
pub struct AssembleOpts {
    /// Inject pending follow-up messages into this turn.
    pub inject_follow: bool,
}

/// The result of `assemble`: a parsed request JSON or an error form.
#[derive(Clone, Debug)]
pub struct RequestFile {
    pub json: Value,
}

/// The model output: the normalized response from the model binary.
#[derive(Clone, Debug)]
pub struct ModelOutput {
    pub json: Value,
}

/// One line of the model's live stream channel
/// (docs/tui-streaming-response.md section 3.2). The model binary
/// writes one of these as a compact JSON line to the session-local
/// `.model-stream` file for each SSE delta event; the loop deletes
/// the file when the model call completes. The TUI polls the file
/// and renders the in-progress response.
///
/// The Phase 2 subprocess runner serializes these to JSON lines. A
/// Phase 3 in-process runner can pass the deltas in memory without a
/// wire-protocol break (docs/tui-streaming-response.md section 8).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelDelta {
    /// A chunk of the model's output text.
    Text(String),
    /// A chunk of one reasoning item's text. `item_id` matches the
    /// id the final `assistant_message` carries in its `reasoning`
    /// array.
    Reasoning {
        item_id: String,
        delta: String,
    },
    /// A chunk of one tool call's JSON arguments. `name` is the tool
    /// name once the item event names it; empty until then.
    ToolCallDelta {
        call_id: String,
        name: String,
        args_delta: String,
    },
    /// The model call's stream is complete. No further lines follow.
    Done {
        stop_reason: String,
    },
}

impl ModelDelta {
    /// Serialize to one compact JSON line, the channel's wire format
    /// (docs/tui-streaming-response.md section 3.2).
    pub fn to_json_line(&self) -> String {
        match self {
            ModelDelta::Text(delta) => {
                serde_json::json!({ "kind": "text", "delta": delta }).to_string()
            }
            ModelDelta::Reasoning { item_id, delta } => {
                serde_json::json!({
                    "kind": "reasoning",
                    "item_id": item_id,
                    "delta": delta,
                })
                .to_string()
            }
            ModelDelta::ToolCallDelta {
                call_id,
                name,
                args_delta,
            } => serde_json::json!({
                "kind": "tool_call_delta",
                "call_id": call_id,
                "name": name,
                "args_delta": args_delta,
            })
            .to_string(),
            ModelDelta::Done { stop_reason } => {
                serde_json::json!({ "kind": "done", "stop_reason": stop_reason }).to_string()
            }
        }
    }

    /// Parse one channel line. `None` on a malformed or unknown line:
    /// a reader never fails on a bad line (the channel is best-effort;
    /// the log stays the record).
    pub fn from_json_line(line: &str) -> Option<Self> {
        let v = serde_json::from_str::<serde_json::Value>(line).ok()?;
        match v.get("kind").and_then(|k| k.as_str()) {
            Some("text") => Some(ModelDelta::Text(
                v.get("delta")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string(),
            )),
            Some("reasoning") => Some(ModelDelta::Reasoning {
                item_id: v
                    .get("item_id")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string(),
                delta: v
                    .get("delta")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string(),
            }),
            Some("tool_call_delta") => Some(ModelDelta::ToolCallDelta {
                call_id: v
                    .get("call_id")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string(),
                name: v
                    .get("name")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string(),
                args_delta: v
                    .get("args_delta")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string(),
            }),
            Some("done") => Some(ModelDelta::Done {
                stop_reason: v
                    .get("stop_reason")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string(),
            }),
            _ => None,
        }
    }
}

/// The parsed events from the `parse` binary.
#[derive(Clone, Debug)]
pub struct ParsedEvents {
    /// The parsed JSONL lines (assistant_message, tool_call, error).
    pub lines: Vec<String>,
    /// 0 = stop, 1 = route, 2 = terminal error.
    pub exit: i32,
}

/// A tool call event to route.
#[derive(Clone, Debug)]
pub struct ToolCallEvent {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

/// The environment for the `route` stage.
#[derive(Clone, Debug)]
pub struct RouteEnv {
    /// The tools root directory.
    pub tools_root: PathBuf,
    /// Extra roots for tool manifests (`[paths] extra_tools_roots`).
    pub extra_tools_roots: Vec<PathBuf>,
    /// The working directory for tool subprocesses.
    pub cwd: Option<PathBuf>,
    /// The per-session tool log path.
    pub tool_log: Option<PathBuf>,
    /// Max chars for legacy inline results.
    pub tool_result_max_chars: usize,
    /// The session directory. Passed to tools as `HARNESS_SESSION_DIR`
    /// so goal tools can read/write the goal files in the session dir.
    pub session_dir: Option<PathBuf>,
}

/// A tool result event produced by `route`.
#[derive(Clone, Debug)]
pub struct ToolResultEvent {
    pub id: String,
    pub value: Value,
    pub is_error: bool,
}

/// Options for the `compact` stage.
#[derive(Clone, Debug)]
pub struct CompactOpts {
    /// The trigger reason: `threshold`, `overflow`, or `last_resort`.
    pub reason: CompactReason,
    /// Skip the kill switch and cooldown.
    pub force: bool,
    /// Exclude the last assistant group (length-stop retry).
    pub strip_last_assistant: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactReason {
    Threshold,
    Overflow,
    LastResort,
}

impl CompactReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            CompactReason::Threshold => "threshold",
            CompactReason::Overflow => "overflow",
            CompactReason::LastResort => "last_resort",
        }
    }
}

/// The outcome of a compact call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactOutcome {
    /// The trigger did not fire; no compact ran.
    Noop,
    /// The compact completed and the summary is in the log.
    Compacted,
    /// The compact failed (summary call failed, empty summary, etc.).
    Failed,
}

/// The status of a compact call, with the metrics the compact
/// binary reports on the success path.
#[derive(Clone, Debug)]
pub struct CompactStatus {
    pub outcome: CompactOutcome,
    /// The first log sequence kept after the shadow compact.
    pub first_kept_seq: Option<u64>,
    pub tokens_before: Option<u64>,
    pub tokens_after: Option<u64>,
    /// The handoff summary text (the LLM-written summary of the
    /// shadowed region). Present only when the compact succeeded.
    pub summary: Option<String>,
}

impl CompactStatus {
    pub fn noop() -> Self {
        Self {
            outcome: CompactOutcome::Noop,
            first_kept_seq: None,
            tokens_before: None,
            tokens_after: None,
            summary: None,
        }
    }
}

/// A stage error: a non-recoverable failure of a stage binary.
#[derive(Debug)]
pub enum StageError {
    /// The stage binary exited non-zero or its output was malformed.
    StageFailed {
        name: &'static str,
        code: i32,
        detail: String,
    },
    /// An I/O error while spawning or reading the stage.
    Io {
        name: &'static str,
        source: std::io::Error,
    },
    /// A generic error (e.g. JSON parse failure).
    Other { name: &'static str, msg: String },
}

impl std::fmt::Display for StageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StageError::StageFailed {
                name,
                code,
                detail,
            } => write!(f, "stage '{name}' failed with exit {code}: {detail}"),
            StageError::Io { name, source } => write!(f, "IO error in stage '{name}': {source}"),
            StageError::Other { name, msg } => write!(f, "stage '{name}': {msg}"),
        }
    }
}

impl std::error::Error for StageError {}

/// The `StageRunner` trait: the one dependency the loop state
/// machine has on stage execution. Phase 2 ships the subprocess
/// implementation; Phase 4 will add in-process and wasm runners.
pub trait StageRunner: Send {
    fn claim(&self, session: &SessionDir) -> Result<Claim, StageError>;
    fn assemble(
        &self,
        session: &SessionDir,
        opts: &AssembleOpts,
    ) -> Result<RequestFile, StageError>;
    /// Run the model stage. `delta_file` is the optional session-local
    /// stream channel the model writes live deltas to
    /// (docs/tui-streaming-response.md section 5.2): the subprocess
    /// runner passes it to the model as `--delta-file`. `None` runs
    /// the stage without the side channel.
    fn model(
        &self,
        request: &RequestFile,
        delta_file: Option<&std::path::Path>,
    ) -> Result<ModelOutput, StageError>;
    fn parse(&self, output: &ModelOutput) -> Result<ParsedEvents, StageError>;
    fn route(
        &self,
        calls: &[ToolCallEvent],
        env: &RouteEnv,
    ) -> Result<Vec<ToolResultEvent>, StageError>;
    fn compact(
        &self,
        session: &SessionDir,
        opts: &CompactOpts,
    ) -> Result<CompactStatus, StageError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_delta_round_trips_through_the_wire_format() {
        let cases = vec![
            ModelDelta::Text("Hello ".into()),
            ModelDelta::Reasoning {
                item_id: "rs_1".into(),
                delta: "thinking ".into(),
            },
            ModelDelta::ToolCallDelta {
                call_id: "c1".into(),
                name: "bash".into(),
                args_delta: "ls ".into(),
            },
            ModelDelta::Done {
                stop_reason: "stop".into(),
            },
        ];
        for d in &cases {
            assert_eq!(
                ModelDelta::from_json_line(&d.to_json_line()),
                Some(d.clone()),
                "round trip for {d:?}"
            );
        }
    }

    #[test]
    fn model_delta_rejects_unknown_and_malformed_lines() {
        assert_eq!(ModelDelta::from_json_line(r#"{"kind":"weird"}"#), None);
        assert_eq!(ModelDelta::from_json_line("not json"), None);
        // A known kind with a missing field degrades to empty, not a
        // parse failure: the channel never fails the TUI on a bad line.
        assert_eq!(
            ModelDelta::from_json_line(r#"{"kind":"done"}"#),
            Some(ModelDelta::Done { stop_reason: String::new() })
        );
        assert_eq!(
            ModelDelta::from_json_line(r#"{"kind":"text"}"#),
            Some(ModelDelta::Text(String::new()))
        );
    }
}
