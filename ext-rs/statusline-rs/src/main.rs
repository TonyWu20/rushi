//! The reference `statusline` extension, Rust port
//! (ui-extension-plan stage 4). The bash reference is
//! ui_extensions/statusline/.
//!
//! One long-lived process. It answers every `tick` op with a status
//! row. The row shows:
//! - live dir: the config dir, where the TUI and the loop operate
//!   ($CONFIG is exported by the host)
//! - git branch + dirty mark, TTL-cached at 3 s so a tick never
//!   spawns git more than once per 3 s (the design tick-cost note)
//! - session, model, and loop state from the tick payload
//! - cumulative usage summed over assistant_message.usage events;
//!   the host re-sends every usage-bearing message at start, so the
//!   totals survive a TUI restart from the log alone
//! - ext_status values that other extensions published into the log;
//!   the row consumes them through the tick payload's `statuses` map
//!
//! Layout: one line on wide terminals, two lines when the terminal
//! is narrow (width under 100). The host reserves one terminal row
//! per line.

use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The git cache TTL: a tick never spawns git more than once per
/// interval (the design's tick-cost note).
const GIT_TTL: Duration = Duration::from_secs(3);

/// The shared git state: branch, dirty count, last refresh time.
type GitCache = (String, u64, Option<Instant>);

/// The live dir: the config dir, where the TUI and the loop operate
/// ($CONFIG is exported by the host; unset means the current dir).
fn live_dir() -> String {
    match std::env::var("CONFIG") {
        Ok(p) => std::path::Path::new(&p)
            .parent()
            .map(|d| d.to_string_lossy().into_owned())
            .unwrap_or_else(|| ".".to_string()),
        Err(_) => ".".to_string(),
    }
}

/// Refresh the git branch + dirty count. The TTL check spawns
/// nothing; the refresh runs in a background thread, so a tick
/// reply never waits on a slow git (a cold cache can take seconds,
/// and the staleness bound is 3 x tick_ms). The thread writes the
/// shared cache; the next tick shows the last finished state.
fn git_refresh(dir: &str, cache: &Arc<Mutex<GitCache>>) {
    let now = Instant::now();
    let stale = {
        let c = cache.lock().unwrap();
        c.2
            .map(|last| now.duration_since(last) >= GIT_TTL)
            .unwrap_or(true)
    };
    if !stale {
        return;
    }
    {
        let mut c = cache.lock().unwrap();
        c.2 = Some(now);
    }
    let dir = dir.to_string();
    let cache = cache.clone();
    std::thread::spawn(move || {
        let branch = Command::new("git")
            .arg("-C")
            .arg(&dir)
            .arg("rev-parse")
            .arg("--abbrev-ref")
            .arg("HEAD")
            .output();
        let status = Command::new("git")
            .arg("-C")
            .arg(&dir)
            .arg("status")
            .arg("--porcelain")
            .output();
        let mut c = cache.lock().unwrap();
        if let Ok(b) = branch {
            if b.status.success() {
                c.0 = String::from_utf8_lossy(&b.stdout).trim().to_string();
            }
        }
        if let Ok(s) = status {
            if s.status.success() {
                c.1 = String::from_utf8_lossy(&s.stdout)
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .count() as u64;
            }
        }
    });
}

