//! `loop.meta` — the session-identity record written by the live loop
//! (issue #44).
//!
//! The bare `loop.pid` carries only a pid. Every consumer re-derived
//! "which session is this pid" on its own: the TUI probe matched the
//! bare session name against the loop's cmdline, and a consumer
//! probing a session dir it resolved itself checked a lock that a
//! differently resolved dir left free. A loop started with an
//! absolute session dir then probed idle forever, and
//! `stop_external_loop` no-oped.
//!
//! The loop process writes one flat TOML record beside `loop.pid`:
//!
//! ```toml
//! pid = 12345
//! session = "issue-29"
//! dir = "/abs/sessions/issue-29"
//! binary = "rushi"
//! started = 1758790200
//! ```
//!
//! The lock stays the authority for liveness. `loop.meta` is the
//! authority for identity. The bare `loop.pid` stays for old readers.
//! A session without `loop.meta` (old loops) replays unchanged: the
//! reader returns `None` and the consumer falls back to the lock plus
//! the bare pid plus its own identity check.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The file name of the identity record inside a session dir.
pub const LOOP_META: &str = "loop.meta";

/// The session-identity record written by the live loop.
///
/// Every field is a recorded fact about the loop process: who it is
/// (`pid`, `binary`), which session it runs (`session`, `dir`), and
/// when it started (`started`). A consumer matches a probe target
/// (a bare session name or a session dir) against the recorded fields
/// via [`LoopMeta::matches_name_or_dir`] instead of re-deriving the
/// identity from a cmdline substring.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopMeta {
    /// The loop process pid. The same value the harness writes to the
    /// bare `loop.pid`.
    pub pid: u32,
    /// The recorded session name. The bare name when the loop was
    /// started with one; the last path component when it was started
    /// with a session dir.
    pub session: String,
    /// The canonical absolute session dir the loop runs.
    pub dir: String,
    /// The base name of the executable running the loop (e.g. `rushi`).
    pub binary: String,
    /// Unix time (seconds) the record was written.
    pub started: i64,
}

impl LoopMeta {
    /// The record path inside a session dir.
    pub fn path_in(dir: &Path) -> PathBuf {
        dir.join(LOOP_META)
    }

    /// Does `target` name this record's session?
    ///
    /// `target` is a bare session name or a session dir, as a
    /// consumer probe resolves it. It matches when it equals the
    /// recorded `session` name, or when it is the recorded `dir` (or
    /// canonicalizes to it). This name-or-dir match replaces the
    /// cmdline substring check (issue #44).
    pub fn matches_name_or_dir(&self, target: &str) -> bool {
        if target == self.session {
            return true;
        }
        let target_path = Path::new(target);
        if target_path == Path::new(&self.dir) {
            return true;
        }
        // The target may differ from the recorded dir only in
        // canonical form (a `..` component, a relative prefix).
        if target_path.is_dir() {
            if let Ok(canonical) = target_path.canonicalize() {
                if canonical.to_string_lossy() == self.dir {
                    return true;
                }
            }
        }
        false
    }
}

/// Derive the recorded `session` name from the loop-start argument.
///
/// A bare name (no path separator) is recorded as given. A dir
/// argument is recorded by its last path component, so a loop started
/// with `/abs/sessions/issue-29` records the name `issue-29`.
pub fn session_name_for(arg: &str) -> String {
    if arg.contains('/') || arg.contains('\\') {
        Path::new(arg)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| arg.to_string())
    } else {
        arg.to_string()
    }
}

/// The canonical absolute form of a session dir.
///
/// The record stores one absolute spelling so any consumer can
/// compare against it. Falls back to the given path when the dir
/// cannot be canonicalized.
pub fn canonical_dir(dir: &Path) -> String {
    dir.canonicalize()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| dir.to_string_lossy().into_owned())
}

/// The base name of the running executable (e.g. `rushi`), with
/// fallbacks for a deleted or unresolvable binary.
pub fn binary_name() -> String {
    if let Some(exe) = crate::paths::resolved_exe() {
        if let Some(name) = exe.file_name() {
            return name.to_string_lossy().into_owned();
        }
    }
    if let Some(arg0) = std::env::args().next() {
        if let Some(name) = Path::new(&arg0).file_name() {
            return name.to_string_lossy().into_owned();
        }
    }
    "rushi".into()
}

