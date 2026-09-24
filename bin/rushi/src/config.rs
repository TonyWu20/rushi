//! Configuration loading for the harness.
//!
//! Reads `config.toml` and resolves the keys the loop needs:
//! sessions root, active model, model context tokens, input budget,
//! compact knobs, approval timeout, and hook definitions and
//! pipelines.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use toml::Value;

use rushi_common::config_check;
use rushi_common::hooks::{HookDef, Window};
use rushi_common::model_settings;

/// Resolved harness configuration for one run.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct HarnessConfig {
    /// Resolved path to the config file.
    pub config_path: PathBuf,
    /// The directory containing the config file (used as CWD for stages).
    pub config_dir: PathBuf,
    /// Root directory for session directories.
    pub sessions_root: PathBuf,
    /// Native tool dirs (each a dir containing a `tool.toml` manifest),
    /// from `[paths] native_tool_paths`. Relative entries resolve
    /// against the config dir.
    pub native_tool_paths: Vec<PathBuf>,
    /// Extension tool dirs (each a dir containing a `tool.toml`
    /// manifest, or a root dir holding several tool sub-dirs), from
    /// `[paths] extension_tool_paths`. Scanned after the native paths;
    /// the native path wins a name collision. Relative entries resolve
    /// against the config dir.
    pub extension_tool_paths: Vec<PathBuf>,

    // -- model resolution --
    /// The active model section name (e.g. "deepseek", "Qwen3.8-27B-...").
    pub active_model: String,
    /// The model id actually sent in requests (from `[model.NAME] model_id`).
    pub model_id: String,
    /// `max_output_tokens` for the length-stop test (default 32768).
    pub max_output_tokens: u64,
    /// The model context window in tokens (default 131072).
    pub context_tokens: u64,
    /// `context_budget_tokens` clamped to `context_tokens - max_output_tokens`.
    pub input_budget: u64,
    /// The raw `context_budget_tokens`, unclamped (the pi-parity base).
    pub context_budget: u64,
    /// The last measured input tokens from the log (for the threshold check).
    #[allow(dead_code)]
    pub last_measured_input: u64,

    // -- compact knobs --
    pub compact_enabled: bool,
    /// The compact strategy (only `compact` ships in Phase 2).
    pub compact_strategy: String,
    pub compact_reserve_tokens: u64,
    pub compact_keep_tokens: u64,
    pub compact_text_chars: u64,
    /// Chars-per-token ratio for context estimation (default 4).
    /// Calibrate via `rushi calibrate` for your model.
    pub estimate_chars_per_token: u64,

    /// The optional approval wait timeout in seconds. `None` = wait forever.
    pub approval_timeout_s: Option<u64>,

    // -- hooks (pipeline model, docs/loop-lifecycle-hooks.md 12) --
    pub hooks_timeout_ms: u64,
    /// Named hook definitions, keyed by the `hooks.defs` name.
    /// Commands are resolved against the package layout at load time.
    pub hook_defs: BTreeMap<String, HookDef>,
    /// Per-window step lists, in `steps` order. A window absent from
    /// this map runs zero steps (the window default applies).
    pub hook_pipelines: BTreeMap<Window, Vec<String>>,

    // -- binary paths --
    pub model_bin: PathBuf,
    pub compact_bin: PathBuf,
    pub assemble_bin: PathBuf,
    pub route_bin: PathBuf,
    pub claim_bin: PathBuf,
    pub parse_bin: PathBuf,
    pub log_bin: PathBuf,
}

/// Resolve a hook command against the package layout.
///
/// A bare name (no path separator) is tried against the sibling of
/// the running binary first. Kernel-shipped hooks such as
/// `harness-hook-compact` live there, in `bin/`. Then the name is
/// tried against the package `hooks/` dir. `lib.mkRushi` bundles
/// external hook binaries there.
///
/// This mirrors the stage-binary sibling resolution. A Nix-store
/// package stays self-contained. The bare names in the generated
/// `config.toml` resolve without relying on `PATH`. A name with a
/// path separator is relative or absolute and is returned as-is.
/// When neither sibling location holds the binary, the raw name is
/// returned. `Command::new` then falls back to a `PATH` lookup
/// (docs/reference/nix/nix-flake-module.md).
fn resolve_hook_command(raw: &str, exe_dir: &Path) -> String {
    if !raw.is_empty() && !raw.contains('/') {
        let sibling = exe_dir.join(raw);
        if sibling.is_file() {
            return sibling.to_string_lossy().into_owned();
        }
        let in_hooks = exe_dir.join("../hooks").join(raw);
        if in_hooks.is_file() {
            return in_hooks.to_string_lossy().into_owned();
        }
    }
    raw.to_string()
}

