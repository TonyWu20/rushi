//! `rushi-sessions` — the television-channel backend.
//!
//! Replaces the inline Python embedded in the `rushi-sessions` television
//! channel (lib/rushi-sessions-channel.nix). Two subcommands:
//!
//! - `rushi-sessions source`
//!   Scan for rushi session directories under the CWD and print one TSV row
//!   per session: `status<TAB>name<TAB>repo<TAB>phase<TAB>last<TAB>mtime<TAB>path`.
//!   The television channel's `display`/`output` split on that tab.
//!
//! - `rushi-sessions preview <status> <name> <repo> <phase> <last> <mtime> <path>`
//!   Render the TOML preview card for one session: the last user/assistant
//!   messages and the most recent events. When `bat` is on PATH the card is
//!   colored with it ($RUSHI_PREVIEW_THEME selects the theme, default
//!   "Catppuccin Macchiato"); otherwise plain TOML is printed.
//!
//! The `source` walk replaces the channel's previous `fd` call and the
//! previous `python3` reader, so the channel no longer needs `fd` or
//! `python3` at run time.

use chrono::{Local, TimeZone};
use clap::{Parser, Subcommand};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

/// Directory names that the `source` walk prunes (the old `fd -E` set).
/// `.git` and `.nix` are also hidden, so the hidden check below covers
/// them too; they are listed here for parity with the old `fd -E` flags.
const PRUNE: &[&str] = &["target", ".git", "node_modules", "scratch", ".nix"];

/// Whether a directory is pruned from the walk: the `fd -E` name set, or
/// any hidden directory. The old `fd` call skipped hidden directories by
/// default (no `--no-hidden`), so skipping them keeps the walk domain
/// identical to the channel's previous behaviour.
fn is_pruned(name: &str) -> bool {
    name.starts_with('.') || PRUNE.iter().any(|p| *p == name)
}

/// ext_status ids that always show in the preview's recent-event list.
const KEEP: &[&str] = &[
    "loop_phase",
    "model_thinking",
    "goal_paused",
    "goal_active",
    "run.refire",
];

#[derive(Parser)]
#[command(
    name = "rushi-sessions",
    about = "rushi-sessions television channel backend",
    version
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scan for rushi sessions under the CWD and print TSV rows.
    Source,
    /// Render the TOML preview card for one session.
    Preview {
        status: String,
        name: String,
        repo: String,
        phase: String,
        last: String,
        /// Epoch seconds of the reference file; carried by the channel,
        /// not used by the card.
        mtime: String,
        /// Session directory path.
        path: String,
    },
}

fn main() {
    // Behave like a normal Unix program when stdout is a pipe that the
    // reader closes early (e.g. `rushi-sessions source | head`): restore
    // the default SIGPIPE disposition so a write to a broken pipe ends the
    // process silently instead of Rust's default EPIPE panic trace.
    #[cfg(unix)]
    {
        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        }
    }

    let args = Args::parse();
    match args.command {
        Command::Source => source(),
        Command::Preview {
            status,
            name,
            repo,
            phase,
            last,
            mtime,
            path,
        } => {
            let _ = &mtime;
            preview(&status, &name, &repo, &phase, &last, &path);
        }
    }
}

// ── source ───────────────────────────────────────────────────────────

struct Row {
    status: &'static str,
    name: String,
    repo: String,
    phase: String,
    last: String,
    mtime: i64,
    path: String,
}

fn source() {
    let mut rows: Vec<Row> = Vec::new();
    for sdir in find_sessions() {
        let base = sdir.trim_end_matches('/').to_string();
        let entries = match fs::read_dir(&base) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let repo = repo_of(&base);
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let p = format!("{}/{}", base, name);
            if !Path::new(&p).is_dir() {
                continue;
            }
            let ev = format!("{}/events.jsonl", p);
            let pidf = format!("{}/loop.pid", p);
            let cwf = format!("{}/cwd", p);
            let has = |s: &str| Path::new(s).exists();
            if !(has(&ev) || has(&pidf) || has(&cwf)) {
                continue;
            }
            let pid = fs::read_to_string(&pidf)
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            let status = if alive(&pid) { "ACTIVE" } else { "IDLE" };
            let phase = if has(&ev) {
                last_phase(&ev)
            } else {
                "?".into()
            };
            let refp = if has(&ev) {
                &ev
            } else if has(&pidf) {
                &pidf
            } else {
                &cwf
            };
            let mtime = mtime_secs(refp);
            rows.push(Row {
                status,
                name,
                repo: repo.clone(),
                phase,
                last: fmt_local(mtime),
                mtime,
                path: p,
            });
        }
    }
    // ACTIVE first, then by mtime descending (the old sort key).
    rows.sort_by(|a, b| {
        let ka = a.status != "ACTIVE";
        let kb = b.status != "ACTIVE";
        ka.cmp(&kb).then_with(|| b.mtime.cmp(&a.mtime))
    });
    for r in &rows {
        let line = [
            r.status,
            &r.name,
            &r.repo,
            &r.phase,
            &r.last,
            &r.mtime.to_string(),
            &r.path,
        ]
        .join("\t");
        println!("{}", line);
    }
}

