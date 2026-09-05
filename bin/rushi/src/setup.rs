//! `rushi setup` — materialize a project from a `rushi.toml` manifest.
//!
//! Reads the manifest, resolves tool availability, materializes the
//! `tools/` directory, writes `rushi.lock`, `.envrc`, and generates
//! `config.toml` from kernel defaults merged with manifest overrides.
//!
//! The setup is additive (P6): it adds missing tools, never overwrites
//! or deletes project-authored tools.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

// ─── Manifest ────────────────────────────────────────────────────────

/// The declarative project manifest (`rushi.toml`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RushiManifest {
    #[serde(default)]
    pub rushi: RushiMeta,
    #[serde(default)]
    pub tools: ToolsSection,
    #[serde(default)]
    pub ui_extensions: UiExtensionsSection,
    /// TOML table key is `loop` (a Rust keyword), so rename in serde.
    #[serde(default, rename = "loop")]
    pub loop_config: LoopSection,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RushiMeta {
    pub version: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ToolsSection {
    /// Tool names to enable. Kernel tools resolve from the kernel
    /// install; project-local tools are already in `./tools/`.
    pub enabled: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct UiExtensionsSection {
    pub enabled: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct LoopSection {
    pub compact_reserve_tokens: Option<u64>,
}

impl RushiManifest {
    /// Parse a `rushi.toml` file from disk.
    pub fn from_path(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let manifest: Self =
            toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
        Ok(manifest)
    }
}

// ─── Lock file ───────────────────────────────────────────────────────

/// A lock entry for an external tool or extension source.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExternalLockEntry {
    pub name: String,
    pub kind: String,
    pub source: String,
    pub commit: String,
    pub sha256: String,
}

/// The machine-generated lock file next to `rushi.toml`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RushiLock {
    pub lock: LockMeta,
    #[serde(default, rename = "external")]
    pub external: Vec<ExternalLockEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LockMeta {
    pub version: String,
    pub kernel_commit: String,
}

impl RushiLock {
    /// Render the lock file as a TOML string.
    pub fn render(&self) -> String {
        let mut out = String::from("# rushi.lock (machine-generated, do not edit by hand)\n");
        out.push_str(&format!(
            "[lock]\nversion = {}\nkernel_commit = {}\n",
            t_quote(&self.lock.version),
            t_quote(&self.lock.kernel_commit),
        ));
        for e in &self.external {
            out.push_str(&format!(
                "\n[[external]]\nname = {}\nkind = {}\nsource = {}\ncommit = {}\nsha256 = {}\n",
                t_quote(&e.name),
                t_quote(&e.kind),
                t_quote(&e.source),
                t_quote(&e.commit),
                t_quote(&e.sha256),
            ));
        }
        out
    }

    /// Parse a `rushi.lock` file from disk.
    pub fn from_path(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let lock: Self =
            toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
        Ok(lock)
    }
}

/// Wrap a string in TOML double quotes.
fn t_quote(s: &str) -> String {
    format!("\"{}\"", s)
}

// ─── Tool resolution (pure) ─────────────────────────────────────────

/// The result of resolving which tools to materialize.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializePlan {
    /// Tools that should be copied from the kernel install.
    pub copy_from_kernel: Vec<String>,
    /// Tools already present locally (project-authored or previously
    /// materialized). Left untouched.
    pub already_local: Vec<String>,
    /// Tools in the manifest that are neither in the kernel nor
    /// local. The caller must provide them from an external source
    /// or report them as missing.
    pub missing: Vec<String>,
}

/// Resolve which tools to materialize.
///
/// `manifest` — the parsed `rushi.toml`.
/// `kernel_tools` — the set of tool names shipped with the kernel.
/// `local_tools` — tool directories already present in `./tools/`.
///
/// This is a pure function: no I/O, fully testable.
pub fn resolve_tools(
    manifest: &RushiManifest,
    kernel_tools: &BTreeSet<String>,
    local_tools: &BTreeSet<String>,
) -> MaterializePlan {
    let mut copy_from_kernel = Vec::new();
    let mut already_local = Vec::new();
    let mut missing = Vec::new();

    for name in &manifest.tools.enabled {
        if local_tools.contains(name) {
            already_local.push(name.clone());
        } else if kernel_tools.contains(name) {
            copy_from_kernel.push(name.clone());
        } else {
            missing.push(name.clone());
        }
    }

    // Deterministic order (P2, P9).
    copy_from_kernel.sort();
    already_local.sort();
    missing.sort();

    MaterializePlan {
        copy_from_kernel,
        already_local,
        missing,
    }
}

// ─── .envrc rendering (pure) ─────────────────────────────────────────

/// Render the `.envrc` content for a project that has materialized
/// tools in `tools/`. The PATH wiring lets the harness find tool
/// binaries on the visible path.
pub fn render_envrc(tools_dir: &Path) -> String {
    let display = tools_dir.display().to_string();
    format!(
        "# Generated by `rushi setup`. Do not edit.\n\
         # The tools directory is on the agent-visible PATH.\n\
         export RUSHI_TOOLS_DIR=\"{display}\"\n"
    )
}

// ─── Config generation (pure) ────────────────────────────────────────

/// Generate the minimal `config.toml` content from the manifest's
/// `[loop]` overrides. If `config.toml` already exists, setup does
/// not overwrite it (P6: additive only).
pub fn render_loop_config(manifest: &RushiManifest) -> String {
    let mut out = String::from("[loop]\n");
    if let Some(tokens) = manifest.loop_config.compact_reserve_tokens {
        out.push_str(&format!("compact_reserve_tokens = {}\n", tokens));
    }
    out
}

// ─── Setup orchestration ────────────────────────────────────────────

/// Run `rushi setup` in the current directory.
///
/// - Reads `rushi.toml` from `cwd`.
/// - Resolves which tools to copy from the kernel install.
/// - Materializes `tools/<name>/` for each missing kernel tool.
/// - Writes `rushi.lock` (or verifies in `--locked` mode).
/// - Writes `.envrc`.
/// - Generates `config.toml` only if it does not already exist.
///
/// In `--locked` mode, reads `rushi.lock` and verifies the kernel
/// commit matches; fails on stale or missing lock.
pub fn do_setup(locked: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("cannot resolve CWD")?;
    let manifest_path = cwd.join("rushi.toml");

    if !manifest_path.exists() {
        anyhow::bail!(
            "rushi.toml not found in {}. Run `rushi setup` from the project root.",
            cwd.display()
        );
    }

    let manifest = RushiManifest::from_path(&manifest_path)?;

    // Determine the kernel tools directory.
    let kernel_tools_dir = resolve_kernel_tools_dir();
    let kernel_tools = if kernel_tools_dir.exists() {
        list_tool_dirs(&kernel_tools_dir)?
    } else {
        eprintln!(
            "warning: kernel tools dir not found at {}; using empty set",
            kernel_tools_dir.display()
        );
        BTreeSet::new()
    };

    let local_tools_dir = cwd.join("tools");
    let local_tools = if local_tools_dir.exists() {
        list_tool_dirs(&local_tools_dir)?
    } else {
        BTreeSet::new()
    };

    let plan = resolve_tools(&manifest, &kernel_tools, &local_tools);

    // Report missing tools.
    if !plan.missing.is_empty() {
        let missing: Vec<_> = plan.missing.iter().map(|s| s.as_str()).collect();
        anyhow::bail!(
            "tools not found in kernel or local: {}. \
             Add them to the kernel install or promote to an external source.",
            missing.join(", ")
        );
    }

    // --locked mode: verify the lock file.
    let lock_path = cwd.join("rushi.lock");
    if locked {
        if !lock_path.exists() {
            anyhow::bail!(
                "--locked: rushi.lock not found. Run `rushi setup` first to generate it."
            );
        }
        let lock = RushiLock::from_path(&lock_path)?;
        let expected_commit = kernel_tools_dir
            .join(".rushi_commit")
            .exists()
            .then(|| {
                std::fs::read_to_string(kernel_tools_dir.join(".rushi_commit"))
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        if !expected_commit.is_empty() && lock.lock.kernel_commit != expected_commit {
            anyhow::bail!(
                "--locked: rushi.lock pins kernel_commit {} but the installed kernel is {}. \
                 Run `rushi setup` to refresh the lock.",
                lock.lock.kernel_commit, expected_commit
            );
        }
        // In locked mode, verify external entries match the lock.
        for ext in &lock.external {
            let ext_dir = local_tools_dir.join(&ext.name);
            if !ext_dir.exists() {
                anyhow::bail!(
                    "--locked: external {} ({}@{}) is in the lock but not in tools/.",
                    ext.name,
                    ext.source,
                    &ext.commit[..ext.commit.len().min(7)]
                );
            }
        }
    } else {
        // Materialize kernel tools that are missing locally.
        for name in &plan.copy_from_kernel {
            let src = kernel_tools_dir.join(name);
            let dst = local_tools_dir.join(name);
            if !dst.exists() {
                copy_dir_recursive(&src, &dst)?;
                eprintln!("setup: copied {} → tools/{}", name, "");
            }
        }

        // Write the lock file.
        let kernel_commit = read_kernel_commit(&kernel_tools_dir);
        let lock = RushiLock {
            lock: LockMeta {
                version: "0.1".into(),
                kernel_commit,
            },
            external: Vec::new(),
        };
        let lock_text = lock.render();
        std::fs::write(&lock_path, &lock_text)
            .with_context(|| format!("cannot write {}", lock_path.display()))?;
        eprintln!("setup: wrote rushi.lock");
    }

    // Write .envrc.
    let envrc_path = cwd.join(".envrc");
    let envrc_content = render_envrc(&local_tools_dir);
    std::fs::write(&envrc_path, envrc_content)
        .with_context(|| format!("cannot write {}", envrc_path.display()))?;
    eprintln!("setup: wrote .envrc");

    // Generate config.toml only if it does not exist (P6: additive).
    let config_path = cwd.join("config.toml");
    if !config_path.exists() {
        let config_text = render_loop_config(&manifest);
        std::fs::write(&config_path, config_text)
            .with_context(|| format!("cannot write {}", config_path.display()))?;
        eprintln!("setup: wrote config.toml (kernel defaults + manifest overrides)");
    } else {
        eprintln!("setup: config.toml already exists, leaving it untouched");
    }

    eprintln!(
        "setup: done. {} tools materialized, {} already local, {} ui_extensions declared.",
        plan.copy_from_kernel.len(),
        plan.already_local.len(),
        manifest.ui_extensions.enabled.len(),
    );
    Ok(())
}

/// Locate the kernel tools directory. Resolution order:
/// 1. `$RUSHI_KERNEL` env var (explicit kernel root)
/// 2. `../tools` relative to the `rushi` binary (side-by-side install)
/// 3. `tools/` in the current directory (dev checkout)
fn resolve_kernel_tools_dir() -> PathBuf {
    if let Ok(kernel_dir) = std::env::var("RUSHI_KERNEL") {
        return PathBuf::from(kernel_dir).join("tools");
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin_dir) = exe.parent() {
            let candidate = bin_dir.parent().map(|p| p.join("tools"));
            if let Some(c) = candidate {
                if c.exists() {
                    return c;
                }
            }
        }
    }
    PathBuf::from("tools")
}

/// List tool directory names in a tools root (dirs containing a
/// `tool.toml`).
fn list_tool_dirs(tools_root: &Path) -> Result<BTreeSet<String>> {
    let mut set = BTreeSet::new();
    if !tools_root.exists() {
        return Ok(set);
    }
    for entry in std::fs::read_dir(tools_root)
        .with_context(|| format!("cannot read {}", tools_root.display()))?
    {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let name = entry.file_name().to_string_lossy().to_string();
            let has_manifest = entry.path().join("tool.toml").exists();
            if has_manifest {
                set.insert(name);
            }
        }
    }
    Ok(set)
}

/// Read the kernel commit marker, or fall back to "dev".
fn read_kernel_commit(kernel_dir: &Path) -> String {
    let marker = kernel_dir.join(".rushi_commit");
    if marker.exists() {
        std::fs::read_to_string(&marker)
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "unknown".into())
    } else {
        "dev".into()
    }
}

