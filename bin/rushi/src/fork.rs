//! `rushi fork` — fork a session at a log entry
//! (docs/fork-design.md).
//!
//! The cut copies the two session logs into a new session dir, cut
//! at log entry N. The new session is idle: it holds no lock and
//! no pid. The caller starts it through the plain `rushi run`
//! flow.

use std::path::{Path, PathBuf};

use rushi_common::logstream;
use rushi_common::rewind;
use serde_json::Value;

use crate::config::{resolve_dir_override, HarnessConfig};

/// The named fork refusal. Every variant is the stderr line
/// `rushi fork:` carries (docs/fork-design.md P6, P7, P8, P11, P12).
#[derive(Debug)]
pub enum ForkError {
    /// `NEW` carries a path separator.
    BadNewName(String),
    /// The parent session dir does not exist.
    ParentNotFound(PathBuf),
    /// The parent dir holds no `events.jsonl`.
    NoEvents(PathBuf),
    /// The parent log holds no event lines.
    EmptyLog(PathBuf),
    /// N sits outside [1, the total line count].
    SeqOutOfRange {
        fork_at: u64,
        total: u64,
        /// The parseable line count, when it sits below `total`
        /// (a hand-edited log).
        parsed: Option<u64>,
    },
    /// The settled-boundary rule refuses the cut. The message names
    /// the stranded pair.
    Stranded(rewind::RewindRejection),
    /// The target dir exists. No overwrite.
    TargetExists(PathBuf),
    /// A dir-form parent has no sessions-root parent.
    NoRoot(PathBuf),
    /// A write or rename failed.
    Io(std::io::Error),
}

impl std::fmt::Display for ForkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ForkError::BadNewName(n) => write!(
                f,
                "the new session name '{n}' must be a bare name: no path separator"
            ),
            ForkError::ParentNotFound(p) => {
                write!(f, "the parent session '{}' was not found", p.display())
            }
            ForkError::NoEvents(p) => {
                write!(f, "the parent session '{}' has no events.jsonl", p.display())
            }
            ForkError::EmptyLog(p) => {
                write!(f, "the parent session '{}' holds no event lines", p.display())
            }
            ForkError::SeqOutOfRange {
                fork_at,
                total,
                parsed,
            } => match parsed {
                Some(k) => write!(
                    f,
                    "the fork boundary seq {fork_at} is out of range: the parent log holds {total} event line(s), of which {k} parse"
                ),
                None => write!(
                    f,
                    "the fork boundary seq {fork_at} is out of range: the parent log holds {total} event line(s)"
                ),
            },
            ForkError::Stranded(rej) => match rej {
                rewind::RewindRejection::StrandedCall {
                    call_id,
                    target,
                    before,
                } => {
                    debug_assert!(!before, "the fork cut is on-mode only");
                    write!(
                        f,
                        "the fork at seq {target} strands tool call {call_id}: the prefix keeps the call without its tool_result"
                    )
                }
                rewind::RewindRejection::StrandedResult {
                    result_id,
                    target,
                    before,
                } => {
                    debug_assert!(!before, "the fork cut is on-mode only");
                    write!(
                        f,
                        "the fork at seq {target} strands tool result {result_id}: the prefix keeps the result without its tool_call"
                    )
                }
                // Unreachable in practice: the caller maps the
                // out-of-range arm to `SeqOutOfRange` before it
                // reaches the fork error.
                rewind::RewindRejection::TargetOutOfRange { target, log_len } => {
                    write!(
                        f,
                        "the fork boundary seq {target} is out of range: the prefix parses {log_len} event line(s)"
                    )
                }
            },
            ForkError::TargetExists(t) => write!(
                f,
                "the target session '{}' already exists: the fork refuses to overwrite",
                t.display()
            ),
            ForkError::NoRoot(p) => write!(
                f,
                "the parent session '{}' has no sessions root: its dir has no parent",
                p.display()
            ),
            ForkError::Io(e) => write!(f, "the fork failed: {e}"),
        }
    }
}

