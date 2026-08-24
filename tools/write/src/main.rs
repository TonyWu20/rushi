use clap::Parser;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::PathBuf;

/// Write content to a file
#[derive(Parser)]
#[command(name = "write", about = "Write content to a file")]
struct Args {
    /// Maximum content bytes
    #[arg(long, default_value = "1048576")]
    write_max_bytes: usize,
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

    let content = input
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or("");

    // Check content size
    let content_bytes = content.len();
    if content_bytes > args.write_max_bytes {
        eprintln!(
            "Error: content is {} bytes, exceeds max {} bytes.",
            content_bytes, args.write_max_bytes
        );
        std::process::exit(1);
    }

    // Resolve file path
    let full_path = PathBuf::from(file_path);

    // Check if path is a directory
    if full_path.is_dir() {
        eprintln!("Error: {} is a directory.", file_path);
        std::process::exit(1);
    }

    // Check if parent path components are files
    if let Some(parent) = full_path.parent() {
        if !parent.as_os_str().is_empty() && parent.exists() {
            for component in parent.components() {
                let comp_path = parent.join(component);
                if comp_path.is_file() {
                    eprintln!(
                        "Error: cannot create directories: {} is a file.",
                        component.as_os_str().to_string_lossy()
                    );
                    std::process::exit(1);
                }
            }
        }
        // Create parent directories
        if let Err(e) = fs::create_dir_all(parent) {
            eprintln!("Error: cannot create directories: {e}");
            std::process::exit(1);
        }
    }

    // Detect if file existed before write
    let existed = full_path.exists();
    let operation = if existed { "update" } else { "create" };

    // Write content
    let mut file = match OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&full_path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Error: cannot write file: {e}");
            std::process::exit(1);
        }
    };

    if let Err(e) = file.write_all(content.as_bytes()) {
        eprintln!("Error: cannot write to file: {e}");
        std::process::exit(1);
    }

    // Output result
    let output = serde_json::json!({
        "text": format!(
            "Successfully wrote {} bytes to {}.",
            content_bytes, file_path
        ),
        "path": file_path,
        "operation": operation,
        "bytes": content_bytes
    });

    println!("{}", serde_json::to_string(&output).unwrap());
}
