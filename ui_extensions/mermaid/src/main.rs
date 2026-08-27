//! The reference `mermaid` transform extension (ui-extension-plan
//! stage 3). A small Rust binary on the `mermaid-text` crate.
//!
//! Protocol (ui-extension.md section 4): one JSON line per request
//! on stdin, one `transformed` reply per request on stdout. The host
//! sends `transform` requests for the `fence:mermaid` spans it
//! extracts from message content. A finished reply replaces the
//! fence with the Unicode box-drawing art. A failed or over-wide
//! render stays silent: the host then shows the raw fence (per-op
//! G5 fallback).

use mermaid_text::{render_with_options, RenderOptions};
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{BufRead, Write};

/// One host request. Fields the binary does not use are optional:
/// serde skips unknown fields, so the host may grow the op later.
#[derive(Debug, Deserialize)]
struct Op {
    #[serde(default)]
    v: Option<u64>,
    #[serde(default)]
    op: Option<String>,
    #[serde(default)]
    req: Option<u64>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    width: Option<usize>,
    #[serde(default)]
    scope: Option<String>,
}

fn main() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = std::io::LineWriter::new(stdout.lock());
    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            break;
        };
        let Ok(op) = serde_json::from_str::<Op>(&line) else {
            // A malformed line: no reply, no state change.
            continue;
        };
        if op.v != Some(1) || op.op.as_deref() != Some("transform") {
            // Not a transform request: the host only sends ops this
            // extension declared, but stay correct anyway.
            continue;
        }
        let Some(req) = op.req else {
            continue;
        };
        let Some(text) = &op.text else {
            continue;
        };
        let scope = op.scope.as_deref().unwrap_or("fence:mermaid");
        if scope != "fence:mermaid" {
            // Not our target: the manifest declares exactly one
            // scope, and the host only sends scopes we declared.
            continue;
        }
        let opts = RenderOptions {
            max_width: op.width,
            // A hard budget: an over-wide render reports an error
            // instead of shipping art the pane would clip. The host
            // then shows the raw fence.
            max_width_strict: true,
            ..RenderOptions::default()
        };
        let Ok(art) = render_with_options(text, &opts) else {
            // Unparseable or over-wide source: no reply, the raw
            // fence shows.
            continue;
        };
        if art.trim().is_empty() {
            // An empty art would erase the block: stay silent.
            continue;
        }
        let lines: Vec<Value> = art.lines().map(|l| Value::String(l.to_string())).collect();
        let reply = json!({
            "v": 1,
            "op": "transformed",
            "req": req,
            "lines": lines,
        });
        let _ = writeln!(out, "{reply}");
        let _ = out.flush();
    }
}
