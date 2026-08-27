//! `FileSessionPort` — the phase-1 implementation of [`SessionPort`]
//! (docs/tui.md section 11).
//!
//! - sessions live as directories under `[paths] sessions_root`
//! - `list_sessions`: scan the root for subdirectories that hold an event log
//! - `read_events`: read the whole log, newest-first byte order
//! - `append_event`: one O_APPEND write, schema-validated before append
//! - `spawn_loop`: run the opaque `[loop]` command in its own process group
//! - `watch`: a dedicated thread tails the log by byte offset
//!
//! This module is the *only* place the TUI source knows the storage
//! layout (docs/tui.md section 10, guardrail 3). A source-scan test in
//! `main.rs` enforces that.

use crate::config::{LoopCommand, TuiConfig};
use crate::event::{Event, EventKind};
use crate::port::{BusError, LoopHandle, LoopLine, SessionId, SessionPort, TailCursor, WatchItem};

use serde_json::Value;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::SyncSender;
use std::time::Duration;

/// Session log file name. A storage detail; never referenced above the port.
const LOG_FILE: &str = "events.jsonl";
/// Working directory recorded at session start. A storage detail.
const CWD_FILE: &str = "cwd";
/// How often the tailer polls the log file.
const TAIL_INTERVAL: Duration = Duration::from_millis(250);
/// How often the tailer retries a missing log file.
const TAIL_RETRY_INTERVAL: Duration = Duration::from_millis(500);
/// Tailer channel capacity: when full, the tailer keeps its place and
/// the next poll resumes. The tailer thread is one per active session;
/// it exits when it next tries to send after the receiver is dropped,
/// or when the process ends.
const TAIL_CAPACITY: usize = 256;
/// Newest log bytes a single `read_events` load may allocate.
const MAX_LOG_READ_BYTES: u64 = 50 * 1024 * 1024;
/// Per-poll read cap; keeps tailing latency bounded without big reads.
const TAIL_CHUNK_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone)]
pub struct FileSessionPort {
    sessions_root: PathBuf,
    schemas_dir: Option<PathBuf>,
    loop_cmd: Option<LoopCommand>,
    config_dir: PathBuf,
    config_path: PathBuf,
}

impl FileSessionPort {
    pub fn new(cfg: &TuiConfig) -> Self {
        FileSessionPort {
            sessions_root: cfg.sessions_root.clone(),
            schemas_dir: cfg.schemas_dir.clone(),
            loop_cmd: cfg.loop_cmd.clone(),
            config_dir: cfg.config_dir.clone(),
            config_path: cfg.config_path.clone(),
        }
    }

    /// Reject session ids that could escape the sessions root.
    fn session_dir(&self, session: &SessionId) -> Result<PathBuf, BusError> {
        let name = session.as_str();
        let p = Path::new(name);
        if name.is_empty()
            || p.is_absolute()
            || p.components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(BusError::Io {
                what: format!("invalid session id `{name}`"),
            });
        }
        Ok(self.sessions_root.join(name))
    }

    fn log_path(&self, session: &SessionId) -> Result<PathBuf, BusError> {
        Ok(self.session_dir(session)?.join(LOG_FILE))
    }
}

/// One session log tailer: reads complete lines after a byte offset,
/// keeps the unterminated tail between polls, survives truncation and
/// removal of the log file, and never blocks the reader.
fn tail_session(path: PathBuf, start: TailCursor, tx: SyncSender<WatchItem>) {
    let mut offset: u64 = match start.offset() {
        Some(n) => n,
        None => file_len_or_zero(&path),
    };
    let mut carry: Vec<u8> = Vec::new();
    let mut present = path.exists();

    loop {
        // When the receiver is dropped, a `try_send` below reports a
        // disconnected channel and the loop exits on that error path.
        // (This std has no `SyncSender::is_closed`.)
        match read_tail(&path, &mut offset, &mut carry, &tx) {
            Ok(()) => {
                if !present {
                    present = true;
                    let _ = tx.try_send(WatchItem::Resumed);
                }
            }
            Err(TailErr::NotFound) => {
                if present {
                    present = false;
                    let _ = tx.try_send(WatchItem::Gone);
                }
                std::thread::sleep(TAIL_RETRY_INTERVAL);
                continue;
            }
            Err(TailErr::Io(e)) => {
                let _ = tx.try_send(WatchItem::IoError {
                    message: e.to_string(),
                });
                std::thread::sleep(TAIL_RETRY_INTERVAL);
                continue;
            }
        }
        std::thread::sleep(TAIL_INTERVAL);
    }
}

enum TailErr {
    NotFound,
    Io(std::io::Error),
}

