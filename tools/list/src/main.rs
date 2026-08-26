#![deny(clippy::todo, clippy::unimplemented, clippy::unreachable)]

use clap::Parser;
use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;

/// List directory contents
#[derive(Parser)]
#[command(name = "list", about = "List directory contents")]
struct Args {
    /// Maximum number of entries to return
    #[arg(long, default_value = "500")]
    list_limit: usize,
}

fn main() {
    let args = Args::parse();

    // Read input from stdin
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

    let dir_path = input
        .get("path")
        .and_then(|p| p.as_str())
        .unwrap_or(".");

    let full_path = PathBuf::from(dir_path);

    // Check directory exists
    if !full_path.exists() {
        eprintln!("Error: directory not found: {}.", dir_path);
        std::process::exit(1);
    }

    if !full_path.is_dir() {
        eprintln!("Error: not a directory: {}.", dir_path);
        std::process::exit(1);
    }

    // Read directory entries
    let entries = match fs::read_dir(&full_path) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("Error: cannot read directory: {e}.");
            std::process::exit(1);
        }
    };

    let mut items: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let is_dir = entry.path().is_dir();
        items.push(format!("{}{}", name, if is_dir { "/" } else { "" }));
        if items.len() >= args.list_limit {
            break;
        }
    }

    // Sort entries (directories first, then alphabetically)
    items.sort();

    if items.is_empty() {
        items.push("(empty directory)".to_string());
    }

    let content = items.join("\n");

    // Build JSON output
    let output = serde_json::json!({
        "text": content,
        "path": dir_path,
        "type": "directory",
        "count": items.len()
    });

    println!("{}", serde_json::to_string(&output).unwrap());
}
