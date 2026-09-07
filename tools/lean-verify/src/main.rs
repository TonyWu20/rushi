//! `lean-verify` — the formal-verification gate tool of the rushi harness.
//!
//! Port of the lean-verify agent skill into a harness tool, per
//! docs/skill-remapped-to-os-apps.md: the multi-step procedure of the
//! skill is this one short-lived, self-documenting command on the
//! agent-visible path. Its interface is `--help` (P2 self-doc); the
//! `tool.toml` `description` is the catalog payload. No SKILL.md.
//!
//! The tool implements the spec-driven Lean 4 workflow (Varun Prant,
//! "Prove It. Don't Guess It."): the Lean kernel re-checks every
//! proof step independently; a clean `lake build` with zero
//! `sorry`/`admit` warnings is the guarantee; a passing
//! differential-random-test run is the regression gate.
//!
//! Contract (same as the other `tools/` binaries): read one JSON
//! object on stdin, emit one JSON object on stdout (the `text` field
//! is what the model sees). Exit 0 when a command-level result was
//! produced (the gate status lives in the JSON: `ok`, `clean`,
//! `pass`). Non-zero exit only for tool-level failures: invalid
//! arguments, missing environment, or a spawn failure.
//!
//! Environment: `lake`/`lean`/`z3` must be on PATH for the
//! lake-bound ops, and `charon` + `aeneas` for `translate` (the
//! flake's devShells provide them; see flake.nix). When a required
//! binary is absent but `nix` and a repo flake are reachable, the
//! tool falls back to the matching devShell:
//! `nix develop --impure .#lean` for the lake ops,
//! `nix develop --impure .#aeneas` for translate.

#![deny(clippy::todo, clippy::unimplemented, clippy::unreachable)]

use std::io::Read;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Tool-level failure: stderr diagnostic, non-zero exit.
fn fail(msg: &str) -> ! {
    eprintln!("Error: {msg}");
    std::process::exit(1);
}

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    if argv.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return;
    }

    let args = parse_args(read_stdin_json());
    let op = match args.op.clone() {
        Some(o) => o,
        None => fail("missing required field: op. Use one of 'init', 'build', 'drt', 'check-inputs', 'translate'. Run `lean-verify --help`."),
    };
    match op.as_str() {
        "init" => op_init(&args),
        "build" => op_build(&args),
        "drt" => op_drt(&args),
        "check-inputs" => op_check_inputs(&args),
        "translate" => op_translate(&args),
        _ => fail(&format!(
            "unknown op '{op}'. Use one of 'init', 'build', 'drt', 'check-inputs', 'translate'. Run `lean-verify --help`."
        )),
    }
}

// ─── Arguments ────────────────────────────────────────────────────

struct Args {
    op: Option<String>,
    name: Option<String>,
    dir: Option<String>,
    cache: Option<bool>,
    model: Option<String>,
    prod: Option<String>,
    n: Option<u64>,
    seed: Option<u64>,
    bound: Option<u64>,
    input_gen: Option<String>,
    stop_on_mismatch: Option<bool>,
    max_mismatches: Option<u64>,
    per_input_timeout_s: Option<u64>,
    smoke: Option<bool>,
    resume: Option<bool>,
    progress: Option<bool>,
    max_rejections: Option<u64>,
    stop_on_reject: Option<bool>,
}

fn read_stdin_json() -> serde_json::Value {
    let mut buf = String::new();
    if std::io::stdin().read_to_string(&mut buf).is_err() || buf.trim().is_empty() {
        return serde_json::json!({});
    }
    serde_json::from_str(&buf).unwrap_or(serde_json::json!({}))
}

fn str_field(v: &serde_json::Value, k: &str) -> Option<String> {
    v.get(k).and_then(|x| x.as_str()).map(|s| s.to_string())
}

fn int_field(v: &serde_json::Value, k: &str) -> Option<u64> {
    v.get(k).and_then(|x| x.as_u64())
}

fn bool_field(v: &serde_json::Value, k: &str) -> Option<bool> {
    v.get(k).and_then(|x| x.as_bool())
}

fn parse_args(v: serde_json::Value) -> Args {
    Args {
        op: str_field(&v, "op"),
        name: str_field(&v, "name"),
        dir: str_field(&v, "dir"),
        cache: bool_field(&v, "cache"),
        model: str_field(&v, "model"),
        prod: str_field(&v, "prod"),
        n: int_field(&v, "n"),
        seed: int_field(&v, "seed"),
        bound: int_field(&v, "bound"),
        input_gen: str_field(&v, "input_gen"),
        stop_on_mismatch: bool_field(&v, "stop_on_mismatch"),
        max_mismatches: int_field(&v, "max_mismatches"),
        per_input_timeout_s: int_field(&v, "per_input_timeout_s"),
        smoke: bool_field(&v, "smoke"),
        resume: bool_field(&v, "resume"),
        progress: bool_field(&v, "progress"),
        max_rejections: int_field(&v, "max_rejections"),
        stop_on_reject: bool_field(&v, "stop_on_reject"),
    }
}

// ─── Self-doc ──────────────────────────────────────────────────────

const HELP: &str = r#"lean-verify — Lean 4 formal-verification gate (spec-driven workflow)

Verify code by writing a formal specification in Lean 4 and proving the
implementation satisfies it. The Lean kernel is a small trusted core:
it re-checks every proof step independently. An incorrect proof is
rejected at build time. A passing build is the guarantee for every
input, not a sample.

Workflow (spec-driven loop; mirrors docs/lean-driven-development.md §3)
  1. Specify    Write or extend the Lean spec: one theorem per invariant,
                frozen before the proof.
  2. Prove      Run op=build; zero-sorry is the kernel gate.
  3. Implement  Follow the Lean spec to implement the Rust code. The
                proven spec is the contract: write or change the Rust so
                it satisfies every proven invariant — the code implements
                the spec, not the other way around. If they disagree, fix
                the code; if the spec is wrong, fix the spec first,
                re-freeze, and re-prove (never weaken a theorem to make a
                build pass).
  4. Regression-gate  Run op=drt against the real Rust binary and fix
                until every input matches. Release only after all pass.

Operations (one JSON object on stdin, one JSON object on stdout):

  init    Create a new Lean project in an empty directory.
          {"op":"init","name":"myproj"}
          Runs `lake init <name>` and reports the toolchain pin and
          layout. Optionally "cache":true runs `lake exe cache get`
          (elan environments only; under Nix the prebuilt oleans come
          from the Nix store via LEAN_PATH, so caching is a no-op).

  build   The kernel gate.
          {"op":"build","dir":"myproj"}
          Runs `lake build` over every target declared in
          lakefile.toml (the [[lean_lib]]/[[lean_exe]] names), so an
          unimported library cannot slip past the gate. Counts open
          obligations: a clean guarantee requires zero "declaration
          uses 'sorry'" / "admit" warnings. JSON fields: targets,
          clean, sorry_count, admit_count, lake_exit. GATE GREEN
          only when clean=true. Iterate at most five times per
          error class; if the budget exhausts, write a triage note.
          Do not silently weaken a theorem to make the build pass.

  drt     Differential random testing (the regression gate).
          {"op":"drt","dir":"myproj","model":"...","prod":"...","n":100000}
          Runs N random inputs through both the Lean model executable
          and the production executable and compares their outputs.
          Both commands receive each input as $1 and as the
          DRT_INPUT environment variable; outputs are compared after
          stripping trailing whitespace. A mismatch on an
          exact/abstraction-classified definition is a real bug; fix
          the model or the source. Run after every model change.
          Long runs and resumptions:
          - "smoke":true is the quick tier (n=2000, ~15 s): the
            one-liner quick check, no judgment call on n. The full
            tier (default n=100000) still gates the release.
          - A progress/heartbeat file <dir>/.drt-progress.json is
            written every ~10 s or 100 inputs (pid, next index,
            rate, eta, updated_at). Poll it to see whether a long
            drt is still alive — no ps needed; it is deleted on a
            clean run. If `updated_at` lags far behind, the run is
            stuck: kill it and re-run with "resume":true.
          - A stop on mismatch/timeout keeps the file as a
            checkpoint. After the fix, re-run the SAME call with
            "resume":true: the run continues from the first failed
            index (parameters must match the checkpoint and
            input_gen must be deterministic). The result JSON
            reports progress_file, checkpoint (kept=true means
            resumable), and resumed_from.

   check-inputs Preflight the DRT input protocol without the full
          comparison.
          {"op":"check-inputs","model":"...","prod":"...","input_gen":"...","n":2000}
          Runs each generated line through the model executable (and
          the production executable when "prod" is given) and
          reports the first line a side rejects. A side rejects an
          input when it exits non-zero or times out; an accepted
          input exits 0. Default n=2000 (~10 s); "n":100000 checks
          every line the full drt would consume. "stop_on_reject"
          (default true) stops at the first rejection;
          "max_rejections" bounds collection otherwise. progress /
          resume work as for drt. Run after every generator or
          protocol change — a broken generator costs seconds here
          instead of a full drt. Note: an executable that exits 0
          even on parse failure is not caught here; op=drt catches
          that through the output comparison.

  translate Translate a Rust crate to a Lean model (the charon +
          aeneas pipeline, borrowed from AeneasVerif/aeneas).
          {"op":"translate","dir":"mycrate"}
          Runs `charon cargo --preset=aeneas` (Rust MIR -> LLBC) and
          `aeneas -backend lean <crate>.llbc` (LLBC -> pure Lean) in
          the cargo crate directory, then reports the LLBC, the
          generated .lean file(s), and how many `axiom`s aeneas
          emitted. A translation is a model, not a proof: an axiom
          asserts nothing and is not a guarantee. Next: state your
          specification as theorems over the generated definitions,
          run op=build (the zero-sorry kernel gate), and
          regression-gate against the real Rust binary with op=drt.
          Requires a cargo crate with a pure core; charon skips the
          constructs it cannot model.