/// Read newly appended bytes after `offset`, emit each complete line as
/// a [`WatchItem::Event`], keep the unterminated tail in `carry`.
/// When the channel is full, emitted lines stop and the next poll
/// resumes where this one left off: no event is lost, none is re-emitted.
fn read_tail(
    path: &Path,
    offset: &mut u64,
    carry: &mut Vec<u8>,
    tx: &SyncSender<WatchItem>,
) -> Result<(), TailErr> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(TailErr::NotFound),
        Err(e) => return Err(TailErr::Io(e)),
    };
    let len = meta.len();
    if len < *offset {
        // Log was truncated or rotated: reread from the start.
        *offset = 0;
        carry.clear();
    }
    // Alignment check: the log is append-only, so the byte just before the
    // read position must be a newline (or the position must be 0). A file
    // rewritten underneath the tailer breaks that; reread from the start.
    if *offset > 0 {
        if let Ok(mut probe) = File::open(path) {
            if probe.seek(SeekFrom::Start(*offset - 1)).is_ok() {
                let mut b = [0u8; 1];
                if probe.read_exact(&mut b).is_ok() && b[0] != b'\n' {
                    *offset = 0;
                    carry.clear();
                }
            }
        }
    }
    let want = len.saturating_sub(*offset);
    if want == 0 {
        return Ok(());
    }

    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(e) => return Err(TailErr::Io(e)),
    };
    if file.seek(SeekFrom::Start(*offset)).is_err() {
        // Race with removal; the next poll handles it.
        return Ok(());
    }
    let mut buf = vec![0u8; (want.min(TAIL_CHUNK_BYTES)) as usize];
    let mut read = 0u64;
    while read < want {
        let chunk = &mut buf[read as usize..];
        let n = match file.read(chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        read += n as u64;
    }

    let mut candidate = carry.clone();
    candidate.extend_from_slice(&buf[..read as usize]);
    let base = *offset;

    if let Some(nl) = candidate.iter().rposition(|b| *b == b'\n') {
        let complete_end = nl + 1;
        let mut pos = 0usize; // bytes of `complete` processed
        let mut stopped = false;
        for line in candidate[..complete_end].split_inclusive(|b| *b == b'\n') {
            let line_end = pos + line.len();
            let text = String::from_utf8_lossy(line);
            if let Some(event) = Event::parse_line(&text) {
                match tx.try_send(WatchItem::Event {
                    event,
                    cursor: TailCursor::at(base + line_end as u64),
                }) {
                    Ok(()) => pos = line_end,
                    Err(_) => {
                        stopped = true;
                        break;
                    }
                }
            } else {
                // Blank line: skip it, but keep the offset moving.
                pos = line_end;
            }
        }
        if stopped {
            *offset = base + pos as u64;
            *carry = candidate[pos..].to_vec();
        } else {
            *offset = base + complete_end as u64;
            *carry = candidate[complete_end..].to_vec();
        }
    } else {
        // No complete line yet: remember everything, advance nothing.
        *carry = candidate;
    }
    Ok(())
}