/// Find every directory named `sessions` under the CWD (a `fd`
/// substitute), returned as **absolute** paths like
/// `/home/u/repo/sessions` (matching the old `fd` output, which was
/// given an absolute start point). Hidden directories are skipped, as
/// the old `fd` call did.
fn find_sessions() -> Vec<String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut rel: Vec<String> = Vec::new();
    walk_rel("", &mut rel);
    rel.into_iter()
        .map(|r| cwd.join(&r).to_string_lossy().into_owned())
        .collect()
}

fn walk_rel(dir: &str, out: &mut Vec<String>) {
    // `dir` is a relative path; the empty string means the CWD.
    let rd = if dir.is_empty() {
        fs::read_dir(".")
    } else {
        fs::read_dir(dir)
    };
    let rd = match rd {
        Ok(r) => r,
        Err(_) => return,
    };
    for entry in rd.flatten() {
        let f = entry.file_name().to_string_lossy().into_owned();
        if is_pruned(&f) {
            continue;
        }
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let child = if dir.is_empty() {
            f.clone()
        } else {
            format!("{}/{}", dir, f)
        };
        if f == "sessions" {
            out.push(child.clone());
        }
        walk_rel(&child, out);
    }
}

/// The repo a `sessions` directory belongs to: the basename of the
/// directory that contains it. Falls back to `.` when the parent has no
/// basename (a bare top-level `sessions`).
fn repo_of(base: &str) -> String {
    Path::new(base)
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".".into())
}

/// Read the last `n` bytes of a file.
fn read_tail(p: &str, n: usize) -> Result<Vec<u8>, std::io::Error> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = fs::File::open(p)?;
    let size = f.metadata()?.len();
    f.seek(SeekFrom::Start(size.saturating_sub(n as u64)))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    Ok(buf)
}

/// The value of the most recent `loop_phase` ext_status in the log tail.
/// Mirrors the old string scan: the last `"id":"loop_phase"` marker, then
/// the following `"value":"..."`.
fn last_phase(ev: &str) -> String {
    let data = match read_tail(ev, 100_000) {
        Ok(d) => d,
        Err(_) => return "?".into(),
    };
    let idm = b"\"id\":\"loop_phase\"";
    let i = match data.windows(idm.len()).rposition(|w| w == idm) {
        Some(x) => x,
        None => return "?".into(),
    };
    let vmark = b"\"value\":\"";
    let jrel = match data[i..].windows(vmark.len()).position(|w| w == vmark) {
        Some(x) => x,
        None => return "?".into(),
    };
    let vstart = i + jrel + vmark.len();
    let kend = match data[vstart..].iter().position(|b| *b == b'"') {
        Some(x) => vstart + x,
        None => return "?".into(),
    };
    String::from_utf8_lossy(&data[vstart..kend]).into_owned()
}

/// Whether a pid is alive (`kill(pid, 0)` succeeds).
fn alive(pid: &str) -> bool {
    if pid.is_empty() {
        return false;
    }
    let p: i32 = match pid.parse() {
        Ok(v) => v,
        Err(_) => return false,
    };
    unsafe { libc::kill(p, 0) == 0 }
}

/// Whole-epoch-seconds mtime of a path, or 0 if it cannot be read.
fn mtime_secs(p: &str) -> i64 {
    fs::metadata(p)
        .and_then(|m| m.modified())
        .map(|mt| {
            mt.duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0)
        })
        .unwrap_or(0)
}

/// Format epoch seconds as local `mm-dd HH:MM`.
fn fmt_local(secs: i64) -> String {
    match Local.timestamp_opt(secs, 0).single() {
        Some(dt) => dt.format("%m-%d %H:%M").to_string(),
        None => String::new(),
    }
}

