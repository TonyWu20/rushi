#![deny(clippy::todo, clippy::unimplemented, clippy::unreachable)]

use clap::Parser;
use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;

/// Edit a file by replacing strings
#[derive(Parser)]
#[command(name = "edit", about = "Edit a file by replacing strings")]
struct Args {}

fn main() {
    let _args = Args::parse();

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

    let file_path = input
        .get("file_path")
        .and_then(|f| f.as_str())
        .unwrap_or("");

    let old_string = input
        .get("old_string")
        .and_then(|o| o.as_str())
        .unwrap_or("");

    let new_string = input
        .get("new_string")
        .and_then(|n| n.as_str())
        .unwrap_or("");

    let replace_all = input
        .get("replace_all")
        .and_then(|r| r.as_bool())
        .unwrap_or(false);

    // Resolve file path
    let full_path = PathBuf::from(file_path);

    // Check if file exists
    if !full_path.exists() {
        eprintln!("Error: file not found: {}.", file_path);
        std::process::exit(1);
    }

    // Read full content
    let content_bytes = match fs::read(&full_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Error: cannot read file: {e}");
            std::process::exit(1);
        }
    };

    // Preserve BOM
    let bom: Option<[u8; 3]> = if content_bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        Some(content_bytes[..3].try_into().unwrap())
    } else {
        None
    };

    // Check for binary file (null byte in the first 8 KB). Runs on the
    // raw bytes: a byte slice never panics at a non-character boundary,
    // unlike a slice of the decoded string (FT-004).
    let check_size = std::cmp::min(content_bytes.len(), 8192);
    if content_bytes[..check_size].contains(&0) {
        eprintln!("Error: {} is not a UTF-8 text file.", file_path);
        std::process::exit(1);
    }

    // Convert to string
    let text = match String::from_utf8(content_bytes) {
        Ok(t) => t,
        Err(_) => {
            eprintln!("Error: {} is not a UTF-8 text file.", file_path);
            std::process::exit(1);
        }
    };

    // Detect line ending style
    let line_ending = if text.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };

    // Check old_string is not empty
    if old_string.is_empty() {
        eprintln!("Error: old_string must not be empty.");
        std::process::exit(1);
    }

    // Check old_string == new_string
    if old_string == new_string {
        eprintln!("Error: old_string and new_string are identical. No change needed.");
        std::process::exit(1);
    }

    // Normalize line endings for matching
    let normalized_text = text.replace(line_ending, "\n");
    let normalized_old = old_string.replace(line_ending, "\n");

    // Count occurrences
    let count = normalized_text.matches(&normalized_old).count();

    if count == 0 {
        eprintln!(
            "Error: old_string not found in file. It may have changed since your last read."
        );
        std::process::exit(1);
    }

    if !replace_all && count > 1 {
        eprintln!(
            "Error: old_string appears {} times. Use a larger old_string with more context, or set replace_all=true.",
            count
        );
        std::process::exit(1);
    }

    // Apply replacement in normalized text
    let normalized_new = if replace_all {
        normalized_text.replace(&normalized_old, new_string)
    } else {
        // Replace only the first occurrence
        if let Some(pos) = normalized_text.find(&normalized_old) {
            let before = &normalized_text[..pos];
            let after = &normalized_text[pos + normalized_old.len()..];
            format!("{}{}{}", before, new_string, after)
        } else {
            normalized_text
        }
    };

    // Convert back to original line endings
    let final_text = if line_ending == "\r\n" {
        normalized_new.replace("\n", "\r\n")
    } else {
        normalized_new
    };

    // Write back with original line endings and BOM
    let mut output = Vec::new();
    if let Some(bom_bytes) = bom {
        output.extend_from_slice(&bom_bytes);
    }
    output.extend_from_slice(final_text.as_bytes());

    if let Err(e) = fs::write(&full_path, &output) {
        eprintln!("Error: cannot write file: {e}");
        std::process::exit(1);
    }

    // Output result
    let output_json = serde_json::json!({
        "text": format!("The file {} has been updated successfully.", file_path),
        "path": file_path,
        "before": old_string,
        "after": new_string,
        "replace_all": replace_all
    });

    println!("{}", serde_json::to_string(&output_json).unwrap());
}
