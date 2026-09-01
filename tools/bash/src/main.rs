#![deny(clippy::todo, clippy::unimplemented, clippy::unreachable)]

use clap::Parser;
use std::io::{self, Read};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Run a shell command in the session working directory
#[derive(Parser)]
#[command(
    name = "bash",
    about = "Run a shell command in the session working directory"
)]
struct Args {
    /// Maximum combined output bytes (shared by stdout and stderr)
    #[arg(long, default_value = "16000")]
    max_output_bytes: usize,

    /// Default command timeout in seconds
    #[arg(long, default_value = "60")]
    timeout_default: u64,

    /// Hard cap on the command timeout in seconds
    #[arg(long, default_value = "300")]
    timeout_max: u64,
}

fn fail(msg: &str) -> ! {
    eprintln!("Error: {msg}");
    std::process::exit(1);
}

fn main() {
    let args = Args::parse();

    // Read JSON input from stdin
    let mut input_str = String::new();
    if let Err(e) = io::stdin().read_to_string(&mut input_str) {
        fail(&format!("failed to read input: {e}"));
    }
    let input: serde_json::Value = match serde_json::from_str(&input_str) {
        Ok(v) => v,
        Err(e) => fail(&format!("invalid JSON input: {e}")),
    };

    let command = input
        .get("command")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();

    // timeout_secs: optional. Falls back to the default when absent.
    let timeout_secs: u64 = match input.get("timeout_secs") {
        None => args.timeout_default,
        Some(v) => match v.as_i64() {
            Some(i) if i >= 0 => i as u64,
            Some(i) => fail(&format!("timeout_secs must be >= 1, got {i}.")),
            None => fail("timeout_secs must be an integer."),
        },
    };

    // Validate the effective timeout against the bounds.
    if timeout_secs < 1 {
        fail(&format!("timeout_secs must be >= 1, got {timeout_secs}."));
    }
    if timeout_secs > args.timeout_max {
        fail(&format!(
            "timeout_secs {timeout_secs} exceeds max {}.",
            args.timeout_max
        ));
    }

    // Spawn the command in its own process group so the whole group can be
    // killed on timeout. The working directory is inherited from the harness.
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(&command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    unsafe {
        cmd.pre_exec(|| {
            // Make the child the leader of a new process group (pgid = pid).
            if libc::setpgid(0, 0) != 0 {
                std::process::abort();
            }
            Ok(())
        });
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => fail(&format!("cannot spawn command: {e}")),
    };
    let pid = child.id() as i32;

    // Capture stdout and stderr concurrently.
    let stdout_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let stderr_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let out_done = Arc::new(AtomicBool::new(false));
    let err_done = Arc::new(AtomicBool::new(false));

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();

    let out_thread = {
        let buf = Arc::clone(&stdout_buf);
        let done = Arc::clone(&out_done);
        let pipe = stdout_pipe;
        thread::spawn(move || {
            if let Some(mut p) = pipe {
                let mut b = Vec::new();
                let _ = p.read_to_end(&mut b);
                *buf.lock().unwrap() = b;
            }
            done.store(true, Ordering::Relaxed);
        })
    };
    let err_thread = {
        let buf = Arc::clone(&stderr_buf);
        let done = Arc::clone(&err_done);
        let pipe = stderr_pipe;
        thread::spawn(move || {
            if let Some(mut p) = pipe {
                let mut b = Vec::new();
                let _ = p.read_to_end(&mut b);
                *buf.lock().unwrap() = b;
            }
            done.store(true, Ordering::Relaxed);
        })
    };

    // Wait for the command with a deadline.
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let mut timed_out = false;
    let mut status: Option<std::process::ExitStatus> = None;
    loop {
        match child.try_wait() {
            Ok(Some(s)) => {
                status = Some(s);
                break;
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    timed_out = true;
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => fail(&format!("failed to wait for command: {e}")),
        }
    }

    let exit_code: i32 = if timed_out {
        // Kill the process group. SIGTERM first, SIGKILL for survivors.
        unsafe {
            libc::kill(-pid, libc::SIGTERM);
        }
        thread::sleep(Duration::from_secs(2));
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
        let _ = child.wait();
        143
    } else {
        match status {
            Some(s) => {
                if let Some(c) = s.code() {
                    c
                } else {
                    #[cfg(unix)]
                    {
                        let sig = s.signal().unwrap_or(15);
                        128 + sig
                    }
                    #[cfg(not(unix))]
                    143
                }
            }
            None => 143,
        }
    };

    // Drain the captured pipes. Bound the wait so a lingering inherited pipe
    // writer cannot hang the tool.
    let drain_deadline = Instant::now() + Duration::from_secs(3);
    while (!out_done.load(Ordering::Relaxed) || !err_done.load(Ordering::Relaxed))
        && Instant::now() < drain_deadline
    {
        thread::sleep(Duration::from_millis(10));
    }
    let _ = out_thread.join();
    let _ = err_thread.join();

    let raw_stdout: Vec<u8> = stdout_buf.lock().unwrap().clone();
    let raw_stderr: Vec<u8> = stderr_buf.lock().unwrap().clone();

    // Cap the combined output. Keep the tail. The cap is one shared limit.
    let cap = args.max_output_bytes;
    let total = raw_stdout.len() + raw_stderr.len();
    let (stdout_str, stderr_str, truncated) = if total <= cap {
        (
            String::from_utf8_lossy(&raw_stdout).into_owned(),
            String::from_utf8_lossy(&raw_stderr).into_owned(),
            false,
        )
    } else {
        // Split the cap in proportion to the stream sizes. Keep each tail.
        let so_share = ((raw_stdout.len() as u128) * (cap as u128) / (total as u128)) as usize;
        let se_share = cap - so_share;
        let so_start = raw_stdout.len().saturating_sub(so_share);
        let se_start = raw_stderr.len().saturating_sub(se_share);
        (
            String::from_utf8_lossy(&raw_stdout[so_start..]).into_owned(),
            String::from_utf8_lossy(&raw_stderr[se_start..]).into_owned(),
            true,
        )
    };

    // Build the model-facing text.
    let mut text = format!("$ {command}");
    if truncated {
        text.push_str(&format!(
            "\n[output truncated: showing last {cap} of {total} bytes]"
        ));
    }
    text.push('\n');
    text.push_str(&stdout_str);
    if !stderr_str.is_empty() {
        text.push_str(&format!("\n[stderr]\n{stderr_str}"));
    }
    text.push_str(&format!("\n(exit code: {exit_code})"));
    if timed_out {
        text.push_str(&format!("\n(timed out after {timeout_secs}s)"));
    }

    // Emit one JSON object on stdout.
    let output = serde_json::json!({
        "text": text,
        "exit_code": exit_code,
        "stdout": stdout_str,
        "stderr": stderr_str,
        "timed_out": timed_out,
        "truncated": truncated
    });
    println!("{}", serde_json::to_string(&output).unwrap());
}