Environment
  `lake` (and `z3` for linarith) must be on PATH for the lake-bound
  ops; `charon` and `aeneas` for translate. The flake's dev shells
  provide them: `nix develop .#lean` (lake + lean + z3 + mathlib) or
  `nix develop .#aeneas` (charon + aeneas + the Lean stack). When a
  required binary is missing but `nix` is reachable with a repo
  flake, the operation falls back to the matching devShell
  (`nix develop --impure .#lean` or `.#aeneas`).

Rules
  - The specification is the source of truth. Do not weaken it to make
    a proof pass. Define the spec before writing code.
  - Validate the spec before proving: `#eval` it on worked examples
    (a test module or the REPL). A definition can type-check but
    compute the wrong value; the eval results are smoke tests.
  - No `sorry`, no `admit`, no `sorry!` in a checked-in proof.
  - Run `build` after every proof edit; a clean build with zero
    open obligations is the acceptance gate.
  - Classify every model definition against the source (exact,
    abstraction, approximation, mismatch) before running `drt`. A
    mismatch classification blocks DRT: fix the model or the source
    first.
  - Run `drt` after every model change; release only after all
    inputs pass. A passing DRT run is the regression gate. The
     quick tier is "smoke":true (n=2000); the full tier (default
     n=100000) gates the release. Long runs: poll the progress
     file, resume a stopped run with "resume":true.
   - Run `check-inputs` after every generator or protocol change,
     before a full `drt`: it verifies the generator's lines are
     well-formed for the DRT protocol in ~10 s.

Exit codes
  0  a command-level result was produced (see the JSON: ok/clean/pass)
  1  tool-level failure (invalid arguments, missing environment,
     spawn failure); diagnostic on stderr
"#;

fn print_help() {
    print!("{HELP}");
}

// ─── Process execution ────────────────────────────────────────────

struct RunResult {
    exit: i32,
    timed_out: bool,
    stdout: String,
    stderr: String,
}

/// Run `sh -c <cmd>` in `cwd` with a deadline. The command runs in
/// its own process group; on timeout the group gets SIGTERM, then
/// SIGKILL after 2 s. A timeout is a result (exit 143), not an
/// error. Spawn failure is an Err (tool-level).
fn run_sh(cmd: &str, cwd: &Path, timeout_s: u64) -> Result<RunResult, String> {
    let mut process = Command::new("sh");
    process
        .arg("-c")
        .arg(cmd)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    unsafe {
        process.pre_exec(|| {
            // Make the child the leader of a new process group
            // (pgid = pid) so the timeout group-kill reaches it.
            if libc::setpgid(0, 0) != 0 {
                std::process::abort();
            }
            Ok(())
        });
    }
    let mut child = match process.spawn() {
        Ok(c) => c,
        Err(e) => return Err(format!("cannot spawn `sh -c`: {e}")),
    };
    let pid = child.id() as i32;

    let stdout_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let stderr_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let out_done = Arc::new(AtomicBool::new(false));
    let err_done = Arc::new(AtomicBool::new(false));

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();

    let out_thread = {
        let buf = Arc::clone(&stdout_buf);
        let done = Arc::clone(&out_done);
        thread::spawn(move || {
            if let Some(mut p) = stdout_pipe {
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
        thread::spawn(move || {
            if let Some(mut p) = stderr_pipe {
                let mut b = Vec::new();
                let _ = p.read_to_end(&mut b);
                *buf.lock().unwrap() = b;
            }
            done.store(true, Ordering::Relaxed);
        })
    };

    let deadline = Instant::now() + Duration::from_secs(timeout_s);
    let mut timed_out = false;
    let mut status = None;
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
            Err(e) => return Err(format!("failed to wait for command: {e}")),
        }
    }

    let exit_code: i32 = if timed_out {
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
            Some(s) => s
                .code()
                .unwrap_or_else(|| 128 + s.signal().unwrap_or(15)),
            None => 143,
        }
    };

    // Drain the captured pipes. Bound the wait so a lingering pipe
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

    Ok(RunResult {
        exit: exit_code,
        timed_out,
        stdout: String::from_utf8_lossy(&raw_stdout).into_owned(),
        stderr: String::from_utf8_lossy(&raw_stderr).into_owned(),
    })
}

// ─── Tool runner (PATH or nix devShell fallback) ─────────────────

/// Which flake devShell provides the toolchain for an op.
#[derive(Clone, Copy)]
enum Shell {
    /// `.#lean`: lake + lean + z3 + mathlib.
    Lean,
    /// `.#aeneas`: charon + aeneas + the Lean stack (Rust -> Lean).
    Aeneas,
}

fn shell_attr(shell: Shell) -> &'static str {
    match shell {
        Shell::Lean => "lean",
        Shell::Aeneas => "aeneas",
    }
}

enum ToolRunner {
    /// The required binary is already on PATH.
    Direct,
    /// Fall back to a repo flake devShell.
    Nix { root: PathBuf, shell: Shell },
}

fn walk_up_flake(start: &Path) -> Option<PathBuf> {
    // Resolve to an absolute path first: a relative walk can hit the
    // empty path segment and "find" nothing (or worse, the cwd with a
    // null display).
    let abs = if start.is_absolute() {
        start.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(start)
    };
    let mut cur: Option<&Path> = Some(&abs);
    for _ in 0..12 {
        let c = cur?;
        if c.join("flake.nix").is_file() {
            return Some(c.to_path_buf());
        }
        cur = c.parent();
    }
    None
}

/// Find the repo flake root: walk up from `start`; then, since a
/// scratch working directory (an e2e /tmp dir, an outside checkout)
/// may live outside the repo, from the tool's own executable — the
/// cargo build dir sits under the checkout that owns the flake.
fn find_flake_root(start: &Path) -> Option<PathBuf> {
    walk_up_flake(start).or_else(|| {
        std::env::current_exe()
            .ok()
            .and_then(|exe| walk_up_flake(&exe))
    })
}

/// Resolve `p` to an absolute path (the tool's cwd is the session cwd).
fn abs_path(p: &Path) -> Result<PathBuf, String> {
    if p.is_absolute() {
        Ok(p.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|c| c.join(p))
            .map_err(|e| format!("cannot resolve working directory: {e}"))
    }
}
fn path_lookup(bin: &str) -> bool {
    let path = match std::env::var_os("PATH") {
        Some(p) => p,
        None => return false,
    };
    std::env::split_paths(&path).any(|dir| dir.join(bin).is_file())
}

