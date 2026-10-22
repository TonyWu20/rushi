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
//! P8 (docs/session-log-compression.md): in the same locked section
//! the commit appends one 32-byte index record to the `<log>.idx`
//! sidecar. The record is a derived fact; the log stays the
//! authority. A commit that tears between the frame and the
//! record leaves the stream grown past its index; the next commit
//! folds the un-indexed tail into its record, and the readers fall
//! back to the full text in the meantime.
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
    /// mid-commit. A new or zstd log carries frames, and each frame
    /// lands with its index record in the same locked section
    /// (P8).
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
            // A legacy plain log stays plain for its life (P2).
            // The sidecar is a zstd-stream fact; no record.
            file.write_all(&self.bytes)
        } else {
            let w = file.write_all(&frame);
            if w.is_ok() {
                // P8: the index record lands in the same locked
                // section as the frame. A record failure warns and
                // continues: the frame is the fact, the record is
                // derived, and the next commit folds the un-indexed
                // tail into its record.
                if let Err(e) = self.append_index_record(path, &frame) {
                    eprintln!("logline: index record for {}: {e}", path.display());
                }
            }
            w
        };
        // The close releases the lock too. The explicit unlock keeps
        // the lock from spanning the close.
        if unsafe { libc::flock(gfd, libc::LOCK_UN) } != 0 {
            // The close still releases it. Nothing to report.
        }
        res
    }

    /// P8: append this commit's index record to the `<log>.idx`
    /// sidecar. The caller holds the log-line lock.
    ///
    /// The record carries the seq start, the frame position, the
    /// frame length, and the decoded line length. The seq start
    /// counts the lines the index has not covered yet: the last
    /// record's own range, and, after a torn commit, the un-indexed
    /// stream tail. A missing or broken index bootstraps: one
    /// record covering the whole existing stream plus this frame,
    /// its seq count from one full decode.
    fn append_index_record(&self, path: &Path, frame: &[u8]) -> io::Result<()> {
        let new_len = path.metadata()?.len();
        let old_len = new_len - frame.len() as u64;
        let idx_path = crate::logstream::index_path(path);
        let records = crate::logstream::read_index(&idx_path)?;
        let record = if records.is_empty() || !crate::logstream::validate_index(&records, old_len) {
            // No valid index against this stream: bootstrap. The
            // record covers the whole existing stream plus this
            // frame.
            let (_, text_bytes) = Self::count_stream_lines(path, 0, old_len)?;
            crate::logstream::IndexRecord {
                seq_start: 1,
                frame_offset: 0,
                frame_len: new_len,
                text_len: text_bytes + self.bytes.len() as u64,
            }
        } else {
            let last = records.last().unwrap();
            let rec_start = last.frame_offset;
            let rec_end = last.frame_offset + last.frame_len;
            // The lines the index has not covered yet: the last
            // record's own range, and the un-indexed tail when a
            // torn commit left the stream grown past the index.
            let (rec_lines, _rec_bytes) =
                Self::count_stream_lines(path, rec_start, last.frame_len)?;
            let mut tail_bytes = 0u64;
            if rec_end < old_len {
                let (_, b) = Self::count_stream_lines(path, rec_end, old_len - rec_end)?;
                tail_bytes = b;
            }
            crate::logstream::IndexRecord {
                seq_start: last.seq_start + rec_lines,
                frame_offset: rec_end,
                frame_len: (old_len - rec_end) + frame.len() as u64,
                text_len: tail_bytes + self.bytes.len() as u64,
            }
        };
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&idx_path)?;
        f.write_all(&crate::logstream::index_record_bytes(&record))?;
        f.flush()?;
        Ok(())
    }

    /// Count the complete lines and the decoded bytes of one
    /// stream range. P4: a torn range tail keeps the complete
    /// frames; the partial line drops.
    fn count_stream_lines(path: &Path, off: u64, len: u64) -> io::Result<(u64, u64)> {
        if len == 0 {
            return Ok((0, 0));
        }
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open(path)?;
        f.seek(SeekFrom::Start(off))?;
        let mut buf = Vec::with_capacity(len as usize);
        std::io::copy(&mut f.take(len), &mut buf)?;
        if !crate::logstream::is_zstd_stream(&buf) {
            // The plain range: newline-terminated lines. The plain
            // path never indexes, so this arm is a safety net.
            let lines = buf.iter().filter(|b| **b == b'\n').count() as u64;
            return Ok((lines, buf.len() as u64));
        }
        let out = crate::logstream::decode_stream_bytes(&buf);
        let lines = out.iter().filter(|b| **b == b'\n').count() as u64;
        Ok((lines, out.len() as u64))
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
    use std::io::Write;

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
    fn commit_on_a_plain_text_log_writes_no_index_record() {
        // P8: the sidecar is a zstd-stream fact. A legacy plain
        // log gets plain lines and no `<log>.idx`.
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("events.jsonl");
        std::fs::write(&log, "seed\n").unwrap();
        LogLine::from_json(r#"{"v":1,"type":"user_message","ts":"t","content":"two"}"#)
            .commit(&log)
            .unwrap();
        assert!(!crate::logstream::index_path(&log).exists());
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

    // ------------------------------------------------------------------
    // P8: the frame index sidecar
    // ------------------------------------------------------------------

    #[test]
    fn commit_appends_the_frame_and_its_record_under_one_lock() {
        // P8: the frame and its index record land in one locked
        // section. The record carries the seq start, the frame
        // position, the frame length, and the decoded line length.
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("events.jsonl");
        let mut expected = Vec::new();
        for c in ["one", "two", "three"] {
            let json = format!(
                r#"{{"v":1,"type":"user_message","ts":"t","content":"{c}"}}"#
            );
            expected.push(json.len() as u64 + 1);
            LogLine::from_json(&json).commit(&log).unwrap();
        }
        let raw = std::fs::read(&log).unwrap();
        let records =
            crate::logstream::read_index(&crate::logstream::index_path(&log)).unwrap();
        assert_eq!(records.len(), 3, "one record per commit");
        for (i, r) in records.iter().enumerate() {
            assert_eq!(r.seq_start, (i + 1) as u64, "seq is the 1-based line number");
            assert_eq!(
                r.text_len,
                expected[i],
                "text_len is the decoded line length"
            );
        }
        for pair in records.windows(2) {
            assert_eq!(
                pair[1].frame_offset,
                pair[0].frame_offset + pair[0].frame_len,
                "the frames are contiguous in the stream"
            );
        }
        let last = records.last().unwrap();
        assert_eq!(
            last.frame_offset + last.frame_len,
            raw.len() as u64,
            "the index covers the stream"
        );
    }

    #[test]
    fn commit_bootstraps_the_index_on_a_pre_p8_stream() {
        // A pre-P8 zstd stream holds frames with no sidecar. The
        // first P8 commit opens the index with one record covering
        // the old stream plus the new frame.
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("events.jsonl");
        let a = crate::logstream::compress_frame(b"old-a\n").unwrap();
        let b = crate::logstream::compress_frame(b"old-b\n").unwrap();
        let mut buf = a;
        buf.extend_from_slice(&b);
        std::fs::write(&log, &buf).unwrap();
        let json = r#"{"v":1,"type":"user_message","ts":"t","content":"new"}"#;
        LogLine::from_json(json).commit(&log).unwrap();
        let raw = std::fs::read(&log).unwrap();
        let records =
            crate::logstream::read_index(&crate::logstream::index_path(&log)).unwrap();
        assert_eq!(records.len(), 1, "the bootstrap is one record");
        let r = &records[0];
        assert_eq!(r.seq_start, 1);
        assert_eq!(r.frame_offset, 0);
        assert_eq!(r.frame_len, raw.len() as u64);
        assert_eq!(
            r.text_len,
            "old-a\nold-b\n".len() as u64 + json.len() as u64 + 1
        );
        // The next commit extends the index; the seq continues
        // past the bootstrapped three lines.
        LogLine::from_json(r#"{"v":1,"type":"user_message","ts":"t","content":"newer"}"#)
            .commit(&log)
            .unwrap();
        let records =
            crate::logstream::read_index(&crate::logstream::index_path(&log)).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[1].seq_start, 4, "the fourth line is seq 4");
    }

    #[test]
    fn commit_heals_a_stream_grown_past_its_index() {
        // A torn commit: the frame landed, the record did not. The
        // next commit folds the un-indexed tail into its record.
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("events.jsonl");
        LogLine::from_json(r#"{"v":1,"type":"user_message","ts":"t","content":"one"}"#)
            .commit(&log)
            .unwrap();
        LogLine::from_json(r#"{"v":1,"type":"user_message","ts":"t","content":"two"}"#)
            .commit(&log)
            .unwrap();
        // Simulate the tear: a frame with no record.
        let extra = crate::logstream::compress_frame(b"three\n").unwrap();
        let mut f = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
        f.write_all(&extra).unwrap();
        let idx = crate::logstream::index_path(&log);
        let records = crate::logstream::read_index(&idx).unwrap();
        assert_eq!(
            records.last().unwrap().frame_offset + records.last().unwrap().frame_len,
            std::fs::read(&log).unwrap().len() as u64 - extra.len() as u64,
            "the index lags the stream by one frame"
        );
        LogLine::from_json(r#"{"v":1,"type":"user_message","ts":"t","content":"four"}"#)
            .commit(&log)
            .unwrap();
        let records = crate::logstream::read_index(&idx).unwrap();
        assert_eq!(records.len(), 3, "the heal appended one record");
        assert_eq!(
            records[2].seq_start,
            3,
            "the record covers the torn line and the new one"
        );
        let raw = std::fs::read(&log).unwrap();
        assert_eq!(
            records[2].frame_offset + records[2].frame_len,
            raw.len() as u64,
            "the healed index covers the stream"
        );
    }

    #[test]
    fn concurrent_commits_keep_the_index_contiguous() {
        // P8 under the FT-005 lock: concurrent commits serialize on
        // the log-line lock. The sidecar stays one contiguous
        // record chain, with no torn record at the tail.
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("events.jsonl");
        let json = r#"{"v":1,"type":"user_message","ts":"t","content":"m"}"#;
        let n = 25;
        let mut handles = Vec::new();
        for _ in 0..2 {
            let log = log.clone();
            let json = json.to_string();
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
        let raw = std::fs::read(&log).unwrap();
        let records =
            crate::logstream::read_index(&crate::logstream::index_path(&log)).unwrap();
        assert_eq!(records.len(), 2 * n, "every commit carries its record");
        for (i, r) in records.iter().enumerate() {
            assert_eq!(r.seq_start, (i + 1) as u64, "the seq runs unbroken");
        }
        let last = records.last().unwrap();
        assert_eq!(
            last.frame_offset + last.frame_len,
            raw.len() as u64,
            "the index covers the stream"
        );
    }
}
