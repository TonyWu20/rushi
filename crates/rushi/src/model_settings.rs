//! Model-settings resolution, shared by every binary that reads a model
//! section from the config (docs/itches.md: the "hard copies of the same
//! settings" itch). The defaults and the per-model/global override chain
//! live in exactly one place so a change cannot desync the binaries.

use toml::Value;

/// Default `max_output_tokens` when the config omits it.
pub const DEFAULT_MAX_OUTPUT_TOKENS: u64 = 32768;
/// Default `context_tokens` when the config omits it.
pub const DEFAULT_CONTEXT_TOKENS: u64 = 262144;
/// Default `reasoning_effort` when the config omits it.
pub const DEFAULT_REASONING_EFFORT: &str = "xhigh";
/// Default request timeout in seconds (0 disables the cap).
pub const DEFAULT_MODEL_TIMEOUT_S: u64 = 3600;
/// Default base URL for a local model server.
pub const DEFAULT_BASE_URL: &str = "http://127.0.0.1:8080";
/// Default env var that holds the API key.
pub const DEFAULT_API_KEY_ENV: &str = "MODEL_API_KEY";
/// Default active model section name.
pub const DEFAULT_ACTIVE_MODEL: &str = "deepseek";

/// Fully resolved settings for one model section. Per-model values override
/// the global `[model]` values, which override the defaults. Consumers pick
/// the fields they need.
pub struct ModelSettings {
    /// Model id sent in requests (`model_id`, falling back to the section name).
    pub model_id: String,
    /// Base URL of the model server.
    pub base_url: String,
    /// Output token cap for the length-stop test.
    pub max_output_tokens: u64,
    /// Model context window in tokens.
    pub context_tokens: u64,
    /// Reasoning effort as sent in the request.
    pub reasoning_effort: String,
    /// Name of the env var that holds the API key.
    pub api_key_env: String,
    /// Request timeout in seconds.
    pub timeout_s: u64,
}

impl Default for ModelSettings {
    fn default() -> Self {
        Self {
            model_id: String::new(),
            base_url: DEFAULT_BASE_URL.to_string(),
            max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
            context_tokens: DEFAULT_CONTEXT_TOKENS,
            reasoning_effort: DEFAULT_REASONING_EFFORT.to_string(),
            api_key_env: DEFAULT_API_KEY_ENV.to_string(),
            timeout_s: DEFAULT_MODEL_TIMEOUT_S,
        }
    }
}

/// Read a string value from a TOML table.
pub fn val_str(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(|s| s.to_string())
}

/// Read an integer value from a TOML table.
pub fn val_int(v: &Value, key: &str) -> Option<i64> {
    v.get(key).and_then(|x| x.as_integer())
}

/// Read a boolean value from a TOML table.
pub fn val_bool(v: &Value, key: &str) -> Option<bool> {
    v.get(key).and_then(|x| x.as_bool())
}

/// The active model name: the `MODEL` env var, else the config's
/// `[active] model`, else [`DEFAULT_ACTIVE_MODEL`]. Used by the standalone
/// stage binaries, which honor a per-invocation override.
pub fn resolve_active_model(config: &Value) -> String {
    std::env::var("MODEL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| active_model_from_config(config))
}

/// The active model name from the config only (no env override). The
/// kernel resolves the active model from the config file as its source of
/// truth.
pub fn active_model_from_config(config: &Value) -> String {
    config
        .get("active")
        .and_then(|a| a.get("model"))
        .and_then(|m| m.as_str())
        .unwrap_or(DEFAULT_ACTIVE_MODEL)
        .to_string()
}

/// Resolve the settings for a named model section. Per-model values win over
/// the global `[model]` section, which wins over the defaults.
pub fn resolve_model_settings(config: &Value, name: &str) -> ModelSettings {
    let empty = Value::Table(toml::map::Map::new());
    let model_root = config.get("model").unwrap_or(&empty);
    let mdl = model_root.get(name).unwrap_or(&empty);

    ModelSettings {
        model_id: val_str(mdl, "model_id").unwrap_or_else(|| name.to_string()),
        base_url: val_str(mdl, "base_url").unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
        max_output_tokens: val_int(mdl, "max_output_tokens")
            .or_else(|| val_int(model_root, "max_output_tokens"))
            .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS as i64)
            as u64,
        context_tokens: val_int(mdl, "context_tokens")
            .unwrap_or(DEFAULT_CONTEXT_TOKENS as i64)
            as u64,
        reasoning_effort: val_str(mdl, "reasoning_effort")
            .or_else(|| val_str(model_root, "reasoning_effort"))
            .unwrap_or_else(|| DEFAULT_REASONING_EFFORT.to_string()),
        api_key_env: val_str(mdl, "api_key_env")
            .unwrap_or_else(|| DEFAULT_API_KEY_ENV.to_string()),
        timeout_s: val_int(mdl, "timeout_s")
            .or_else(|| val_int(model_root, "model_timeout_s"))
            .unwrap_or(DEFAULT_MODEL_TIMEOUT_S as i64)
            as u64,
    }
}