/// The ext_status values of the tick, compact `k=v` text. The row
/// shows at most two; the rest stay in the log.
fn status_text(statuses: &Value) -> String {
    let Some(map) = statuses.as_object() else {
        return String::new();
    };
    map.iter()
        .take(2)
        .map(|(k, v)| match v {
            Value::String(s) => format!("{k}={s}"),
            other => format!("{k}={other}"),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Truncate the dir to the last 16 chars with a leading ellipsis.
fn short_dir(dir: &str) -> String {
    let chars: Vec<char> = dir.chars().collect();
    if chars.len() > 16 {
        format!("...{}", chars[chars.len() - 16..].iter().collect::<String>())
    } else {
        dir.to_string()
    }
}

fn main() {
    let dir = live_dir();
    let mut in_total: u64 = 0;
    let mut out_total: u64 = 0;
    // The git cache starts "fresh": the first tick skips the git
    // spawn and the row shows git:none; the first TTL refresh lands
    // about 3 s in, on a background thread. A tick reply never
    // waits on a slow git (the staleness bound is 3 x tick_ms).
    let git: Arc<Mutex<GitCache>> =
        Arc::new(Mutex::new((String::new(), 0, Some(Instant::now()))));

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = std::io::LineWriter::new(stdout.lock());
    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            break;
        };
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            // A malformed line: no state change, no reply.
            continue;
        };
        match v.get("op").and_then(|o| o.as_str()) {
            Some("event") => {
                // Cumulative usage: the host re-sends every
                // usage-bearing message at start, so the totals
                // survive a TUI restart from the log alone.
                let ev = &v["event"];
                if ev.get("type").and_then(|t| t.as_str()) == Some("assistant_message")
                {
                    if let Some(usage) = ev.get("usage").and_then(|u| u.as_object()) {
                        in_total += usage
                            .get("input_tokens")
                            .and_then(|x| x.as_u64())
                            .unwrap_or(0);
                        out_total += usage
                            .get("output_tokens")
                            .and_then(|x| x.as_u64())
                            .unwrap_or(0);
                    }
                }
            }
            Some("tick") => {
                git_refresh(&dir, &git);
                let (branch, dirty) = {
                    let c = git.lock().unwrap();
                    (c.0.clone(), c.1)
                };
                let width = v.get("width").and_then(|w| w.as_u64()).unwrap_or(80) as usize;
                let sess = v
                    .get("session")
                    .and_then(|s| s.as_str())
                    .filter(|s| !s.is_empty())
                    .unwrap_or("no-session");
                let model = v
                    .get("model")
                    .and_then(|m| m.as_str())
                    .filter(|m| !m.is_empty())
                    .unwrap_or("no-model");
                let running = v.get("loop_running").and_then(|r| r.as_bool()).unwrap_or(false);
                let st = if running { "running" } else { "idle" };
                let stt = status_text(v.get("statuses").unwrap_or(&Value::Null));
                let dirty_mark = if dirty > 0 {
                    format!("*({dirty})")
                } else {
                    String::new()
                };
                let branch_s: &str = if branch.is_empty() { "none" } else { &branch };
                let totals = format!("in:{in_total} out:{out_total} sum:{}", in_total + out_total);
                let lines: Vec<(String, Value)> = if width >= 100 {
                    let mut l = format!(
                        " [{}] (git:{branch_s}{dirty_mark}) {sess} {model} {st} {totals}",
                        short_dir(&dir)
                    );
                    if !stt.is_empty() {
                        l.push_str(&format!(" st:{stt}"));
                    }
                    let l = l.chars().take(width).collect::<String>();
                    vec![(l, json!({"fg": "darkgray"}))]
                } else {
                    let l1 = format!(
                        " [{}] (git:{branch_s}{dirty_mark}) {sess} {st}",
                        short_dir(&dir)
                    )
                    .chars()
                    .take(width)
                    .collect::<String>();
                    let mut l2 = format!("{model} {totals}");
                    if !stt.is_empty() {
                        l2.push_str(&format!(" st:{stt}"));
                    }
                    let l2 = l2.chars().take(width).collect::<String>();
                    vec![
                        (l1, json!({"fg": "darkgray"})),
                        (l2, json!({"fg": "darkgray"})),
                    ]
                };
                let payload: Vec<Value> =
                    lines.into_iter().map(|(t, s)| json!([t, s])).collect();
                let reply = json!({"v": 1, "op": "status", "lines": payload});
                let _ = writeln!(out, "{reply}");
                let _ = out.flush();
            }
            _ => {}
        }
    }
}
