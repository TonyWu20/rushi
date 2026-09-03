#![deny(clippy::todo, clippy::unimplemented, clippy::unreachable)]

use clap::Parser;
use harness_common::event_validation;
use harness_common::logline::LogLine;
use std::fs;
use std::io::{self, Read};
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

    /// Delivery queue for the message (docs/tui-pending-user-
    /// messages.md stage 2): `steer` injects at the next step of
    /// the running loop; `follow` runs only after the loop would
    /// stop. The field is written for `follow` only: the steer
    /// line keeps the stage-1 shape.
    #[arg(long, default_value = "steer")]
    queue: String,
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
    let mut event = serde_json::json!({
        "v": 1,
        "type": "user_message",
        "ts": ts,
        "content": content
    });
    // The follow queue writes the field; the steer queue leaves it
    // absent (a missing field means `steer`).
    if args.queue == "follow" {
        event["queue"] = serde_json::json!("follow");
    } else if args.queue != "steer" {
        eprintln!("Error: queue must be `steer` or `follow`.");
        std::process::exit(1);
    }

    // Validate the produced event against the schemas (G3).
    let schemas = event_validation::load_schemas(&args.schemas);
    if let Err(e) = event_validation::validate_value(&event, &schemas) {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }

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

    // One locked single-write append (FT-005). `LogLine` is the only
    // type that may write the session log.
    let line = serde_json::to_string(&event).unwrap();
    if let Err(e) = LogLine::from_json(&line).commit(&log_path) {
        eprintln!("Error: cannot write to log: {e}");
        std::process::exit(1);
    }

    if args.no_run {
        println!("{line}");
        return;
    }

    // Run the agent loop through the [loop] table.
    run_turn(&args.session, &args.config);
}

/// Run the agent loop via the `[loop]` table of config.toml.
///
/// The table holds `command`, `args`, and `arg_style` with the same
/// semantics as the TUI (docs/tui.md section 2.3). A missing table
/// is a hard error (docs/phase-2-plan.md section 5.3). The argv is
/// `<command> <args...> <session>`; the session is the last argument
/// for `arg_style = "append_session"`. The command runs with
/// `CONFIG` set to the absolute config path and the working
/// directory set to the config file's parent (the repo root).
fn run_turn(session: &str, config: &str) {
    let config_path = PathBuf::from(config);
    let config_content = match fs::read_to_string(&config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: cannot read config {}: {e}", config_path.display());
            std::process::exit(1);
        }
    };
    let config_val: toml::Value = match config_content.parse() {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "Error: invalid TOML in {}: {e}",
                config_path.display()
            );
            std::process::exit(1);
        }
    };

    // The [loop] table is required (docs/phase-2-plan.md section 5.3).
    let loop_table = match config_val.get("loop") {
        Some(v) => v,
        None => {
            eprintln!(
                "Error: missing [loop] table in {}",
                config_path.display()
            );
            std::process::exit(1);
        }
    };

    // command: required non-empty string.
    let command = match loop_table.get("command").and_then(|v| v.as_str()) {
        Some(c) if !c.trim().is_empty() => c.to_string(),
        Some(_) => {
            eprintln!(
                "Error: [loop].command must be a non-empty string in {}",
                config_path.display()
            );
            std::process::exit(1);
        }
        None => {
            eprintln!(
                "Error: [loop].command is missing in {}",
                config_path.display()
            );
            std::process::exit(1);
        }
    };

    // args: optional array of strings (defaults to empty).
    let args: Vec<String> = match loop_table.get("args") {
        None => Vec::new(),
        Some(v) => match v.as_array() {
            Some(arr) => {
                let mut out = Vec::with_capacity(arr.len());
                for item in arr {
                    match item.as_str() {
                        Some(s) => out.push(s.to_string()),
                        None => {
                            eprintln!(
                                "Error: [loop].args must be an array of strings in {}",
                                config_path.display()
                            );
                            std::process::exit(1);
                        }
                    }
                }
                out
            }
            None => {
                eprintln!(
                    "Error: [loop].args must be an array of strings in {}",
                    config_path.display()
                );
                std::process::exit(1);
            }
        },
    };

    // arg_style: defaults to "append_session" when absent; only that value
    // is supported (docs/tui.md section 2.3).
    let arg_style = loop_table
        .get("arg_style")
        .and_then(|v| v.as_str())
        .unwrap_or("append_session");
    if arg_style != "append_session" {
        eprintln!(
            "Error: [loop] arg_style \"{}\" is not supported (expected \"append_session\")",
            arg_style
        );
        std::process::exit(1);
    }

    let config_abs = config_path
        .canonicalize()
        .unwrap_or_else(|_| config_path.clone());
    let repo_root = config_abs
        .parent()
        .map(|d| d.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    // argv: `<command> <args...> <session>`. `arg_style` is
    // `append_session`, so the session is the last argument.
    let mut cmd_args: Vec<String> = args;
    cmd_args.push(session.to_string());

    let status = match std::process::Command::new(&command)
        .args(&cmd_args)
        .env("CONFIG", config_abs.to_string_lossy().as_ref())
        .current_dir(&repo_root)
        .status()
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "Error: cannot execute loop command \"{}\": {e}",
                command
            );
            std::process::exit(1);
        }
    };

    if !status.success() {
        let code = status.code().unwrap_or(1);
        eprintln!("Error: loop command exited with status {code}");
        std::process::exit(code);
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
    let val: toml::Value = content
        .parse()
        .unwrap_or_else(|_| toml::Value::Table(toml::map::Map::new()));
    val.get("paths")
        .and_then(|p| p.get("sessions_root"))
        .and_then(|s| s.as_str())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("sessions"))
}

fn chrono_utc_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}
