#![deny(clippy::todo, clippy::unimplemented, clippy::unreachable)]

use clap::Parser;
use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;

/// Read a file and output its contents
#[derive(Parser)]
#[command(name = "read", about = "Read a file and output its contents")]
struct Args {
    /// Maximum number of lines to read
    #[arg(long, default_value = "2000")]
    read_limit: usize,

    /// Maximum line length in characters
    #[arg(long, default_value = "2000")]
    read_max_line_length: usize,

    /// Maximum output bytes
    #[arg(long, default_value = "51200")]
    read_max_bytes: usize,

    /// Minimum file size to stream
    #[arg(long, default_value = "10485760")]
    read_stream_min_size: usize,
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

    let file_path = input
        .get("file_path")
        .and_then(|f| f.as_str())
        .unwrap_or("");

    let offset: usize = input.get("offset").and_then(|o| o.as_u64()).unwrap_or(1) as usize;

    let mut limit: usize = input
        .get("limit")
        .and_then(|l| l.as_u64())
        .unwrap_or(args.read_limit as u64) as usize;

    // Validate offset
    if offset < 1 {
        eprintln!("Error: offset must be >= 1.");
        std::process::exit(1);
    }

    // Validate and cap limit
    if limit < 1 {
        eprintln!("Error: limit must be >= 1.");
        std::process::exit(1);
    }
    if limit > args.read_limit {
        limit = args.read_limit;
    }

    // Resolve file path
    let full_path = PathBuf::from(file_path);

    // Check file exists
    if !full_path.exists() {
        eprintln!("Error: file not found: {}.", file_path);
        std::process::exit(1);
    }

    // Read file content
    let mut content = Vec::new();
    if let Err(e) = fs::File::open(&full_path).and_then(|mut f| f.read_to_end(&mut content)) {
        eprintln!("Error: cannot read file: {e}.");
        std::process::exit(1);
    }

    // Check for binary file (null bytes in first 8KB)
    let check_size = std::cmp::min(content.len(), 8192);
    if content[..check_size].contains(&0u8) {
        eprintln!("Error: {} is not a UTF-8 text file.", file_path);
        std::process::exit(1);
    }

    // Convert to string
    let text = match String::from_utf8(content) {
        Ok(t) => t,
        Err(_) => {
            eprintln!("Error: {} is not a UTF-8 text file.", file_path);
            std::process::exit(1);
        }
    };

    // Count total lines
    let total_lines = text.lines().count();

    // Check offset past EOF
    // For empty files, offset 1 is valid (shows "End of file")
    // For non-empty files, offset must be <= total_lines
    if total_lines > 0 && offset > total_lines {
        eprintln!(
            "Error: offset {} is past end of file. File has {} lines.",
            offset, total_lines
        );
        std::process::exit(1);
    }

    // Scan line by line, buffer only lines in [offset, offset + limit)
    let mut output_lines: Vec<String> = Vec::new();
    let mut line_num = 0;
    let mut output_bytes = 0;
    let showing_start = offset == 1;
    let mut showing_end = false;

    for line in text.lines() {
        line_num += 1;

        if line_num < offset {
            continue;
        }

        if line_num > offset + limit - 1 {
            showing_end = true;
            break;
        }

        // Cap line length
        let display_line = if line.len() > args.read_max_line_length {
            let truncated: String = line.chars().take(args.read_max_line_length).collect();
            format!(
                "{} ... (line truncated to {} chars)",
                truncated, args.read_max_line_length
            )
        } else {
            line.to_string()
        };

        let line_output = format!("{}: {}\n", line_num, display_line);
        output_bytes += line_output.len();

        if output_bytes > args.read_max_bytes {
            showing_end = true;
            break;
        }

        output_lines.push(line_output);
    }

    // Build output
    let content_str = if total_lines == 0 {
        "(End of file - total 0 lines)".to_string()
    } else if showing_start && showing_end {
        let prefix = if offset > 1 {
            format!("({} lines omitted)\n", offset - 1)
        } else {
            String::new()
        };
        format!(
            "{}{}\n({} lines omitted)",
            prefix,
            output_lines.join(""),
            total_lines - (offset + output_lines.len() - 1)
        )
    } else if showing_start {
        let prefix = if offset > 1 {
            format!("({} lines omitted)\n", offset - 1)
        } else {
            String::new()
        };
        format!("{}{}", prefix, output_lines.join(""))
    } else if showing_end {
        format!(
            "{}\n({} lines omitted)",
            output_lines.join(""),
            total_lines - (offset + output_lines.len() - 1)
        )
    } else {
        output_lines.join("")
    };

    // Build JSON output. The text field carries the model-facing content.
    let output = serde_json::json!({
        "text": content_str,
        "path": file_path,
        "type": "file",
        "total_lines": total_lines
    });

    println!("{}", serde_json::to_string(&output).unwrap());
}
