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
}