/// The path computation for one fork (docs/fork-design.md P11).
///
/// The effective root: a `--sessions-root` flag, else the parent
/// dir's parent for a dir-form parent, else the config root. The
/// parent dir: a dir-form parent as-is, a bare name under the root.
/// The target: always `<root>/<new>`.
#[derive(Debug)]
pub struct ForkPaths {
    /// The sessions root the target lands under.
    pub root: PathBuf,
    /// The parent session dir.
    pub parent_dir: PathBuf,
    /// The target session dir.
    pub target_dir: PathBuf,
}

/// Compute the fork paths. The flag root must be resolved to an
/// absolute path by the caller.
pub fn resolve_paths(
    cfg: &HarnessConfig,
    parent: &str,
    new: &str,
    flag_root: Option<&Path>,
) -> Result<ForkPaths, ForkError> {
    if new.is_empty() || new.contains('/') || new.contains('\\') {
        return Err(ForkError::BadNewName(new.to_string()));
    }
    let parent_path = PathBuf::from(parent);
    // The dir-form test mirrors `resolve_session` (issue #49 rule:
    // a path or an existing dir is used as-is).
    let dir_form = parent.contains('/')
        || parent.contains('\\')
        || parent_path.is_dir();
    let root = match flag_root {
        Some(r) => r.to_path_buf(),
        None if dir_form => {
            let p = Path::new(parent);
            match p.parent() {
                Some(par) if !par.as_os_str().is_empty() => par.to_path_buf(),
                _ => return Err(ForkError::NoRoot(parent_path)),
            }
        }
        None => cfg.sessions_root.clone(),
    };
    let parent_dir = if dir_form {
        parent_path
    } else {
        root.join(parent)
    };
    let target_dir = root.join(new);
    Ok(ForkPaths {
        root,
        parent_dir,
        target_dir,
    })
}

/// The settled-boundary guard over a parsed event prefix
/// (docs/fork-design.md P6, the P8 guard in cut form).
///
/// The target is the prefix end in on mode: the projected prefix
/// keeps every tool call paired with its result, both sides. The
/// out-of-range arm maps to `SeqOutOfRange`: it only fires when
/// the prefix holds unparseable lines (a hand-edited log).
pub fn settled_guard(
    parsed: &[Value],
    fork_at: u64,
    total: u64,
) -> Result<(), ForkError> {
    match rewind::guard_rewind(parsed, fork_at, false) {
        Ok(()) => Ok(()),
        Err(rewind::RewindRejection::TargetOutOfRange { .. }) => Err(
            ForkError::SeqOutOfRange {
                fork_at,
                total,
                parsed: Some(parsed.len() as u64),
            },
        ),
        Err(other) => Err(ForkError::Stranded(other)),
    }
}

/// The last kept tool-log line count, keyed by the last
/// `tool_result` call id at or before the boundary
/// (docs/fork-design.md P4).
///
/// The tool log holds one record per `tool_result`, in call order
/// (`route` writes the line before the event, same call id). The
/// key that sits in no record keeps every record: the fork never
/// drops a record a kept event can reference.
pub fn kept_tool_lines(tool_lines: &[Value], key: &str) -> usize {
    tool_lines
        .iter()
        .rposition(|rec| rec.get("id").and_then(|i| i.as_str()) == Some(key))
        .map_or(tool_lines.len(), |i| i + 1)
}

/// The call id of the last `tool_result` at or before the boundary,
/// over the parsed prefix in seq order. `None` keeps no tool log.
pub fn last_result_key(parsed_prefix: &[Value]) -> Option<String> {
    parsed_prefix.iter().rev().find_map(|ev| {
        if ev.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
            ev.get("id").and_then(|i| i.as_str()).map(String::from)
        } else {
            None
        }
    })
}

/// The parent's recorded working dir, when the `cwd` file holds
/// a non-empty path.
pub fn parent_cwd(parent_dir: &Path) -> Option<PathBuf> {
    std::fs::read_to_string(parent_dir.join("cwd"))
        .ok()
        .map(|s| PathBuf::from(s.trim()))
        .filter(|p| !p.as_os_str().is_empty())
}

/// The complete lines of a decoded log text: the lines that carry
/// a trailing newline. A partial final line (a torn tail frame)
/// drops, the P4 rule. Mirrors `logstream`'s private helper.
fn complete_lines(text: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = text.lines().collect();
    if !text.is_empty() && !text.ends_with('\n') {
        lines.pop();
    }
    lines
}

