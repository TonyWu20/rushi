//! Session-local model stream channel
//! (docs/tui-streaming-response.md section 3.1).
//!
//! The loop owns the file's lifecycle: it creates
//! `sessions/<n>/.model-stream` before the model call, the model
//! binary appends one JSON line per SSE delta event, and the loop
//! deletes the file when the call returns (success or error). The
//! TUI polls the file to render the in-progress response.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The stream-channel file name inside a session directory.
pub const MODEL_STREAM_FILE: &str = ".model-stream";

/// The stream file of the in-flight model call, if any.
static CURRENT: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Register the stream file of the model call starting now.
pub fn register(path: &Path) {
    *CURRENT.lock().unwrap() = Some(path.to_path_buf());
}

/// Drop the registration after the normal delete (success or error).
pub fn unregister() {
    *CURRENT.lock().unwrap() = None;
}

/// Delete the registered stream file. The signal-exit path calls this
/// because process exit skips the normal delete. A no-op when no call
/// is in flight; a failed delete is dropped (a stale file is inert:
/// the TUI only polls the channel while the loop is running).
pub fn cleanup() {
    if let Some(path) = CURRENT.lock().unwrap().take() {
        let _ = std::fs::remove_file(path);
    }
}
