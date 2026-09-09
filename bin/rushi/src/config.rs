//! Configuration loading for the harness.
//!
//! Reads `config.toml` and resolves the keys the loop needs:
//! sessions root, active model, model context tokens, input budget,
//! compact knobs, approval timeout, and hook registrations.

use std::path::{Path, PathBuf};
use toml::Value;

use rushi_common::hooks::HookRegistration;
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
    /// Root directory for tool manifests.
    pub tools_root: PathBuf,
    /// Extra roots for tool manifests (extension-provided tools, e.g.
    /// the exts repo's `goal-tools/` group), if `[paths]
    /// extra_tools_roots` is set. Scanned after `tools_root`; the
    /// primary root wins a name collision. Relative entries resolve
    /// against the stage CWD (the config dir).
    pub extra_tools_roots: Vec<PathBuf>,
    /// Directory containing event schemas.
    pub schemas_dir: PathBuf,

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

    // -- hooks --
    pub hooks_timeout_ms: u64,
    pub hooks: Vec<HookRegistration>,

    // -- binary paths --
    pub model_bin: PathBuf,
    pub compact_bin: PathBuf,
    pub assemble_bin: PathBuf,
    pub route_bin: PathBuf,
    pub claim_bin: PathBuf,
    pub parse_bin: PathBuf,
    pub log_bin: PathBuf,
}

impl HarnessConfig {
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

        let config_dir = config_path
            .canonicalize()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."));

        // Sessions root
        let sessions_root = cfg
            .get("paths")
            .and_then(|p| p.get("sessions_root"))
            .and_then(|s| s.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("sessions"));

        // Tools root
        let tools_root = cfg
            .get("paths")
            .and_then(|p| p.get("tools_root"))
            .and_then(|s| s.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("tools"));

        // Extra tools roots (extension-provided tool manifests, e.g.
        // the exts repo's goal-tools/ group; docs/tui-ext-repo-split.md
        // section 4, item 16).
        let extra_tools_roots = cfg
            .get("paths")
            .and_then(|p| p.get("extra_tools_roots"))
            .and_then(|l| l.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(PathBuf::from)
                    .collect()
            })
            .unwrap_or_default();

        // Schemas directory: sibling of the config file
        let schemas_dir = config_dir.join("schemas/events/v1");

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

        // Hooks
        let hooks_timeout_ms = cfg
            .get("hooks")
            .and_then(|h| h.get("timeout_ms"))
            .and_then(|v| v.as_integer())
            .unwrap_or(30000) as u64;

        let mut hooks: Vec<HookRegistration> = Vec::new();
        if let Some(on_list) = cfg
            .get("hooks")
            .and_then(|h| h.get("on"))
            .and_then(|o| o.as_array())
        {
            for entry in on_list {
                let window_str = entry.get("window").and_then(|w| w.as_str()).unwrap_or("");
                let command = entry.get("command").and_then(|c| c.as_str()).unwrap_or("").to_string();
                let window = match rushi_common::hooks::Window::parse(window_str) {
                    Some(w) => w,
                    None => {
                        eprintln!("Warning: unknown hook window '{window_str}', skipping");
                        continue;
                    }
                };
                let args: Vec<String> = entry
                    .get("args")
                    .and_then(|a| a.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                hooks.push(HookRegistration {
                    window,
                    command,
                    args,
                });
            }
        }

        // Binary paths: env overrides, then sibling of the running binary
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."));

        fn resolve_bin(env_var: &str, name: &str, exe_dir: &Path) -> PathBuf {
            if let Ok(p) = std::env::var(env_var) {
                return PathBuf::from(p);
            }
            exe_dir.join(name)
        }

        HarnessConfig {
            config_path: config_path.to_path_buf(),
            config_dir,
            sessions_root,
            tools_root,
            extra_tools_roots,
            schemas_dir,
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
            hooks,
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
}