fn file_len_or_zero(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

impl SessionPort for FileSessionPort {
    async fn list_sessions(&self) -> Result<Vec<SessionId>, BusError> {
        let root = self.sessions_root.clone();
        let res = tokio::task::spawn_blocking(move || {
            let Ok(entries) = std::fs::read_dir(&root) else {
                return Vec::new();
            };
            let mut out: Vec<(std::time::SystemTime, String)> = Vec::new();
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let log = path.join(LOG_FILE);
                if !log.is_file() {
                    continue;
                }
                let mtime = std::fs::metadata(&log)
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                out.push((mtime, entry.file_name().to_string_lossy().into_owned()));
            }
            out.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            out.into_iter()
                .map(|(_, name)| SessionId::new(name))
                .collect()
        })
        .await;
        res.map_err(|e| BusError::Io {
            what: e.to_string(),
        })
    }

    async fn read_events(&self, session: &SessionId) -> Result<Vec<Event>, BusError> {
        let path = self.log_path(session)?;
        let res = tokio::task::spawn_blocking(move || -> Result<Vec<Event>, BusError> {
            let data = match std::fs::read(&path) {
                Ok(d) => d,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
                Err(e) => return Err(BusError::from(e)),
            };
            let start = if data.len() as u64 > MAX_LOG_READ_BYTES {
                let cut = data.len() - MAX_LOG_READ_BYTES as usize;
                // Drop the (possibly partial) first line after the cut.
                data[cut..]
                    .iter()
                    .position(|b| *b == b'\n')
                    .map(|i| cut + i + 1)
                    .unwrap_or(data.len())
            } else {
                0
            };
            let mut events = Vec::new();
            for line in data[start..].split(|b| *b == b'\n') {
                let text = String::from_utf8_lossy(line);
                if let Some(ev) = Event::parse_line(&text) {
                    events.push(ev);
                }
            }
            Ok(events)
        })
        .await;
        res.map_err(|e| BusError::Io {
            what: e.to_string(),
        })?
    }

    async fn append_event(&self, session: &SessionId, event: &Event) -> Result<(), BusError> {
        let obj = event
            .obj()
            .ok_or_else(|| BusError::InvalidEvent {
                reason: "a malformed log line cannot be appended as an event".to_string(),
            })?
            .clone();
        let ty =
            obj.get("type")
                .and_then(|v| v.as_str())
                .ok_or_else(|| BusError::InvalidEvent {
                    reason: "event has no string `type` field".to_string(),
                })?;
        let json_line = serde_json::to_string(&obj).map_err(|e| BusError::InvalidEvent {
            reason: e.to_string(),
        })?;
        // One event per line (architecture.md 4): the trailing newline is
        // part of the write so the event stays a single atomic append.
        let line = format!("{json_line}\n");

        // G3: producers validate before append, when the schema file exists.
        if let Some(dir) = &self.schemas_dir {
            let schema_path = dir.join(format!("{ty}.json"));
            if schema_path.is_file() {
                let raw = std::fs::read_to_string(&schema_path).map_err(|e| BusError::Io {
                    what: format!("cannot read schema {}: {e}", schema_path.display()),
                })?;
                let schema: Value = serde_json::from_str(&raw).map_err(|e| BusError::Io {
                    what: format!("invalid schema {}: {e}", schema_path.display()),
                })?;
                if !matches_schema(&obj, &schema) {
                    return Err(BusError::InvalidEvent {
                        reason: format!(
                            "does not match {}",
                            schema_path
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                        ),
                    });
                }
            }
        }

        let session_dir = self.session_dir(session)?;
        let log_path = session_dir.join(LOG_FILE);
        let is_user_message = EventKind::from_wire(ty) == Some(EventKind::UserMessage);
        let res = tokio::task::spawn_blocking(move || -> Result<(), BusError> {
            std::fs::create_dir_all(&session_dir)?;
            // Entry points record the working directory on the first user
            // message of a session; the TUI is one such entry point.
            if is_user_message {
                let cwd_path = session_dir.join(CWD_FILE);
                if !cwd_path.exists() {
                    if let Ok(cwd) = std::env::current_dir() {
                        let _ = std::fs::write(&cwd_path, cwd.to_string_lossy().as_bytes());
                    }
                }
            }
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)?;
            // O_APPEND + one write(2) keeps each event a single atomic
            // append for lines well under PIPE_BUF.
            let n = file.write(line.as_bytes()).map_err(BusError::from)?;
            if n != line.len() {
                return Err(BusError::Io {
                    what: "short write while appending event".to_string(),
                });
            }
            Ok(())
        })
        .await;
        res.map_err(|e| BusError::Io {
            what: e.to_string(),
        })?
    }

    async fn spawn_loop(&self, session: &SessionId) -> Result<Box<dyn LoopHandle>, BusError> {
        let loop_cmd = self.loop_cmd.as_ref().ok_or(BusError::LoopNotConfigured)?;
        let argv = loop_cmd.argv(session);
        let program = argv[0].clone();
        let mut cmd = tokio::process::Command::new(&program);
        cmd.args(&argv[1..]);
        cmd.current_dir(&self.config_dir);
        cmd.env("CONFIG", &self.config_path);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        // Own process group: stop() can terminate the whole pipeline,
        // not just the top process (architecture.md 5.3: process-group
        // kill). `setsid` returns the new session id on success (the
        // child pid) and -1 only on failure (e.g. EPERM when the
        // caller is already a process-group leader).
        unsafe {
            cmd.pre_exec(|| match libc::setsid() {
                -1 => Err(std::io::Error::last_os_error()),
                _ => Ok(()),
            })
        };

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return Err(BusError::CommandFailed {
                    cmd: program,
                    msg: e.to_string(),
                })
            }
        };
        let pid = child.id().map(|p| p as i32).ok_or_else(|| BusError::Io {
            what: "loop process has no pid".to_string(),
        })?;

        // Output plumbing: two pump tasks (stdout, stderr) share one
        // unbounded channel; each reports EOF on a one-shot so the
        // reaper can place `Exited` strictly after the last output line.
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<LoopLine>();
        let (out_done_tx, out_done_rx) = tokio::sync::oneshot::channel::<()>();
        let (err_done_tx, err_done_rx) = tokio::sync::oneshot::channel::<()>();
        if let Some(out) = child.stdout.take() {
            let reader = tokio::io::BufReader::new(out);
            let line_tx = tx.clone();
            let done = out_done_tx;
            tokio::spawn(async move {
                pump_lines(reader, line_tx, LoopLine::Stdout, done).await;
            });
        } else {
            let _ = out_done_tx.send(());
        }
        if let Some(err) = child.stderr.take() {
            let reader = tokio::io::BufReader::new(err);
            let line_tx = tx.clone();
            let done = err_done_tx;
            tokio::spawn(async move {
                pump_lines(reader, line_tx, LoopLine::Stderr, done).await;
            });
        } else {
            let _ = err_done_tx.send(());
        }

        let shared = std::sync::Arc::new(LoopShared {
            pid,
            child: std::sync::Mutex::new(Some(child)),
            exit_code: std::sync::Mutex::new(None),
            stopped: std::sync::atomic::AtomicBool::new(false),
        });
        // wait_exit() is a sync, blocking wait on this channel; the
        // reaper is its only sender.
        let (std_tx, std_rx) = std::sync::mpsc::channel::<i32>();
        // The reaper is the only place that waits on the child; stop()
        // kills the group, the reaper reports the exit code exactly
        // once, after all output lines.
        let reaper_shared = std::sync::Arc::clone(&shared);
        let reaper_lines = tx.clone();
        tokio::spawn(async move {
            let c = reaper_shared.child.lock().unwrap().take();
            let code = match c {
                Some(mut c) => c
                    .wait()
                    .await
                    .map(|st| st.code().unwrap_or(-1))
                    .unwrap_or(-1),
                None => -1,
            };
            // Ordering barrier: `Exited` must be the last line.
            let _ = out_done_rx.await;
            let _ = err_done_rx.await;
            *reaper_shared.exit_code.lock().unwrap() = Some(code);
            let _ = std_tx.send(code);
            let _ = reaper_lines.send(LoopLine::Exited(code));
        });

        Ok(Box::new(ProcessLoopHandle {
            shared,
            lines: std::sync::Mutex::new(Some(rx)),
            wait_rx: std::sync::Mutex::new(Some(std_rx)),
        }))
    }

    fn watch(&self, session: &SessionId, from: TailCursor) -> std::sync::mpsc::Receiver<WatchItem> {
        let path = match self.log_path(session) {
            Ok(p) => p,
            Err(_) => {
                // Invalid session id: a closed receiver keeps the TUI
                // alive; nothing is ever delivered.
                let (tx, rx) = std::sync::mpsc::channel::<WatchItem>();
                drop(tx);
                return rx;
            }
        };
        let start = if from.is_start() {
            TailCursor::start()
        } else {
            TailCursor::at(file_len_or_zero(&path))
        };
        let (tx, rx) = std::sync::mpsc::sync_channel(TAIL_CAPACITY);
        let _ = std::thread::Builder::new()
            .name(format!("tui-tail-{}", session))
            .spawn(move || tail_session(path, start, tx));
        rx
    }
}

