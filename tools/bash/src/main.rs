#![deny(clippy::todo, clippy::unimplemented, clippy::unreachable)]

use clap::Parser;
use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

    /// Opt-in: scrub credential-shaped environment variables (names
    /// containing KEY, PASSWORD, SECRET, or TOKEN, case-insensitive) from
    /// the child environment. Off by default (open-core philosophy); enable
    /// by adding `--env-scrub` to the tool's `args` in tool.toml.
    #[arg(long)]
    env_scrub: bool,

    /// Pin the shell executable (pi-style `shellPath` operator override).
    /// Must name an executable file; when absent the normal resolution
    /// (`/bin/bash` -> PATH `bash` -> `sh`) applies.
    #[arg(long)]
    shell_path: Option<String>,
}

fn fail(msg: &str) -> ! {
    eprintln!("Error: {msg}");
    std::process::exit(1);
}

// Post-exit pipe-drain parameters (pi `waitForChildProcess` semantics):
// quiet pipes release after DRAIN_IDLE_MS without data, an active writer
// keeps the drain alive, and DRAIN_MAX_MS is the hard cap so a writer that
// escaped the process group cannot hang the tool.
const DRAIN_IDLE_MS: u64 = 100;
const DRAIN_MAX_MS: u64 = 3_000;

/// Resolve the command shell: prefer bash, fall back to POSIX sh.
///
/// Order: `--shell-path` pin (when given) -> `/bin/bash` -> first
/// executable `bash` on `PATH` -> `sh`. Mirrors pi's `getShellConfig`
/// (Unix branches). The fallback keeps the tool working on minimal
/// systems without bash.
fn resolve_shell(pin: Option<&str>) -> String {
    if let Some(pin) = pin {
        let p = Path::new(pin);
        if is_executable_file(p) {
            return pin.to_string();
        }
        fail(&format!("shell path {pin} is not an executable file."));
    }
    const FIXED: &str = "/bin/bash";
    let fixed = Path::new(FIXED);
    if is_executable_file(fixed) {
        return FIXED.to_string();
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let cand = dir.join("bash");
            if is_executable_file(&cand) {
                return cand.to_string_lossy().into_owned();
            }
        }
    }
    "sh".to_string()
}

fn is_executable_file(p: &Path) -> bool {
    match std::fs::metadata(p) {
        Ok(md) => md.is_file() && md.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

/// One credential-shaped env name (dsh's SENSITIVE_ENV_PATTERN, rendered as
/// case-insensitive substring match; Unix env names are case-sensitive, so
/// this is the conservative superset).
fn is_sensitive_env_name(name: &std::ffi::OsStr) -> bool {
    let lower = name.to_string_lossy().to_ascii_lowercase();
    lower.contains("key")
        || lower.contains("password")
        || lower.contains("secret")
        || lower.contains("token")
}

/// Spawn a detached reader for one pipe. Reads chunks and flushes them to
/// the shared buffer incrementally, so any data read so far is captured
/// even if the pipe never reaches EOF (a lingering background writer that
/// inherits the pipe keeps it open). A storage ceiling bounds memory: past
/// it the pipe is still drained (read + discard) so writers never block on
/// a full pipe, but no more bytes are stored. The activity clock is
/// updated on every read so the idle-based drain can tell an active writer
/// from a quiet one. The thread is never joined: it either exits on EOF or
/// is killed when the tool process exits.
fn spawn_pipe_reader(
    pipe: impl Read + Send + 'static,
    buf: Arc<Mutex<Vec<u8>>>,
    done: Arc<AtomicBool>,
    activity: Arc<AtomicU64>,
    t0: Instant,
    store_cap: usize,
) {
    thread::spawn(move || {
        let mut pipe = pipe;
        let mut chunk = [0u8; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) | Err(_) => {
                    done.store(true, Ordering::Relaxed);
                    break;
                }
                Ok(n) => {
                    {
                        let mut g = buf.lock().unwrap();
                        if g.len() < store_cap {
                            let take = n.min(store_cap - g.len());
                            g.extend_from_slice(&chunk[..take]);
                        }
                    }
                    activity.store(t0.elapsed().as_millis() as u64, Ordering::Relaxed);
                }
            }
        }
    });
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

    // Resolve the command shell: --shell-path pin -> /bin/bash -> PATH
    // bash -> sh.
    let shell = resolve_shell(args.shell_path.as_deref());

    // Spawn the command in its own process group so the whole group can be
    // killed on timeout. The working directory is inherited from the harness.
    let mut cmd = Command::new(&shell);
    cmd.arg("-c")
        .arg(&command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // POSIX shells auto-source $ENV (and bash $BASH_ENV) for non-interactive
    // -c invocations. That is a silent code-execution channel: a hostile or
    // leaking parent env could inject a command into every tool call. Close it.
    cmd.env_remove("ENV");
    cmd.env_remove("BASH_ENV");
    if args.env_scrub {
        let names: Vec<String> = std::env::vars_os()
            .filter_map(|(k, _)| {
                is_sensitive_env_name(&k).then(|| k.to_string_lossy().into_owned())
            })
            .collect();
        for name in names {
            cmd.env_remove(&name);
        }
    }
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

    let t0 = Instant::now();

    // Per-stream storage ceiling: keep enough to make the tail-truncation
    // meaningful, bound memory for pathological outputs. Scales with the
    // model-facing cap but never below 1 MiB.
    let store_cap = args.max_output_bytes.saturating_mul(256).max(1 << 20);

    // Capture stdout and stderr concurrently.
    let stdout_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let stderr_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let out_done = Arc::new(AtomicBool::new(false));
    let err_done = Arc::new(AtomicBool::new(false));
    let out_act = Arc::new(AtomicU64::new(0));
    let err_act = Arc::new(AtomicU64::new(0));

    if let Some(pipe) = child.stdout.take() {
        spawn_pipe_reader(
            pipe,
            Arc::clone(&stdout_buf),
            Arc::clone(&out_done),
            Arc::clone(&out_act),
            t0,
            store_cap,
        );
    } else {
        out_done.store(true, Ordering::Relaxed);
    }
    if let Some(pipe) = child.stderr.take() {
        spawn_pipe_reader(
            pipe,
            Arc::clone(&stderr_buf),
            Arc::clone(&err_done),
            Arc::clone(&err_act),
            t0,
            store_cap,
        );
    } else {
        err_done.store(true, Ordering::Relaxed);
    }

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

    // Idle-based pipe drain. Quiet pipes release after DRAIN_IDLE_MS
    // without new data; an active writer keeps the drain alive (its bytes
    // are captured incrementally into the shared buffers); and the hard
    // DRAIN_MAX_MS cap means a writer that escaped the process group
    // cannot hang the tool.
    let drain_t0 = Instant::now();
    loop {
        let out = out_done.load(Ordering::Relaxed);
        let err = err_done.load(Ordering::Relaxed);
        if out && err {
            break;
        }
        let now = t0.elapsed().as_millis() as u64;
        let out_quiet =
            out || now.saturating_sub(out_act.load(Ordering::Relaxed)) >= DRAIN_IDLE_MS;
        let err_quiet =
            err || now.saturating_sub(err_act.load(Ordering::Relaxed)) >= DRAIN_IDLE_MS;
        if out_quiet && err_quiet {
            break;
        }
        if drain_t0.elapsed().as_millis() as u64 >= DRAIN_MAX_MS {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }

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
        "truncated": truncated,
        "shell": shell
    });
    println!("{}", serde_json::to_string(&output).unwrap());
}