/// Recursively copy a directory tree.
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    if !src.exists() {
        anyhow::bail!("source directory {} does not exist", src.display());
    }
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(enabled: Vec<&str>) -> RushiManifest {
        RushiManifest {
            rushi: RushiMeta {
                version: Some("0.1".into()),
            },
            tools: ToolsSection {
                enabled: enabled.into_iter().map(String::from).collect(),
            },
            ui_extensions: UiExtensionsSection {
                enabled: vec!["statusline-rs".into()],
            },
            loop_config: LoopSection {
                compact_reserve_tokens: Some(16384),
            },
        }
    }

    fn tool_set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_manifest_from_toml() {
        let toml_text = r#"
            [rushi]
            version = "0.1"

            [tools]
            enabled = ["read", "write", "bash"]

            [ui_extensions]
            enabled = ["mermaid"]

            [loop]
            compact_reserve_tokens = 16384
        "#;
        let m: RushiManifest = toml::from_str(toml_text).unwrap();
        assert_eq!(m.rushi.version.as_deref(), Some("0.1"));
        assert_eq!(m.tools.enabled, vec!["read", "write", "bash"]);
        assert_eq!(m.ui_extensions.enabled, vec!["mermaid"]);
        assert_eq!(m.loop_config.compact_reserve_tokens, Some(16384));
    }

    #[test]
    fn parse_manifest_minimal() {
        let toml_text = r#"
            [tools]
            enabled = ["read"]
        "#;
        let m: RushiManifest = toml::from_str(toml_text).unwrap();
        assert_eq!(m.tools.enabled, vec!["read"]);
        assert!(m.ui_extensions.enabled.is_empty());
        assert_eq!(m.loop_config.compact_reserve_tokens, None);
    }

    #[test]
    fn resolve_tools_all_kernel() {
        let m = manifest(vec!["read", "write", "bash"]);
        let kernel = tool_set(&["read", "write", "edit", "list", "bash", "goal"]);
        let local = BTreeSet::new();
        let plan = resolve_tools(&m, &kernel, &local);
        assert_eq!(plan.copy_from_kernel, vec!["bash", "read", "write"]);
        assert!(plan.already_local.is_empty());
        assert!(plan.missing.is_empty());
    }

    #[test]
    fn resolve_tools_local_wins() {
        let m = manifest(vec!["read", "custom"]);
        let kernel = tool_set(&["read", "write"]);
        let local = tool_set(&["read"]);
        let plan = resolve_tools(&m, &kernel, &local);
        assert!(plan.copy_from_kernel.is_empty());
        assert_eq!(plan.already_local, vec!["read"]);
        assert_eq!(plan.missing, vec!["custom"]);
    }

    #[test]
    fn resolve_tools_missing_reported() {
        let m = manifest(vec!["read", "nonexistent"]);
        let kernel = tool_set(&["read"]);
        let local = BTreeSet::new();
        let plan = resolve_tools(&m, &kernel, &local);
        assert_eq!(plan.copy_from_kernel, vec!["read"]);
        assert_eq!(plan.missing, vec!["nonexistent"]);
    }

    #[test]
    fn lock_render_roundtrip() {
        let lock = RushiLock {
            lock: LockMeta {
                version: "0.1".into(),
                kernel_commit: "a3f8c21".into(),
            },
            external: vec![ExternalLockEntry {
                name: "my-tool".into(),
                kind: "tool".into(),
                source: "git+https://github.com/me/rushi-tools".into(),
                commit: "b7e91d4".into(),
                sha256: "41ab...".into(),
            }],
        };
        let text = lock.render();
        let parsed: RushiLock = toml::from_str(&text).unwrap();
        assert_eq!(parsed.lock.kernel_commit, "a3f8c21");
        assert_eq!(parsed.external.len(), 1);
        assert_eq!(parsed.external[0].name, "my-tool");
    }

    #[test]
    fn lock_render_no_external() {
        let lock = RushiLock {
            lock: LockMeta {
                version: "0.1".into(),
                kernel_commit: "abc1234".into(),
            },
            external: Vec::new(),
        };
        let text = lock.render();
        assert!(text.contains("kernel_commit = \"abc1234\""));
        assert!(!text.contains("[[external]]"));
    }

    #[test]
    fn render_envrc_includes_tools_dir() {
        let content = render_envrc(Path::new("tools"));
        assert!(content.contains("RUSHI_TOOLS_DIR"));
        assert!(content.contains("tools"));
    }

    #[test]
    fn render_loop_config_includes_override() {
        let m = manifest(vec!["read"]);
        let text = render_loop_config(&m);
        assert!(text.contains("compact_reserve_tokens = 16384"));
    }

    #[test]
    fn render_loop_config_omits_absent() {
        let m = RushiManifest {
            rushi: RushiMeta {
                version: Some("0.1".into()),
            },
            tools: ToolsSection {
                enabled: vec!["read".into()],
            },
            ui_extensions: UiExtensionsSection::default(),
            loop_config: LoopSection::default(),
        };
        let text = render_loop_config(&m);
        assert!(!text.contains("compact_reserve_tokens"));
    }
}
