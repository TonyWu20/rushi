//! `LogLine` — the only type that may write a session log.
//!
//! FT-005: a regular file gets no `O_APPEND` write atomicity at any
//! size. The `PIPE_BUF` guarantee applies to pipes only. Two
//! concurrent writers can resolve the same end offset and corrupt
//! each other's bytes into lines that look complete. This type
//! closes the hole:
//!
//! - one `LogLine` owns the full event bytes, including the trailing
//!   newline. It is the only value that reaches the log.
//! - `commit` is the only write path. It takes an exclusive `flock`
//!   and does one `write(2)` of the whole buffer. The type has no
//!   `impl Write`, so a line cannot be appended in pieces. Concurrent
//!   appends from any number of processes serialize on the lock.
//!
//! Copy policy: deliberate duplication (phase 1, no shared crate.
//! See `notes/itches.md`). Keep the three copies in sync.

use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::path::Path;

/// One complete session log line: event bytes plus the trailing newline.
#[derive(Debug)]
pub struct LogLine {
    bytes: Vec<u8>,
}

impl LogLine {
    /// Build a line from a serialized JSON event.
    ///
    /// The input holds no raw newline. `serde_json` emits only
    /// escaped newlines inside strings, and every caller passes one
    /// single line.
    pub fn from_json(json_line: &str) -> Self {
        let mut bytes = Vec::with_capacity(json_line.len() + 1);
        bytes.extend_from_slice(json_line.as_bytes());
        bytes.push(b'\n');
        LogLine { bytes }
    }

    /// The only append path. An exclusive lock, one `write(2)`, unlock.
    ///
    /// The lock serializes appends across processes. The lock also
    /// dies with the writer. A dead writer cannot wedge the log.
    pub fn commit(&self, path: &Path) -> io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .write(true)
            .open(path)?;
        let fd = file.as_raw_fd();
        if unsafe { libc::flock(fd, libc::LOCK_EX) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let res = file.write_all(&self.bytes);
        // The close releases the lock too. The explicit unlock keeps
        // the lock from spanning the close.
        if unsafe { libc::flock(fd, libc::LOCK_UN) } != 0 {
            // The close still releases it. Nothing to report.
        }
        res
    }
}

#[cfg(test)]
mod tests {
    use super::LogLine;

    #[test]
    fn commit_appends_one_complete_line_per_event() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("events.jsonl");
        LogLine::from_json(r#"{"v":1,"type":"user_message","ts":"t","content":"one"}"#)
            .commit(&log)
            .unwrap();
        LogLine::from_json(r#"{"v":1,"type":"user_message","ts":"t","content":"two"}"#)
            .commit(&log)
            .unwrap();
        let text = std::fs::read_to_string(&log).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text:?}");
        assert!(lines[0].contains("\"one\""), "{lines:?}");
        assert!(lines[1].contains("\"two\""), "{lines:?}");
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn concurrent_commits_stay_line_granular() {
        // FT-005 regression. Two writers, multi-KB lines. Without the
        // per-commit lock, two concurrent `O_APPEND` writes can
        // resolve the same end offset and corrupt each other's bytes
        // into lines that look complete.
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("events.jsonl");
        let payload = "x".repeat(4000);
        let json = format!(
            r#"{{"v":1,"type":"tool_result","ts":"t","result":"{}"}}"#,
            payload
        );
        let n = 50;
        let mut handles = Vec::new();
        for _ in 0..2 {
            let log = log.clone();
            let json = json.clone();
            handles.push(std::thread::spawn(move || {
                let line = LogLine::from_json(&json);
                for _ in 0..n {
                    line.commit(&log).unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let text = std::fs::read_to_string(&log).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines.len(),
            2 * n,
            "every commit must land as one whole line"
        );
        for line in &lines {
            let v: serde_json::Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("a line is not valid JSON: {e}"));
            let got = v["result"].as_str().expect("the payload field");
            assert_eq!(got.len(), payload.len(), "the payload must survive intact");
        }
    }
}