fn resolve_tool_runner(cwd: &Path, bin: &str, shell: Shell) -> Result<ToolRunner, String> {
    if path_lookup(bin) {
        return Ok(ToolRunner::Direct);
    }
    if path_lookup("nix") {
        if let Some(root) = find_flake_root(cwd) {
            return Ok(ToolRunner::Nix { root, shell });
        }
        return Err(format!(
            "{bin} is not on PATH and no flake.nix with a `{attr}` devShell was found above \
             the working directory or the tool binary. Enter the Lean environment via the \
             repo flake (devShells are managed in flake.nix): `nix develop .#{attr}`.",
            attr = shell_attr(shell)
        ));
    }
    Err(format!(
        "{bin} is not on PATH and nix is unavailable. The Lean toolchains are provided by \
         the repo flake (devShells.lean and devShells.aeneas in flake.nix); enter one with \
         `nix develop .#{attr}`.",
        attr = shell_attr(shell)
    ))
}

/// Build the shell command that runs `cmd` in `workdir` under the runner.
fn tool_command(runner: &ToolRunner, workdir: &Path, cmd: &str) -> String {
    match runner {
        ToolRunner::Direct => cmd.to_string(),
        ToolRunner::Nix { root, shell } => {
            let inner = format!("cd {} && {cmd}", sh_quote(&workdir.display().to_string()));
            format!(
                "cd {} && nix develop --impure .#{} --command sh -c {}",
                sh_quote(&root.display().to_string()),
                shell_attr(*shell),
                sh_quote(&inner)
            )
        }
    }
}