/// The running executable's path, symlinks resolved.
///
/// Lives in the shared crate so front-end binaries (rushi-tui,
/// rushi-web) resolve the Nix side-by-side layout the same way the
/// kernel does (docs/itches.md, 2026-09-20: front-ends self-wire).
/// The issue-#25 background (macOS launch path, profile symlink
/// chain, store package root two links down) is documented at the
/// definition: `crates/rushi/src/paths.rs`.
pub use rushi_common::paths::resolved_exe;

/// Load-time validation of the hook pipeline config (issue #38).
///
/// - A pipeline step name with no matching `[hooks.defs.<name>]`
///   entry is a hard error (a typo must not silently drop a hook).
/// - A def referenced by no pipeline is a warning (dead config),
///   not an error: defs may be staged ahead of their pipeline entry.
pub fn validate_hook_pipelines(
    defs: &BTreeMap<String, HookDef>,
    pipelines: &BTreeMap<Window, Vec<String>>,
) -> (Vec<String>, Vec<String>) {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let mut used = BTreeMap::<String, ()>::new();
    for (window, steps) in pipelines {
        for name in steps {
            if !defs.contains_key(name) {
                errors.push(format!(
                    "pipeline step '{name}' in [hooks.pipeline.\"{}\"] has no matching \
                     [hooks.defs.{}] entry; add the def or remove the step",
                    window.name(),
                    name
                ));
            }
            used.insert(name.clone(), ());
        }
    }
    for name in defs.keys() {
        if !used.contains_key(name) {
            warnings.push(format!(
                "[hooks.defs.{name}] is not referenced by any [hooks.pipeline] and will never run"
            ));
        }
    }
    (errors, warnings)
}

impl HarnessConfig {
    /// The ordered step names for one window's pipeline (empty when
    /// the window has no pipeline entry: zero steps, the window
    /// default applies and nothing is logged for it).
    pub fn pipeline_for(&self, window: Window) -> &[String] {
        self.hook_pipelines.get(&window).map(|s| s.as_slice()).unwrap_or(&[])
    }

    /// Load and resolve the config from a TOML file.
    pub fn load(config_path: &Path) -> Self {
        let raw = std::fs::read_to_string(config_path)
            .unwrap_or_else(|e| {
                eprintln!("Error: cannot read config: {e}");
                std::process::exit(1);
            });
        let cfg: Value = raw.parse().unwrap_or_else(|e| {
            eprintln!("Error: invalid config TOML: {e}");
            std::process::exit(1);
        });

        // Hard-fail on legacy `[paths]` keys that the rename in
        // 353424a made inert; they silently drop tool discovery.
        let legacy = config_check::legacy_key_report(&cfg);
        if !legacy.is_empty() {
            for msg in &legacy {
                eprintln!("Error: {msg}");
            }
            std::process::exit(1);
        }

        let config_dir = config_path
            .canonicalize()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."));

        // Sessions root (issue #16: resolve to an absolute path so the
        // hook env vars SESSION / SESSIONS_ROOT are unambiguous
        // regardless of where the kernel process is launched from).
        let sessions_root = {
            let raw = cfg
                .get("paths")
                .and_then(|p| p.get("sessions_root"))
                .and_then(|s| s.as_str())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("sessions"));
            if raw.is_relative() {
                match std::env::current_dir() {
                    Ok(cwd) => cwd.join(raw),
                    Err(_) => raw,
                }
            } else {
                raw
            }
        };

