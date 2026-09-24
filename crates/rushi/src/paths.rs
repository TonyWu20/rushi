//! Shared path resolution for kernel and front-end binaries.
//!
//! [`resolve_config_path`] is the canonical config discovery order
//! (explicit CLI path → `$CONFIG` → Nix side-by-side → CWD). The
//! kernel (`bin/rushi`) uses it for its own subcommands; front-end
//! binaries (`rushi-tui`, `rushi-web`, ...) import the same function
//! through the `rushi-common` path dep, so their config discovery is
//! identical to the kernel's by construction (docs/itches.md,
//! 2026-09-20: front-ends self-wire, the kernel keeps no launcher
//! subcommands).
//!
//! Issue #36: the explicit `--config` flag outranks the ambient
//! `$CONFIG` env var — a flag is the most specific input a program
//! gets, so it must win even when a dev shell or tmux session has
//! `CONFIG` exported.

use std::path::{Path, PathBuf};

/// The running executable's path, symlinks resolved.
///
/// `std::env::current_exe()` on macOS reports the launch path with no
/// symlink resolution (unlike Linux's `/proc/self/exe`). Under a
/// per-user profile symlink chain (nix-darwin:
/// `/etc/profiles/<user>/bin/rushi` -> home-manager path -> store)
/// every sibling resolution against the raw path misses, because the
/// package layout (`config.toml`, `hooks/`, `tools/`) lives at the
/// store package root, two links down (issue #25). Canonicalize once
/// and reuse the result wherever sibling resolution happens. When
/// canonicalization fails (e.g. the binary was deleted after launch)
/// fall back to the raw reported path.
pub fn resolved_exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(std::fs::canonicalize(&exe).unwrap_or(exe))
}

/// Resolve the config path.
///
/// Priority (highest to lowest, issue #36):
/// 1. `cli_config` — the binary's explicit `--config` flag; outranks
///    everything, including an ambient `$CONFIG`
/// 2. `$CONFIG` env var — ambient override, still above the package
///    layouts
/// 3. Side-by-side `<exe_dir>/../config.toml` — Nix package layout
///    (`$out/bin/…` finds `$out/config.toml`)
/// 4. `config.toml` in CWD — dev checkout fallback
pub fn resolve_config_path(cli_config: Option<&Path>) -> String {
    // 1. Explicit --config flag: the most specific input a program
    // gets. It must win even when $CONFIG is set in the environment
    // (issue #36).
    if let Some(p) = cli_config {
        return p.to_string_lossy().into_owned();
    }

    // 2. $CONFIG env var (set by the `user` binary, or by the user).
    if let Ok(p) = std::env::var("CONFIG") {
        return p;
    }

    // 3. Side-by-side: <exe_dir>/../config.toml (Nix: $out/config.toml).
    // Use the canonicalized exe (issue #25): on macOS current_exe() is
    // the launch path, so a profile symlink chain would miss the
    // store package's sibling config.toml.
    if let Some(exe) = resolved_exe() {
        if let Some(bin_dir) = exe.parent() {
            if let Some(candidate) = bin_dir.parent().map(|p| p.join("config.toml")) {
                if candidate.exists() {
                    return candidate.to_string_lossy().into_owned();
                }
            }
        }
    }

    // 4. CWD fallback (dev checkout).
    "config.toml".into()
}

/// Resolve one `[paths]` tool entry against the config dir.
///
/// Absolute entries stay as given. Relative entries join against
/// `config_dir` (the dir that holds `config.toml`). The kernel
/// (`assemble`) applies this rule to `native_tool_paths` and
/// `extension_tool_paths`. The TUI applies the same rule at startup
/// so a tool entry means the same thing on both sides.
pub fn resolve_tool_entry(config_dir: &Path, entry: &str) -> PathBuf {
    let p = PathBuf::from(entry);
    if p.is_absolute() {
        p
    } else {
        config_dir.join(p)
    }
}