/// Quote a string for `sh`. Safe strings pass bare; anything else is
/// single-quoted with `'` escaped as `'\''`.
fn sh_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    if s.bytes().all(|b| matches!(b,
        b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-' | b'/' | b'.' | b':' | b'='))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

// ─── Ops ───────────────────────────────────────────────────────────

const INIT_TIMEOUT_S: u64 = 600;
const BUILD_TIMEOUT_S: u64 = 10800; // 3h; below the manifest backstop
const GEN_TIMEOUT_S: u64 = 300;
const TRANSLATE_CHARON_TIMEOUT_S: u64 = 1800; // charon cargo + MIR extraction
const TRANSLATE_AENEAS_TIMEOUT_S: u64 = 600; // aeneas LLBC -> Lean
const SMOKE_N: u64 = 2000; // quick-check tier ("smoke":true)
const DRT_DEFAULT_N: u64 = 100000;
const CHECK_INPUTS_DEFAULT_N: u64 = 2000;
const PROGRESS_FILE_NAME: &str = ".drt-progress.json";
const PROGRESS_VERSION: u64 = 1; // bump to invalidate stored checkpoints
const PROGRESS_EVERY_INPUTS: u64 = 100;
const PROGRESS_EVERY_SECS: u64 = 10;

fn emit(obj: serde_json::Value) {
    println!("{}", serde_json::to_string(&obj).unwrap_or_default());
}

fn tail_cap(s: &str, cap: usize) -> String {
    if s.len() <= cap {
        s.to_string()
    } else {
        format!(
            "[output truncated: showing last {cap} of {} bytes]\n{}",
            s.len(),
            &s[s.len() - cap..]
        )
    }
}

/// Count non-overlapping occurrences of `needle` in `hay`.
fn occurrences(hay: &str, needle: &str) -> u64 {
    hay.matches(needle).count() as u64
}

fn op_init(args: &Args) {
    let name = args
        .name
        .clone()
        .unwrap_or_else(|| fail("init requires 'name' (the lake init target)."));
    if !is_valid_package_name(&name) {
        fail(&format!(
            "invalid package name '{name}'. Must match [A-Za-z][A-Za-z0-9_-]*."
        ));
    }
    let dir = match abs_path(
        &PathBuf::from(args.dir.clone().unwrap_or_else(|| ".".into())),
    ) {
        Ok(p) => p,
        Err(e) => fail(&e),
    };
    if !dir.exists() {
        fail(&format!("directory '{}' does not exist.", dir.display()));
    }
    if !dir.is_dir() {
        fail(&format!("'{}' is not a directory.", dir.display()));
    }
    let occupied: Vec<String> = std::fs::read_dir(&dir)
        .ok()
        .into_iter()
        .flat_map(|rd| rd.flatten())
        .filter(|e| e.file_name().to_string_lossy() != ".git")
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    if !occupied.is_empty() {
        fail(&format!(
            "directory '{}' is not empty ({:?}). Refusing to overwrite; run init in an empty directory.",
            dir.display(),
            &occupied[..occupied.len().min(5)]
        ));
    }

    let runner = match resolve_tool_runner(&dir, "lake", Shell::Lean) {
        Ok(r) => r,
        Err(e) => fail(&e),
    };
    let init = match run_sh(
        &tool_command(&runner, &dir, &format!("lake init {name}")),
        &dir,
        INIT_TIMEOUT_S,
    ) {
        Ok(r) => r,
        Err(e) => fail(&e),
    };

    let mut cache_note = String::new();
    let mut cache_exit: Option<i32> = None;
    if args.cache == Some(true) {
        let r = match run_sh(
            &tool_command(&runner, &dir, "lake exe cache get"),
            &dir,
            INIT_TIMEOUT_S,
        ) {
            Ok(r) => r,
            Err(e) => fail(&e),
        };
        cache_exit = Some(r.exit);
        if r.exit == 0 {
            cache_note = "Prebuilt library artifacts fetched (`lake exe cache get`).".into();
        } else {
            cache_note = format!(
                "Note: `lake exe cache get` exited {0} — under Nix this is expected: prebuilt \
                 artifacts (Mathlib oleans) come from the Nix store via LEAN_PATH, not from a \
                 toolchain cache. In an elan-managed environment, fix the toolchain install \
                 first.",
                r.exit
            );
        }
    }

    let toolchain = std::fs::read_to_string(dir.join("lean-toolchain"))
        .ok()
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let files: Vec<String> = std::fs::read_dir(&dir)
        .ok()
        .into_iter()
        .flat_map(|rd| rd.flatten())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();

    let files_tail = tail_cap(&files.join(" "), 800);
    let mut text = format!(
        "Initialized Lean project '{name}' in '{}' (lake exit {}).\n",
        dir.display(),
        init.exit
    );
    if init.exit == 0 {
        text.push_str(&format!(
            "Toolchain: {toolchain}\nFiles: {files_tail}\n"
        ));
        text.push_str(
            "Next: write the specification (one theorem per non-trivial invariant, frozen \
             before the proof) plus the implementation. Define the spec before writing code; \
             never change it to make a proof pass.\n",
        );
        text.push_str(
            "Then run op=build (the zero-sorry kernel gate), review the model against the \
             source (classify every definition), and finish with op=drt (the regression gate).",
        );
        if !cache_note.is_empty() {
            text.push_str(&format!("\n{cache_note}"));
        }
    } else {
        text.push_str(&format!(
            "{}\n(exit code: {})",
            tail_cap(&init.stdout, 4000),
            init.exit
        ));
    }
    if init.timed_out {
        text.push_str(&format!("\n(timed out after {INIT_TIMEOUT_S}s)"));
    }
    emit(serde_json::json!({
        "op": "init",
        "name": name,
        "ok": init.exit == 0,
        "lake_exit": init.exit,
        "cache_exit": cache_exit,
        "timed_out": init.timed_out,
        "text": text
    }));
}

fn is_valid_package_name(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Parse the declared build-target names for a Lake project.
///
/// `lakefile.toml`: the `name = "..."` of every `[[lean_*]]` section,
/// skipping `[[lean_pkg]]` (external dependencies, not build targets).
/// Lake's `lake init` template sets `defaultTargets = ["<exe>"]`, so a
/// bare `lake build` skips declared lib targets the exe does not
/// import.
///
/// `lakefile.lean` (Lean DSL, e.g. this repo's `lean/` project): the
/// first identifier of every `lean_lib «X»` / `lean_exe «X»` line. A
/// bare `lake build` on a Lean-DSL package builds only the default
/// target, which a package of separate libs declares as none — so the
/// gate must list every target, or an unimported library slips past
/// unchecked.
fn declared_targets(dir: &Path) -> Vec<String> {
    let toml = dir.join("lakefile.toml");
    if toml.is_file() {
        let Ok(contents) = std::fs::read_to_string(&toml) else {
            return Vec::new();
        };
        let mut names: Vec<String> = Vec::new();
        let mut in_target = false;
        for raw in contents.lines() {
            let line = raw.trim();
            if let Some(sec) = line.strip_prefix("[[").and_then(|s| s.strip_suffix("]]")) {
                in_target = sec.starts_with("lean_") && sec != "lean_pkg";
            } else if in_target {
                if let Some(eq) = line.find('=') {
                    let key = line[..eq].trim();
                    if key == "name" {
                        let v = line[eq + 1..].trim().trim_matches('"').trim();
                        if !v.is_empty() && !names.iter().any(|n| n == v) {
                            names.push(v.to_string());
                        }
                    }
                }
            }
        }
        return names;
    }

    let lean = dir.join("lakefile.lean");
    let Ok(contents) = std::fs::read_to_string(&lean) else {
        return Vec::new();
    };
    let mut names: Vec<String> = Vec::new();
    for raw in contents.lines() {
        let line = raw.trim();
        let Some(rest) = line
            .strip_prefix("lean_lib ")
            .or_else(|| line.strip_prefix("lean_exe "))
        else {
            continue;
        };
        // First token: `«Name»` (DSL) or `"Name"` (string form).
        let token = rest.split_whitespace().next().unwrap_or("");
        let name = token.trim_matches(|c| c == '«' || c == '»' || c == '"');
        if !name.is_empty() && !names.iter().any(|n| n == name) {
            names.push(name.to_string());
        }
    }
    names
}

fn op_build(args: &Args) {
    let dir = match abs_path(
        &PathBuf::from(args.dir.clone().unwrap_or_else(|| ".".into())),
    ) {
        Ok(p) => p,
        Err(e) => fail(&e),
    };
    if !dir.is_dir() {
        fail(&format!("directory '{}' does not exist.", dir.display()));
    }
    let runner = match resolve_tool_runner(&dir, "lake", Shell::Lean) {
        Ok(r) => r,
        Err(e) => fail(&e),
    };
    // Build every declared target (the [[lean_lib]]/[[lean_exe]]
    // names in lakefile.toml). A bare `lake build` builds only
    // defaultTargets, which the `lake init` template limits to the
    // exe target — an unimported library would slip past the gate
    // unchecked.
    let targets = declared_targets(&dir);
    let lake_args = if targets.is_empty() {
        "build".to_string()
    } else {
        format!("build {}", targets.join(" "))
    };
    let build = match run_sh(
        &tool_command(&runner, &dir, &format!("lake {lake_args}")),
        &dir,
        BUILD_TIMEOUT_S,
    ) {
        Ok(r) => r,
        Err(e) => fail(&e),
    };

    let combined = format!("{}{}", build.stdout, build.stderr);
    // The kernel warning marker varies by toolchain spelling:
    // `declaration uses 'sorry'` (older) and
    // `declaration uses `sorry`` (4.30). Count both; ditto admit.
    let sorry_count = occurrences(&combined, "declaration uses 'sorry'")
        + occurrences(&combined, "declaration uses `sorry`");
    let admit_count = occurrences(&combined, "declaration uses 'admit'")
        + occurrences(&combined, "declaration uses `admit`");
    let open = sorry_count + admit_count;
    let clean = build.exit == 0 && open == 0;

    let mut text = if clean {
        "lake build passed (exit 0): 0 open obligations. GATE GREEN — the Lean kernel re-checked \
         every proof step; the specification holds for all inputs of the declared types."
            .to_string()
    } else if build.exit == 0 {
        format!(
            "lake build exit 0 but {open} open obligation(s) ({sorry_count} sorry, {admit_count} admit). \
             GATE RED — a clean guarantee requires zero. Every \"declaration uses 'sorry'\" is an \
             open obligation: prove it, or write a triage note naming the stuck property, why it \
             is stuck, and three options (adjust the spec, adjust the strategy, abandon). Do not \
             silently weaken the theorem."
        )
    } else {
        format!(
            "lake build failed (exit {}):\n{}",
            build.exit,
            tail_cap(&combined, 4000)
        )
    };
    if build.timed_out {
        text.push_str(&format!("\n(timed out after {BUILD_TIMEOUT_S}s)"));
    }
    emit(serde_json::json!({
        "op": "build",
        "ok": clean,
        "clean": clean,
        "targets": targets,
        "lake_exit": build.exit,
        "sorry_count": sorry_count,
        "admit_count": admit_count,
        "timed_out": build.timed_out,
        "text": text
    }));
}

// ─── Differential random testing ──────────────────────────────────

/// Deterministic 64-bit LCG. `bound` 0 means full 64-bit range.
fn lcg_values(seed: u64, bound: u64, n: u64) -> Vec<u64> {
    let mut state = seed;
    let mut out = Vec::with_capacity(n as usize);
    for _ in 0..n {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let v = state >> 1;
        out.push(if bound == 0 { v } else { v % bound });
    }
    out
}

/// Run one side of a pair on one input: `sh -c <cmd> '' <input>` —
/// the empty first argument is $0, so the input lands on $1. The
/// input is also exported as DRT_INPUT for commands that read the
/// environment.
fn run_input_side(cmd: &str, input: &str, dir: &Path, timeout_s: u64) -> Result<RunResult, String> {
    let c = format!(
        "DRT_INPUT={} sh -c {} '' {}",
        sh_quote(input),
        sh_quote(cmd),
        sh_quote(input)
    );
    run_sh(&c, dir, timeout_s)
}

fn run_input_pair(
    model: &str,
    prod: &str,
    input: &str,
    dir: &Path,
    timeout_s: u64,
) -> Result<(RunResult, RunResult), String> {
    let m = run_input_side(model, input, dir, timeout_s)?;
    let p = run_input_side(prod, input, dir, timeout_s)?;
    Ok((m, p))
}

fn norm_output(s: &str) -> String {
    s.trim_end().to_string()
}

fn mismatch_entry(input: &str, m: &RunResult, p: &RunResult) -> serde_json::Value {
    serde_json::json!({
        "input": input,
        "lean": {
            "out": tail_cap(&norm_output(&m.stdout), 400),
            "exit": m.exit,
            "timed_out": m.timed_out
        },
        "prod": {
            "out": tail_cap(&norm_output(&p.stdout), 400),
            "exit": p.exit,
            "timed_out": p.timed_out
        }
    })
}

/// Resolve `n` with the quick tier: an explicit "n" wins; "smoke":true
/// (with no explicit n) selects the smoke tier; otherwise the op's
/// default.
fn resolve_n(args: &Args, default: u64) -> u64 {
    match args.n {
        Some(n) => n,
        None if args.smoke == Some(true) => SMOKE_N,
        None => default,
    }
}

fn op_drt(args: &Args) {
    let model = args
        .model
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| fail("drt requires 'model' (the sh -c command of the Lean model executable)."));
    let prod = args
        .prod
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| fail("drt requires 'prod' (the sh -c command of the production executable)."));
    let n = match resolve_n(args, DRT_DEFAULT_N) {
        x if (1..=1000000).contains(&x) => x,
        x => fail(&format!("drt 'n' must be in 1..=1000000, got {x}.")),
    };
    let smoke = args.smoke == Some(true) && args.n.is_none();
    let seed = args.seed.unwrap_or(42);
    let bound = args.bound.unwrap_or(0);
    let stop = args.stop_on_mismatch.unwrap_or(true);
    let max_mismatches = match args.max_mismatches.unwrap_or(10) {
        x if (1..=1000).contains(&x) => x,
        x => fail(&format!("drt 'max_mismatches' must be in 1..=1000, got {x}.")),
    };
    let per_input = match args.per_input_timeout_s.unwrap_or(30) {
        x if (1..=300).contains(&x) => x,
        x => fail(&format!("drt 'per_input_timeout_s' must be in 1..=300, got {x}.")),
    };
    let progress = args.progress.unwrap_or(true);
    let resume = args.resume.unwrap_or(false);
    let dir = match abs_path(&PathBuf::from(args.dir.clone().unwrap_or_else(|| ".".into())))
    {
        Ok(p) => p,
        Err(e) => fail(&e),
    };
    if !dir.is_dir() {
        fail(&format!("directory '{}' does not exist.", dir.display()));
    }

    // Resolve the input sequence.
    let inputs: Vec<String> = match &args.input_gen {
        Some(gen) => {
            let r = match run_sh(gen, &dir, GEN_TIMEOUT_S) {
                Ok(r) => r,
                Err(e) => fail(&e),
            };
            if r.exit != 0 {
                emit(serde_json::json!({
                    "op": "drt",
                    "ok": false,
                    "pass": false,
                    "n": 0,
                    "checked": 0,
                    "seed": seed,
                    "mismatches": [],
                    "text": format!(
                        "Input generator failed (exit {}):\n{}\n(drt not run)",
                        r.exit,
                        tail_cap(&r.stderr, 2000)
                    ),
                }));
                return;
            }
            r.stdout
                .lines()
                .map(|l| l.trim_end().to_string())
                .filter(|l| !l.is_empty())
                .take(n as usize)
                .collect()
        }
        None => lcg_values(seed, bound, n)
            .into_iter()
            .map(|v| v.to_string())
            .collect(),
    };
    if inputs.is_empty() {
        emit(serde_json::json!({
            "op": "drt",
            "ok": false,
            "pass": false,
            "n": 0,
            "checked": 0,
            "seed": seed,
            "mismatches": [],
            "text": "No inputs produced (generator empty). drt not run.",
        }));
        return;
    }

    let params = checkpoint_params(
        args,
        "drt",
        n,
        seed,
        bound,
        per_input,
        stop,
        max_mismatches,
        args.max_rejections.unwrap_or(10),
        args.stop_on_reject.unwrap_or(true),
        prod.as_str(),
        args.input_gen.as_deref().unwrap_or(""),
    );
    let (start, started_at, mut problems) = if resume {
        if !progress {
            fail("drt \"resume\":true requires the checkpoint file; run with the default \"progress\":true.");
        }
        resume_from(&dir, "drt", &params, inputs.len(), &inputs)
    } else {
        if progress {
            guard_live_run(&dir);
        }
        (0, now_s(), 0)
    };
    let ctx = ProgressCtx {
        dir: dir.clone(),
        op: "drt",
        params,
        started_at,
    };
    if progress {
        ctx.write(
            "running",
            start as u64,
            inputs.len() as u64,
            start as u64,
            problems,
            inputs.get(start.saturating_sub(1)).map(|s| s.as_str()),
        )
        .unwrap_or_else(|e| fail(&e));
    }

    let mut mismatches: Vec<serde_json::Value> = Vec::new();
    let mut stop_reason: Option<String> = None;
    let mut first_problem: Option<usize> = None;
    let mut last_write = Instant::now();
    let mut i = start;
    while i < inputs.len() {
        let input = &inputs[i];
        let (m, p) = match run_input_pair(&model, &prod, input, &dir, per_input) {
            Ok(x) => x,
            Err(e) => fail(&e),
        };
        i += 1;
        let mut stopped = false;
        if m.timed_out || p.timed_out {
            mismatches.push(mismatch_entry(input, &m, &p));
            if first_problem.is_none() {
                first_problem = Some(i - 1);
            }
            problems += 1;
            stop_reason = Some("timeout".into());
            stopped = true;
        } else if norm_output(&m.stdout) != norm_output(&p.stdout) {
            mismatches.push(mismatch_entry(input, &m, &p));
            if first_problem.is_none() {
                first_problem = Some(i - 1);
            }
            problems += 1;
            if stop {
                stop_reason = Some("mismatch".into());
                stopped = true;
            } else if mismatches.len() as u64 >= max_mismatches {
                stop_reason = Some("limit".into());
                stopped = true;
            }
        }
        if stopped {
            if progress {
                ctx.write(
                    stop_reason.as_deref().unwrap_or("stop"),
                    first_problem.unwrap_or(i - 1) as u64,
                    inputs.len() as u64,
                    i as u64,
                    problems,
                    Some(input.as_str()),
                )
                .unwrap_or_else(|e| fail(&e));
            }
            break;
        }
        if progress
            && ((i as u64).is_multiple_of(PROGRESS_EVERY_INPUTS)
                || last_write.elapsed() >= Duration::from_secs(PROGRESS_EVERY_SECS))
        {
            ctx.write(
                "running",
                i as u64,
                inputs.len() as u64,
                i as u64,
                problems,
                Some(input.as_str()),
            )
            .ok();
            last_write = Instant::now();
        }
    }
    let checked: usize = i;

    let pass = mismatches.is_empty();
    let pfile = progress_path(&dir);
    let checkpoint_kept = match &stop_reason {
        Some(_) => progress,
        None => {
            if progress {
                let _ = std::fs::remove_file(&pfile);
            }
            false
        }
    };
    let text = if pass {
        let mut t = format!(
            "DRT: all {checked} inputs match (seed {seed}). Regression gate GREEN — the model \
             and the production code agree on every input tested."
        );
        if smoke {
            t.push_str(" (smoke tier — the quick check; the full n still gates the release)");
        }
        if resume {
            t.push_str(&format!(" (resumed from index {start})"));
        }
        t
    } else {
        let first = &mismatches[0];
        let first_input = first.get("input").and_then(|i| i.as_str()).unwrap_or("");
        let lean_out = first
            .get("lean")
            .and_then(|l| l.get("out"))
            .and_then(|o| o.as_str())
            .unwrap_or("");
        let prod_out = first
            .get("prod")
            .and_then(|l| l.get("out"))
            .and_then(|o| o.as_str())
            .unwrap_or("");
        let mut t = format!(
            "DRT: MISMATCH input={first_input}\n  lean: {lean_out}\n  prod: {prod_out}\n\
             ({} of {} inputs checked; {} mismatch(es) collected)\n\
             GATE RED — a mismatch on an exact/abstraction-classified definition is a real bug: \
             fix the model or the source, then re-run drt. A mismatch on an approximation-classified \
             definition is expected and is not a DRT failure.",
            checked,
            inputs.len(),
            mismatches.len()
        );
        if checkpoint_kept {
            t.push_str(&format!(
                "\nCheckpoint kept at {} — after the fix, re-run the same call with \
                 \"resume\":true; it re-checks index {} (the first failed input) and continues \
                 from there.",
                pfile.display(),
                first_problem.unwrap_or(0)
            ));
        }
        t
    };

    let mut result = serde_json::json!({
        "op": "drt",
        "ok": pass,
        "pass": pass,
        "n": inputs.len() as u64,
        "checked": checked as u64,
        "seed": seed,
        "stop_reason": stop_reason,
        "mismatches": mismatches,
    });
    if smoke {
        result["tier"] = serde_json::json!("smoke");
    }
    if resume {
        result["resumed_from"] = serde_json::json!(start as u64);
    }
    if progress {
        result["progress_file"] = serde_json::json!(pfile.display().to_string());
        result["checkpoint"] = serde_json::json!(checkpoint_kept);
    }
    result["text"] = serde_json::json!(text);
    emit(result);
}

// ─── Progress / checkpoint (drt + check-inputs) ──────────────────
//
// A long DRT run must answer "is it still alive?" without `ps aux`,
// and a re-posted goal (context overflow) must be able to pick a
// stopped run back up. Both come from one small file,
// <dir>/.drt-progress.json, written atomically (tmp + rename) every
// PROGRESS_EVERY_INPUTS inputs or PROGRESS_EVERY_SECS seconds: pid,
// status, next index, rate, eta, updated_at, and the parameter block
// that identifies the run. A clean run deletes the file; a stopped
// run keeps it as the checkpoint that "resume":true continues from.

/// Path of the progress/heartbeat file for a run rooted at `dir`.
fn progress_path(dir: &Path) -> PathBuf {
    dir.join(PROGRESS_FILE_NAME)
}

fn now_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// True when `pid` names a live process: kill(pid, 0) succeeds for a
/// live process, reports EPERM for a live process that is not ours,
/// and fails with ESRCH when the process is gone.
fn pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error()
        .raw_os_error()
        .is_some_and(|e| e == libc::EPERM)
}

