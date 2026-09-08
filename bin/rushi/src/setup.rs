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

/// Generate a complete `config.toml` template: every tunable option
/// appears in the file, defaults are commented out, and the manifest's
/// `[loop]` overrides are the only active lines. If `config.toml`
/// already exists, setup does not overwrite it (P6: additive only).
///
/// The header records the generating `rushi` version and kernel
/// commit, so the reader knows which defaults the template reflects.
pub fn render_config(manifest: &RushiManifest, version: &str, kernel_commit: &str) -> String {
    #[derive(Serialize)]
    struct TActive {
        model: String,
    }
    #[derive(Serialize)]
    struct TModel {
        api: String,
        max_output_tokens: u64,
        reasoning_effort: String,
        model_timeout_s: u64,
    }
    #[derive(Serialize)]
    struct TPaths {
        sessions_root: String,
        tools_root: String,
        extra_tools_roots: Vec<String>,
    }
    #[derive(Serialize)]
    struct TLimits {
        read_limit: u32,
        read_max_line_length: u32,
        read_max_bytes: u32,
        read_stream_min_size: u32,
        write_max_bytes: u32,
        tool_result_max_chars: u32,
        bash_max_output_bytes: u32,
        bash_timeout_default: u32,
        bash_timeout_max: u32,
        compact_enabled: bool,
        compact_reserve_tokens: u64,
        compact_keep_tokens: u64,
        compact_trigger_base: String,
        compact_strategy: String,
    }
    #[derive(Serialize)]
    struct THooks {
        timeout_ms: u64,
    }

    // Distribution defaults, mirroring the loader fallbacks in
    // bin/rushi/src/config.rs and bin/model/src/main.rs.
    let mut limits = TLimits {
        read_limit: 2000,
        read_max_line_length: 2000,
        read_max_bytes: 51200,
        read_stream_min_size: 10485760,
        write_max_bytes: 1048576,
        tool_result_max_chars: 20000,
        bash_max_output_bytes: 16000,
        bash_timeout_default: 60,
        bash_timeout_max: 300,
        compact_enabled: true,
        compact_reserve_tokens: 16384,
        compact_keep_tokens: 20000,
        compact_trigger_base: "input_budget".into(),
        compact_strategy: "compact".into(),
    };
    // Keys the manifest overrides stay active; the rest stay commented.
    let mut active_keys: BTreeSet<&str> = BTreeSet::new();
    if let Some(tokens) = manifest.loop_config.compact_reserve_tokens {
        limits.compact_reserve_tokens = tokens;
        active_keys.insert("compact_reserve_tokens");
    }

    let active = TActive {
        model: "deepseek".into(),
    };
    let model = TModel {
        api: "responses".into(),
        max_output_tokens: 32768,
        reasoning_effort: "xhigh".into(),
        model_timeout_s: 3600,
    };
    let paths = TPaths {
        sessions_root: "sessions".into(),
        tools_root: "tools".into(),
        extra_tools_roots: Vec::new(),
    };
    let hooks = THooks {
        timeout_ms: 30000,
    };

    /// Prefix `# ` on every `key = value` line whose key the manifest
    /// did not override.
    fn comment_defaults(section: &str, active: &BTreeSet<&str>) -> String {
        section
            .lines()
            .map(|line| {
                let key = line.split('=').next().unwrap_or("").trim();
                if !line.is_empty() && line.contains('=') && !active.contains(key) {
                    format!("# {line}")
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    let mut out = String::new();
    out.push_str(&format!(
        "# config.toml — generated by `rushi setup` v{version} \
         (kernel commit: {kernel_commit})\n\
         # Uncomment any option to override the distribution default.\n\
         #\n"
    ));

    out.push_str("[active]\n");
    out.push_str(&comment_defaults(&toml::to_string(&active).unwrap(), &active_keys));
    out.push_str("\n\n");

    out.push_str("[model]\n");
    out.push_str(&comment_defaults(&toml::to_string(&model).unwrap(), &active_keys));
    out.push('\n');
    out.push_str(
        "# EXAMPLE ONLY, not activated by default — copy and edit to add per-model overrides:\n\
         # [model.my-model]\n\
         # model_id = \"my-model\"\n\
         # base_url = \"http://127.0.0.1:8080\"\n\
         # api_key_env = \"MODEL_API_KEY\"\n\
         # context_tokens = 131072\n\
         # timeout_s = 3600\n\
         # reasoning_effort = \"medium\"\n",
    );
    out.push('\n');

    out.push_str("[paths]\n");
    out.push_str(&comment_defaults(&toml::to_string(&paths).unwrap(), &active_keys));
    out.push_str("\n\n");

    out.push_str("[limits]\n");
    out.push_str(&comment_defaults(&toml::to_string(&limits).unwrap(), &active_keys));
    out.push('\n');
    out.push_str(
        "# EXAMPLE ONLY, not activated by default — uncomment to override:\n\
         # context_budget_tokens = 262144      # default: context_tokens - max_output_tokens\n\
         # approval_timeout_s = 300             # absent: approval waits forever\n\
         # compact_reasoning_effort = \"low\"    # absent: inherit session effort\n",
    );
    out.push('\n');

    out.push_str("[hooks]\n");
    out.push_str(&comment_defaults(&toml::to_string(&hooks).unwrap(), &active_keys));
    out.push('\n');
    out.push_str(
        "# EXAMPLE ONLY, not activated by default — one [[hooks.on]] table per entry:\n\
         # [[hooks.on]]\n\
         # window  = \"exhausted.handle\"\n\
         # command = \"harness-hook-compact\"\n\
         #\n\
         # [[hooks.on]]\n\
         # window  = \"overflow.resolve\"\n\
         # command = \"harness-hook-compact\"\n",
    );
    out.push('\n');

    out.push_str(
        "# EXAMPLE ONLY, not activated by default — uncomment to override the loop command:\n\
         # [loop]\n\
         # Opaque loop command the TUI spawns for each session\n\
         # (docs/tui.md section 2.3). `rushi run <session>` is the\n\
         # kernel turn-loop runner, resolved on PATH.\n\
         # command = \"rushi\"\n\
         # args = [\"run\"]\n\
         # arg_style = \"append_session\"\n\
         #\n",
    );
    out.push('\n');

    out.push_str(
        "# EXAMPLE ONLY, not activated by default — uncomment to customize:\n\
         # [system_prompt]\n\
         # text = \"You are an expert coding assistant...\"\n\
         #\n\
         # [tui]\n\
         # binary = \"target/release/tui\"   # path to the TUI binary the launcher spawns (relative to this config)\n\
         # color = \"truecolor\"              # force color level: truecolor, 256, 8, 16 (default: detect)\n\
         # color_scheme = \"catppuccin macchiato\"\n\
         #\n\
         # [tui.tool_display]\n\
         # preset = \"opencode\"\n\
         # preview_lines = 8\n\
         # bash_collapsed_lines = 5\n\
         # diff_collapsed_lines = 24\n\
         # expanded_preview_max_lines = 50\n\
         # diff_view = \"auto\"\n",
    );

    out
}

// ─── Setup orchestration ────────────────────────────────────────────

/// Run `rushi setup` against `project_dir`.
///
/// - Reads `rushi.toml` from `project_dir`.
/// - Resolves which tools to copy from `kernel_tools_dir`.
/// - Materializes `tools/<name>/` for each missing kernel tool.
/// - Writes `rushi.lock` (or verifies in `--locked` mode).
/// - Writes `.envrc`: creates it when missing, appends the
///   `RUSHI_TOOLS_DIR` export when absent, never clobbers user content
///   (P6: additive).
/// - Generates a fully commented `config.toml` template (manifest `[loop]`
///   overrides active) only if it does not already exist.
///
/// In `--locked` mode, reads `rushi.lock` and verifies the kernel
/// commit matches; fails on stale or missing lock.
pub fn do_setup(locked: bool, project_dir: &Path, kernel_tools_dir: &Path) -> Result<()> {
    let cwd = project_dir;
    let manifest_path = cwd.join("rushi.toml");

    if !manifest_path.exists() {
        anyhow::bail!(
            "rushi.toml not found in {}. Run `rushi setup` from the project root.",
            cwd.display()
        );
    }

    let manifest = RushiManifest::from_path(&manifest_path)?;

    let kernel_tools = if kernel_tools_dir.exists() {
        list_tool_dirs(kernel_tools_dir)?
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
    let kernel_commit: String = if locked {
        if !lock_path.exists() {
            anyhow::bail!(
                "--locked: rushi.lock not found. Run `rushi setup` first to generate it."
            );
        }
        let lock = RushiLock::from_path(&lock_path)?;
        let expected_commit = if kernel_tools_dir
            .join(".rushi_commit")
            .exists()
        {
            std::fs::read_to_string(kernel_tools_dir.join(".rushi_commit"))
                .map(|s| s.trim().to_string())
                .unwrap_or_default()
        } else {
            String::new()
        };
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
        lock.lock.kernel_commit.clone()
    } else {
        // Materialize kernel tools that are missing locally.
        for name in &plan.copy_from_kernel {
            let src = kernel_tools_dir.join(name);
            let dst = local_tools_dir.join(name);
            if !dst.exists() {
                copy_dir_recursive(&src, &dst)?;
                eprintln!("setup: copied {} → tools/", name);
            }
        }

        // Write the lock file.
        let kc = read_kernel_commit(kernel_tools_dir);
        let lock = RushiLock {
            lock: LockMeta {
                version: "0.1".into(),
                kernel_commit: kc.clone(),
            },
            external: Vec::new(),
        };
        let lock_text = lock.render();
        std::fs::write(&lock_path, &lock_text)
            .with_context(|| format!("cannot write {}", lock_path.display()))?;
        eprintln!("setup: wrote rushi.lock");
        kc
    };

    // Write .envrc.
    let envrc_path = cwd.join(".envrc");
    let envrc_content = render_envrc(&local_tools_dir);
    if !envrc_path.exists() {
        std::fs::write(&envrc_path, &envrc_content)
            .with_context(|| format!("cannot write {}", envrc_path.display()))?;
        eprintln!("setup: wrote .envrc");
    } else {
        eprintln!("setup: .envrc already exists, leaving it untouched");
    }

    // Generate config.toml only if it does not exist (P6: additive).
    let config_path = cwd.join("config.toml");
    if !config_path.exists() {
        let config_text = render_config(&manifest, env!("CARGO_PKG_VERSION"), &kernel_commit);
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
/// 3. `tools/` in `project_dir` (dev checkout)
pub fn resolve_kernel_tools_dir(project_dir: &Path) -> PathBuf {
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
    project_dir.join("tools")
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
    fn render_config_includes_header_and_options() {
        let m = manifest(vec!["read"]);
        let text = render_config(&m, "0.1.0", "abc1234");
        // Header records the generating version and commit.
        assert!(text.contains("rushi setup` v0.1.0"));
        assert!(text.contains("kernel commit: abc1234"));
        // The manifest override stays active (uncommented).
        assert!(text.contains("compact_reserve_tokens = 16384"));
        assert!(!text.contains("# compact_reserve_tokens"));
        // Untouched options stay commented out.
        assert!(text.contains("# read_limit = 2000"));
        assert!(text.contains("# sessions_root = \"sessions\""));
        // All major sections are present.
        assert!(text.contains("[active]"));
        assert!(text.contains("[model]"));
        assert!(text.contains("[paths]"));
        assert!(text.contains("[limits]"));
        assert!(text.contains("[hooks]"));
    }

    #[test]
    fn render_config_comments_defaults_when_no_override() {
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
        let text = render_config(&m, "0.1.0", "dev");
        assert!(text.contains("# compact_reserve_tokens = 16384"));
        assert!(text.contains("[paths]"));
        assert!(text.contains("# extra_tools_roots = []"));
        // Every section is present as a template.
        assert!(text.contains("[active]"));
        assert!(text.contains("[model]"));
        assert!(text.contains("[paths]"));
        assert!(text.contains("[limits]"));
        assert!(text.contains("[hooks]"));
    }

    // ── Materialization tests (P2, P6, P7) ──────────────────────────

    /// Build a fake kernel dir with the given tool names.
    fn make_kernel(tmp: &std::path::Path, tools: &[&str]) -> std::path::PathBuf {
        let kdir = tmp.join("kernel").join("tools");
        for t in tools {
            let td = kdir.join(t);
            std::fs::create_dir_all(&td).unwrap();
            std::fs::write(td.join("tool.toml"), "[tool]\ndescription = \"test\"\n")
                .unwrap();
            std::fs::write(td.join(t), "fn main() {}\n").unwrap();
        }
        kdir
    }

    fn make_project(tmp: &std::path::Path, enabled: &[&str]) -> std::path::PathBuf {
        let pdir = tmp.join("project");
        std::fs::create_dir_all(&pdir).unwrap();
        let toml = format!(
            "[tools]\nenabled = [{}]\n",
            enabled
                .iter()
                .map(|t| format!("\"{}\"", t))
                .collect::<Vec<_>>()
                .join(", ")
        );
        std::fs::write(pdir.join("rushi.toml"), toml).unwrap();
        pdir
    }

    fn tool_names_in(dir: &Path) -> BTreeSet<String> {
        if !dir.exists() {
            return BTreeSet::new();
        }
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name() != "tool.toml")
            .filter_map(|e| {
                e.path()
                    .join("tool.toml")
                    .exists()
                    .then(|| e.file_name().to_string_lossy().into_owned())
            })
            .collect()
    }

    #[test]
    fn p2_materialized_set_equals_declared() {
        let tmp = tempfile::tempdir().unwrap();
        let kdir = make_kernel(tmp.path(), &["read", "write", "edit", "list", "bash"]);
        let pdir = make_project(tmp.path(), &["read", "bash"]);

        do_setup(false, &pdir, &kdir).unwrap();

        let materialized = tool_names_in(&pdir.join("tools"));
        assert_eq!(
            materialized,
            tool_set(&["bash", "read"]),
            "only declared tools should be materialized"
        );
        assert!(pdir.join("rushi.lock").exists(), "lock file written");
        assert!(pdir.join(".envrc").exists(), ".envrc written");
        assert!(
            pdir.join("config.toml").exists(),
            "config.toml generated"
        );
    }

    #[test]
    fn p6_idempotent_setup() {
        let tmp = tempfile::tempdir().unwrap();
        let kdir = make_kernel(tmp.path(), &["read", "write", "bash"]);
        let pdir = make_project(tmp.path(), &["read", "write", "bash"]);

        do_setup(false, &pdir, &kdir).unwrap();

        // User adds a project-local tool between runs.
        let local_tool = pdir.join("tools").join("mytool");
        std::fs::create_dir_all(&local_tool).unwrap();
        std::fs::write(local_tool.join("tool.toml"), "[tool]\ndescription = \"mine\"\n")
            .unwrap();

        let before = std::fs::read_to_string(pdir.join("tools").join("read").join("read")).unwrap();

        do_setup(false, &pdir, &kdir).unwrap();

        // Kernel tools unchanged (byte-identical).
        let after = std::fs::read_to_string(pdir.join("tools").join("read").join("read")).unwrap();
        assert_eq!(before, after, "kernel tool not overwritten");

        // Project tool survives.
        assert!(
            local_tool.join("tool.toml").exists(),
            "project tool survived second setup"
        );
    }

    #[test]
    fn p7_local_masks_kernel() {
        let tmp = tempfile::tempdir().unwrap();
        let kdir = make_kernel(tmp.path(), &["read", "write"]);
        let pdir = make_project(tmp.path(), &["read", "write"]);

        // Pre-create a local `read` with different content (a project override).
        let local_read = pdir.join("tools").join("read");
        std::fs::create_dir_all(&local_read).unwrap();
        std::fs::write(local_read.join("tool.toml"), "[tool]\ndescription = \"local override\"\n")
            .unwrap();
        std::fs::write(local_read.join("read"), "// local read\n").unwrap();

        do_setup(false, &pdir, &kdir).unwrap();

        // The local override must be untouched.
        let content = std::fs::read_to_string(local_read.join("read")).unwrap();
        assert_eq!(content, "// local read\n", "local tool not overwritten");
    }

    #[test]
    fn p10_missing_tool_fails_explicitly() {
        let tmp = tempfile::tempdir().unwrap();
        let kdir = make_kernel(tmp.path(), &["read"]);
        let pdir = make_project(tmp.path(), &["read", "nonexistent"]);

        let err = do_setup(false, &pdir, &kdir).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("nonexistent"),
            "error names the missing tool: {msg}"
        );
        // No partial tools/ dir: the bail happens before any copy.
        assert!(
            !pdir.join("tools").exists()
                || tool_names_in(&pdir.join("tools")).is_empty(),
            "no partial tools/ dir on failure"
        );
        assert!(
            !pdir.join("rushi.lock").exists(),
            "no lock file written on failure"
        );
    }

    #[test]
    fn p6_envrc_not_overwritten() {
        let tmp = tempfile::tempdir().unwrap();
        let kdir = make_kernel(tmp.path(), &["read", "bash"]);
        let pdir = make_project(tmp.path(), &[]);

        // Pre-create a user-authored .envrc with custom content.
        let user_envrc = "# my custom envrc\nexport FOO=bar\n";
        std::fs::write(pdir.join(".envrc"), user_envrc).unwrap();

        do_setup(false, &pdir, &kdir).unwrap();

        let content = std::fs::read_to_string(pdir.join(".envrc")).unwrap();
        assert_eq!(
            content, user_envrc,
            "user .envrc must not be overwritten by setup"
        );
    }
}