// ── preview ──────────────────────────────────────────────────────────

fn preview(status: &str, name: &str, repo: &str, phase: &str, last: &str, path: &str) {
    let pid = fs::read_to_string(Path::new(path).join("loop.pid"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    // Refresh the status from the live pid when one is present.
    let status = if !pid.is_empty() {
        if alive(&pid) {
            "ACTIVE"
        } else {
            "IDLE"
        }
    } else {
        status
    };

    let epath = Path::new(path).join("events.jsonl");
    let events = if epath.exists() {
        read_events(&epath)
    } else {
        Vec::new()
    };

    let mut think = String::new();
    let mut last_user = String::new();
    let mut last_assistant = String::new();
    for o in &events {
        let t = o.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if t == "ext_status" && o.get("id").and_then(|v| v.as_str()) == Some("model_thinking") {
            think = value_to_str(o.get("value"));
        }
        if t == "user_message" {
            last_user = o
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
        }
        if t == "assistant_message" {
            last_assistant = o
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
        }
    }

    let mut out: Vec<String> = Vec::new();
    out.push("[rushi-session]".into());
    out.push(format!("name    = {}", json_str(name)));
    out.push(format!("repo    = {}", json_str(repo)));
    out.push(format!("status  = {}", json_str(status)));
    out.push(format!("pid     = {}", json_str(&pid)));
    out.push(format!("phase   = {}", json_str(phase)));
    if !think.is_empty() {
        out.push(format!("think   = {}", json_str(&think)));
    }
    out.push(format!("updated = {}", json_str(last)));
    out.push(String::new());
    out.push("[last-user-message]".into());
    out.push(format!("content = {}", json_str(&clip(&last_user, 400))));
    out.push(String::new());
    out.push("[last-assistant-message]".into());
    out.push(format!(
        "content = {}",
        json_str(&clip(&last_assistant, 400))
    ));

    // The last 40 events, reduced to (ts, type, brief), then the last 5 in
    // log order (the old `events[-5:]`), so ts ties keep log order.
    let mut last40: Vec<&Value> = events.iter().rev().take(40).collect();
    last40.reverse();
    let tuples: Vec<(String, String, String)> = last40
        .iter()
        .filter_map(|o| {
            let brief = brief(o)?;
            let ts_raw = o.get("ts").map(ts_to_str).unwrap_or_default();
            let ts = ts_slice(&ts_raw);
            let ty = o
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string();
            Some((ts, ty, brief))
        })
        .collect();
    let recent: Vec<&(String, String, String)> =
        tuples.iter().skip(tuples.len().saturating_sub(5)).collect();
    for (ts, ty, brief) in recent {
        out.push(String::new());
        out.push("[[recent-event]]".into());
        out.push(format!("ts    = {}", json_str(ts)));
        out.push(format!("type  = {}", json_str(ty)));
        out.push(format!("brief = {}", json_str(&clip(brief, 80))));
    }

    let card = out.join("\n") + "\n";

    // Optional colorization through `bat` when it is available.
    match which("bat") {
        Some(bat) => {
            let theme = std::env::var("RUSHI_PREVIEW_THEME")
                .unwrap_or_else(|_| "Catppuccin Macchiato".to_string());
            let mut child = std::process::Command::new(&bat)
                .args([
                    "-l", "toml", "--theme", &theme, "--color", "always", "--style", "plain",
                ])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .ok();
            if let Some(mut c) = child.take() {
                use std::io::Write;
                if let Some(mut stdin) = c.stdin.take() {
                    let _ = stdin.write_all(card.as_bytes());
                }
                match c.wait_with_output() {
                    Ok(o) if o.status.success() && !o.stdout.is_empty() => {
                        print!("{}", String::from_utf8_lossy(&o.stdout));
                        return;
                    }
                    _ => {}
                }
            }
            print!("{}", card);
        }
        None => print!("{}", card),
    }
}

/// Parse the JSONL event log, skipping lines that do not parse.
fn read_events(p: &Path) -> Vec<Value> {
    let content = match fs::read_to_string(p) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    content
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
        .collect()
}

/// `str(o.get("value",""))` — the value of an ext_status as a string.
fn value_to_str(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Null) => "None".into(),
        None => String::new(),
        Some(other) => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// A one-line summary of an event, or None to skip it. Mirrors the
/// channel's `brief()` for tool_call, tool_result and ext_status.
fn brief(o: &Value) -> Option<String> {
    let t = o.get("type").and_then(|v| v.as_str()).unwrap_or("?");
    match t {
        "tool_call" => {
            let a = match o.get("arguments") {
                Some(Value::Null) | None => Value::Object(serde_json::Map::new()),
                Some(v) => v.clone(),
            };
            let s = if a.is_object() {
                a.get("command")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string()
            } else {
                String::new()
            };
            let s = if s.is_empty() {
                serde_json::to_string(&a).unwrap_or_default()
            } else {
                s
            };
            let name = o
                .get("name")
                .and_then(|n| n.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("?");
            Some(format!("{} {}", name, s).replace('\n', " "))
        }
        "tool_result" => {
            let is_err = o.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false);
            let mut s: String = if is_err { "error".into() } else { "ok".into() };
            if let Some(b) = o.get("bytes").and_then(|v| v.as_i64()) {
                s = format!("{} {}b", s, b);
            }
            Some(s)
        }
        "ext_status" => {
            let id = o.get("id").and_then(|v| v.as_str()).unwrap_or("status");
            let v = o.get("value");
            if matches!(v, Some(Value::Object(_)) | Some(Value::Array(_))) {
                return None;
            }
            let shown =
                KEEP.contains(&id) || id.ends_with(".error") || id == "hook.exhausted.handle";
            if !shown {
                return None;
            }
            match v {
                None | Some(Value::Null) => Some(id.to_string()),
                Some(Value::String(s)) => Some(format!("{}={}", id, s)),
                Some(other) => Some(format!(
                    "{}={}",
                    id,
                    serde_json::to_string(other).unwrap_or_default()
                )),
            }
        }
        _ => {
            let c = o.get("content").and_then(|v| v.as_str()).unwrap_or("");
            Some(c.replace('\n', " "))
        }
    }
}

/// `s[:n]` character clip with a trailing " ..." when truncated.
fn clip(s: &str, n: usize) -> String {
    let s: String = s
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = s.chars().collect();
    if chars.len() > n {
        let mut out: String = chars[..n].iter().collect();
        out.push_str(" ...");
        out
    } else {
        s
    }
}

/// `str(ts)[11:19]` — the HH:MM:SS slice of an ISO timestamp (or the
/// leftover of a relative one), empty when the slice is out of range.
fn ts_slice(ts: &str) -> String {
    let chars: Vec<char> = ts.chars().collect();
    let start = 11.min(chars.len());
    let stop = 19.min(chars.len());
    if start < stop {
        chars[start..stop].iter().collect()
    } else {
        String::new()
    }
}

fn ts_to_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        _ => v.to_string(),
    }
}

