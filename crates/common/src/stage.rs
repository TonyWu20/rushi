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
    /// The working directory for tool subprocesses.
    pub cwd: Option<PathBuf>,
    /// The per-session tool log path.
    pub tool_log: Option<PathBuf>,
    /// Max chars for legacy inline results.
    pub tool_result_max_chars: usize,
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
}

impl CompactStatus {
    pub fn noop() -> Self {
        Self {
            outcome: CompactOutcome::Noop,
            first_kept_seq: None,
            tokens_before: None,
            tokens_after: None,
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
    fn model(&self, request: &RequestFile) -> Result<ModelOutput, StageError>;
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
