use clap::Parser;
use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

/// Dispatch tool calls to subprocesses
#[derive(Parser)]
#[command(name = "route", about = "Dispatch tool calls to tool subprocesses")]
struct Args {
    /// Path to tools directory
    #[arg(long, default_value = "tools")]
    tools: String,

    /// Tool result max chars
    #[arg(long, default_value = "20000")]
    tool_result_max_chars: usize,
}

fn main() {
    let args = Args::parse();

    // Read tool_call events from stdin
    let stdin = io::stdin();
    let mut tool_calls: Vec<serde_json::Value> = Vec::new();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let event: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if event.get("type").and_then(|t| t.as_str()) == Some("tool_call") {
            tool_calls.push(event);
        }
    }

    let tools_root = PathBuf::from(&args.tools);

    // Load tool manifests and validate arguments
    let mut tool_manifests: HashMap<String, (String, serde_json::Value)> = HashMap::new();
    if let Ok(entries) = fs::read_dir(&tools_root) {
        let entries_vec: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        for entry in entries_vec {
            let tool_path = entry.path();
            if tool_path.is_dir() {
                let tool_toml = tool_path.join("tool.toml");
                if tool_toml.exists() {
                    if let Some(name) = tool_path.file_name() {
                        let name_str = name.to_string_lossy().to_string();
                        if let Ok(content) = fs::read_to_string(&tool_toml) {
                            match content.parse::<toml::Value>() {
                                Ok(config) => {
                                    let raw_command = config
                                        .get("tool")
                                        .and_then(|t| t.get("command"))
                                        .and_then(|c| c.as_str())
                                        .unwrap_or(&name_str)
                                        .to_string();
                                    // Resolve binary path: tools/<name>/bin/<name>
                                    let command = if raw_command == name_str {
                                        tool_path.join("bin").join(&name_str)
                                            .to_string_lossy().to_string()
                                    } else {
                                        raw_command
                                    };
                                    let args_val = config
                                        .get("tool")
                                        .and_then(|t| t.get("args"))
                                        .cloned()
                                        .unwrap_or(toml::Value::Array(toml::value::Array::new()));
                                    let timeout_ms = config
                                        .get("tool")
                                        .and_then(|t| t.get("timeout_ms"))
                                        .and_then(|t| t.as_integer())
                                        .unwrap_or(30000) as u64;
                                    let schema = config
                                        .get("tool")
                                        .and_then(|t| t.get("schema"))
                                        .cloned()
                                        .unwrap_or_else(|| {
                                            let mut tbl = toml::value::Table::new();
                                            tbl.insert("type".to_string(), toml::Value::String("object".to_string()));
                                            tbl.insert("properties".to_string(), toml::Value::Table(toml::value::Table::new()));
                                            tbl.insert("required".to_string(), toml::Value::Array(toml::value::Array::new()));
                                            toml::Value::Table(tbl)
                                        });
                                    let args_json: serde_json::Value = toml_to_json(&args_val);
                                    let schema_json: serde_json::Value = toml_to_json(&schema);
                                    tool_manifests.insert(
                                        name_str.clone(),
                                        (
                                            command,
                                            serde_json::json!({
                                                "args": args_json,
                                                "timeout_ms": timeout_ms,
                                                "schema": schema_json
                                            }),
                                        ),
                                    );
                                }
                                Err(_) => {
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Process each tool call
    let ts = chrono_utc_now();
    for tc in &tool_calls {
        let tc_id = tc.get("id").and_then(|id| id.as_str()).unwrap_or("");
        let tc_name = tc.get("name").and_then(|n| n.as_str()).unwrap_or("");
        // Arguments may be a JSON string or already a JSON object
        let tc_args: serde_json::Value = match tc.get("arguments") {
            Some(serde_json::Value::String(s)) => {
                match serde_json::from_str(s) {
                    Ok(v) => v,
                    Err(_) => {
                        let tool_result = serde_json::json!({
                            "v": 1,
                            "type": "tool_result",
                            "ts": ts,
                            "id": tc_id,
                            "value": {
                                "text": "Tool arguments failed schema validation: invalid JSON."
                            },
                            "is_error": true
                        });
                        println!("{}", tool_result);
                        continue;
                    }
                }
            }
            Some(v) => v.clone(),
            None => serde_json::json!({}),
        };
        let tc_args_str = serde_json::to_string(&tc_args).unwrap_or_default();

        // Check if tool manifest exists
        let manifest = match tool_manifests.get(tc_name) {
            Some(m) => m,
            None => {
                let tool_result = serde_json::json!({
                    "v": 1,
                    "type": "tool_result",
                    "ts": ts,
                    "id": tc_id,
                    "value": {
                        "text": format!("Unknown tool {}.", tc_name)
                    },
                    "is_error": true
                });
                println!("{}", tool_result);
                continue;
            }
        };

        let schema = manifest.1.get("schema").unwrap();
        if let Some(field) = validate_args(&tc_args, schema) {
            let tool_result = serde_json::json!({
                "v": 1,
                "type": "tool_result",
                "ts": ts,
                "id": tc_id,
                "value": {
                    "text": format!("Tool arguments failed schema validation: {}.", field)
                },
                "is_error": true
            });
            println!("{}", tool_result);
            continue;
        }

        // Spawn subprocess
        let command = &manifest.0;
        let args_json = manifest.1.get("args").unwrap();
        let timeout_ms = manifest.1.get("timeout_ms").and_then(|t| t.as_u64()).unwrap_or(30000);

        let mut child = match Command::new(command)
            .args(
                args_json
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str())
                            .map(|s| s.to_string())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
                    .iter(),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                let tool_result = serde_json::json!({
                    "v": 1,
                    "type": "tool_result",
                    "ts": ts,
                    "id": tc_id,
                    "value": {
                        "text": format!("Failed to spawn tool: {e}")
                    },
                    "is_error": true
                });
                println!("{}", tool_result);
                continue;
            }
        };

        // Write arguments to stdin
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(tc_args_str.as_bytes());
        }

        // Wait for subprocess with timeout
        let output = if timeout_ms > 0 {
            // Use a simple timeout approach
            match child.wait_with_output_timeout(timeout_ms as u64) {
                Ok(o) => o,
                Err(_) => {
                    let _ = child.kill();
                    let tool_result = serde_json::json!({
                        "v": 1,
                        "type": "tool_result",
                        "ts": ts,
                        "id": tc_id,
                        "value": {
                            "text": format!("Tool timed out after {} ms.", timeout_ms)
                        },
                        "is_error": true
                    });
                    println!("{}", tool_result);
                    continue;
                }
            }
        } else {
            match child.wait_with_output() {
                Ok(o) => o,
                Err(e) => {
                    let tool_result = serde_json::json!({
                        "v": 1,
                        "type": "tool_result",
                        "ts": ts,
                        "id": tc_id,
                        "value": {
                            "text": format!("Failed to wait for tool: {e}")
                        },
                        "is_error": true
                    });
                    println!("{}", tool_result);
                    continue;
                }
            }
        };

        let exit_code = output.status.code().unwrap_or(1);
        let stdout_str = String::from_utf8_lossy(&output.stdout);
        let stderr_str = String::from_utf8_lossy(&output.stderr);

        // Process stdout
        let value_text = process_stdout(&stdout_str, args.tool_result_max_chars);

        let is_error = exit_code != 0;
        let final_text = if is_error {
            // Use stderr on error
            let err_text = if !stderr_str.is_empty() {
                let mut text = stderr_str.to_string();
                if text.len() > args.tool_result_max_chars {
                    text = text.chars().take(args.tool_result_max_chars).collect();
                }
                text
            } else {
                format!("Tool exited with code {}.", exit_code)
            };
            err_text
        } else {
            value_text
        };

        let tool_result = serde_json::json!({
            "v": 1,
            "type": "tool_result",
            "ts": ts,
            "id": tc_id,
            "value": {
                "text": final_text
            },
            "is_error": is_error
        });
        println!("{}", tool_result);
    }
}

fn process_stdout(stdout: &str, max_chars: usize) -> String {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return "".to_string();
    }

    // Try to parse as JSON object with text field
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if let Some(obj) = val.as_object() {
            if let Some(text) = obj.get("text").and_then(|t| t.as_str()) {
                return text.to_string();
            }
            // JSON object without text field
            return serde_json::to_string(&val).unwrap_or_default();
        }
        // JSON but not an object
        return serde_json::to_string(&val).unwrap_or_default();
    }

    // Non-JSON text
    let mut text = trimmed.to_string();
    if text.len() > max_chars {
        text = text.chars().take(max_chars).collect();
    }
    text
}

fn validate_args(args: &serde_json::Value, schema: &serde_json::Value) -> Option<String> {
    if let Some(obj) = args.as_object() {
        if let Some(required) = schema.get("required").and_then(|r| r.as_array()) {
            for req in required {
                if let Some(field) = req.as_str() {
                    if !obj.contains_key(field) {
                        return Some(field.to_string());
                    }
                }
            }
        }
        return None;
    }
    Some("arguments".to_string())
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

trait WaitWithOutput {
    fn wait_with_output_timeout(&mut self, timeout_ms: u64) -> io::Result<std::process::Output>;
}

impl WaitWithOutput for Child {
    fn wait_with_output_timeout(&mut self, timeout_ms: u64) -> io::Result<std::process::Output> {
        let start = std::time::Instant::now();
        let duration = std::time::Duration::from_millis(timeout_ms);

        while start.elapsed() < duration {
            match self.try_wait() {
                Ok(Some(status)) => {
                    let stdout = self.stdout.take().map(|mut s| {
                        let mut buf = Vec::new();
                        io::Read::read_to_end(&mut s, &mut buf).unwrap_or(0);
                        buf
                    }).unwrap_or_default();
                    let stderr = self.stderr.take().map(|mut s| {
                        let mut buf = Vec::new();
                        io::Read::read_to_end(&mut s, &mut buf).unwrap_or(0);
                        buf
                    }).unwrap_or_default();
                    return Ok(std::process::Output {
                        status,
                        stdout,
                        stderr,
                    });
                }
                Ok(None) => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(e) => return Err(e),
            }
        }

        Err(io::Error::new(io::ErrorKind::TimedOut, "Timeout"))
    }
}