async fn pump_lines(
    reader: impl tokio::io::AsyncBufReadExt + Unpin + Sized,
    tx: tokio::sync::mpsc::UnboundedSender<LoopLine>,
    wrap: fn(String) -> LoopLine,
    done: tokio::sync::oneshot::Sender<()>,
) {
    let mut reader = reader;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = line.trim_end().to_string();
                if !trimmed.is_empty() && tx.send(wrap(trimmed)).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let _ = done.send(());
}

struct LoopShared {
    pid: i32,
    child: std::sync::Mutex<Option<tokio::process::Child>>,
    exit_code: std::sync::Mutex<Option<i32>>,
    stopped: std::sync::atomic::AtomicBool,
}

struct ProcessLoopHandle {
    shared: std::sync::Arc<LoopShared>,
    lines: std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<LoopLine>>>,
    #[allow(dead_code)] // consumed only by `wait_exit`, a non-UI API
    wait_rx: std::sync::Mutex<Option<std::sync::mpsc::Receiver<i32>>>,
}

impl LoopHandle for ProcessLoopHandle {
    fn stop(&self) {
        let pid = self.shared.pid;
        // Only the first stop() call escalates; repeats are no-ops
        // (a dead group tolerates extra kills anyway).
        if self
            .shared
            .stopped
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        if group_alive(pid) {
            unsafe { libc::kill(-pid, libc::SIGTERM) };
        }
        // SIGKILL escalation after the grace window. A plain thread:
        // no runtime is needed to kill a process group.
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(3));
            if group_alive(pid) {
                unsafe { libc::kill(-pid, libc::SIGKILL) };
            }
        });
    }

    fn wait_exit(&self) -> i32 {
        if let Some(code) = *self.shared.exit_code.lock().unwrap() {
            return code;
        }
        if let Some(rx) = self.wait_rx.lock().unwrap().take() {
            return rx.recv().unwrap_or(-1);
        }
        // Another wait_exit() already took the channel; the code is
        // cached by then (the reaper sets it before sending).
        self.shared.exit_code.lock().unwrap().unwrap_or(-1)
    }

    fn take_lines(&self) -> Option<tokio::sync::mpsc::UnboundedReceiver<LoopLine>> {
        self.lines.lock().unwrap().take()
    }
}

fn group_alive(pid: i32) -> bool {
    // Signal 0 checks group existence without delivering anything.
    // ESRCH: the group is gone (or already reaped).
    let r = unsafe { libc::kill(-pid, 0) };
    r == 0
}

