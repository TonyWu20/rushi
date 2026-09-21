//! Shared path resolution for kernel and front-end binaries.
//!
//! [`resolve_config_path`] is the canonical config discovery order
//! (`$CONFIG` → explicit CLI path → Nix side-by-side → CWD). The
//! kernel (`bin/rushi`) uses it for its own subcommands; front-end
//! binaries (`rushi-tui`, `rushi-web`, ...) import the same function
//! through the `rushi-common` path dep, so their config discovery is
//! identical to the kernel's by construction (docs/itches.md,
//! 2026-09-20: front-ends self-wire, the kernel keeps no launcher
//! subcommands).

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
/// Priority (highest to lowest):
/// 1. `$CONFIG` env var — explicit user/system override
/// 2. `cli_config` — the binary's own `--config` flag
/// 3. Side-by-side `<exe_dir>/../config.toml` — Nix package layout
///    (`$out/bin/…` finds `$out/config.toml`)
/// 4. `config.toml` in CWD — dev checkout fallback
pub fn resolve_config_path(cli_config: Option<&Path>) -> String {
    // 1. $CONFIG env var (set by the `user` binary, or by the user).
    if let Ok(p) = std::env::var("CONFIG") {
        return p;
    }

    // 2. Explicit --config flag.
    if let Some(p) = cli_config {
        return p.to_string_lossy().into_owned();
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
        // $CONFIG outranks the CLI flag when set; in the test env it
        // is not set, so the explicit path must win over the Nix
        // side-by-side probe and the CWD fallback.
        if std::env::var_os("CONFIG").is_some() {
            eprintln!("skipping: $CONFIG is set in this environment");
            return;
        }
        let got = resolve_config_path(Some(Path::new("custom.toml")));
        assert_eq!(got, "custom.toml");
    }

    #[test]
    fn resolve_config_path_falls_back_to_cwd() {
        // No $CONFIG, no CLI flag, and no Nix side-by-side sibling of
        // the test binary (target/debug/config.toml does not exist),
        // so the resolver returns the CWD-relative dev-checkout name.
        if std::env::var_os("CONFIG").is_some() {
            eprintln!("skipping: $CONFIG is set in this environment");
            return;
        }
        let got = resolve_config_path(None);
        assert_eq!(got, "config.toml");
    }
}
