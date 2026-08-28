//! The reference `notify` extension, Rust port (ui-extension-plan
//! stage 4). The bash reference is ui_extensions/notify/notify.sh.
//!
//! Watches assistant_message events (the manifest's kinds filter).
//! A finished turn is stop_reason `stop` or `length`. The binary
//! rings through the host's `notify` op:
//! - kind `bell`: the host writes the terminal bell
//! - kind `osc`: the host writes OSC 0 (window title) with the
//!   finished-turn text
//!
//! Burst suppression: at start the host re-sends the whole visible
//! transcript. Ringing once per finished turn would be a burst.
//! For the first 5 s of process life the binary only remembers the
//! most recent finished turn. When the op stream goes quiet past
//! that window (or a new event arrives), live mode starts and the
//! remembered turn rings once. In live mode every finished turn
//! rings; the id guard rings nothing twice.
//!
//! Tmux tty resolution: under tmux the inner pane's terminal
//! stream may not reach the user's terminal. The binary resolves
//! the tmux client tty and writes the ring there as a
//! best-effort fallback channel. The host op stays the primary
//! path.

use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// The start burst window: ring only after this much process life
/// (the design's burst-suppression note).
const LIVE_AFTER: Duration = Duration::from_secs(5);
/// A quiet op stream for this long means the start resend is over.
const QUIET: Duration = Duration::from_secs(1);

/// One remembered finished turn: the op id and the OSC title.
#[derive(Debug)]
struct Ring {
    fid: i64,
    title: String,
}

impl Default for Ring {
    fn default() -> Self {
        Ring {
            // -1: no turn remembered yet (the log index starts at
            // 0, like the bash reference's `last_fid=-1`).
            fid: -1,
            title: String::new(),
        }
    }
}

/// The best-effort tmux client tty fallback: a bell and the OSC
/// title written to the client tty under tmux. The host op stays
/// the primary path.
fn tmux_ring(title: &str) {
    if std::env::var_os("TMUX").is_none() {
        return;
    }
    let Ok(out) = Command::new("tmux")
        .args(["display-message", "-p", "#{client_tty}"])
        .output()
    else {
        return;
    };
    let tty = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if tty.is_empty() {
        return;
    }
    let Ok(mut f) = std::fs::OpenOptions::new().write(true).open(&tty) else {
        return;
    };
    let _ = f.write_all(b"\x07");
    let _ = f.write_all(format!("\x1b]0;{title}\x07").as_bytes());
}

fn main() {
    // The reader thread feeds lines; the main loop owns the quiet
    // timeout (the bash reference's 1 s read timeout).
    let (tx, rx) = mpsc::channel::<Result<String, ()>>();
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines() {
                match line {
                    Ok(l) => {
                        if tx.send(Ok(l)).is_err() {
                            break;
                        }
                    }
                    Err(_) => {
                        let _ = tx.send(Err(()));
                        break;
                    }
                }
            }
        });
    }
    let start = Instant::now();
    let mut in_live = false;
    let mut ring = Ring::default();

    let ring_now = |title: &str| {
        let out = std::io::stdout();
        let mut out = out.lock();
        let _ = writeln!(
            out,
            "{}",
            json!({"v": 1, "op": "notify", "kind": "bell"})
        );
        let _ = writeln!(
            out,
            "{}",
            json!({"v": 1, "op": "notify", "kind": "osc", "code": 0, "args": title})
        );
        let _ = out.flush();
        tmux_ring(title);
    };

    loop {
        match rx.recv_timeout(QUIET) {
            Ok(Ok(line)) => {
                let Ok(v) = serde_json::from_str::<Value>(&line) else {
                    // A malformed line: no state change.
                    continue;
                };
                if v.get("op").and_then(|o| o.as_str()) != Some("event") {
                    continue;
                }
                let ev = v.get("event").unwrap_or(&Value::Null);
                let finished = matches!(
                    ev.get("stop_reason").and_then(|s| s.as_str()),
                    Some("stop") | Some("length")
                );
                if !finished {
                    continue;
                }
                let Some(id) = v.get("id").and_then(|x| x.as_i64()) else {
                    continue;
                };
                // The id guard: the host re-sends the transcript at
                // start; ring nothing twice.
                if id <= ring.fid {
                    continue;
                }
                ring.fid = id;
                // The title: "turn finished", plus the first 40
                // chars of the content when it has some.
                let content = ev
                    .get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("");
                let short: String = content.chars().take(40).collect();
                ring.title = if short.is_empty() {
                    "turn finished".to_string()
                } else {
                    format!("turn finished: {short}")
                };
                // Live mode starts past the burst window.
                if !in_live && start.elapsed() >= LIVE_AFTER {
                    in_live = true;
                }
                if in_live {
                    ring_now(&ring.title);
                }
            }
            Ok(Err(_)) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // The op stream went quiet. A quiet stream past the
                // burst window means the start resend is over:
                // flush the remembered finished turn, if one exists.
                if in_live {
                    continue;
                }
                if start.elapsed() < LIVE_AFTER {
                    continue;
                }
                if ring.fid < 0 {
                    continue;
                }
                in_live = true;
                ring_now(&ring.title);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}