/// Minimal JSON Schema validator for the event schemas under
/// `schemas/events/v1/`. Supports the subset those schemas use:
/// `const`, `required`, `properties`, and `type` for
/// string/integer/boolean/number/boolean/array/object.
///
/// NOTE: this is a deliberate third copy of the validator that lives in
/// the one-shot binaries (phase 1 has no shared Rust crate, P3:
/// duplicate deliberately). Parked in `notes/itches.md` for promotion.
fn matches_schema(value: &Value, schema: &Value) -> bool {
    if let Some(const_val) = schema.get("const") {
        return value == const_val;
    }
    match schema.get("type").and_then(|t| t.as_str()) {
        Some("object") => {
            let Some(obj) = value.as_object() else {
                return false;
            };
            if let Some(required) = schema.get("required").and_then(|r| r.as_array()) {
                for req in required {
                    if let Some(field) = req.as_str() {
                        if !obj.contains_key(field) {
                            return false;
                        }
                    }
                }
            }
            if let Some(props) = schema.get("properties").and_then(|p| p.as_object()) {
                for (key, prop_schema) in props {
                    if let Some(val) = obj.get(key) {
                        if !matches_schema(val, prop_schema) {
                            return false;
                        }
                    }
                }
            }
            true
        }
        Some("string") => value.is_string(),
        Some("integer") => value.is_i64(),
        Some("number") => value.is_f64(),
        Some("boolean") => value.is_boolean(),
        Some("array") => {
            if let Some(arr) = value.as_array() {
                if let Some(items) = schema.get("items") {
                    for item in arr {
                        if !matches_schema(item, items) {
                            return false;
                        }
                    }
                }
                true
            } else {
                false
            }
        }
        _ => true,
    }
}

impl TailCursor {
    /// Private constructor used by the port; the TUI only ever uses
    /// `start()` / `end()` / cursors returned by the port itself.
    pub(crate) const fn at(offset: u64) -> Self {
        TailCursor {
            offset: Some(offset),
        }
    }
    pub(crate) const fn offset(&self) -> Option<u64> {
        self.offset
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::port::SessionPort;
    use std::sync::mpsc;
    use tempfile::TempDir;

    struct Cfg {
        dir: TempDir,
        port: FileSessionPort,
    }

    fn make_cfg(schemas: bool, loop_cmd: Option<(&str, Vec<&str>)>) -> Cfg {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_path_buf();
        if schemas {
            let sdir = root.join("schemas").join("events").join("v1");
            std::fs::create_dir_all(&sdir).unwrap();
            std::fs::write(
                sdir.join("user_message.json"),
                r#"{"type":"object","required":["v","type","ts","content"],"properties":{"v":{"type":"integer","const":1},"type":{"type":"string","const":"user_message"},"ts":{"type":"string"},"content":{"type":"string"}}}"#,
            )
            .unwrap();
        }
        let cfg = TuiConfig {
            sessions_root: root.join("sessions"),
            schemas_dir: if schemas {
                Some(root.join("schemas").join("events").join("v1"))
            } else {
                None
            },
            loop_cmd: loop_cmd.map(|(c, a)| LoopCommand {
                command: c.to_string(),
                args: a.into_iter().map(|s| s.to_string()).collect(),
                arg_style: crate::config::ArgStyle::AppendSession,
            }),
            config_dir: root.clone(),
            config_path: root.join("config.toml"),
            active_model: None,
            ext_dir: None,
        };
        Cfg {
            dir,
            port: FileSessionPort::new(&cfg),
        }
    }

    fn log_path(cfg: &Cfg, session: &str) -> PathBuf {
        cfg.dir.path().join("sessions").join(session).join(LOG_FILE)
    }

    /// One runtime per test group: the reaper and pump tasks that
    /// `spawn_loop` starts must keep running between `block_on` calls.
    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn block_on<T, F>(rt: &tokio::runtime::Runtime, fut: F) -> T
    where
        F: std::future::Future<Output = T> + Send,
        T: Send,
    {
        rt.block_on(fut)
    }

    #[test]
    fn list_sessions_finds_only_dirs_with_logs() {
        let c = make_cfg(false, None);
        let rt = runtime();
        std::fs::create_dir_all(c.dir.path().join("sessions").join("s1")).unwrap();
        std::fs::write(log_path(&c, "s1"), "{}\n").unwrap();
        std::fs::create_dir_all(c.dir.path().join("sessions").join("s2")).unwrap();
        std::fs::write(log_path(&c, "s2"), "{}\n").unwrap();
        std::fs::create_dir_all(c.dir.path().join("sessions").join("no-log")).unwrap();

        let ids = block_on(&rt, c.port.list_sessions()).unwrap();
        let names: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(names.contains(&"s1") && names.contains(&"s2"), "{names:?}");
    }

    #[test]
    fn list_sessions_missing_root_is_empty() {
        let c = make_cfg(false, None);
        let rt = runtime();
        let ids = block_on(&rt, c.port.list_sessions()).unwrap();
        assert!(ids.is_empty());
    }

    #[test]
    fn read_events_parses_lines_and_keeps_bad_lines() {
        let c = make_cfg(false, None);
        let rt = runtime();
        std::fs::create_dir_all(c.dir.path().join("sessions").join("s1")).unwrap();
        std::fs::write(
            log_path(&c, "s1"),
            "{\"v\":1,\"type\":\"user_message\",\"ts\":\"t\",\"content\":\"a\"}\nnot json at all\n{\"v\":1,\"type\":\"error\",\"ts\":\"t\",\"message\":\"boom\"}\n",
        )
        .unwrap();
        let evs = block_on(&rt, c.port.read_events(&SessionId::new("s1"))).unwrap();
        assert_eq!(evs.len(), 3);
        assert_eq!(evs[0].kind(), EventKind::UserMessage);
        assert_eq!(evs[1].kind(), EventKind::BadLine);
        assert_eq!(evs[2].kind(), EventKind::Error);
    }

    #[test]
    fn read_events_missing_session_is_empty_not_error() {
        let c = make_cfg(false, None);
        let rt = runtime();
        let evs = block_on(&rt, c.port.read_events(&SessionId::new("ghost"))).unwrap();
        assert!(evs.is_empty());
    }

    #[test]
    fn append_event_creates_session_and_appends_one_line() {
        let c = make_cfg(false, None);
        let rt = runtime();
        let ev = Event::Json {
            obj: serde_json::json!({"v":1,"type":"user_message","ts":"t","content":"hi"}),
        };
        block_on(&rt, c.port.append_event(&SessionId::new("s1"), &ev)).unwrap();
        let content = std::fs::read_to_string(log_path(&c, "s1")).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("\"content\":\"hi\""));
        assert!(
            content.ends_with('\n'),
            "each appended event must own its newline"
        );
    }

