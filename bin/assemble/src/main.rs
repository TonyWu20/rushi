use clap::Parser;
use std::fs;
use std::path::PathBuf;

/// Project session log to ModelRequest
#[derive(Parser)]
#[command(name = "assemble", about = "Project the session log into a ModelRequest")]
struct Args {
    /// Session directory path
    #[arg(long)]
    session: String,

    /// Path to config file
    #[arg(long, default_value = "config.toml")]
    config: String,
}

struct ModelSettings {
    model_id: String,
    max_output_tokens: u64,
    context_tokens: usize,
    chars_per_token: usize,
}

/// Resolve the active model name from the MODEL env var or config.
fn resolve_active_model(config: &toml::Value) -> String {
    std::env::var("MODEL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            config
                .get("active")
                .and_then(|a| a.get("model"))
                .and_then(|m| m.as_str())
                .unwrap_or("deepseek")
                .to_string()
        })
}

fn val_str(v: &toml::Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(|s| s.to_string())
}

fn val_int(v: &toml::Value, key: &str) -> Option<i64> {
    v.get(key).and_then(|x| x.as_integer())
}

/// Resolve settings for a named model. Per-model values override defaults.
fn resolve_model_settings(config: &toml::Value, name: &str) -> ModelSettings {
    let empty = toml::Value::Table(toml::map::Map::new());
    let model_root = config.get("model").unwrap_or(&empty);
    let mdl = model_root.get(name).unwrap_or(&empty);
    ModelSettings {
        model_id: val_str(mdl, "model_id").unwrap_or_else(|| name.to_string()),
        max_output_tokens: val_int(mdl, "max_output_tokens")
            .or_else(|| val_int(model_root, "max_output_tokens"))
            .unwrap_or(4096) as u64,
        context_tokens: val_int(mdl, "context_tokens").unwrap_or(131072) as usize,
        chars_per_token: val_int(model_root, "chars_per_token").unwrap_or(4) as usize,
    }
}

