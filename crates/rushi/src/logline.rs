//! `LogLine` — the one type that may write a session log.
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
//! Migrated from four copies (`bin/log`, `bin/user`, `bin/route`,
//! `bin/tui`) into the shared crate (docs/phase-2-plan.md section 6).

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
    ///
    /// The line compresses into one zstd frame (P1,
    /// docs/session-log-compression.md). The frame is the write
    /// unit. Readers stream the frames back in order; a torn tail
    /// frame drops at parse time (P4).
    ///
    /// The format is decided at creation and held for the file's
    /// life. A legacy plain-text log keeps plain lines: the peek
    /// runs under the lock, so an archiver cannot swap the format
    /// mid-commit. A new or zstd log carries frames.
    pub fn commit(&self, path: &Path) -> io::Result<()> {
        let frame = crate::logstream::compress_frame(&self.bytes)
            .map_err(|e| io::Error::other(format!("zstd compress: {e}")))?;
        // The gate opens the lock target (creating it when new) and
        // carries the exclusive lock.
        let gate = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        let gfd = gate.as_raw_fd();
        if unsafe { libc::flock(gfd, libc::LOCK_EX) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // The write resolves the current inode under the lock: a
        // concurrent archiver rename cannot hand us a stale inode.
        // The peek reads the format from its own plain fd. A kernel
        // that refuses to seek an `O_APPEND` fd cannot stop the
        // peek, because the peek never seeks.
        let mut file = std::fs::OpenOptions::new().append(true).open(path)?;
        let legacy = Self::peek_plain_text(path)?;
        let res = if legacy {
            file.write_all(&self.bytes)
        } else {
            file.write_all(&frame)
        };
        // The close releases the lock too. The explicit unlock keeps
        // the lock from spanning the close.
        if unsafe { libc::flock(gfd, libc::LOCK_UN) } != 0 {
            // The close still releases it. Nothing to report.
        }
        res
    }

    /// True when the current log is a legacy plain-text log: it
    /// exists, it holds bytes, and its head is not the zstd magic.
    /// The peek opens its own read-only descriptor and reads four
    /// bytes. It never seeks. It runs under the commit lock.
    fn peek_plain_text(path: &Path) -> io::Result<bool> {
        use std::io::Read;
        // A plain read-only fd: no seek, no append flag. The fd
        // opens at offset zero on the current inode.
        let mut file = match std::fs::File::open(path) {
            Ok(f) => f,
            // A missing file: the gate created it; the commit
            // creates the stream.
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e),
        };
        let mut head = [0u8; 4];
        match Read::read(&mut file, &mut head) {
            // An empty file creates the stream: not legacy.
            Ok(0) => Ok(false),
            // Content exists: legacy unless the head is the magic.
            Ok(_) => Ok(!crate::logstream::is_zstd_stream(&head)),
            Err(e) => Err(e),
        }
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
        // The file is a zstd stream of one frame per commit (P1, P2).
        assert!(crate::logstream::is_zstd_stream(
            &std::fs::read(&log).unwrap()
        ));
        let text = crate::logstream::read_log_text(&log).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text:?}");
        assert!(lines[0].contains("\"one\""), "{lines:?}");
        assert!(lines[1].contains("\"two\""), "{lines:?}");
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn commit_on_a_plain_text_log_keeps_plain_lines() {
        // The format is decided at creation (P2): a legacy plain
        // text log stays plain for its life. No mixed streams.
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("events.jsonl");
        std::fs::write(&log, "{\"v\":1,\"type\":\"user_message\",\"ts\":\"t0\",\"content\":\"seed\"}\n").unwrap();
        LogLine::from_json(r#"{"v":1,"type":"user_message","ts":"t","content":"two"}"#)
            .commit(&log)
            .unwrap();
        let raw = std::fs::read(&log).unwrap();
        assert!(!crate::logstream::is_zstd_stream(&raw), "a legacy log stays plain");
        let text = crate::logstream::read_log_text(&log).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text:?}");
        assert!(lines[0].contains("seed"), "{lines:?}");
        assert!(lines[1].contains("two"), "{lines:?}");
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
        let text = crate::logstream::read_log_text(&log).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines.len(),
            2 * n,
            "every commit must land as one whole line"
        );
        for line in &lines {
            let v: serde_json::Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("a line is not valid JSON: {e}"));
            assert!(v.as_object().is_some());
        }
    }
}