/// Atomically write the progress document (tmp file + rename, so a
/// poller never reads a torn file).
fn write_checkpoint(dir: &Path, doc: &serde_json::Value) -> Result<(), String> {
    let path = progress_path(dir);
    let tmp = dir.join(format!(".drt-progress.json.tmp.{}", std::process::id()));
    let json =
        serde_json::to_string(doc).map_err(|e| format!("cannot serialize checkpoint: {e}"))?;
    std::fs::write(&tmp, json)
        .map_err(|e| format!("cannot write checkpoint '{}': {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .map_err(|e| format!("cannot update checkpoint '{}': {e}", path.display()))?;
    Ok(())
}

/// The progress-file context of one run: where the file lives, which
/// run it identifies (its parameter block), and when the run started
/// (preserved across resumes so rate/eta stay meaningful).
struct ProgressCtx {
    dir: PathBuf,
    op: &'static str,
    params: serde_json::Value,
    started_at: u64,
}

impl ProgressCtx {
    /// Write one progress document (atomically), computing rate/eta
    /// from the time elapsed since `started_at`. `next` is the index
    /// of the next input to (re-)check; `processed` counts inputs
    /// checked so far (cumulative across resumes); `last_input` is
    /// the line at `processed - 1`.
    fn write(
        &self,
        status: &str,
        next: u64,
        total: u64,
        processed: u64,
        problems: u64,
        last_input: Option<&str>,
    ) -> Result<(), String> {
        let now = now_s();
        let elapsed = now.saturating_sub(self.started_at);
        let rate = if elapsed > 0 && processed > 0 {
            Some((processed as f64 / elapsed as f64).round() * 10.0 / 10.0)
        } else {
            None
        };
        let eta = rate.map(|r| {
            if r > 0.0 {
                (total.saturating_sub(processed) as f64 / r).ceil() as u64
            } else {
                u64::MAX
            }
        });
        let doc = serde_json::json!({
            "version": PROGRESS_VERSION,
            "op": self.op,
            "pid": std::process::id(),
            "status": status,
            "next": next,
            "total": total,
            "processed": processed,
            "problems": problems,
            "last_input": last_input,
            "params": self.params,
            "started_at": self.started_at,
            "updated_at": now,
            "rate_sps": rate,
            "eta_s": eta
        });
        write_checkpoint(&self.dir, &doc)
    }
}

#[allow(clippy::too_many_arguments)]
/// The parameter block stored in the checkpoint — the resume
/// identity. A resume is only valid when every field still matches
/// the call that is resuming it.
fn checkpoint_params(
    a: &Args,
    op: &str,
    n: u64,
    seed: u64,
    bound: u64,
    per_input: u64,
    stop_mismatch: bool,
    max_mismatches: u64,
    max_rejections: u64,
    stop_reject: bool,
    prod: &str,
    input_gen: &str,
) -> serde_json::Value {
    serde_json::json!({
        "op": op,
        "seed": seed,
        "bound": bound,
        "n": n,
        "model": a.model.as_deref().unwrap_or(""),
        "prod": prod,
        "input_gen": input_gen,
        "per_input_timeout_s": per_input,
        "stop_on_mismatch": stop_mismatch,
        "max_mismatches": max_mismatches,
        "max_rejections": max_rejections,
        "stop_on_reject": stop_reject
    })
}

/// Validate the checkpoint at `dir` for a resume and return
/// `(next, started_at, problems)` — the index to (re-)check first,
/// the run's original start time, and the cumulative problem count.
/// Fails the tool on any mismatch: a resume must continue exactly
/// the run it claims to.
fn resume_from(
    dir: &Path,
    op: &str,
    params: &serde_json::Value,
    total: usize,
    inputs: &[String],
) -> (usize, u64, u64) {
    let path = progress_path(dir);
    let s = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => fail(&format!(
            "no checkpoint at '{}'; start without \"resume\":true.",
            path.display()
        )),
    };
    let ck: serde_json::Value = match serde_json::from_str(&s) {
        Ok(v) => v,
        Err(e) => fail(&format!("corrupt checkpoint at '{}': {e}", path.display())),
    };
    if ck.get("version").and_then(|v| v.as_u64()) != Some(PROGRESS_VERSION) {
        fail("checkpoint was written by a different lean-verify version; re-run without \"resume\":true.");
    }
    if ck.get("op").and_then(|v| v.as_str()) != Some(op) {
        fail(&format!(
            "checkpoint was written by op '{}', not '{op}'; re-run without \"resume\":true.",
            ck.get("op").and_then(|v| v.as_str()).unwrap_or("?")
        ));
    }
    let status = ck.get("status").and_then(|v| v.as_str()).unwrap_or("");
    if status == "done" {
        fail("checkpoint marks a completed run; nothing to resume — re-run without \"resume\":true.");
    }
    let stored = match ck.get("params").and_then(|p| p.as_object()) {
        Some(o) => o,
        None => fail("checkpoint has no parameter block; re-run without \"resume\":true."),
    };
    let cur = match params.as_object() {
        Some(o) => o,
        None => fail("internal error: current parameters are not an object."),
    };
    let mut bad: Vec<String> = Vec::new();
    {
        let keys: std::collections::BTreeSet<String> =
            stored.keys().chain(cur.keys()).map(|k| k.to_string()).collect();
        for k in &keys {
            if stored.get(k) != cur.get(k) {
                bad.push(k.to_string());
            }
        }
    }
    if !bad.is_empty() {
        fail(&format!(
            "checkpoint parameter mismatch ({}): this call differs from the checkpointed run. \
             Re-run without \"resume\":true to start fresh.",
            bad.join(", ")
        ));
    }
    let next = ck.get("next").and_then(|v| v.as_u64()).unwrap_or(0);
    let total_ck = ck.get("total").and_then(|v| v.as_u64()).unwrap_or(u64::MAX);
    if total_ck != total as u64 {
        fail(&format!(
            "checkpoint recorded {total_ck} inputs; this run resolved {total}. The input \
             sequence changed — re-run without \"resume\":true."
        ));
    }
    if next >= total as u64 {
        fail("checkpoint points past the end of the input sequence; re-run without \"resume\":true.");
    }
    let next = next as usize;
    let processed_ck = ck.get("processed").and_then(|v| v.as_u64()).unwrap_or(0);
    if processed_ck > 0 && processed_ck as usize <= inputs.len() {
        // Determinism guard: the line just before the checkpoint
        // (`last_input`, the line at `processed - 1`) must be what
        // the regenerated sequence produces there. LCG sequences are
        // deterministic by construction; this catches a
        // non-deterministic input_gen.
        let recorded = ck.get("last_input").and_then(|v| v.as_str()).unwrap_or("");
        let actual = inputs
            .get(processed_ck as usize - 1)
            .map(|s| s.as_str())
            .unwrap_or("");
        if recorded != actual {
            fail(&format!(
                "input line {} is now '{}', the checkpoint recorded '{}': the input generator \
                 is not deterministic — re-run without \"resume\":true.",
                processed_ck - 1,
                actual,
                recorded
            ));
        }
    }
    if status == "running" {
        if let Some(pid) = ck.get("pid").and_then(|v| v.as_u64()) {
            if pid_alive(pid as i32) {
                fail(&format!(
                    "another lean-verify run (pid {pid}) still owns this checkpoint; wait for \
                     it to finish or kill it, then retry."
                ));
            }
        }
    }
    let started_at = ck.get("started_at").and_then(|v| v.as_u64()).unwrap_or_else(now_s);
    let problems = ck.get("problems").and_then(|v| v.as_u64()).unwrap_or(0);
    (next, started_at, problems)
}

/// A fresh run must not clobber the checkpoint of a live run. A
/// stale file (dead pid, or a stopped status) is simply replaced.
fn guard_live_run(dir: &Path) {
    let Ok(s) = std::fs::read_to_string(progress_path(dir)) else {
        return;
    };
    let Ok(ck) = serde_json::from_str::<serde_json::Value>(&s) else {
        return;
    };
    if ck.get("version").and_then(|v| v.as_u64()) != Some(PROGRESS_VERSION) {
        return;
    }
    if ck.get("status").and_then(|v| v.as_str()) != Some("running") {
        return;
    }
    if let Some(pid) = ck.get("pid").and_then(|v| v.as_u64()) {
        if pid_alive(pid as i32) {
            fail(&format!(
                "another lean-verify run (pid {pid}) owns this checkpoint (status running); \
                 wait for it to finish or kill it before starting a fresh run."
            ));
        }
    }
}

// ─── Input-protocol preflight (check-inputs) ──────────────────────

/// `check-inputs` — the DRT preflight. Feeds the generator's lines
/// through the model executable (and the production executable when
/// given) and reports the lines a side rejects. A side rejects an
/// input when it exits non-zero or times out; an accepted input
/// exits 0. This is the ~10 s check for "the generator emits
/// well-formed lines" that used to cost a full drt run to find out.
/// (An executable that exits 0 even on parse failure is not caught
/// here; op=drt catches that through the output comparison.)
fn op_check_inputs(args: &Args) {
    let model = args
        .model
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| {
            fail("check-inputs requires 'model' (the sh -c command of the Lean model executable).")
        });
    let prod = args.prod.clone().filter(|s| !s.trim().is_empty());
    let n = match resolve_n(args, CHECK_INPUTS_DEFAULT_N) {
        x if (1..=1000000).contains(&x) => x,
        x => fail(&format!("check-inputs 'n' must be in 1..=1000000, got {x}.")),
    };
    let seed = args.seed.unwrap_or(42);
    let bound = args.bound.unwrap_or(0);
    let per_input = match args.per_input_timeout_s.unwrap_or(30) {
        x if (1..=300).contains(&x) => x,
        x => fail(&format!(
            "check-inputs 'per_input_timeout_s' must be in 1..=300, got {x}."
        )),
    };
    let stop_reject = args.stop_on_reject.unwrap_or(true);
    let max_rejections = match args.max_rejections.unwrap_or(10) {
        x if (1..=1000).contains(&x) => x,
        x => fail(&format!(
            "check-inputs 'max_rejections' must be in 1..=1000, got {x}."
        )),
    };
    let progress = args.progress.unwrap_or(false);
    let resume = args.resume.unwrap_or(false);
    let dir = match abs_path(&PathBuf::from(args.dir.clone().unwrap_or_else(|| ".".into())))
    {
        Ok(p) => p,
        Err(e) => fail(&e),
    };
    if !dir.is_dir() {
        fail(&format!("directory '{}' does not exist.", dir.display()));
    }

    // Resolve the input sequence (same contract as drt).
    let inputs: Vec<String> = match &args.input_gen {
        Some(gen) => {
            let r = match run_sh(gen, &dir, GEN_TIMEOUT_S) {
                Ok(r) => r,
                Err(e) => fail(&e),
            };
            if r.exit != 0 {
                emit(serde_json::json!({
                    "op": "check-inputs",
                    "ok": false,
                    "n": 0,
                    "checked": 0,
                    "seed": seed,
                    "rejected": [],
                    "text": format!(
                        "Input generator failed (exit {}):\n{}\n(check-inputs not run)",
                        r.exit,
                        tail_cap(&r.stderr, 2000)
                    ),
                }));
                return;
            }
            r.stdout
                .lines()
                .map(|l| l.trim_end().to_string())
                .filter(|l| !l.is_empty())
                .take(n as usize)
                .collect()
        }
        None => lcg_values(seed, bound, n)
            .into_iter()
            .map(|v| v.to_string())
            .collect(),
    };
    if inputs.is_empty() {
        emit(serde_json::json!({
            "op": "check-inputs",
            "ok": false,
            "n": 0,
            "checked": 0,
            "seed": seed,
            "rejected": [],
            "text": "No inputs produced (generator empty). check-inputs not run.",
        }));
        return;
    }

    let params = checkpoint_params(
        args,
        "check-inputs",
        n,
        seed,
        bound,
        per_input,
        args.stop_on_mismatch.unwrap_or(true),
        args.max_mismatches.unwrap_or(10),
        max_rejections,
        stop_reject,
        prod.as_deref().unwrap_or(""),
        args.input_gen.as_deref().unwrap_or(""),
    );
    let (start, started_at, mut problems) = if resume {
        if !progress {
            fail("check-inputs \"resume\":true requires the checkpoint file; run with \"progress\":true.");
        }
        resume_from(&dir, "check-inputs", &params, inputs.len(), &inputs)
    } else {
        if progress {
            guard_live_run(&dir);
        }
        (0, now_s(), 0)
    };
    let ctx = ProgressCtx {
        dir: dir.clone(),
        op: "check-inputs",
        params,
        started_at,
    };
    if progress {
        ctx.write(
            "running",
            start as u64,
            inputs.len() as u64,
            start as u64,
            problems,
            inputs.get(start.saturating_sub(1)).map(|s| s.as_str()),
        )
        .unwrap_or_else(|e| fail(&e));
    }

    let mut rejections: Vec<serde_json::Value> = Vec::new();
    let mut stop_reason: Option<String> = None;
    let mut first_problem: Option<usize> = None;
    let mut last_write = Instant::now();
    let mut i = start;
    while i < inputs.len() {
        let input = &inputs[i];
        let m = match run_input_side(&model, input, &dir, per_input) {
            Ok(r) => r,
            Err(e) => fail(&e),
        };
        let p = prod.as_deref().map(|pc| match run_input_side(pc, input, &dir, per_input) {
            Ok(r) => r,
            Err(e) => fail(&e),
        });
        let mut reasons: Vec<String> = Vec::new();
        if m.timed_out {
            reasons.push("model:timeout".to_string());
        }
        if m.exit != 0 {
            reasons.push(format!("model:exit_{}", m.exit));
        }
        if let Some(pr) = &p {
            if pr.timed_out {
                reasons.push("prod:timeout".to_string());
            }
            if pr.exit != 0 {
                reasons.push(format!("prod:exit_{}", pr.exit));
            }
        }
        i += 1;
        if !reasons.is_empty() {
            if first_problem.is_none() {
                first_problem = Some(i - 1);
            }
            problems += 1;
            rejections.push(serde_json::json!({
                "input": input,
                "reasons": reasons,
                "model": {
                    "exit": m.exit,
                    "timed_out": m.timed_out,
                    "stderr": tail_cap(&m.stderr, 400)
                },
                "prod": p.as_ref().map(|pr| serde_json::json!({
                    "exit": pr.exit,
                    "timed_out": pr.timed_out,
                    "stderr": tail_cap(&pr.stderr, 400)
                })),
            }));
            let reached = rejections.len() as u64 >= max_rejections;
            if stop_reject || reached {
                stop_reason = Some(if stop_reject {
                    "reject".to_string()
                } else {
                    "limit".to_string()
                });
                if progress {
                    ctx.write(
                        stop_reason.as_deref().unwrap(),
                        first_problem.unwrap_or(i - 1) as u64,
                        inputs.len() as u64,
                        i as u64,
                        problems,
                        Some(input.as_str()),
                    )
                    .unwrap_or_else(|e| fail(&e));
                }
                break;
            }
        }
        if progress
            && ((i as u64).is_multiple_of(PROGRESS_EVERY_INPUTS)
                || last_write.elapsed() >= Duration::from_secs(PROGRESS_EVERY_SECS))
        {
            ctx.write(
                "running",
                i as u64,
                inputs.len() as u64,
                i as u64,
                problems,
                Some(input.as_str()),
            )
            .ok();
            last_write = Instant::now();
        }
    }
    let checked: usize = i;
    let ok = rejections.is_empty();
    let pfile = progress_path(&dir);
    let checkpoint_kept = match &stop_reason {
        Some(_) => progress,
        None => {
            if progress {
                let _ = std::fs::remove_file(&pfile);
            }
            false
        }
    };

    let who = if prod.is_some() { "model and prod" } else { "model" };
    let text = if ok {
        let mut t = format!(
            "check-inputs: all {checked} inputs accepted by the {who} side (a side rejects an \
             input when it exits non-zero or times out; accepted inputs exit 0). The \
             generator's lines are well-formed for the DRT protocol — run op=drt for the \
             full regression gate."
        );
        if resume {
            t.push_str(&format!(" (resumed from index {start})"));
        }
        t
    } else {
        let first = &rejections[0];
        let first_input = first.get("input").and_then(|x| x.as_str()).unwrap_or("");
        let reasons: Vec<String> = first
            .get("reasons")
            .and_then(|r| r.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let m_err = first
            .get("model")
            .and_then(|x| x.get("stderr"))
            .and_then(|x| x.as_str())
            .unwrap_or("");
        let p_err = first
            .get("prod")
            .and_then(|x| x.get("stderr"))
            .and_then(|x| x.as_str())
            .unwrap_or("");
        let mut t = format!(
            "check-inputs: REJECTED input={first_input} ({})\n  model stderr: {m_err}\n",
            reasons.join(", ")
        );
        if prod.is_some() {
            t.push_str(&format!("  prod stderr: {p_err}\n"));
        }
        t.push_str(&format!(
            "({checked} of {} inputs checked; {} rejection(s); stop: {})\n\
             GATE RED — the generator is emitting lines the executable(s) reject. Fix the \
             generator or the protocol, re-run check-inputs, then op=drt.",
            inputs.len(),
            rejections.len(),
            stop_reason.as_deref().unwrap_or("end")
        ));
        if checkpoint_kept {
            t.push_str(&format!(
                "\nCheckpoint kept at {} — after the fix, re-run the same call with \
                 \"resume\":true; it re-checks index {} (the first rejected input) and \
                 continues from there.",
                pfile.display(),
                first_problem.unwrap_or(0)
            ));
        }
        t
    };

    let mut result = serde_json::json!({
        "op": "check-inputs",
        "ok": ok,
        "n": inputs.len() as u64,
        "checked": checked as u64,
        "seed": seed,
        "stop_reason": stop_reason,
        "rejected": rejections,
    });
    if resume {
        result["resumed_from"] = serde_json::json!(start as u64);
    }
    if progress {
        result["progress_file"] = serde_json::json!(pfile.display().to_string());
        result["checkpoint"] = serde_json::json!(checkpoint_kept);
    }
    result["text"] = serde_json::json!(text);
    emit(result);
}

// ─── Rust -> Lean translation (charon + aeneas pipeline) ─────────

/// Borrowed pipeline (AeneasVerif/aeneas + e6qu/rust-lean-aeneas):
/// `charon cargo --preset=aeneas` extracts the crate's MIR to an LLBC
/// file; `aeneas -backend lean` translates the LLBC into a pure Lean
/// model. A translation is a model, not a proof: aeneas may emit
/// `axiom`s for constructs it cannot model, and an axiom asserts
/// nothing — the zero-sorry kernel gate (op=build) is still the
/// guarantee, and op=drt the regression gate.
fn op_translate(args: &Args) {
    let dir = match abs_path(&PathBuf::from(args.dir.clone().unwrap_or_else(|| ".".into()))) {
        Ok(p) => p,
        Err(e) => fail(&e),
    };
    if !dir.is_dir() {
        fail(&format!("directory '{}' does not exist.", dir.display()));
    }
    if !dir.join("Cargo.toml").is_file() {
        fail(&format!(
            "translate requires a cargo crate: no Cargo.toml in '{}'. Point 'dir' at the crate root.",
            dir.display()
        ));
    }
    let runner = match resolve_tool_runner(&dir, "charon", Shell::Aeneas) {
        Ok(r) => r,
        Err(e) => fail(&e),
    };
    // Mark the op start so we can attribute the LLBC / .lean files that
    // appear afterwards to this run.
    let since = std::time::SystemTime::now();

    // Stage 1: charon — Rust MIR -> LLBC (written next to Cargo.toml).
    let ch = match run_sh(
        &tool_command(&runner, &dir, "charon cargo --preset=aeneas"),
        &dir,
        TRANSLATE_CHARON_TIMEOUT_S,
    ) {
        Ok(r) => r,
        Err(e) => fail(&e),
    };
    let llbc = find_newest(&dir, "llbc", since);
    if ch.exit != 0 || ch.timed_out || llbc.is_none() {
        let text = if ch.timed_out {
            format!("charon timed out after {TRANSLATE_CHARON_TIMEOUT_S}s.")
        } else if llbc.is_some() {
            "charon reported failure but produced an LLBC; inspect the output above.".to_string()
        } else {
            format!(
                "charon failed (exit {}):\n{}",
                ch.exit,
                tail_cap(&format!("{}{}", ch.stdout, ch.stderr), 4000)
            )
        };
        emit(serde_json::json!({
            "op": "translate",
            "ok": false,
            "stage": "charon",
            "charon_exit": ch.exit,
            "timed_out": ch.timed_out,
            "text": text
        }));
        return;
    }
    let llbc = llbc.unwrap();
    let llbc_rel = llbc
        .strip_prefix(&dir)
        .unwrap_or(&llbc)
        .display()
        .to_string();

    // Stage 2: aeneas — LLBC -> pure Lean (written into the crate dir).
    let ae = match run_sh(
        &tool_command(
            &runner,
            &dir,
            &format!("aeneas -backend lean {llbc_rel}"),
        ),
        &dir,
        TRANSLATE_AENEAS_TIMEOUT_S,
    ) {
        Ok(r) => r,
        Err(e) => fail(&e),
    };
    if ae.exit != 0 || ae.timed_out {
        emit(serde_json::json!({
            "op": "translate",
            "ok": false,
            "stage": "aeneas",
            "llbc": llbc_rel,
            "aeneas_exit": ae.exit,
            "timed_out": ae.timed_out,
            "text": format!(
                "aeneas failed (exit {}):\n{}",
                ae.exit,
                tail_cap(&format!("{}{}", ae.stdout, ae.stderr), 4000)
            )
        }));
        return;
    }

    let generated = find_newer(&dir, "lean", since);
    let axiom_count = count_axioms(&dir, &generated);
    let ok = !generated.is_empty();
    let text = if ok {
        format!(
            "Translated the crate in '{}' via charon (Rust MIR -> LLBC: {llbc_rel}) and \
             aeneas (LLBC -> Lean: {}). A translation is a model, not a proof: aeneas may \
             emit `axiom`s for constructs it cannot model ({n} found here) — an axiom \
             asserts nothing and is not a guarantee. Next: state your specification as \
             theorems over the generated definitions, run op=build (the zero-sorry kernel \
             gate), and regression-gate against the real Rust binary with op=drt.",
            dir.display(),
            generated.join(", "),
            n = axiom_count
        )
    } else {
        format!(
            "aeneas exited 0 but no .lean file appeared under '{}'; re-run manually \
             (`aeneas -backend lean {llbc_rel}`) and inspect the crate directory.",
            dir.display()
        )
    };
    emit(serde_json::json!({
        "op": "translate",
        "ok": ok,
        "stage": "done",
        "llbc": llbc_rel,
        "generated": generated,
        "axiom_count": axiom_count,
        "charon_exit": ch.exit,
        "aeneas_exit": ae.exit,
        "timed_out": false,
        "text": text
    }));
}

/// Collect `*.<ext>` files under `dir` modified since `since`, as
/// paths relative to `dir` (depth-bounded walk; build/VCS junk dirs
/// skipped).
fn find_newer(dir: &Path, ext: &str, since: std::time::SystemTime) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    collect_newer(dir, dir, ext, since, 0, &mut out);
    out.sort();
    out
}