/// A JSON string (the `q()` helper): double-quoted, ASCII-escaped the way
/// Python's `json.dumps` does, keeping non-ASCII as is.
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Find an executable on PATH (the `shutil.which` equivalent).
fn which(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let paths = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&paths) {
        let cand = dir.join(name);
        if let Ok(m) = fs::metadata(&cand) {
            if m.is_file() && m.permissions().mode() & 0o111 != 0 {
                return Some(cand);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_str_escapes() {
        assert_eq!(json_str("a\"b"), "\"a\\\"b\"");
        assert_eq!(json_str("a\\b"), "\"a\\\\b\"");
        assert_eq!(json_str("a\nb"), "\"a\\nb\"");
        assert_eq!(json_str("héllo"), "\"héllo\"");
    }

    #[test]
    fn clip_truncates_by_chars() {
        let s = "x".repeat(200);
        let c = clip(&s, 400);
        assert_eq!(c.len(), 200);
        let c = clip(&"a".repeat(50), 10);
        assert_eq!(c, "aaaaaaaaaa ...");
    }

    #[test]
    fn ts_slice_iso() {
        assert_eq!(ts_slice("2026-09-17T21:13:24Z"), "21:13:24");
        assert_eq!(ts_slice(""), "");
    }

    #[test]
    fn repo_of_top_level() {
        assert_eq!(repo_of("sessions"), ".");
        assert_eq!(repo_of("repo/sessions"), "repo");
        assert_eq!(repo_of("/home/u/repo/sessions"), "repo");
    }

    #[test]
    fn prunes_hidden_and_fd_names() {
        for p in [
            ".cache",
            ".git",
            ".nix",
            "target",
            "node_modules",
            "scratch",
        ] {
            assert!(is_pruned(p), "must prune {p}");
        }
        assert!(!is_pruned("sessions"));
        assert!(!is_pruned("programming"));
    }
}