/// Join complete lines into the log text: each line plus its
/// trailing newline.
fn lines_to_text(lines: &[&str]) -> String {
    let mut text = String::new();
    for line in lines {
        text.push_str(line);
        text.push('\n');
    }
    text
}

/// True when the log head carries the zstd magic bytes.
fn log_is_zstd(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 4];
    match std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut head)) {
        Ok(_) => logstream::is_zstd_stream(&head),
        Err(_) => false,
    }
}

/// One log cut: write the prefix text into `out` in the parent's
/// layout. The prefix came from the single parent read; the parent
/// path sniffs only the layout (plain or zstd). A live parent keeps
/// committing between the two reads: the cut holds the prefix the
/// first read captured.
fn cut_log(
    parent_log: &Path,
    out: &Path,
    prefix: &[&str],
) -> Result<(), ForkError> {
    let text = lines_to_text(prefix);
    if log_is_zstd(parent_log) {
        logstream::write_consolidated(out, &text).map_err(ForkError::Io)
    } else {
        std::fs::write(out, text).map_err(ForkError::Io)
    }
}

/// Fork one session (docs/fork-design.md).
///
/// `sessions_root` and `cwd` are the raw flag values, resolved
/// against CWD by the `rushi run` rule. The parent reads through
/// `logstream` (plain or zstd stream). The target builds in a temp
/// dir and renames into place, the `consolidate` pattern.
pub fn fork_session(
    cfg: &HarnessConfig,
    parent: &str,
    new: &str,
    fork_at: u64,
    sessions_root: Option<&Path>,
    cwd: Option<&Path>,
) -> Result<PathBuf, ForkError> {
    let flag_root = sessions_root.map(resolve_dir_override);
    let paths = resolve_paths(cfg, parent, new, flag_root.as_deref())?;
    if !paths.parent_dir.is_dir() {
        return Err(ForkError::ParentNotFound(paths.parent_dir));
    }
    let events = paths.parent_dir.join("events.jsonl");
    if !events.is_file() {
        return Err(ForkError::NoEvents(paths.parent_dir.clone()));
    }

    // The event prefix and its settled-boundary check.
    let text = logstream::read_log_text(&events).map_err(ForkError::Io)?;
    let lines = complete_lines(&text);
    let total = lines.len() as u64;
    if total == 0 {
        return Err(ForkError::EmptyLog(paths.parent_dir.clone()));
    }
    if fork_at < 1 || fork_at > total {
        return Err(ForkError::SeqOutOfRange {
            fork_at,
            total,
            parsed: None,
        });
    }
    let prefix = &lines[..fork_at as usize];
    // The lenient parse: a hand-edited log skips bad lines, like
    // the P8 write guard does (G7: readers never fail on a
    // malformed line).
    let parsed: Vec<Value> = prefix
        .iter()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    settled_guard(&parsed, fork_at, total)?;

    // The tool-log boundary (P4, P5).
    let tools = paths.parent_dir.join("tools.jsonl");
    let tool_cut: Option<Vec<String>> = match last_result_key(&parsed) {
        Some(key) if tools.is_file() => {
            let tool_text =
                logstream::read_log_text(&tools).map_err(ForkError::Io)?;
            let tool_lines = complete_lines(&tool_text);
            let parsed_tools: Vec<Value> = tool_lines
                .iter()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect();
            let keep = kept_tool_lines(&parsed_tools, &key);
            Some(tool_lines[..keep].iter().copied().map(String::from).collect())
        }
        _ => None,
    };

    // The working dir (P9): the flag wins, else the parent's
    // recorded `cwd` file, else none.
    let cwd_file = match cwd {
        Some(raw) => Some(resolve_dir_override(raw)),
        None => parent_cwd(&paths.parent_dir),
    };

    // The atomics (P8, P12): an existing target refuses; the
    // session builds in a temp dir and renames into place.
    if paths.target_dir.exists() {
        return Err(ForkError::TargetExists(paths.target_dir.clone()));
    }
    let tmp = paths.root.join(format!(".{new}.fork-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let result: Result<PathBuf, ForkError> = (|| {
        std::fs::create_dir_all(&tmp).map_err(ForkError::Io)?;
        cut_log(&events, &tmp.join("events.jsonl"), prefix)?;
        if let Some(tool_lines) = &tool_cut {
            let out = tmp.join("tools.jsonl");
            let strs: Vec<&str> = tool_lines.iter().map(String::as_str).collect();
            let text = lines_to_text(&strs);
            if log_is_zstd(&tools) {
                logstream::write_consolidated(&out, &text).map_err(ForkError::Io)?;
            } else {
                std::fs::write(&out, text).map_err(ForkError::Io)?;
            }
        }
        if let Some(dir) = &cwd_file {
            std::fs::write(tmp.join("cwd"), dir.to_string_lossy().as_bytes())
                .map_err(ForkError::Io)?;
        }
        std::fs::rename(&tmp, &paths.target_dir).map_err(ForkError::Io)?;
        Ok(paths.target_dir.clone())
    })();
    if result.is_err() {
        // No partial write survives (P12).
        let _ = std::fs::remove_dir_all(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HarnessConfig;
    use rushi_common::logline::LogLine;

    /// One minimal config with a controllable sessions root.
    fn cfg_with_root(root: &Path) -> HarnessConfig {
        let mut cfg = cfg_base();
        cfg.sessions_root = root.to_path_buf();
        cfg
    }

    /// The full-struct literal a fork test needs. Mirrors the
    /// `archive` test harness.
    fn cfg_base() -> HarnessConfig {
        let dir = std::env::current_dir().unwrap();
        HarnessConfig {
            config_path: std::path::PathBuf::from("config.toml"),
            config_dir: dir.clone(),
            sessions_root: dir.join("sessions"),
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
        }
    }

    /// A plain-text parent session: events, an optional tool log,
    /// and an optional `cwd` record.
    fn plain_session(dir: &Path, name: &str) -> PathBuf {
        let sess = dir.join(name);
        std::fs::create_dir_all(&sess).unwrap();
        sess
    }

    fn events(dir: &Path, name: &str, lines: &[&str]) -> PathBuf {
        let sess = plain_session(dir, name);
        std::fs::write(sess.join("events.jsonl"), lines_to_text(lines)).unwrap();
        sess
    }

    fn user(content: &str) -> &'static str {
        match content {
            "task" => r#"{"v":1,"type":"user_message","ts":"t","content":"task"}"#,
            "more" => r#"{"v":1,"type":"user_message","ts":"t","content":"more"}"#,
            other => panic!("unknown user content: {other}"),
        }
    }

    fn asst(calls: &[&str]) -> String {
        let calls: Vec<Value> = calls
            .iter()
            .map(|id| {
                serde_json::json!({"id":id,"name":"bash","arguments":{"command":"c"}})
            })
            .collect();
        serde_json::json!({
            "v":1,"type":"assistant_message","ts":"t","content":"",
            "tool_calls":calls,"stop_reason":"tool_calls"
        })
        .to_string()
    }

    fn result(id: &str) -> String {
        serde_json::json!({
            "v":1,"type":"tool_result","ts":"t","id":id,
            "value":{"text":format!("{id} result")},"is_error":false
        })
        .to_string()
    }

    fn tool_rec(id: &str) -> String {
        serde_json::json!({
            "v":1,"ts":"t","id":id,"exit":0,"stdout":format!("{id} out"),
            "stderr":"","text":format!("{id} out"),"is_error":false
        })
        .to_string()
    }

    /// P1: the forked event log decodes to the parent lines 1..=N.
    #[test]
    fn fork_copies_the_event_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sessions");
        events(
            &root,
            "p",
            &[user("task"), &asst(&["c1"]), &result("c1"), user("more")],
        );
        let cfg = cfg_with_root(&root);
        let target = fork_session(&cfg, "p", "f", 3, None, None).unwrap();
        assert_eq!(target, root.join("f"));
        let text = std::fs::read_to_string(target.join("events.jsonl")).unwrap();
        let want = lines_to_text(&[user("task"), &asst(&["c1"]), &result("c1")]);
        assert_eq!(text, want, "the fork holds lines 1..=N, in order");
    }

    /// P2: a plain parent stays plain. No sidecar lands.
    #[test]
    fn fork_keeps_a_plain_log_plain() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sessions");
        events(&root, "p", &[user("task"), user("more")]);
        let cfg = cfg_with_root(&root);
        let target = fork_session(&cfg, "p", "f", 2, None, None).unwrap();
        let raw = std::fs::read(target.join("events.jsonl")).unwrap();
        assert!(
            !logstream::is_zstd_stream(&raw),
            "the forked log stays plain text"
        );
        assert!(!target.join("events.jsonl.idx").exists());
    }

    /// P2: a zstd parent stays a zstd stream. The rebuilt sidecar
    /// passes the open-time checks.
    #[test]
    fn fork_writes_a_consolidated_zstd_log_with_a_valid_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sessions");
        let sess = plain_session(&root, "p");
        let log = sess.join("events.jsonl");
        LogLine::from_json(user("task")).commit(&log).unwrap();
        LogLine::from_json(&result("c1")).commit(&log).unwrap();
        let cfg = cfg_with_root(&root);
        let target = fork_session(&cfg, "p", "f", 1, None, None).unwrap();
        let raw = std::fs::read(target.join("events.jsonl")).unwrap();
        assert!(logstream::is_zstd_stream(&raw));
        assert_eq!(logstream::read_log_text(&target.join("events.jsonl")).unwrap(), lines_to_text(&[user("task")]));
        let records = logstream::read_index(&logstream::index_path(&target.join("events.jsonl"))).unwrap();
        assert_eq!(records.len(), 1, "the forked log carries one record");
        assert_eq!(records[0].seq_start, 1);
        assert_eq!(records[0].frame_offset, 0);
        assert_eq!(records[0].frame_len, raw.len() as u64);
        assert!(logstream::validate_index(&records, raw.len() as u64));
        // The open-time validation the readers run: the sidecar
        // must validate against the stream or the read drops it.
        let stream = logstream::open_indexed(&target.join("events.jsonl")).unwrap();
        assert!(stream.has_index(), "the forked log opens validated");
    }

    /// P3: the next append lands after the forked frame. The seq
    /// numbering keeps running.
    #[test]
    fn a_commit_after_the_forked_stream_continues_the_seq() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sessions");
        let sess = plain_session(&root, "p");
        let log = sess.join("events.jsonl");
        for line in [user("task"), &asst(&["c1"]), &result("c1")] {
            LogLine::from_json(line).commit(&log).unwrap();
        }
        let cfg = cfg_with_root(&root);
        let target = fork_session(&cfg, "p", "f", 3, None, None).unwrap();
        let forked = target.join("events.jsonl");
        LogLine::from_json(user("more")).commit(&forked).unwrap();
        let records = logstream::read_index(&logstream::index_path(&forked)).unwrap();
        assert_eq!(records.len(), 2, "the resume adds one record");
        assert_eq!(records[1].seq_start, 4, "the fourth line is seq 4");
        let text = logstream::read_log_text(&forked).unwrap();
        let lines = text.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 4, "{text}");
        assert!(lines[3].contains("more"), "the resume line is last: {text}");
    }

    /// P4: the last kept tool line keys the last `tool_result` at
    /// or before N.
    #[test]
    fn the_tool_boundary_keys_the_last_kept_line() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sessions");
        let sess = events(
            &root,
            "p",
            &[
                user("task"),
                &asst(&["c1"]),
                &result("c1"),
                &asst(&["c2"]),
                &result("c2"),
                &asst(&["c3"]),
                &result("c3"),
            ],
        );
        std::fs::write(
            sess.join("tools.jsonl"),
            lines_to_text(&[&tool_rec("c1"), &tool_rec("c2"), &tool_rec("c3")]),
        )
        .unwrap();
        let cfg = cfg_with_root(&root);
        // N = 5: the last `tool_result` at or before 5 is c2.
        let target = fork_session(&cfg, "p", "f", 5, None, None).unwrap();
        let text = std::fs::read_to_string(target.join("tools.jsonl")).unwrap();
        let want = lines_to_text(&[&tool_rec("c1"), &tool_rec("c2")]);
        assert_eq!(text, want, "the fork keeps the c1 and c2 records only");
    }

    /// P4 edge: a key that sits in no tool record keeps every
    /// record.
    #[test]
    fn a_missing_key_keeps_every_tool_line() {
        let lines: Vec<Value> = vec![
            serde_json::json!({"id":"a","text":"a"}),
            serde_json::json!({"id":"b","text":"b"}),
        ];
        assert_eq!(kept_tool_lines(&lines, "zz"), 2, "the miss keeps all");
        assert_eq!(kept_tool_lines(&lines, "b"), 2, "the last match keeps the tail");
        assert_eq!(kept_tool_lines(&lines, "a"), 1, "the first match cuts the tail");
    }

    /// P5: no `tool_result` at or before N keeps no tool log.
    #[test]
    fn a_prefix_without_results_keeps_no_tool_log() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sessions");
        let sess = events(&root, "p", &[user("task"), user("more")]);
        std::fs::write(sess.join("tools.jsonl"), lines_to_text(&[&tool_rec("c1")])).unwrap();
        let cfg = cfg_with_root(&root);
        let target = fork_session(&cfg, "p", "f", 2, None, None).unwrap();
        assert!(
            !target.join("tools.jsonl").exists(),
            "the prefix has no tool result: no tool log"
        );
    }

    /// P5: a parent without a tool log keeps none.
    #[test]
    fn a_parent_without_a_tool_log_keeps_none() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sessions");
        events(
            &root,
            "p",
            &[user("task"), &asst(&["c1"]), &result("c1")],
        );
        let cfg = cfg_with_root(&root);
        let target = fork_session(&cfg, "p", "f", 3, None, None).unwrap();
        assert!(!target.join("tools.jsonl").exists());
    }

    /// P6: the stranded prefix refuses by name. Nothing writes.
    #[test]
    fn a_stranded_prefix_is_refused_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sessions");
        events(
            &root,
            "p",
            &[user("task"), &asst(&["c1"]), &result("c1")],
        );
        let cfg = cfg_with_root(&root);
        // N = 2: the prefix keeps the c1 call without its result.
        let err = fork_session(&cfg, "p", "f", 2, None, None).unwrap_err();
        match err {
            ForkError::Stranded(rewind::RewindRejection::StrandedCall {
                call_id,
                target,
                before,
            }) => {
                assert_eq!(call_id, "c1");
                assert_eq!(target, 2);
                assert!(!before);
            }
            other => panic!("expected the stranded-call refusal: {other:?}"),
        }
        assert!(!root.join("f").exists(), "the refusal writes nothing");
    }

    /// P7: N outside [1, total] refuses by name. Nothing writes.
    #[test]
    fn an_out_of_range_boundary_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sessions");
        events(
            &root,
            "p",
            &[user("task"), &asst(&["c1"]), &result("c1")],
        );
        let cfg = cfg_with_root(&root);
        for n in [0, 4] {
            let err = fork_session(&cfg, "p", "f", n, None, None).unwrap_err();
            assert!(
                matches!(err, ForkError::SeqOutOfRange { fork_at, total, .. } if fork_at == n && total == 3),
                "n = {n}: {err:?}"
            );
        }
        assert!(!root.join("f").exists());
    }

    /// P8: an existing target refuses. The existing session stays
    /// untouched.
    #[test]
    fn an_existing_target_is_refused_and_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sessions");
        events(&root, "p", &[user("task")]);
        let target = root.join("f");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("events.jsonl"), "old\n").unwrap();
        let cfg = cfg_with_root(&root);
        let err = fork_session(&cfg, "p", "f", 1, None, None).unwrap_err();
        assert!(matches!(err, ForkError::TargetExists(_)), "{err:?}");
        assert_eq!(
            std::fs::read_to_string(target.join("events.jsonl")).unwrap(),
            "old\n",
            "the existing session is untouched"
        );
    }

    /// P9: the cwd precedence is flag, then the parent's record,
    /// then none.
    #[test]
    fn the_cwd_precedence_is_flag_then_record() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sessions");
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let sess = events(&root, "p", &[user("task")]);
        std::fs::write(sess.join("cwd"), "/parent/work").unwrap();
        let cfg = cfg_with_root(&root);
        // No flag: the parent's record wins.
        let target = fork_session(&cfg, "p", "f1", 1, None, None).unwrap();
        assert_eq!(
            std::fs::read_to_string(target.join("cwd")).unwrap(),
            "/parent/work"
        );
        // The flag wins over the record.
        let target = fork_session(&cfg, "p", "f2", 1, None, Some(&work)).unwrap();
        assert_eq!(std::fs::read_to_string(target.join("cwd")).unwrap(), work.to_string_lossy());
        // No record, no flag: no `cwd` file.
        let bare = events(&root, "q", &[user("task")]);
        let target = fork_session(&cfg, "q", "f3", 1, None, None).unwrap();
        let _ = &bare;
        assert!(!target.join("cwd").exists());
    }

    /// P10: the forked session holds no runtime files.
    #[test]
    fn the_fork_holds_no_loop_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sessions");
        let sess = events(&root, "p", &[user("task")]);
        // The parent runtime files stay out of the fork.
        std::fs::write(sess.join("loop.pid"), "123\n").unwrap();
        std::fs::write(sess.join("loop.meta"), "pid = 123\n").unwrap();
        std::fs::write(sess.join(".loop.lock"), "").unwrap();
        let cfg = cfg_with_root(&root);
        let target = fork_session(&cfg, "p", "f", 1, None, None).unwrap();
        for name in ["loop.pid", "loop.meta", ".loop.lock"] {
            assert!(!target.join(name).exists(), "the fork holds no {name}");
        }
    }

    /// P11: the root rule is flag, then the parent's parent, then
    /// the config root. A missing parent raises a named error.
    #[test]
    fn the_root_rule_is_flag_then_parent_parent_then_config() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        // A dir-form parent under `a`: the fork lands in `a`.
        let sess = events(&a, "p", &[user("task")]);
        let cfg = cfg_with_root(&b.join("sessions"));
        let target = fork_session(
            &cfg,
            sess.to_string_lossy().as_ref(),
            "f",
            1,
            None,
            None,
        )
        .unwrap();
        assert_eq!(target, a.join("f"), "the dir-form parent keeps its own root");
        // A flag overrides the parent's own root.
        let target = fork_session(
            &cfg,
            sess.to_string_lossy().as_ref(),
            "g",
            1,
            Some(&b.join("sessions")),
            None,
        )
        .unwrap();
        assert_eq!(target, b.join("sessions").join("g"));
        // A bare name resolves under the config root.
        events(&b.join("sessions"), "n", &[user("task")]);
        let target = fork_session(&cfg, "n", "h", 1, None, None).unwrap();
        assert_eq!(target, b.join("sessions").join("h"));
        // A name with a separator is not a bare name.
        let err = fork_session(&cfg, "n", "a/b", 1, None, None).unwrap_err();
        assert!(matches!(err, ForkError::BadNewName(_)), "{err:?}");
    }

    /// P11: a missing parent is a named error. No directory
    /// creates.
    #[test]
    fn a_missing_parent_raises_a_named_error() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sessions");
        let cfg = cfg_with_root(&root);
        let err = fork_session(&cfg, "ghost", "f", 1, None, None).unwrap_err();
        match err {
            ForkError::ParentNotFound(p) => {
                assert_eq!(p, root.join("ghost"));
            }
            other => panic!("expected the missing-parent error: {other:?}"),
        }
        assert!(!root.join("f").exists());
        assert!(!root.join("ghost").exists(), "no directory creates");
    }

    /// P12: a write failure leaves no partial target.
    #[test]
    fn a_failed_write_leaves_no_partial_target() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sessions");
        events(&root, "p", &[user("task")]);
        // Block the temp dir: a regular file holds the name.
        let tmp_name = format!(".f.fork-{}", std::process::id());
        std::fs::write(root.join(&tmp_name), "blocker").unwrap();
        let cfg = cfg_with_root(&root);
        let err = fork_session(&cfg, "p", "f", 1, None, None).unwrap_err();
        assert!(matches!(err, ForkError::Io(_)), "{err:?}");
        assert!(!root.join("f").exists(), "no partial target");
        assert_eq!(
            std::fs::read_to_string(root.join(&tmp_name)).unwrap(),
            "blocker",
            "the blocker stays: the cleanup only removes a dir"
        );
    }
}