fn collect_newer(
    base: &Path,
    dir: &Path,
    ext: &str,
    since: std::time::SystemTime,
    depth: usize,
    out: &mut Vec<String>,
) {
    if depth > 3 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if path.is_dir() {
            if !matches!(
                name.as_str(),
                ".git" | ".lake" | ".lakebuild" | "target" | "node_modules" | ".charon"
            ) {
                collect_newer(base, &path, ext, since, depth + 1, out);
            }
            continue;
        }
        if !name.ends_with(ext) {
            continue;
        }
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        if mtime.map_or(true, |t| t >= since) {
            if let Ok(rel) = path.strip_prefix(base) {
                out.push(rel.display().to_string());
            }
        }
    }
}

/// The newest (by mtime) `*.<ext>` file under `dir` modified since
/// `since`, as an absolute path.
fn find_newest(dir: &Path, ext: &str, since: std::time::SystemTime) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for rel in find_newer(dir, ext, since) {
        let full = dir.join(&rel);
        if let Ok(t) = std::fs::metadata(&full).and_then(|m| m.modified()) {
            if best.as_ref().map_or(true, |(t0, _)| t > *t0) {
                best = Some((t, full));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// Count `axiom` declarations across the generated files — the
/// STATUS.md caveat of the aeneas reference repos: a translation may
/// hide unproven holes behind axioms; only the kernel gate counts.
fn count_axioms(dir: &Path, files: &[String]) -> u64 {
    files.iter().map(|f| {
        std::fs::read_to_string(dir.join(f))
            .map(|s| {
                s.lines()
                    .filter(|l| l.trim_start().starts_with("axiom "))
                    .count() as u64
            })
            .unwrap_or(0)
    }).sum()
}