/// The tool dirs one resolved tool entry yields.
///
/// A tool dir holds `tool.toml` directly. A root dir holds tool
/// sub-dirs, one `tool.toml` each. A missing dir yields none. The
/// kernel skips an empty entry silently; the TUI reports it at
/// startup. Both sides share this scan so they cannot drift.
pub fn tool_dirs_in(path: &Path) -> Vec<PathBuf> {
    let direct_toml = path.join("tool.toml");
    if direct_toml.exists() {
        return vec![path.to_path_buf()];
    }
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let sub = entry.path();
            if sub.is_dir() && sub.join("tool.toml").exists() {
                dirs.push(sub);
            }
        }
    }
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Tests that read or write the process-global `CONFIG` env var must
    /// hold this lock so the parallel test harness cannot observe a
    /// half-updated environment (issue #36).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn resolved_exe_is_canonicalized() {
        // resolved_exe() should return the same path that
        // fs::canonicalize produces for the real executable.
        let raw = std::env::current_exe().unwrap();
        let want = std::fs::canonicalize(&raw).unwrap();
        let got = resolved_exe().unwrap();
        assert_eq!(got, want);
    }

    #[test]
    fn resolve_config_path_prefers_cli_override() {
        // The explicit --config flag must outrank the ambient $CONFIG
        // env var (issue #36), whatever the outer environment holds.
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let prev = std::env::var_os("CONFIG");
        std::env::set_var("CONFIG", "from-env.toml");
        // 1. The flag beats the env var.
        let got = resolve_config_path(Some(Path::new("custom.toml")));
        assert_eq!(got, "custom.toml", "the --config flag must outrank $CONFIG");
        // 2. Without the flag, $CONFIG still outranks the package layouts.
        let got2 = resolve_config_path(None);
        assert_eq!(
            got2, "from-env.toml",
            "without the flag, $CONFIG still wins"
        );
        // Restore whatever the outer environment had.
        match prev {
            Some(v) => std::env::set_var("CONFIG", v),
            None => std::env::remove_var("CONFIG"),
        }
    }

    #[test]
    fn resolve_config_path_falls_back_to_cwd() {
        // No $CONFIG, no CLI flag, and no Nix side-by-side sibling of
        // the test binary (target/debug/config.toml does not exist),
        // so the resolver returns the CWD-relative dev-checkout name.
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let prev = std::env::var_os("CONFIG");
        std::env::remove_var("CONFIG");
        let got = resolve_config_path(None);
        assert_eq!(got, "config.toml");
        if let Some(v) = prev {
            std::env::set_var("CONFIG", v);
        }
    }

    #[test]
    fn resolve_tool_entry_keeps_absolute_and_joins_relative() {
        let base = std::path::PathBuf::from("/nix/store/abc-config");
        let abs = resolve_tool_entry(&base, "/nix/store/tool-pkg/tools/bash");
        assert_eq!(abs, std::path::Path::new("/nix/store/tool-pkg/tools/bash"));
        let rel = resolve_tool_entry(&base, "tools/bash");
        assert_eq!(rel, std::path::Path::new("/nix/store/abc-config/tools/bash"));
    }

    #[test]
    fn tool_dirs_in_direct_manifest_and_root_layouts() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // A tool dir: tool.toml directly under the entry.
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::write(root.join("a").join("tool.toml"), "[tool]\n").unwrap();
        // A root dir: tool sub-dirs, each with a tool.toml.
        std::fs::create_dir_all(root.join("root").join("b")).unwrap();
        std::fs::write(root.join("root").join("b").join("tool.toml"), "[tool]\n").unwrap();
        std::fs::create_dir_all(root.join("root").join("c")).unwrap();
        std::fs::write(root.join("root").join("c").join("tool.toml"), "[tool]\n").unwrap();
        // An empty dir yields nothing.
        std::fs::create_dir_all(root.join("empty")).unwrap();
        let direct = tool_dirs_in(&root.join("a"));
        assert_eq!(direct, vec![root.join("a")]);
        let root_dirs = tool_dirs_in(&root.join("root"));
        let mut sorted: Vec<_> = root_dirs.clone();
        sorted.sort();
        assert_eq!(sorted, vec![root.join("root").join("b"), root.join("root").join("c")]);
        assert!(tool_dirs_in(&root.join("empty")).is_empty());
        assert!(tool_dirs_in(&root.join("no-such-dir")).is_empty());
    }
}
