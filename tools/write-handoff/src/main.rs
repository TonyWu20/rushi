#![deny(clippy::todo, clippy::unimplemented, clippy::unreachable)]

//! The `write-handoff` tool. The single tool the compact agent uses.
//!
//! The agent supplies only the summary text (the tool argument). The
//! target directory and the version number are harness-side: the
//! compact driver writes a `handoff-target.json` sidecar into the
//! agent's own session dir before forking it. This tool reads that
//! sidecar via `HARNESS_SESSION_DIR` and writes the two handoff files
//! into the parent session dir. The model never learns the target
//! path or the version, so it cannot get them wrong.

use clap::Parser;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// Write the compact handoff to the parent session's handoff files
#[derive(Parser)]
#[command(name = "write-handoff", about = "Write the compact handoff summary", version)]
struct Args {
    /// Maximum summary bytes
    #[arg(long, default_value = "1048576")]
    write_max_bytes: usize,
}

/// The sidecar the compact driver writes into the agent session dir.
struct HandoffTarget {
    target_dir: PathBuf,
    version: u64,
    /// `handoff` (the default) writes the versioned handoff files.
    /// `branch` writes the versioned branch summary file.
    mode: String,
}

fn main() {
    let args = Args::parse();

    // The tool arguments, one JSON object on stdin (the `summary` key).
    let mut input_str = String::new();
    io::stdin()
        .read_to_string(&mut input_str)
        .expect("Failed to read stdin");

    let input: serde_json::Value = match serde_json::from_str(&input_str) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Error: invalid JSON input: {e}");
            std::process::exit(1);
        }
    };

    let summary = input.get("summary").and_then(|s| s.as_str()).unwrap_or("");
    let summary_bytes = summary.len();
    if summary_bytes > args.write_max_bytes {
        eprintln!(
            "Error: summary is {} bytes, exceeds max {} bytes.",
            summary_bytes, args.write_max_bytes
        );
        std::process::exit(1);
    }

    // The agent's own session dir, set by route. The sidecar lives here.
    let session_dir = match std::env::var("HARNESS_SESSION_DIR") {
        Ok(v) => PathBuf::from(v),
        Err(e) => {
            eprintln!("Error: HARNESS_SESSION_DIR is not set: {e}");
            std::process::exit(1);
        }
    };

    let target = read_sidecar(&session_dir);

    // The output paths, by mode. The handoff mode writes the versioned
    // file plus the latest copy. The branch mode writes only the
    // versioned branch summary file.
    let (subdir, write_latest): (&str, bool) = match target.mode.as_str() {
        "branch" => ("branch-summary", false),
        _ => ("handoff", true),
    };
    let version_path = target
        .target_dir
        .join(subdir)
        .join(format!("v{}.md", target.version));
    let latest_path = target.target_dir.join("handoff.md");

    // Create the output dir in the parent session dir.
    if let Err(e) = fs::create_dir_all(target.target_dir.join(subdir)) {
        eprintln!("Error: cannot create the {subdir} dir: {e}");
        std::process::exit(1);
    }

    // Atomic writes: a temp file then a rename, so a reader never sees
    // a half-written file.
    if let Err(e) = atomic_write(&version_path, summary) {
        eprintln!("Error: cannot write {}: {e}", version_path.display());
        std::process::exit(1);
    }
    if write_latest {
        if let Err(e) = atomic_write(&latest_path, summary) {
            eprintln!("Error: cannot write {}: {e}", latest_path.display());
            std::process::exit(1);
        }
    }

    let mut output = serde_json::json!({
        "text": format!(
            "Wrote the {} summary ({} bytes) to {}.",
            target.mode,
            summary_bytes,
            version_path.display()
        ),
        "version_path": version_path.to_string_lossy(),
        "version": target.version,
        "bytes": summary_bytes
    });
    if write_latest {
        if let Some(obj) = output.as_object_mut() {
            obj.insert(
                "latest_path".to_string(),
                serde_json::json!(latest_path.to_string_lossy()),
            );
        }
    }

    println!("{}", serde_json::to_string(&output).unwrap());
}

/// Read the `handoff-target.json` sidecar from the agent session dir.
fn read_sidecar(session_dir: &Path) -> HandoffTarget {
    let sidecar = session_dir.join("handoff-target.json");
    let raw = match fs::read_to_string(&sidecar) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Error: cannot read sidecar {}: {e}", sidecar.display());
            std::process::exit(1);
        }
    };
    let v: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Error: sidecar is not JSON: {e}");
            std::process::exit(1);
        }
    };
    let target_dir = v
        .get("target_dir")
        .and_then(|t| t.as_str())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            eprintln!("Error: sidecar is missing target_dir");
            std::process::exit(1);
        });
    let version = v
        .get("version")
        .and_then(|n| n.as_u64())
        .unwrap_or_else(|| {
            eprintln!("Error: sidecar is missing version");
            std::process::exit(1);
        });
    // Absent: the handoff mode. The compact driver sets `branch` for
    // the branch compact agent.
    let mode = v
        .get("mode")
        .and_then(|m| m.as_str())
        .unwrap_or("handoff")
        .to_string();
    HandoffTarget {
        target_dir,
        version,
        mode,
    }
}

/// Write to a temp file in the same dir, then rename over the target.
fn atomic_write(path: &Path, content: &str) -> Result<(), String> {
    let dir = path
        .parent()
        .ok_or_else(|| "no parent dir".to_string())?;
    let file_name = path
        .file_name()
        .ok_or_else(|| "no file name".to_string())?;
    let tmp_name = format!(".{}.tmp", file_name.to_string_lossy());
    let tmp_path = dir.join(tmp_name);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp_path)
        .map_err(|e| e.to_string())?;
    file.write_all(content.as_bytes()).map_err(|e| e.to_string())?;
    fs::rename(&tmp_path, path).map_err(|e| e.to_string())?;
    Ok(())
}