fn main() {
    let args = Args::parse();

    let config_path = &args.config;

    // Read config
    let config_content = match fs::read_to_string(config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: cannot read config: {e}");
            std::process::exit(1);
        }
    };

    let config: toml::Value = match config_content.parse() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Error: invalid config TOML: {e}");
            std::process::exit(1);
        }
    };

    let system_prompt = config
        .get("system_prompt")
        .and_then(|p| p.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();

    let tool_result_max_chars: usize = config
        .get("limits")
        .and_then(|l| l.get("tool_result_max_chars"))
        .and_then(|c| c.as_integer())
        .unwrap_or(20000) as usize;

    let tools_root = config
        .get("paths")
        .and_then(|p| p.get("tools_root"))
        .and_then(|t| t.as_str())
        .unwrap_or("tools");

    // Resolve the active model and derive the context budget from its window.
    let active_model = resolve_active_model(&config);
    let model_settings = resolve_model_settings(&config, &active_model);
    let derived_budget = model_settings
        .context_tokens
        .saturating_sub(model_settings.max_output_tokens as usize)
        .saturating_mul(model_settings.chars_per_token);
    let explicit_budget = config
        .get("limits")
        .and_then(|l| l.get("context_budget_chars"))
        .and_then(|c| c.as_integer())
        .map(|v| v as usize);
    let context_budget_chars = match explicit_budget {
        Some(e) => e.min(derived_budget),
        None => derived_budget,
    };

    // Read events
    let log_path = PathBuf::from(&args.session).join("events.jsonl");
    let lines = match fs::read_to_string(&log_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: cannot read log: {e}");
            std::process::exit(1);
        }
    };

    // Parse events and build input items
    let mut input_items: Vec<serde_json::Value> = Vec::new();

    for line in lines.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let event: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let event_type = event.get("type").and_then(|t| t.as_str()).unwrap_or("");

        match event_type {
            "user_message" => {
                let content = event
                    .get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("");
                input_items.push(serde_json::json!({
                    "type": "message",
                    "role": "user",
                    "content": content
                }));
            }
            "assistant_message" => {
                let content = event
                    .get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("");
                input_items.push(serde_json::json!({
                    "type": "message",
                    "role": "assistant",
                    "content": content
                }));

                // Add one function_call item per tool call
                if let Some(tool_calls) = event.get("tool_calls") {
                    if let Some(tc_arr) = tool_calls.as_array() {
                        for tc in tc_arr {
                            let call_id = tc
                                .get("id")
                                .and_then(|id| id.as_str())
                                .unwrap_or("")
                                .to_string();
                            let name = tc
                                .get("name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("")
                                .to_string();
                            // The log stores arguments as a JSON object.
                            // The Responses API needs them as a JSON string.
                            let args_str = tc
                                .get("arguments")
                                .map(|a| serde_json::to_string(a).unwrap_or_else(|_| "{}".to_string()))
                                .unwrap_or_else(|| "{}".to_string());
                            input_items.push(serde_json::json!({
                                "type": "function_call",
                                "call_id": call_id,
                                "name": name,
                                "arguments": args_str
                            }));
                        }
                    }
                }
            }
            "tool_result" => {
                let call_id = event
                    .get("id")
                    .and_then(|id| id.as_str())
                    .unwrap_or("")
                    .to_string();
                let value = event.get("value");
                let text = match value {
                    Some(v) => v.get("text").and_then(|t| t.as_str()).unwrap_or(""),
                    None => "",
                };

                // Apply tool_result_max_chars cap
                let (clipped_text, _clipped) = if text.len() > tool_result_max_chars {
                    let mut chars = text.chars();
                    let clipped_str: String = chars
                        .by_ref()
                        .take(tool_result_max_chars)
                        .collect();
                    let original_len = text.len();
                    let clipped_len = clipped_str.len();
                    let suffix = format!(
                        "\n[tool result clipped: {} -> {} chars]",
                        original_len, clipped_len
                    );
                    (clipped_str + &suffix, true)
                } else {
                    (text.to_string(), false)
                };

                input_items.push(serde_json::json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": clipped_text
                }));
            }
            "error" => {
                // Skip error events - they are terminal
                continue;
            }
            _ => {}
        }
    }

    // Load tool schemas
    let tools_root_path = PathBuf::from(&tools_root);
    let mut tool_schemas: Vec<serde_json::Value> = Vec::new();

    if let Ok(entries) = fs::read_dir(&tools_root_path) {
        let mut tool_names: Vec<String> = Vec::new();
        for entry in entries.flatten() {
            let tool_path = entry.path();
            if tool_path.is_dir() {
                let tool_toml = tool_path.join("tool.toml");
                if tool_toml.exists() {
                    if let Some(name) = tool_path.file_name() {
                        tool_names.push(name.to_string_lossy().to_string());
                    }
                }
            }
        }
        tool_names.sort();

        for name in &tool_names {
            let tool_toml = tools_root_path.join(name).join("tool.toml");
            if let Ok(content) = fs::read_to_string(&tool_toml) {
                if let Ok(tool_config) = content.parse::<toml::Value>() {
                    if let Some(tool_def) = tool_config.get("tool") {
                        let desc = tool_def
                            .get("description")
                            .and_then(|d| d.as_str())
                            .unwrap_or("");
                        let params = match tool_def.get("schema") {
                            Some(p) => {
                                let s: serde_json::Value = toml_to_json(p);
                                s
                            }
                            None => serde_json::json!({
                                "type": "object",
                                "properties": {},
                                "required": []
                            }),
                        };

                        // Convert to Responses API tool schema format
                        tool_schemas.push(serde_json::json!({
                            "type": "function",
                            "name": name,
                            "description": desc,
                            "parameters": params
                        }));
                    }
                }
            }
        }
    }

    // Compute char count of the request
    let request = serde_json::json!({
        "model": model_settings.model_id,
        "instructions": system_prompt,
        "input": input_items,
        "tools": tool_schemas
    });

    // Serialize to compute char count
    let request_json = serde_json::to_string(&request).unwrap();
    let char_count = request_json.chars().count();

    if char_count > context_budget_chars {
        let ts = chrono_utc_now();
        let error_event = serde_json::json!({
            "v": 1,
            "type": "error",
            "ts": ts,
            "message": "Context budget exceeded. Start a new session or reduce scope."
        });
        println!("{}", error_event);
        return;
    }

    // Output ModelRequest
    println!("{}", request_json);
}

fn chrono_utc_now() -> String {
    chrono::Utc::now()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn toml_to_json(val: &toml::Value) -> serde_json::Value {
    match val {
        toml::Value::String(s) => serde_json::json!(s),
        toml::Value::Integer(i) => serde_json::json!(*i),
        toml::Value::Float(f) => serde_json::json!(*f),
        toml::Value::Boolean(b) => serde_json::json!(*b),
        toml::Value::Array(arr) => {
            serde_json::json!(arr.iter().map(|v| toml_to_json(v)).collect::<Vec<_>>())
        }
        toml::Value::Table(tbl) => {
            let mut map = serde_json::Map::new();
            for (k, v) in tbl {
                map.insert(k.clone(), toml_to_json(v));
            }
            serde_json::Value::Object(map)
        }
        toml::Value::Datetime(dt) => serde_json::json!(dt.to_string()),
    }
}