/// Write `meta` as the flat TOML record `<dir>/loop.meta`.
///
/// The write is atomic on POSIX: the record lands in a pid-suffixed
/// temp file in the same dir, then is renamed over `loop.meta`, so a
/// reader never observes a torn record.
pub fn write_loop_meta(dir: &Path, meta: &LoopMeta) -> std::io::Result<()> {
    let text = toml::to_string(meta)
        .map_err(std::io::Error::other)?;
    let final_path = LoopMeta::path_in(dir);
    let tmp = dir.join(format!(".{LOOP_META}.tmp.{}", std::process::id()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &final_path)?;
    Ok(())
}

/// Read `<dir>/loop.meta`.
///
/// - `Ok(None)`: the record is absent (an old loop or session). The
///   caller falls back to the lock plus the bare `loop.pid`.
/// - `Ok(Some(meta))`: the live loop recorded its identity.
/// - `Err`: the record is present but unreadable or malformed. The
///   caller falls back the same way; a corrupt record must not
///   masquerade as an identity.
pub fn read_loop_meta(dir: &Path) -> std::io::Result<Option<LoopMeta>> {
    let path = LoopMeta::path_in(dir);
    let data = match std::fs::read_to_string(&path) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let meta: LoopMeta = toml::from_str(&data)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok(Some(meta))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> LoopMeta {
        LoopMeta {
            pid: 4242,
            session: "issue-29".into(),
            dir: "/abs/sessions/issue-29".into(),
            binary: "rushi".into(),
            started: 1758790200,
        }
    }

    #[test]
    fn roundtrip_write_and_read() {
        let dir = tempfile::tempdir().unwrap();
        let meta = sample();
        write_loop_meta(dir.path(), &meta).unwrap();
        // The record is flat TOML with the issue #44 key order.
        let text = std::fs::read_to_string(dir.path().join(LOOP_META)).unwrap();
        let keys: Vec<&str> = text
            .lines()
            .filter_map(|l| l.split('=').next().map(str::trim))
            .collect();
        assert_eq!(keys, ["pid", "session", "dir", "binary", "started"]);
        let read = read_loop_meta(dir.path()).unwrap().unwrap();
        assert_eq!(read, meta);
    }

    #[test]
    fn absent_record_reads_as_none() {
        // Old sessions and old loops carry no record. The reader
        // returns None so the consumer replays today's fallback.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("loop.pid"), "4242\n").unwrap();
        assert!(read_loop_meta(dir.path()).unwrap().is_none());
    }

    #[test]
    fn malformed_record_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(LOOP_META), "pid = ").unwrap();
        assert!(read_loop_meta(dir.path()).is_err());
    }

    #[test]
    fn matches_the_recorded_name_and_dir() {
        let meta = sample();
        assert!(meta.matches_name_or_dir("issue-29"), "the bare name");
        assert!(meta.matches_name_or_dir("/abs/sessions/issue-29"), "the dir");
        assert!(!meta.matches_name_or_dir("other-1"), "a different name");
        assert!(!meta.matches_name_or_dir("/abs/sessions/other-1"), "a different dir");
    }

    #[test]
    fn a_dir_started_loop_matches_a_consumer_probing_the_bare_name() {
        // The issue #44 case: the loop started with an absolute dir.
        // The consumer probes with the bare name its own config
        // ladder resolved. The recorded fields close the match.
        let dir = tempfile::tempdir().unwrap();
        let sess = dir.path().join("issue-29");
        std::fs::create_dir_all(&sess).unwrap();
        let canonical = sess.canonicalize().unwrap();
        let meta = LoopMeta {
            pid: 1,
            session: session_name_for(canonical.to_str().unwrap()).into(),
            dir: canonical_dir(&sess),
            binary: "rushi".into(),
            started: 1758790200,
        };
        assert_eq!(meta.session, "issue-29");
        assert!(meta.matches_name_or_dir("issue-29"));
        // The consumer's own spelling of the dir may carry a
        // non-canonical prefix. The match still holds.
        let noncanonical = canonical.parent().unwrap().join(".").join("issue-29");
        assert!(meta.matches_name_or_dir(noncanonical.to_str().unwrap()));
    }

    #[test]
    fn session_name_for_bare_and_dir_args() {
        assert_eq!(session_name_for("issue-29"), "issue-29");
        assert_eq!(session_name_for("/abs/sessions/issue-29"), "issue-29");
        assert_eq!(session_name_for("sessions/foo"), "foo");
        assert_eq!(session_name_for("/abs/x/"), "x");
    }

    #[test]
    fn binary_name_is_the_running_exe_base() {
        // In a test the running exe is the test binary itself.
        let name = binary_name();
        assert!(!name.is_empty());
    }
}