    #[test]
    fn append_events_stay_separate_lines() {
        let c = make_cfg(false, None);
        let rt = runtime();
        let sid = SessionId::new("s2");
        for i in 0..3 {
            let ev = Event::Json {
                obj: serde_json::json!({"v":1,"type":"user_message","ts":"t","content":i}),
            };
            block_on(&rt, c.port.append_event(&sid, &ev)).unwrap();
        }
        let content = std::fs::read_to_string(log_path(&c, "s2")).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 3, "consecutive appends must not glue lines");
        for line in &lines {
            let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(parsed["type"], "user_message");
        }
    }

    #[test]
    fn append_event_rejects_malformed_line_events() {
        let c = make_cfg(false, None);
        let rt = runtime();
        let ev = Event::MalformedLine {
            line: "garbage".into(),
        };
        let err = block_on(&rt, c.port.append_event(&SessionId::new("s1"), &ev)).unwrap_err();
        assert!(matches!(err, BusError::InvalidEvent { .. }), "{err:?}");
    }

    #[test]
    fn append_event_rejects_object_without_type() {
        let c = make_cfg(false, None);
        let rt = runtime();
        let ev = Event::Json {
            obj: serde_json::json!({"v":1,"ts":"t"}),
        };
        let err = block_on(&rt, c.port.append_event(&SessionId::new("s1"), &ev)).unwrap_err();
        assert!(matches!(err, BusError::InvalidEvent { .. }), "{err:?}");
    }

    #[test]
    fn append_event_validates_against_schema_when_present() {
        let c = make_cfg(true, None);
        let rt = runtime();
        let good = Event::Json {
            obj: serde_json::json!({"v":1,"type":"user_message","ts":"t","content":"ok"}),
        };
        block_on(&rt, c.port.append_event(&SessionId::new("s1"), &good)).unwrap();

        let bad_content_type = Event::Json {
            obj: serde_json::json!({"v":1,"type":"user_message","ts":"t","content":42}),
        };
        let err = block_on(
            &rt,
            c.port
                .append_event(&SessionId::new("s1"), &bad_content_type),
        )
        .unwrap_err();
        assert!(matches!(err, BusError::InvalidEvent { .. }), "{err:?}");

        let missing_required = Event::Json {
            obj: serde_json::json!({"v":1,"type":"user_message","ts":"t"}),
        };
        let err = block_on(
            &rt,
            c.port
                .append_event(&SessionId::new("s1"), &missing_required),
        )
        .unwrap_err();
        assert!(matches!(err, BusError::InvalidEvent { .. }), "{err:?}");

        // Unknown type with no schema file: appended freely (P1b:
        // additive types need no schema to flow through the TUI).
        let novel = Event::Json {
            obj: serde_json::json!({"v":1,"type":"flux_capacitor","ts":"t"}),
        };
        block_on(&rt, c.port.append_event(&SessionId::new("s1"), &novel)).unwrap();
    }

    /// The repo's ext_status schema, two levels above the crate root.
    /// Tests run from the crate root. This file is the single source
    /// of truth for the ext_status schema (stage 0, G3 producer
    /// coverage).
    fn repo_ext_status_schema_path() -> std::path::PathBuf {
        std::path::Path::new("../..")
            .join("schemas")
            .join("events")
            .join("v1")
            .join("ext_status.json")
            .to_path_buf()
    }

    /// Copy the repo's ext_status schema into the temp schemas dir.
    /// The port reads schemas from its config dir, which the test sets
    /// to the temp dir. The copy puts the repo file in that dir.
    fn write_ext_status_schema(c: &Cfg) {
        let sdir = c.dir.path().join("schemas").join("events").join("v1");
        std::fs::create_dir_all(&sdir).unwrap();
        let src = repo_ext_status_schema_path();
        std::fs::copy(&src, sdir.join("ext_status.json"))
            .expect("repo schema file must exist");
    }

    #[test]
    fn ext_status_producer_event_passes_schema_and_appends() {
        // G3: with the schema file present, the typed envelope from
        // `produce::ext_status` validates and lands in the log.
        let c = make_cfg(true, None);
        write_ext_status_schema(&c);
        let rt = runtime();
        let ev = crate::event::produce::ext_status("vim_mode", serde_json::json!("insert"));
        block_on(&rt, c.port.append_event(&SessionId::new("s1"), &ev)).unwrap();
        let content = std::fs::read_to_string(log_path(&c, "s1")).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 1, "one line in the log: {lines:?}");
        let parsed: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed["type"], "ext_status");
        assert_eq!(parsed["id"], "vim_mode");
        assert_eq!(parsed["value"], "insert");
    }

    #[test]
    fn ext_status_missing_value_is_rejected() {
        // Stage 0 acceptance: a missing `value` field is rejected by
        // the schema check. Nothing lands in the log.
        let c = make_cfg(true, None);
        write_ext_status_schema(&c);
        let rt = runtime();
        let ev = Event::Json {
            obj: serde_json::json!({"v":1,"type":"ext_status","ts":"t","id":"vim_mode"}),
        };
        let err = block_on(
            &rt,
            c.port.append_event(&SessionId::new("s1"), &ev),
        )
        .unwrap_err();
        assert!(matches!(err, BusError::InvalidEvent { .. }), "{err:?}");
        assert!(
            !log_path(&c, "s1").exists(),
            "a rejected event must not create a log file"
        );
    }

    #[test]
    fn repo_ext_status_schema_accepts_producer_and_rejects_missing_value() {
        // The shipped schema file must pass under the port validator.
        // A producer envelope passes. A missing `value` fails.
        let raw = std::fs::read_to_string(repo_ext_status_schema_path())
            .expect("repo schema file must exist");
        let schema: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let produced = crate::event::produce::ext_status("vim_mode", serde_json::json!("insert"));
        let produced_obj = produced.obj().expect("a produced event has an object").clone();
        assert!(
            matches_schema(&produced_obj, &schema),
            "producer envelope must match the repo schema: {produced_obj:?}"
        );
        let missing_value =
            serde_json::json!({"v":1,"type":"ext_status","ts":"t","id":"vim_mode"});
        assert!(
            !matches_schema(&missing_value, &schema),
            "a missing `value` must fail the repo schema"
        );
    }

    #[test]
    fn append_event_rejects_path_traversal_session_ids() {
        let c = make_cfg(false, None);
        let rt = runtime();
        let ev = Event::Json {
            obj: serde_json::json!({"v":1,"type":"user_message","ts":"t","content":"x"}),
        };
        for evil in ["../etc", "a/../../b", "/abs", ""] {
            let err = block_on(&rt, c.port.append_event(&SessionId::new(evil), &ev)).unwrap_err();
            assert!(matches!(err, BusError::Io { .. }), "{evil}: {err:?}");
        }
    }

    #[test]
    fn watch_sees_new_events_from_end() {
        let c = make_cfg(false, None);
        std::fs::create_dir_all(c.dir.path().join("sessions").join("s1")).unwrap();
        std::fs::write(log_path(&c, "s1"), "old line\n").unwrap();

        let sid = SessionId::new("s1");
        let rx = c.port.watch(&sid, TailCursor::end());
        std::thread::sleep(Duration::from_millis(200)); // let the tailer attach

        append_raw(
            &log_path(&c, "s1"),
            r#"{"v":1,"type":"user_message","ts":"t","content":"new"}"#,
        );
        let file_len = file_len_or_zero(&log_path(&c, "s1"));
        match wait_item(&rx) {
            WatchItem::Event { event, cursor } => {
                assert_eq!(event.kind(), EventKind::UserMessage);
                // The cursor is the byte offset just past the event's line.
                assert_eq!(
                    cursor,
                    TailCursor::at(file_len),
                    "cursor must point past the appended line"
                );
            }
            other => panic!("expected event, got {other:?}"),
        }
        drop(rx);
    }

    #[test]
    fn watch_holds_partial_line_until_newline() {
        let c = make_cfg(false, None);
        std::fs::create_dir_all(c.dir.path().join("sessions").join("s1")).unwrap();
        let path = log_path(&c, "s1");
        std::fs::write(&path, "").unwrap();

        let sid = SessionId::new("s1");
        let rx = c.port.watch(&sid, TailCursor::end());
        std::thread::sleep(Duration::from_millis(200));

        // Write a partial line: no event yet.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"\"par")
            .unwrap();
        std::thread::sleep(Duration::from_millis(700));
        assert!(rx.try_recv().is_err(), "partial line must not be emitted");

        // Finish the line: now one event.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"tial\",\"ts\":\"t\",\"content\":\"partial\"}\n")
            .unwrap();
        match wait_item(&rx) {
            WatchItem::Event { event, .. } => {
                assert!(event.compact().contains("partial"), "{}", event.compact())
            }
            other => panic!("expected event, got {other:?}"),
        }
        drop(rx);
    }

    #[test]
    fn watch_survives_truncation() {
        let c = make_cfg(false, None);
        std::fs::create_dir_all(c.dir.path().join("sessions").join("s1")).unwrap();
        let path = log_path(&c, "s1");
        std::fs::write(&path, "one\n").unwrap();

        let sid = SessionId::new("s1");
        let rx = c.port.watch(&sid, TailCursor::start());
        match wait_item(&rx) {
            WatchItem::Event { event, .. } => assert_eq!(event.kind(), EventKind::BadLine),
            other => panic!("expected event, got {other:?}"),
        }

        // Truncate to empty: the tailer must reset to byte 0.
        std::fs::write(&path, "").unwrap();
        append_raw(
            &path,
            r#"{"v":1,"type":"user_message","ts":"t","content":"fresh"}"#,
        );
        match wait_item(&rx) {
            WatchItem::Event { event, .. } => {
                assert_eq!(event.kind(), EventKind::UserMessage)
            }
            other => panic!("expected fresh event, got {other:?}"),
        }
        drop(rx);
    }

    #[test]
    fn watch_reports_gone_and_resumed() {
        let c = make_cfg(false, None);
        let path = log_path(&c, "s1");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "x\n").unwrap();

        let sid = SessionId::new("s1");
        let rx = c.port.watch(&sid, TailCursor::start());
        let _ = wait_item(&rx); // the initial event
        std::fs::remove_file(&path).unwrap();
        match wait_item(&rx) {
            WatchItem::Gone => {}
            other => panic!("expected Gone, got {other:?}"),
        }
        std::fs::write(&path, "back\n").unwrap();
        let mut saw_resumed = false;
        let mut saw_event = false;
        for _ in 0..60 {
            match rx.try_recv() {
                Ok(WatchItem::Resumed) => saw_resumed = true,
                Ok(WatchItem::Event { .. }) => saw_event = true,
                _ => std::thread::sleep(Duration::from_millis(100)),
            }
            if saw_resumed && saw_event {
                break;
            }
        }
        assert!(saw_resumed && saw_event);
        drop(rx);
    }

    #[test]
    fn spawn_loop_streams_lines_and_reports_exit() {
        let c = make_cfg(
            false,
            Some((
                "bash",
                vec!["-c", "echo out-line; echo err-line >&2; exit 3"],
            )),
        );
        let rt = runtime();
        let sid = SessionId::new("s9");
        let handle = block_on(&rt, c.port.spawn_loop(&sid)).unwrap();
        let mut lines = handle.take_lines().unwrap();

        let mut got: Vec<LoopLine> = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(l) = lines.try_recv() {
                let is_exit = matches!(l, LoopLine::Exited(_));
                got.push(l);
                if is_exit {
                    break;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no exit reported; got {got:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }

        let outs: Vec<&str> = got
            .iter()
            .filter_map(|l| match l {
                LoopLine::Stdout(s) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        let errs: Vec<&str> = got
            .iter()
            .filter_map(|l| match l {
                LoopLine::Stderr(s) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(outs, vec!["out-line"]);
        assert_eq!(errs, vec!["err-line"]);
        match got.last().unwrap() {
            LoopLine::Exited(code) => assert_eq!(*code, 3),
            other => panic!("last line must be Exited, got {other:?}"),
        }
        assert_eq!(handle.wait_exit(), 3);
    }

    #[test]
    fn spawn_loop_missing_config_is_bus_error() {
        let c = make_cfg(false, None);
        let rt = runtime();
        let err = match block_on(&rt, c.port.spawn_loop(&SessionId::new("s9"))) {
            Err(e) => e,
            Ok(_) => panic!("expected LoopNotConfigured, got a handle"),
        };
        assert!(matches!(err, BusError::LoopNotConfigured), "{err:?}");
    }

    #[test]
    fn stop_terminates_group_and_wait_exit_resolves() {
        let c = make_cfg(false, Some(("bash", vec!["-c", "sleep 30"])));
        let rt = runtime();
        let sid = SessionId::new("s9");
        let handle = block_on(&rt, c.port.spawn_loop(&sid)).unwrap();
        let mut lines = handle.take_lines().unwrap();
        handle.stop();
        // Killed by a signal: exit status has no code, the handle
        // reports -1. wait_exit blocks the test thread; the reaper
        // task runs on the (multi-threaded) runtime, so it can make
        // progress while this thread waits.
        assert_eq!(handle.wait_exit(), -1);
        let mut saw_exit = false;
        for _ in 0..100 {
            match lines.try_recv() {
                Ok(LoopLine::Exited(code)) => {
                    assert_eq!(code, -1);
                    saw_exit = true;
                    break;
                }
                Ok(_) => {}
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        }
        assert!(saw_exit, "the Exited line must arrive after stop");
    }

    fn append_raw(path: &Path, line: &str) {
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)
            .unwrap()
            .write_all(format!("{line}\n").as_bytes())
            .unwrap();
    }

    fn wait_item(rx: &mpsc::Receiver<WatchItem>) -> WatchItem {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(item) => return item,
                Err(mpsc::RecvTimeoutError::Timeout) if std::time::Instant::now() < deadline => {}
                Err(e) => panic!("watcher stopped: {e:?}"),
            }
        }
    }
}
