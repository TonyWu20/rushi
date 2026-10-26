//! `rushi archive` — the idle consolidation pass (P5, P7,
//! docs/session-log-compression.md).
//!
//! Each session log is one zstd stream. While active it carries one
//! frame per commit. The pass rewrites the stream as one level-19
//! frame. The pass holds the log-line lock for the whole swap, so
//! no writer interleaves. A resumed session appends frames after
//! the consolidated frame; the reader never notices (P6).

use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;

use rushi_common::logstream;

/// The two session logs the pass covers, in write order.
const LOGS: [&str; 2] = ["events.jsonl", "tools.jsonl"];

/// Consolidate every present session log in `session_dir`.
///
/// Each pass: open the log (the lock target), take the exclusive
/// log-line lock, run the read-write-verify-rename under it,
/// release. A missing log skips. A failure reports and stops the
/// pass; the untouched logs keep their stream.
pub fn consolidate_session(session_dir: &Path) -> io::Result<()> {
    for name in LOGS {
        let path = session_dir.join(name);
        if !path.exists() {
            continue;
        }
        consolidate_locked(&path)?;
    }
    Ok(())
}

/// One log under the log-line lock: the same exclusive `flock` the
/// `LogLine` commit takes. The rename inside `consolidate` swaps
/// the file while the lock is held, so no commit lands mid-swap.
fn consolidate_locked(path: &Path) -> io::Result<()> {
    let file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    let fd = file.as_raw_fd();
    if unsafe { libc::flock(fd, libc::LOCK_EX) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let res = logstream::consolidate(path);
    // The close releases the lock too. The explicit unlock keeps
    // the lock from spanning the close.
    unsafe { libc::flock(fd, libc::LOCK_UN) };
    res
}

/// The auto pass of P7: the loop's session-end hook. Runs only when
/// the reason is `idle` and the config gate is on. A failure warns;
/// it never changes the loop's exit state.
pub fn run_auto_pass(cfg: &crate::config::HarnessConfig, session_dir: &Path, reason: &str) {
    if reason != "idle" || !cfg.log_consolidate_on_idle {
        return;
    }
    if let Err(e) = consolidate_session(session_dir) {
        eprintln!("rushi: log consolidation failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rushi_common::logstream;
    use rushi_common::logline::LogLine;

    /// Count the zstd frames in a log stream: one magic byte hit
    /// per frame head, scanned on the raw bytes.
    fn frame_count(path: &Path) -> usize {
        let buf = std::fs::read(path).unwrap();
        assert!(logstream::is_zstd_stream(&buf));
        let magic = logstream::ZSTD_MAGIC;
        let mut n = 0;
        let mut i = 0;
        while i + 4 <= buf.len() {
            if buf[i..i + 4] == magic[..] {
                n += 1;
                // A frame owns its own span. The next magic hit is
                // the next frame head. The scan finds both only
                // because inner frames never embed the magic.
                i += 4;
            } else {
                i += 1;
            }
        }
        n
    }

    /// One index record for a consolidated log: seq from 1, the
    /// whole stream, the whole decoded text (P8).
    fn assert_consolidated_index(log: &Path, expected_text: &str) {
        let raw = std::fs::read(log).unwrap();
        let records =
            logstream::read_index(&logstream::index_path(log)).unwrap();
        assert_eq!(records.len(), 1, "the consolidated frame carries one record");
        let r = &records[0];
        assert_eq!(r.seq_start, 1);
        assert_eq!(r.frame_offset, 0);
        assert_eq!(r.frame_len, raw.len() as u64);
        assert_eq!(r.text_len, expected_text.len() as u64);
    }

    #[test]
    fn consolidate_session_rewrites_both_logs_under_lock() {
        // P5: two per-commit frames become one -19 frame, per log.
        let dir = tempfile::tempdir().unwrap();
        let sess = dir.path().join("sess");
        std::fs::create_dir_all(&sess).unwrap();
        let ev = sess.join("events.jsonl");
        LogLine::from_json(r#"{"v":1,"type":"user_message","ts":"t","content":"one"}"#)
            .commit(&ev)
            .unwrap();
        LogLine::from_json(r#"{"v":1,"type":"user_message","ts":"t","content":"two"}"#)
            .commit(&ev)
            .unwrap();
        let tl = sess.join("tools.jsonl");
        LogLine::from_json(r#"{"v":1,"ts":"t","id":"c1","exit":0,"stdout":"s","stderr":"","text":"s"}"#)
            .commit(&tl)
            .unwrap();
        assert_eq!(frame_count(&ev), 2, "two per-commit frames");
        consolidate_session(&sess).unwrap();
        assert_eq!(frame_count(&ev), 1, "one consolidated frame");
        assert_eq!(frame_count(&tl), 1, "one consolidated frame");
        let expected = String::from(
                r#"{"v":1,"type":"user_message","ts":"t","content":"one"}"#,
            ) + "\n"
                + r#"{"v":1,"type":"user_message","ts":"t","content":"two"}"#
                + "\n";
        assert_eq!(logstream::read_log_text(&ev).unwrap(), expected);
        // P8: the index pair rewrites with the stream.
        assert_consolidated_index(&ev, &expected);
        let tool_text = logstream::read_log_text(&tl).unwrap();
        assert_consolidated_index(&tl, &tool_text);
    }

    #[test]
    fn consolidate_session_skips_missing_logs() {
        let dir = tempfile::tempdir().unwrap();
        let sess = dir.path().join("sess");
        std::fs::create_dir_all(&sess).unwrap();
        consolidate_session(&sess).unwrap();
    }

    #[test]
    fn auto_pass_gates_on_the_reason_and_flag() {
        // P7: the pass runs only on the idle reason with the gate on.
        let dir = tempfile::tempdir().unwrap();
        let sess = dir.path().join("sess");
        std::fs::create_dir_all(&sess).unwrap();
        let ev = sess.join("events.jsonl");
        LogLine::from_json(r#"{"v":1,"type":"user_message","ts":"t","content":"one"}"#)
            .commit(&ev)
            .unwrap();
        let mut cfg = crate::config::HarnessConfig {
            config_path: std::path::PathBuf::from("config.toml"),
            config_dir: dir.path().to_path_buf(),
            sessions_root: dir.path().join("sessions"),
            native_tool_paths: Vec::new(),
            extension_tool_paths: Vec::new(),
            active_model: "stub".into(),
            model_id: "stub".into(),
            max_output_tokens: 32768,
            context_tokens: 8192,
            input_budget: 1000,
            context_budget: 1000,
            last_measured_input: 0,
            compact_enabled: false,
            compact_strategy: "compact".into(),
            compact_reserve_tokens: 16384,
            compact_keep_tokens: 20000,
            compact_text_chars: 200,
            estimate_chars_per_token: 4,
            approval_timeout_s: None,
            hooks_timeout_ms: 30000,
            hook_defs: std::collections::BTreeMap::new(),
            hook_pipelines: std::collections::BTreeMap::new(),
            model_bin: std::path::PathBuf::from("model"),
            compact_bin: std::path::PathBuf::from("compact"),
            assemble_bin: std::path::PathBuf::from("assemble"),
            route_bin: std::path::PathBuf::from("route"),
            claim_bin: std::path::PathBuf::from("claim"),
            parse_bin: std::path::PathBuf::from("parse"),
            log_bin: std::path::PathBuf::from("rushi-log"),
            log_consolidate_on_idle: true,
        };
        // The signal reason skips the pass: the stream keeps its frame.
        run_auto_pass(&cfg, &sess, "signal");
        assert_eq!(frame_count(&ev), 1, "the signal reason skips");
        // The idle reason runs it: two commits, one frame.
        LogLine::from_json(r#"{"v":1,"type":"user_message","ts":"t","content":"two"}"#)
            .commit(&ev)
            .unwrap();
        run_auto_pass(&cfg, &sess, "idle");
        assert_eq!(frame_count(&ev), 1, "the idle reason consolidates");
        // The gate off skips the pass.
        cfg.log_consolidate_on_idle = false;
        LogLine::from_json(r#"{"v":1,"type":"user_message","ts":"t","content":"three"}"#)
            .commit(&ev)
            .unwrap();
        run_auto_pass(&cfg, &sess, "idle");
        assert_eq!(frame_count(&ev), 2, "the gate off skips");
    }
}
