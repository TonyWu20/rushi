#![deny(clippy::todo, clippy::unimplemented, clippy::unreachable)]

use clap::Parser;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::PathBuf;

/// Append a user_message event to a session log and run the agent loop
#[derive(Parser)]
#[command(name = "user", about = "Append a user message and run the agent loop")]
struct Args {
    /// Session name or directory
    #[arg(long)]
    session: String,

    /// Path to config file (used to resolve session names)
    #[arg(long, default_value = "config.toml")]
    config: String,

    /// Path to schema directory (for validation)
    #[arg(long, default_value = "schemas/events/v1")]
    schemas: String,

    /// Do not run the agent loop after appending
    #[arg(long)]
    no_run: bool,

    /// Message content. If omitted, read from stdin.
    content: Option<String>,
}

fn main() {
    let args = Args::parse();

    let content = match args.content {
        Some(c) => c,
        None => {
            let mut buf = String::new();
            io::stdin()
                .read_to_string(&mut buf)
                .expect("Failed to read stdin");
            // Trim one trailing newline from piped input. The rest stays.
            if buf.ends_with('\n') {
                buf.pop();
                if buf.ends_with('\r') {
                    buf.pop();
                }
            }
            buf
        }
    };

    if content.is_empty() {
        eprintln!("Error: content must not be empty.");
        std::process::exit(1);
    }

    let ts = chrono_utc_now();
    let event = serde_json::json!({
        "v": 1,
        "type": "user_message",
        "ts": ts,
        "content": content
    });

    // Validate the produced event against the schema (G3).
    validate_event(&event, &args.schemas);

    let session_dir = resolve_session_dir(&args.session, &args.config);
    let log_path = session_dir.join("events.jsonl");

    if !session_dir.exists() {
        if let Err(e) = fs::create_dir_all(&session_dir) {
            eprintln!("Error: cannot create session directory: {e}");
            std::process::exit(1);
        }
    }

    // Record the working directory on the first user message. The entry
    // point owns this decision; later stages only read it.
    let cwd_path = session_dir.join("cwd");
    if !cwd_path.exists() {
        if let Ok(cwd) = std::env::current_dir() {
            if let Err(e) = fs::write(&cwd_path, cwd.to_string_lossy().as_bytes()) {
                eprintln!("Warning: cannot write cwd file: {e}");
            }
        }
    }

    let mut file = match OpenOptions::new()
        .create(true)
        .append(true)
        .write(true)
        .open(&log_path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Error: cannot open log file {}: {e}", log_path.display());
            std::process::exit(1);
        }
    };

    let line = serde_json::to_string(&event).unwrap();
    if let Err(e) = writeln!(file, "{}", line) {
        eprintln!("Error: cannot write to log: {e}");
        std::process::exit(1);
    }

    if args.no_run {
        println!("{}", line);
        return;
    }

    // Run the agent loop
    run_turn(&args.session, &args.config);
}

/// Run the agent loop via turn.sh
fn run_turn(session: &str, config: &str) {
    // Find turn.sh relative to the config file location
    let config_path = PathBuf::from(config);
    let repo_root = config_path
        .canonicalize()
        .map(|p| p.parent().map(|d| d.to_path_buf()))
        .ok()
        .flatten()
        .unwrap_or_else(|| PathBuf::from("."));
    let turn_script = repo_root.join("scripts").join("turn.sh");

    if !turn_script.exists() {
        eprintln!("Error: turn.sh not found at {}", turn_script.display());
        std::process::exit(1);
    }

    let config_abs = config_path
        .canonicalize()
        .unwrap_or_else(|_| config_path.clone());

    let status = std::process::Command::new("bash")
        .arg(&turn_script)
        .arg(session)
        .env("CONFIG", config_abs.to_string_lossy().as_ref())
        .current_dir(&repo_root)
        .status()
        .expect("Failed to execute turn.sh");

    if !status.success() {
        eprintln!("Error: turn.sh exited with status {}", status.code().unwrap_or(-1));
        std::process::exit(status.code().unwrap_or(1));
    }
}

/// Resolve a session name or path to a session directory.
fn resolve_session_dir(session: &str, config_path: &str) -> PathBuf {
    let p = PathBuf::from(session);
    if session.contains('/') || session.contains('\\') || p.is_dir() {
        return p;
    }
    read_sessions_root(config_path).join(session)
}

fn read_sessions_root(config_path: &str) -> PathBuf {
    let content = fs::read_to_string(config_path).unwrap_or_default();
    let val: toml::Value = content.parse().unwrap_or_else(|_| {
        toml::Value::Table(toml::map::Map::new())
    });
    val.get("paths")
        .and_then(|p| p.get("sessions_root"))
        .and_then(|s| s.as_str())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("sessions"))
}

fn chrono_utc_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Validate an event against a schema file in the schema directory.
fn validate_event(event: &serde_json::Value, schemas_dir: &str) {
    let schema_path = PathBuf::from(schemas_dir).join("user_message.json");
    let schema: serde_json::Value = match fs::read_to_string(&schema_path) {
        Ok(c) => match serde_json::from_str(&c) {
            Ok(v) => v,
            Err(_) => {
                eprintln!("Error: invalid schema file {}", schema_path.display());
                std::process::exit(1);
            }
        },
        Err(_) => {
            eprintln!("Error: cannot read schema {}", schema_path.display());
            std::process::exit(1);
        }
    };

    if !matches_schema(event, &schema) {
        eprintln!("Error: produced event does not match user_message schema.");
        std::process::exit(1);
    }
}

fn matches_schema(value: &serde_json::Value, schema: &serde_json::Value) -> bool {
    if let Some(const_val) = schema.get("const") {
        return value == const_val;
    }
    match schema.get("type").and_then(|t| t.as_str()) {
        Some("object") => {
            let Some(obj) = value.as_object() else {
                return false;
            };
            if let Some(required) = schema.get("required").and_then(|r| r.as_array()) {
                for req in required {
                    if let Some(field) = req.as_str() {
                        if !obj.contains_key(field) {
                            return false;
                        }
                    }
                }
            }
            if let Some(props) = schema.get("properties").and_then(|p| p.as_object()) {
                for (key, prop_schema) in props {
                    if let Some(val) = obj.get(key) {
                        if !matches_schema(val, prop_schema) {
                            return false;
                        }
                    }
                }
            }
            true
        }
        Some("string") => value.is_string(),
        Some("integer") => value.is_i64(),
        _ => true,
    }
}
