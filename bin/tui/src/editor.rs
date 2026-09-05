//! `$EDITOR` support (docs/tui.md key Ctrl+E: "or shell out to $EDITOR
//! for long messages").
//!
//! The editor runs with the terminal in plain (non-raw) mode so it can
//! read its own input; the TUI returns to raw mode afterwards. The
//! alternate screen is kept, so the user never leaves the TUI.

use std::path::PathBuf;

pub enum EditorError {
    /// No usable editor: `$VISUAL`, `$EDITOR`, and `vi` all missing.
    NoEditor,
    /// The editor process failed to start.
    Spawn(std::io::Error),
    /// The editor exited non-zero, or the file could not be read back.
    /// The draft is left unchanged.
    Aborted(String),
}

impl std::fmt::Display for EditorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EditorError::NoEditor => write!(f, "no editor found: set $VISUAL or $EDITOR"),
            EditorError::Spawn(e) => write!(f, "cannot start editor: {e}"),
            EditorError::Aborted(msg) => write!(f, "edit aborted: {msg}"),
        }
    }
}

impl std::fmt::Debug for EditorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

/// `$VISUAL`, then `$EDITOR`, then `vi`.
pub fn editor_command() -> Option<String> {
    if let Ok(v) = std::env::var("VISUAL") {
        if !v.is_empty() {
            return Some(v);
        }
    }
    if let Ok(e) = std::env::var("EDITOR") {
        if !e.is_empty() {
            return Some(e);
        }
    }
    Some("vi".to_string())
}

/// A scratch file, unique per process, under the temp dir.
pub fn scratch_file() -> PathBuf {
    std::env::temp_dir().join(format!("rushi-tui-edit-{}.txt", std::process::id()))
}

/// Open the editor on `initial` and return the edited text.
///
/// `suspend` tears the terminal down to plain mode (raw off, mouse
/// capture off); `resume` brings the TUI back. Both run in the order
/// suspend, resume, no matter how the editor call fails.
pub fn run_editor(initial: &str, suspend: fn(), resume: fn()) -> Result<String, EditorError> {
    let cmd = editor_command().ok_or(EditorError::NoEditor)?;
    let path = scratch_file();
    if let Err(e) = std::fs::write(&path, initial) {
        return Err(EditorError::Spawn(e));
    }

    suspend();
    let status = match std::process::Command::new(&cmd).arg(&path).status() {
        Ok(s) => s,
        Err(e) => {
            resume();
            let _ = std::fs::remove_file(&path);
            return Err(EditorError::Spawn(e));
        }
    };

    let result = if status.success() {
        match std::fs::read_to_string(&path) {
            Ok(t) => Ok(t),
            Err(e) => Err(EditorError::Aborted(e.to_string())),
        }
    } else {
        let code = status.code().unwrap_or(-1);
        Err(EditorError::Aborted(format!(
            "editor exited with status {code}; draft unchanged"
        )))
    };
    resume();
    let _ = std::fs::remove_file(&path);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_command_and_behavior() {
        // One test owns the VISUAL/EDITOR vars: env is process-global
        // and the harness runs tests in parallel.
        std::env::remove_var("VISUAL");
        std::env::remove_var("EDITOR");
        assert_eq!(editor_command().as_deref(), Some("vi"), "fallback is vi");

        let script = std::env::temp_dir().join(format!("tui-test-editor-{}", std::process::id()));
        std::fs::write(&script, "#!/bin/sh\ncat >> \"$1\" <<'MARK'\nEDITED\nMARK\n").unwrap();
        std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();

        // A working editor appends a marker and exits 0.
        std::env::set_var("VISUAL", script.to_str().unwrap());
        let out = run_editor("line one\n", || {}, || {}).unwrap();
        assert_eq!(out, "line one\nEDITED\n");

        // A failing editor (exit 1) aborts without changing the draft.
        std::env::set_var("VISUAL", "false");
        let err = run_editor("x", || {}, || {}).unwrap_err();
        assert!(matches!(err, EditorError::Aborted(_)), "{err:?}");

        std::fs::remove_file(&script).ok();
    }
}
