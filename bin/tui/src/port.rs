//! `SessionPort` — the single internal abstraction the TUI depends on
//! (docs/tui.md section 2.1).
//!
//! Every file path, loop script name, lockfile, and socket lives behind
//! this trait. The TUI above this module never sees a path or a command;
//! it lists sessions, reads/appends events, spawns the opaque loop, and
//! watches a session for new events. Phase 1 ships a file-based
//! implementation ([`super::port_file::FileSessionPort`]); a later daemon
//! phase swaps in a socket implementation without touching the TUI loop.

use crate::event::Event;
use std::fmt;

/// An opaque session identifier. The TUI treats it as a name; the port
/// decides what it maps to (directory, socket channel, ...).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionId(String);

impl SessionId {
    pub fn new(s: impl Into<String>) -> Self {
        SessionId(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Errors the port reports to the TUI. The TUI shows `Display` text in
/// its status line; it never branches on internals.
#[derive(Debug)]
pub enum BusError {
    /// A storage or I/O problem under the port.
    Io { what: String },
    /// No `[loop]` section in the config: the loop command is opaque and
    /// supplied by config, so the TUI cannot start one without it.
    LoopNotConfigured,
    /// The event failed the port's producer-side validation (G3).
    InvalidEvent { reason: String },
    /// The loop command could not be started.
    CommandFailed { cmd: String, msg: String },
}

impl fmt::Display for BusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BusError::Io { what } => write!(f, "session store: {what}"),
            BusError::LoopNotConfigured => {
                write!(
                    f,
                    "no [loop] command in config; the TUI cannot start the agent loop"
                )
            }
            BusError::InvalidEvent { reason } => {
                write!(f, "event rejected before append: {reason}")
            }
            BusError::CommandFailed { cmd, msg } => {
                write!(f, "failed to start loop command `{cmd}`: {msg}")
            }
        }
    }
}

impl std::error::Error for BusError {}

impl From<std::io::Error> for BusError {
    fn from(e: std::io::Error) -> Self {
        BusError::Io {
            what: e.to_string(),
        }
    }
}

/// Position in a session's event stream, used to resume watching without
/// re-reading history. Opaque to the TUI: phase 1 is a byte offset into
/// the session log, a later socket phase is a sequence number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TailCursor {
    /// `None` means "start at the current end of the log".
    pub(crate) offset: Option<u64>,
}
impl TailCursor {
    /// From the beginning of the log.
    pub const fn start() -> Self {
        TailCursor { offset: Some(0) }
    }
    /// From the current end of the log (resolved by the port at watch time).
    pub const fn end() -> Self {
        TailCursor { offset: None }
    }
    pub fn is_start(&self) -> bool {
        self.offset == Some(0)
    }
}

/// Items the port's event watcher delivers (docs/tui.md section 8: an
/// event-tailer behind the port; phase 1 tails the file by offset).
#[derive(Debug, Clone, PartialEq)]
pub enum WatchItem {
    /// A new event in the active session, with the cursor to resume from.
    Event { event: Event, cursor: TailCursor },
    /// The session log disappeared (e.g. directory removed). The tailer
    /// keeps retrying; nothing is lost, the log is the source of truth.
    Gone,
    /// The session log came back after [`WatchItem::Gone`]; the tailer
    /// resumes and re-emits from where it left off.
    Resumed,
    /// A transient I/O error while reading. The last message is kept in
    /// the TUI status line; the tailer retries.
    IoError { message: String },
}

/// Output of the opaque loop process, streamed to the TUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopLine {
    Stdout(String),
    Stderr(String),
    /// The loop process exited. `code` is the exit code, or `-1` when
    /// killed by a signal or reaped without status.
    Exited(i32),
}