        // Native tool paths (each entry is a tool dir containing a
        // tool.toml, or a root dir holding tool sub-dirs). Relative
        // entries resolve against the config dir so Nix-store installs
        // work without full store paths.
        let native_tool_paths = cfg
            .get("paths")
            .and_then(|p| p.get("native_tool_paths"))
            .and_then(|l| l.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| {
                        let p = PathBuf::from(s);
                        if p.is_relative() {
                            config_dir.join(p)
                        } else {
                            p
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Extension tool paths (extension-provided tool manifests, e.g.
        // the exts repo's goal-tools/ group; docs/tui-ext-repo-split.md
        // section 4, item 16).
        let extension_tool_paths = cfg
            .get("paths")
            .and_then(|p| p.get("extension_tool_paths"))
            .and_then(|l| l.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| {
                        let p = PathBuf::from(s);
                        if p.is_relative() {
                            config_dir.join(p)
                        } else {
                            p
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Active model (config-only resolution; the kernel is the
        // source of truth — stage binaries use resolve_active_model
        // which also honours the MODEL env var).
        let active_model = model_settings::active_model_from_config(&cfg);

        // Model section resolution via the shared module (single source
        // of truth for defaults — docs/itches.md).
        let ms = model_settings::resolve_model_settings(&cfg, &active_model);
        let max_output_tokens = ms.max_output_tokens;
        let context_tokens = ms.context_tokens;
        let model_id = ms.model_id;

        // Input budget: context_budget_tokens clamped to window minus output
        let window_input = context_tokens.saturating_sub(max_output_tokens);
        let budget_raw = cfg
            .get("limits")
            .and_then(|l| l.get("context_budget_tokens"))
            .and_then(|v| v.as_integer())
            .map(|v| (v.max(1)) as u64)
            .unwrap_or(window_input);
        let input_budget = budget_raw.min(window_input.max(1)).max(1);
        // The raw (unclamped) context budget: the pi-parity trigger base.
        let context_budget = budget_raw.max(1);

        // Compact knobs
        let empty_limits = Value::Table(toml::map::Map::new());
        let limits = cfg.get("limits").unwrap_or(&empty_limits);
        let compact_enabled = limits
            .get("compact_enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let compact_strategy = limits
            .get("compact_strategy")
            .and_then(|v| v.as_str())
            .unwrap_or("compact")
            .to_string();
        let compact_reserve_tokens = model_settings::val_int(limits, "compact_reserve_tokens").unwrap_or(16384) as u64;
        let compact_keep_tokens = model_settings::val_int(limits, "compact_keep_tokens").unwrap_or(20000) as u64;
        let compact_text_chars = model_settings::val_int(limits, "compact_text_chars").unwrap_or(200) as u64;
        let estimate_chars_per_token = model_settings::resolve_model_settings(&cfg, &active_model)
            .estimate_chars_per_token;

        let approval_timeout_s = limits
            .get("approval_timeout_s")
            .and_then(|v| v.as_integer())
            .map(|v| v as u64);

        // Binary paths: env overrides, then sibling of the running binary.
        // Computed before the hook loop so bare hook commands can resolve
        // against the sibling `bin/` dir and the package `hooks/` dir.
        // The exe path is canonicalized (issue #25): on macOS
        // `current_exe()` reports the launch path without resolving
        // symlinks, so a profile symlink chain would otherwise miss the
        // store package's sibling `bin/` and `hooks/` dirs.
        let exe_dir = resolved_exe()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."));

        fn resolve_bin(env_var: &str, name: &str, exe_dir: &Path) -> PathBuf {
            if let Ok(p) = std::env::var(env_var) {
                return PathBuf::from(p);
            }
            exe_dir.join(name)
        }

        // Hooks (pipeline model, docs/loop-lifecycle-hooks.md 12):
        // named defs under `[hooks.defs.<name>]` and ordered step
        // lists under `[hooks.pipeline."<window>"]`. A step name with
        // no def is a load error. A def used by no pipeline warns.
        // A legacy `[[hooks.on]]` list is rejected by `config_check`
        // above (hard cutover, issue #38).
        let hooks_timeout_ms = cfg
            .get("hooks")
            .and_then(|h| h.get("timeout_ms"))
            .and_then(|v| v.as_integer())
            .unwrap_or(30000) as u64;

        let mut hook_defs: BTreeMap<String, HookDef> = BTreeMap::new();
        if let Some(defs) = cfg
            .get("hooks")
            .and_then(|h| h.get("defs"))
            .and_then(|d| d.as_table())
        {
            for (name, def) in defs {
                let raw_command = def
                    .get("command")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string();
                let command = resolve_hook_command(&raw_command, &exe_dir);
                let args: Vec<String> = def
                    .get("args")
                    .and_then(|a| a.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                let timeout_ms = def
                    .get("timeout_ms")
                    .and_then(|v| v.as_integer())
                    .map(|v| v as u64);
                hook_defs.insert(
                    name.clone(),
                    HookDef {
                        name: name.clone(),
                        command,
                        args,
                        timeout_ms,
                    },
                );
            }
        }

        let mut hook_pipelines: BTreeMap<Window, Vec<String>> = BTreeMap::new();
        if let Some(pipelines) = cfg
            .get("hooks")
            .and_then(|h| h.get("pipeline"))
            .and_then(|p| p.as_table())
        {
            for (window_str, pipeline) in pipelines {
                let window = match Window::parse(window_str) {
                    Some(w) => w,
                    None => {
                        eprintln!(
                            "Error: unknown hook window '{window_str}' in [hooks.pipeline]; \
                             known windows are the lifecycle names from \
                             docs/loop-lifecycle-hooks.md section 3"
                        );
                        std::process::exit(1);
                    }
                };
                let steps: Vec<String> = pipeline
                    .get("steps")
                    .and_then(|s| s.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                hook_pipelines.insert(window, steps);
            }
        }

        // Load-time validation: a step with no matching def is a hard
        // error; a def used by no pipeline is a warning.
        let (errors, warnings) =
            validate_hook_pipelines(&hook_defs, &hook_pipelines);
        if !errors.is_empty() {
            for msg in &errors {
                eprintln!("Error: {msg}");
            }
            std::process::exit(1);
        }
        for msg in &warnings {
            eprintln!("Warning: {msg}");
        }

        HarnessConfig {
            config_path: config_path.to_path_buf(),
            config_dir,
            sessions_root,
            native_tool_paths,
            extension_tool_paths,
            active_model,
            model_id,
            max_output_tokens,
            context_tokens,
            input_budget,
            context_budget,
            last_measured_input: 0,
            compact_enabled,
            compact_strategy,
            compact_reserve_tokens,
            compact_keep_tokens,
            compact_text_chars,
            estimate_chars_per_token,
            approval_timeout_s,
            hooks_timeout_ms,
            hook_defs,
            hook_pipelines,
            model_bin: resolve_bin("MODEL_BIN", "model", &exe_dir),
            compact_bin: resolve_bin("COMPACT_BIN", "compact", &exe_dir),
            assemble_bin: resolve_bin("ASSEMBLE_BIN", "assemble", &exe_dir),
            route_bin: resolve_bin("ROUTE_BIN", "route", &exe_dir),
            claim_bin: resolve_bin("CLAIM_BIN", "claim", &exe_dir),
            parse_bin: resolve_bin("PARSE_BIN", "parse", &exe_dir),
            log_bin: resolve_bin("LOG_BIN", "log", &exe_dir),
        }
    }

    /// Resolve a session name to a full path.
    pub fn resolve_session(&self, session: &str) -> PathBuf {
        let p = PathBuf::from(session);
        if session.contains('/') || session.contains('\\') || p.is_dir() {
            p
        } else {
            self.sessions_root.join(session)
        }
    }

    /// Trigger level: `context_budget - compact_reserve_tokens`.
    /// The trigger sits at the full context budget minus the reserve,
    /// giving the maximum useful context before compacting.
    pub fn trigger_level(&self) -> u64 {
        rushi_common::compact_math::trigger_level_for(
            self.context_budget,
            self.compact_reserve_tokens,
        )
    }

    /// The budget the silent-overflow backstop compares against:
    /// the full context budget, so the backstop sits at the provider
    /// wall instead of preempting the trigger threshold.
    pub fn compact_overflow_budget(&self) -> u64 {
        self.context_budget
    }
}

/// Read the raw text of a config file, guaranteeing a trailing newline.
///
/// Backs the `rushi config` subcommand: it prints this to stdout so a
/// redirect (`rushi config > config.toml`) ends in a complete line that
/// is safe to edit and re-save. The content is the file verbatim; no
/// key is added, removed, or reordered.
pub fn config_dump(config_path: &str) -> Result<String, std::io::Error> {
    let contents = std::fs::read_to_string(config_path)?;
    if contents.ends_with('\n') {
        Ok(contents)
    } else {
        Ok(format!("{contents}\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Write a minimal config to a temp file and load it.
    fn load_toml(extra_limits: &str) -> HarnessConfig {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        write!(
            f,
            r#"
[model]
api = "responses"
max_output_tokens = 32768

[model.stub]
model_id = "stub"
context_tokens = 262144

[active]
model = "stub"

[limits]
context_budget_tokens = 262144
compact_reserve_tokens = 16384
{extra_limits}
"#,
            extra_limits = extra_limits,
        )
        .unwrap();
        HarnessConfig::load(&path)
    }

    #[test]
    fn trigger_level_uses_context_budget_base() {
        let cfg = load_toml("");
        assert_eq!(cfg.input_budget, 229376);
        assert_eq!(cfg.context_budget, 262144);
        // Trigger is always context_budget - reserve = 262144 - 16384.
        assert_eq!(cfg.trigger_level(), 245760);
        assert_eq!(cfg.compact_overflow_budget(), 262144);
    }

    // Build a fake Nix package layout under a tempdir and return the
    // `bin/` dir (the exe_dir a `bin/rushi` would report).
    fn fake_pkg_layout() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        let hooks = root.path().join("hooks");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&hooks).unwrap();
        // Kernel-shipped hook binary sits in bin/ next to the kernel.
        std::fs::write(bin.join("harness-hook-compact"), "").unwrap();
        // External hook binary is bundled in hooks/ by lib.mkRushi.
        std::fs::write(hooks.join("harness-hook-goal-idle"), "").unwrap();
        (root, bin)
    }

    #[test]
    fn bare_hook_resolves_to_sibling_bin() {
        let (_root, bin) = fake_pkg_layout();
        let got = resolve_hook_command("harness-hook-compact", &bin);
        let want = bin.join("harness-hook-compact");
        assert_eq!(got, want.to_string_lossy().to_string());
    }

    #[test]
    fn bare_hook_resolves_to_package_hooks_dir() {
        let (_root, bin) = fake_pkg_layout();
        let got = resolve_hook_command("harness-hook-goal-idle", &bin);
        let want = bin.join("../hooks/harness-hook-goal-idle");
        assert_eq!(got, want.to_string_lossy().to_string());
    }

    #[test]
    fn bare_hook_missing_everywhere_stays_bare() {
        let (_root, bin) = fake_pkg_layout();
        // No such binary in bin/ or hooks/ -> fall back to PATH.
        assert_eq!(resolve_hook_command("no-such-hook", &bin), "no-such-hook");
    }

    #[test]
    fn path_hook_command_is_used_verbatim() {
        let (_root, bin) = fake_pkg_layout();
        // A name with a path separator is relative or absolute; as-is.
        assert_eq!(resolve_hook_command("./my/hook.sh", &bin), "./my/hook.sh");
        assert_eq!(resolve_hook_command("/abs/hook.sh", &bin), "/abs/hook.sh");
    }

    #[test]
    fn config_dump_reads_file_and_guarantees_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        let with_nl = dir.path().join("with-nl.toml");
        std::fs::write(&with_nl, "[paths]\nsessions_root = \"sessions\"\n").unwrap();
        assert_eq!(
            config_dump(&with_nl.to_string_lossy()).unwrap(),
            "[paths]\nsessions_root = \"sessions\"\n"
        );

        let no_nl = dir.path().join("no-nl.toml");
        std::fs::write(&no_nl, "[paths]\nsessions_root = \"sessions\"").unwrap();
        assert_eq!(
            config_dump(&no_nl.to_string_lossy()).unwrap(),
            "[paths]\nsessions_root = \"sessions\"\n"
        );
    }

    #[test]
    fn config_dump_missing_file_errors() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.toml");
        assert!(config_dump(&missing.to_string_lossy()).is_err());
    }

    /// A step name with no matching def is a hard error.
    #[test]
    fn pipeline_step_without_def_is_an_error() {
        let defs: BTreeMap<String, HookDef> = BTreeMap::new();
        let mut pipelines: BTreeMap<Window, Vec<String>> = BTreeMap::new();
        pipelines.insert(Window::ModelBefore, vec!["ghost".to_string()]);
        let (errors, warnings) = validate_hook_pipelines(&defs, &pipelines);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("ghost"));
        assert!(errors[0].contains("[hooks.defs.ghost]"));
        assert!(warnings.is_empty());
    }

    /// A def used by no pipeline warns; used defs do not.
    #[test]
    fn unused_def_is_a_warning() {
        let defs: BTreeMap<String, HookDef> = [
            ("used".to_string(), HookDef {
                name: "used".into(),
                command: "used".into(),
                args: vec![],
                timeout_ms: None,
            }),
            ("dead".to_string(), HookDef {
                name: "dead".into(),
                command: "dead".into(),
                args: vec![],
                timeout_ms: None,
            }),
        ]
        .into_iter()
        .collect();
        let mut pipelines: BTreeMap<Window, Vec<String>> = BTreeMap::new();
        pipelines.insert(Window::ModelBefore, vec!["used".to_string()]);
        let (errors, warnings) = validate_hook_pipelines(&defs, &pipelines);
        assert!(errors.is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("dead"));
    }

    /// Empty defs + empty pipelines validate cleanly.
    #[test]
    fn empty_hook_config_is_clean() {
        let defs: BTreeMap<String, HookDef> = BTreeMap::new();
        let pipelines: BTreeMap<Window, Vec<String>> = BTreeMap::new();
        let (errors, warnings) = validate_hook_pipelines(&defs, &pipelines);
        assert!(errors.is_empty());
        assert!(warnings.is_empty());
    }

    /// The new surface loads: defs resolve bare commands, pipelines
    /// keep `steps` order, a window with no entry runs zero steps.
    #[test]
    fn defs_and_pipelines_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[active]
model = "stub"

[model.stub]
model_id = "stub"

[hooks]
timeout_ms = 15000

[hooks.defs.compact]
command = "harness-hook-compact"

[hooks.defs.arm]
command = "/x/hook-arm"
args = ["--goal"]

[hooks.pipeline."model.before"]
steps = ["arm", "compact"]

[hooks.pipeline."model.after"]
steps = []
"#,
        )
        .unwrap();
        let cfg = HarnessConfig::load(&path);
        assert_eq!(cfg.hooks_timeout_ms, 15000);
        assert_eq!(cfg.hook_defs.len(), 2);
        assert!(cfg.hook_defs["arm"].args.iter().any(|a| a == "--goal"));
        let mb = cfg.pipeline_for(Window::ModelBefore);
        assert_eq!(mb, &["arm".to_string(), "compact".to_string()]);
        // A window with an empty `steps` list runs zero steps.
        assert!(cfg.pipeline_for(Window::ModelAfter).is_empty());
        // A window with no pipeline entry runs zero steps.
        assert!(cfg.pipeline_for(Window::RunIdle).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_exe_recovers_store_layout() {
        // Regression for issue #25: a two-level symlink chain
        // (profile -> hm path -> store) must resolve to the store
        // package root so sibling dirs are found.
        let (_root, bin) = fake_pkg_layout();
        // Compare against a canonical base: the resolved side below
        // passes through fs::canonicalize, which also resolves
        // platform-level symlinks (macOS $TMPDIR lives under /var,
        // which links to /private/var), while the raw tempdir path is
        // not canonical there. A raw-vs-canonical string comparison
        // would pass on Linux (/tmp is real) and fail on macOS.
        let bin = std::fs::canonicalize(&bin).unwrap();
        // Create the "rushi" binary placeholder inside the fake pkg.
        std::fs::write(bin.join("rushi"), "").unwrap();

        // Build a two-level symlink chain:
        //   outer/bin/rushi -> mid/bin/rushi -> <root>/bin/rushi
        let outer = tempfile::tempdir().unwrap();
        let mid = tempfile::tempdir().unwrap();
        let outer_bin = outer.path().join("bin");
        let mid_bin = mid.path().join("bin");
        std::fs::create_dir_all(&outer_bin).unwrap();
        std::fs::create_dir_all(&mid_bin).unwrap();

        let mid_link = mid_bin.join("rushi");
        std::os::unix::fs::symlink(bin.join("rushi"), &mid_link).unwrap();
        let outer_link = outer_bin.join("rushi");
        std::os::unix::fs::symlink(&mid_link, &outer_link).unwrap();

        // The raw launch path's parent has no sibling hooks/ dir.
        let raw_dir = outer_link.parent().unwrap().to_path_buf();
        assert!(
            !raw_dir.join("../hooks/harness-hook-goal-idle").is_file(),
            "raw launch dir should NOT have the hooks sibling (bug repro)"
        );

        // canonicalize the outer symlink -> resolves through both links
        // to the real <root>/bin/rushi, whose parent is <root>/bin.
        let canon = std::fs::canonicalize(&outer_link).unwrap();
        let canon_dir = canon.parent().unwrap().to_path_buf();
        let got = resolve_hook_command("harness-hook-goal-idle", &canon_dir);
        let want = bin.join("../hooks/harness-hook-goal-idle");
        assert_eq!(got, want.to_string_lossy().to_string());
    }
}