/// A handle to one running (or just-finished) opaque loop.
///
/// The TUI starts the loop, streams its output, and stops it on
/// request. It never knows what the loop runs — start, stream, stop.
/// All methods are synchronous on purpose: the TUI event loop is sync,
/// and the handle only ever talks to the process group and its output
/// channel. A future daemon handle implements the same trait over a
/// socket.
pub trait LoopHandle: Send + Sync {
    /// Request the loop to stop: SIGTERM to the whole process group,
    /// SIGKILL escalation after a short grace, so that stage children
    /// cannot outlive the request. Idempotent and safe to call after
    /// the loop has already exited.
    fn stop(&self);

    /// Block the calling thread until the loop process has exited.
    /// Returns the exit code, or `-1` when killed by a signal or
    /// reaped without status. The TUI event loop does not call this
    /// (it uses the `Exited` line); it exists for non-UI callers.
    #[allow(dead_code)]
    fn wait_exit(&self) -> i32;

    /// Take the output stream. Exactly one caller; returns `None`
    /// after the first call. The stream ends when the loop exits.
    fn take_lines(&self) -> Option<tokio::sync::mpsc::UnboundedReceiver<LoopLine>>;
}

/// The one internal port the TUI depends on (docs/tui.md section 2.1).
///
/// All TUI access to sessions goes through this trait:
/// - list known sessions
/// - read a session's events, oldest first
/// - append one event (atomic, validated before append per G3)
/// - start the opaque loop and stream its output
/// - watch a session for new events without re-reading history
///
/// Native `async fn` is used instead of the `async_trait` macro
/// (stable since Rust 1.75): the TUI never boxes this trait, it only
/// ever holds the concrete port.
pub trait SessionPort: Send + Sync {
    /// List known session ids, most recently active first.
    async fn list_sessions(&self) -> Result<Vec<SessionId>, BusError>;

    /// Return all events of a session, oldest first. A session whose log
    /// does not exist yet yields an empty list, not an error.
    async fn read_events(&self, session: &SessionId) -> Result<Vec<Event>, BusError>;

    /// Append one event to a session's log. Must be atomic: a single
    /// append, never a rewrite. Validates the event against the type's
    /// JSON Schema when the schema file exists (G3).
    async fn append_event(&self, session: &SessionId, event: &Event) -> Result<(), BusError>;

    /// Resolve the on-disk directory backing a session
    /// (docs/goal-ux.md section 1.7). The file-based implementation
    /// maps it to `<sessions_root>/<session>`; a future daemon port
    /// would return the daemon's working directory. Extensions use it
    /// (via their own goal-state access) to locate the session's goal
    /// files; the TUI itself never reads goal files.
    fn session_dir(&self, session: &SessionId) -> Result<std::path::PathBuf, BusError>;

    /// The session-local model stream channel file
    /// (docs/tui-streaming-response.md section 3.1). The loop creates
    /// and deletes this file around each model call; the TUI polls it
    /// each frame to render the in-progress response.
    fn model_stream_path(&self, session: &SessionId) -> Result<std::path::PathBuf, BusError>;

    /// Append one TUI trace record to the session trace log.
    ///
    /// The TUI writes its own errors and warnings to a trace a human
    /// can read on their own (docs/tool-log-design_from_human.md):
    /// render failures, port errors, malformed-line hints, key
    /// handling faults, and loop spawn/stop events. Each record
    /// carries a timestamp. A failed trace write must not take the
    /// UI down: callers may drop the result.
    async fn append_trace(
        &self,
        session: &SessionId,
        kind: &str,
        message: &str,
    ) -> Result<(), BusError>;

    /// Start the loop for a session and return its handle. The command
    /// that runs comes from config; the TUI passes it a session id only.
    async fn spawn_loop(&self, session: &SessionId) -> Result<Box<dyn LoopHandle>, BusError>;

    /// Watch a session for new events, starting at `from`. Returns a
    /// receiver that yields [`WatchItem`]s until it is dropped. The
    /// tailer retries transient errors and survives the log being
    /// removed and recreated.
    fn watch(&self, session: &SessionId, from: TailCursor) -> std::sync::mpsc::Receiver<WatchItem>;
}
