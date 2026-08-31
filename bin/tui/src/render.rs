//! Rendering: event -> terminal lines, and the frame layout.
//!
//! Rendering rules (docs/tui.md section 2.2 and 10.4):
//! - known category -> semantic pretty-print
//! - unknown `type` -> raw JSON with a hint
//! - unsupported `v` -> raw JSON with a "newer log version" hint
//! - malformed line -> raw text with a hint
//!
//! Text content (user/assistant messages, tool output) wraps across as
//! many lines as it needs: the `content` field is displayed in full,
//! never truncated or folded (docs/tui_feature_requests_from_human.md
//! item 1). Message content gets markdown syntax highlighting; tool
//! result text that is a complete JSON document gets JSON syntax
//! highlighting (item 4). The log file stays the record.
//!
//! The input area is a multi-line textarea in a rounded-corner border
//! whose color tracks the active model's thinking level, and is
//! customizable by a `frame` extension (docs/ui-extensions design:
//! the input area is not hardwired into the TUI; an external process
//! owns its frame through the `frame_spec` reply, never its content).

use ratatui::layout::Constraint;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};
use ratatui::Frame;
use std::collections::HashMap;

use crate::app::App;
use crate::event::{Event, EventKind};
use crate::highlight;

/// The input-area border colors, one per thinking level. Level 0 is
/// the idle gray (no thinking published); higher levels warm the
/// border from blue through green toward the "thinking" green.
fn thinking_border(level: u32) -> Color {
    match level {
        0 => Color::DarkGray,
        1 => Color::Blue,
        2 => Color::Cyan,
        3 => Color::Green,
        _ => Color::Yellow,
    }
}

/// Map the frame spec's border choice to a ratatui border type. The
/// default (a `None` border on the spec) is the rounded corners.
fn border_style(b: crate::ext::FrameBorderStyle) -> BorderType {
    match b {
        crate::ext::FrameBorderStyle::Rounded => BorderType::Rounded,
        crate::ext::FrameBorderStyle::Plain => BorderType::Plain,
        crate::ext::FrameBorderStyle::Double => BorderType::Double,
        crate::ext::FrameBorderStyle::Thick => BorderType::Thick,
    }
}

const LABEL: &str = " ";
/// Uniform left gutter in visual columns: label, gap, then content.
/// Continuation lines align under the content of the first line.
const GUTTER: usize = 12;
/// Body lines a single tool call's `command` argument may occupy.
/// The `content` field and tool result text have no cap: they are
/// displayed in full (docs/tui_feature_requests_from_human.md item 1).
const TOOL_CALL_BODY_LINES: usize = 4;
/// Events rendered into the transcript at once. The oldest are dropped
/// to bound memory on huge logs. The log file is the record.
pub const TRANSCRIPT_EVENT_CAP: usize = 2000;
/// Raw JSON lines a fallback event block may show. The fallback is for
/// opaque data the TUI does not model; the log keeps the full text.
const RAW_FALLBACK_MAX_LINES: usize = 6;

fn trunc(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        out.push('…');
    }
    out
}

/// Clip a styled row to `max` visual columns. Each character is one
/// column (the Nerd Font glyphs the statusline extension ships are
/// one column wide). A status row owns one reserved terminal row,
/// and an overflow would wrap to the next row.
fn clip_spans(spans: Vec<Span<'static>>, max: usize) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    for span in spans {
        let n = span.content.chars().count();
        if used + n > max {
            let keep = max.saturating_sub(used);
            if keep > 0 {
                let cut: String = span.content.chars().take(keep).collect();
                out.push(Span::styled(cut, span.style));
            }
            break;
        }
        used += n;
        out.push(span);
    }
    out
}

/// Wrap `text` at `wrap_w`, one gutter-prefixed line each, capped at
/// `cap` lines with a hint for the remainder.
fn body(text: &str, style: Style, cap: usize, wrap_w: usize, gutter: &str) -> Vec<Line<'static>> {
    let text = text.trim_end_matches('\n');
    if text.is_empty() {
        return Vec::new();
    }
    let wrapped = wrap_styled(vec![(style, text.to_string())], wrap_w);
    let dim = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::DIM);
    let mut out = Vec::with_capacity(cap + 1);
    for l in wrapped.iter().take(cap) {
        out.push(Line::from(vec![
            Span::raw(gutter.to_string()),
            Span::raw(l.to_string()),
        ]));
    }
    let dropped = wrapped.len().saturating_sub(cap);
    if dropped > 0 {
        out.push(Line::from(Span::styled(
            format!("{gutter}… +{dropped} more lines (full text: bin/log)"),
            dim,
        )));
    }
    out
}

/// Prefix every line with the content gutter, keeping each line's own
/// styled spans. No cap: the content is displayed in full
/// (docs/tui_feature_requests_from_human.md item 1).
fn guttered(lines: &[Line<'static>], gutter: &str) -> Vec<Line<'static>> {
    lines
        .iter()
        .map(|l| {
            let mut spans = vec![Span::raw(gutter.to_string())];
            spans.extend(l.spans.iter().cloned());
            Line::from(spans)
        })
        .collect()
}

/// Human-readable status text for a tool_result value.
fn result_status(value: Option<&serde_json::Value>, err: bool) -> String {
    let code = value
        .and_then(|v| v.get("exit_code").or_else(|| v.get("exit")))
        .and_then(|c| c.as_i64());
    match (code, err) {
        (Some(c), _) => format!("exit {c}{}", if err { " (error)" } else { "" }),
        (None, true) => "error".to_string(),
        (None, false) => "ok".to_string(),
    }
}

/// Extract display text from a tool_result value, most readable first:
/// `text`, then `stdout` + `stderr`, then a plain string value, then
/// full compact JSON as the last resort. Nothing is hidden: the last
/// resort carries the whole value (item 1: no truncation). JSON
/// highlighting is applied at render time when the text parses as a
/// complete JSON document.
fn result_text(value: Option<&serde_json::Value>, err: bool) -> String {
    let Some(v) = value else {
        return if err {
            "[missing value]".to_string()
        } else {
            String::new()
        };
    };
    if let Some(t) = v.get("text").and_then(|x| x.as_str()) {
        return t.to_string();
    }
    let mut parts: Vec<String> = Vec::new();
    if let Some(so) = v.get("stdout").and_then(|x| x.as_str()) {
        if !so.is_empty() {
            parts.push(so.to_string());
        }
    }
    if let Some(se) = v.get("stderr").and_then(|x| x.as_str()) {
        if !se.is_empty() {
            parts.push(format!("[stderr]\n{se}"));
        }
    }
    if !parts.is_empty() {
        return parts.join("\n");
    }
    if let Some(s) = v.as_str() {
        return s.to_string();
    }
    // Last resort: compact JSON so nothing is hidden. No cap: the
    // value is displayed in full.
    v.to_string()
}

/// One visual event block: a header line plus wrapped, capped body
/// lines, each continuation line aligned under the content gutter.
/// `event_id` is the log index of the event; `ext` enables the stage
/// 3 span extraction (transform owners may rewrite the message
/// spans in place). `None` ext renders exactly the built-in path.
fn event_lines(
    e: &Event,
    pending: bool,
    call_names: &HashMap<String, String>,
    width: usize,
    event_id: u64,
    ext: Option<&crate::ext::ExtHost>,
) -> Vec<Line<'static>> {
    let gutter = " ".repeat(GUTTER);
    let wrap_w = width.saturating_sub(GUTTER).max(4);
    let label_style = |fg: Color| Style::default().fg(fg).add_modifier(Modifier::BOLD);
    let dim = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::DIM);

    let mut out: Vec<Line<'static>> = Vec::new();
    match e.kind() {
        EventKind::UserMessage => {
            let content = e
                .get_str("content")
                .unwrap_or("[missing content]")
                .to_string();
            let wrapped = render_message_content(&content, event_id, ext, wrap_w);
            let mut spans = vec![Span::styled(
                format!("{LABEL}user"),
                label_style(Color::Cyan),
            )];
            if let Some(first) = wrapped.first() {
                spans.push(Span::raw("  "));
                spans.extend(first.spans.iter().cloned());
            }
            out.push(Line::from(spans));
            // The content is displayed in full: no cap, no hint.
            // An empty `wrapped` has no body line; the header stands
            // alone (an empty-slice `wrapped[1..]` panics).
            if !wrapped.is_empty() {
                out.extend(guttered(&wrapped[1..], &gutter));
            }
        }
        EventKind::AssistantMessage => {
            let content = e.get_str("content").unwrap_or("").to_string();
            let tool_calls = e.get("tool_calls").and_then(|v| v.as_array());
            let mut header = vec![Span::styled(
                format!("{LABEL}assistant"),
                label_style(Color::Green),
            )];
            if let Some(calls) = tool_calls {
                if !calls.is_empty() {
                    header.push(Span::styled(
                        format!(
                            " ({} tool call{})",
                            calls.len(),
                            if calls.len() == 1 { "" } else { "s" }
                        ),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
            }
            let wrapped = if content.is_empty() {
                Vec::new()
            } else {
                render_message_content(&content, event_id, ext, wrap_w)
            };
            if let Some(first) = wrapped.first() {
                header.push(Span::raw("  "));
                header.extend(first.spans.iter().cloned());
            }
            out.push(Line::from(header));
            // An empty content (a model output that carries only tool
            // calls) has no body line; the header stands alone.
            // FT-006: an unguarded `wrapped[1..]` panicked on the
            // first launch draw.
            if !wrapped.is_empty() {
                out.extend(guttered(&wrapped[1..], &gutter));
            }
        }
        EventKind::ToolCall => {
            let name = e.get_str("name").unwrap_or("?");
            let args = e
                .get("arguments")
                .map(|v| v.to_string())
                .unwrap_or_else(|| "[missing arguments]".to_string());
            out.push(Line::from(vec![
                Span::styled(format!("{LABEL}tool:{name}"), label_style(Color::Magenta)),
                Span::styled(
                    format!(" {}", trunc(&args, wrap_w.max(20))),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
            // The command of a bash call is the interesting part; show
            // it as its own dim line instead of raw JSON noise.
            if name == "bash" {
                if let Some(cmd) = e
                    .get("arguments")
                    .and_then(|a| a.get("command"))
                    .and_then(|c| c.as_str())
                {
                    out.extend(body(cmd, dim, TOOL_CALL_BODY_LINES, wrap_w, &gutter));
                }
            }
        }
        EventKind::ToolResult => {
            let id = e.get_str("id").unwrap_or("?");
            let name = call_names
                .get(id)
                .cloned()
                .unwrap_or_else(|| id.to_string());
            let value = e.get("value");
            let err = e.get_bool("is_error").unwrap_or(false);
            let status = result_status(value, err);
            let status_style = if err {
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
            } else {
                dim
            };
            out.push(Line::from(vec![
                Span::styled(format!("{LABEL}tool:{name}"), label_style(Color::Magenta)),
                Span::styled(format!("  {status}"), status_style),
            ]));
            let text = result_text(value, err);
            let text_style = if err {
                Style::default().fg(Color::Red)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            // Tool result text: JSON syntax highlighting when the text
            // is a complete JSON document, plain otherwise. No cap:
            // the result is displayed in full.
            let wrapped = if highlight::looks_like_json(&text) {
                wrap_json(&text, wrap_w)
            } else {
                wrap_styled(vec![(text_style, text)], wrap_w)
            };
            out.extend(guttered(&wrapped, &gutter));
        }
        EventKind::ApprovalRequest => {
            let id = e.get_str("id").unwrap_or("?");
            let prompt = e.get_str("prompt").unwrap_or("[no prompt]").to_string();
            let st = Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD);
            let mut line = vec![
                Span::styled(format!("[approval {id}] "), st),
                Span::raw(trunc(&prompt, wrap_w.max(20))),
            ];
            if pending {
                line.push(Span::styled(
                    "  [y allow] [n deny] [e edit]",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            out.push(Line::from(line));
        }
        EventKind::Approval => {
            let id = e.get_str("id").unwrap_or("?");
            let decision = e.get_str("decision").unwrap_or("?");
            let edited = e.get("arguments").is_some();
            let mut spans = vec![Span::styled(
                format!("[approval {id}] -> {decision}"),
                Style::default().fg(Color::Green),
            )];
            if edited {
                spans.push(Span::styled(
                    " (edited arguments)",
                    Style::default().fg(Color::DarkGray),
                ));
            }
            out.push(Line::from(spans));
        }
        EventKind::Cancel => {
            let target = e.get_str("target").unwrap_or("?");
            out.push(Line::from(Span::styled(
                format!("{LABEL}[cancel] target={target}"),
                dim,
            )));
        }
        EventKind::ExtStatus => {
            // Shared UI state: the transcript shows no row for the
            // event (docs/ui-extension.md section 5). The log keeps
            // the event.
        }
        EventKind::ContextExhausted => {
            let msg = e.get_str("message").unwrap_or("").to_string();
            let ns = e.get_str("new_session").unwrap_or("").to_string();
            let st = Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD);
            let mut spans = vec![Span::styled(format!("{LABEL}[context exhausted]"), st)];
            let wrapped = if msg.is_empty() {
                Vec::new()
            } else {
                wrap_styled(vec![(Style::default(), msg)], wrap_w)
            };
            if let Some(first) = wrapped.first() {
                spans.push(Span::raw("  "));
                spans.extend(first.spans.iter().cloned());
            }
            out.push(Line::from(spans));
            // The message body follows under the gutter, like the
            // error event. An empty `wrapped` leaves the header alone.
            if !wrapped.is_empty() {
                out.extend(guttered(&wrapped[1..], &gutter));
            }
            // The seeded handoff session. The status row carries the
            // one-key hint; this line names the target.
            if !ns.is_empty() {
                out.push(Line::from(vec![
                    Span::raw(gutter.clone()),
                    Span::styled(format!("handoff session: {ns}"), st),
                ]));
            }
        }
        EventKind::Error => {
            let msg = e
                .get_str("message")
                .unwrap_or("[missing message]")
                .to_string();
            let wrapped = wrap_styled(vec![(Style::default(), msg)], wrap_w);
            let mut spans = vec![Span::styled(
                format!("{LABEL}[error]"),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            )];
            if let Some(first) = wrapped.first() {
                spans.push(Span::raw("  "));
                spans.extend(first.spans.iter().cloned());
            }
            out.push(Line::from(spans));
            // The whole message is displayed, multi-line included.
            // An empty `wrapped` has no body line; the header stands
            // alone (an empty-slice `wrapped[1..]` panics).
            if !wrapped.is_empty() {
                out.extend(guttered(&wrapped[1..], &gutter));
            }
        }
        EventKind::UnknownType => {
            let ty = e.type_name().unwrap_or("?").to_string();
            out.push(Line::from(Span::styled(
                format!("{LABEL}[unknown event type \"{ty}\" — raw JSON]"),
                dim,
            )));
            for l in e.pretty_capped(RAW_FALLBACK_MAX_LINES).lines() {
                out.push(Line::from(Span::styled(
                    format!("{gutter}{l}"),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }
        EventKind::UnsupportedVersion => {
            let ty = e.type_name().unwrap_or("?").to_string();
            let v = e
                .version()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "?".into());
            out.push(Line::from(Span::styled(
                format!("{LABEL}[event {ty} v={v}: log version newer than this TUI — raw JSON]"),
                dim,
            )));
            for l in e.pretty_capped(RAW_FALLBACK_MAX_LINES).lines() {
                out.push(Line::from(Span::styled(
                    format!("{gutter}{l}"),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }
        EventKind::BadLine => {
            let raw = e.raw_line().unwrap_or("").to_string();
            out.push(Line::from(Span::styled(
                format!(
                    "{LABEL}[malformed log line] {}",
                    trunc(&raw, wrap_w.max(20))
                ),
                Style::default().fg(Color::Red).add_modifier(Modifier::DIM),
            )));
        }
    }
    out
}

/// Push the accumulated spans as one visual line, clearing `cur`.
fn push_line(cur: &mut Vec<Span<'static>>, out: &mut Vec<Line<'static>>) {
    let line: Vec<Span<'static>> = std::mem::take(cur);
    out.push(Line::from(line));
}

/// Word-wrap styled segments to a fixed width. A newline in the text is
/// a hard break: each hard line word-wraps independently. Width is
/// measured in characters; CJK and combining characters will drift a
/// few columns on non-ASCII lines (phase-1 log content is ASCII).
fn wrap_styled(segs: Vec<(Style, String)>, width: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    for (style, text) in segs {
        // A newline is a hard break, not a space: split into hard
        // lines first, then word-wrap each hard line.
        for hard in text.split('\n') {
            let mut cur: Vec<Span<'static>> = Vec::new();
            let mut cur_w = 0usize;
            for word in hard.split_inclusive(' ') {
                let w = word.chars().count();
                if cur_w + w > width && cur_w > 0 {
                    push_line(&mut cur, &mut out);
                    cur_w = 0;
                }
                if w > width {
                    // Hard-break an overlong word into width-sized
                    // pieces, each full piece its own line.
                    if cur_w > 0 {
                        push_line(&mut cur, &mut out);
                        cur_w = 0;
                    }
                    let mut piece = String::new();
                    for ch in word.chars() {
                        if piece.chars().count() == width {
                            out.push(Line::from(vec![Span::styled(
                                std::mem::take(&mut piece),
                                style,
                            )]));
                        }
                        piece.push(ch);
                    }
                    if !piece.is_empty() {
                        cur.push(Span::styled(piece.clone(), style));
                        cur_w = piece.chars().count();
                    }
                    continue;
                }
                cur_w += w;
                cur.push(Span::styled(word.to_string(), style));
            }
            if !cur.is_empty() || hard.is_empty() {
                push_line(&mut cur, &mut out);
            }
        }
    }
    out
}

/// Wrap markdown-like message content, preserving the highlight
/// module's styles per segment. Fence state spans the hard lines; a
/// trailing newline is dropped like every other body render. Segments
/// of one hard line wrap continuously (one visual line per wrap,
/// not one per token).
fn wrap_markdown(text: &str, wrap_w: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut fence = false;
    for hard in text.trim_end_matches('\n').split('\n') {
        if hard.is_empty() {
            out.push(Line::default());
            continue;
        }
        let segs = highlight::markdown_line(hard, &mut fence);
        out.extend(wrap_flow(segs, wrap_w));
    }
    out
}

/// Wrap JSON tool-result text with the highlight module's JSON styles.
fn wrap_json(text: &str, wrap_w: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    for hard in text.trim_end_matches('\n').split('\n') {
        if hard.is_empty() {
            out.push(Line::default());
            continue;
        }
        out.extend(wrap_flow(highlight::json_line(hard), wrap_w));
    }
    out
}

/// Word-wrap styled segments that form one continuous flow: all
/// segments wrap into the same visual lines, unlike [`wrap_styled`],
/// where each segment's hard lines are independent.
/// Overlong words hard-break into width-sized pieces, each piece its
/// own line, like `wrap_styled`.
fn wrap_flow(segs: Vec<(Style, String)>, width: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut cur: Vec<Span<'static>> = Vec::new();
    let mut cur_w = 0usize;
    for (style, text) in segs {
        for word in text.split_inclusive(' ') {
            let w = word.chars().count();
            if cur_w + w > width && cur_w > 0 {
                push_line(&mut cur, &mut out);
                cur_w = 0;
            }
            if w > width {
                // Hard-break an overlong word into width-sized
                // pieces, each full piece its own line.
                if cur_w > 0 {
                    push_line(&mut cur, &mut out);
                    cur_w = 0;
                }
                let mut piece = String::new();
                for ch in word.chars() {
                    if piece.chars().count() == width {
                        out.push(Line::from(vec![Span::styled(
                            std::mem::take(&mut piece),
                            style,
                        )]));
                    }
                    piece.push(ch);
                }
                if !piece.is_empty() {
                    cur.push(Span::styled(piece.clone(), style));
                    cur_w = piece.chars().count();
                }
                continue;
            }
            cur_w += w;
            cur.push(Span::styled(word.to_string(), style));
        }
    }
    if !cur.is_empty() {
        push_line(&mut cur, &mut out);
    }
    out
}

// ── transform span extraction (ui-extension-plan stage 3) ──────
/// One extracted span of a message, in content order. The `idx`
/// numbering is a pure function of the content, so it is stable
/// across transcript rebuilds: the host dedupes transform requests
/// per (event log index, span index).
#[derive(Debug)]
enum MBlock<'a> {
    /// A run of hard lines with no mermaid fence. Each line is
    /// pre-split into flow parts at split time. Fence-aware: no
    /// span inside a code fence.
    Text { parts: Vec<Vec<Part<'a>>> },
    /// A `fence:mermaid` code fence. `raw` is the whole fence
    /// (backtick lines included) for the raw fallback; `text` is
    /// the body the extension rewrites.
    Mermaid { idx: u32, raw: String, text: String },
}

/// One flow part of a hard line: markdown text, or an extracted
/// span.
#[derive(Debug, Clone)]
enum Part<'a> {
    /// Markdown flow text, highlighted with the shared fence state.
    Text(&'a str),
    /// An `inline:latex` span. `raw` is the source with delimiters;
    /// `text` is what the extension rewrites.
    Latex {
        idx: u32,
        raw: &'a str,
        text: &'a str,
    },
}

/// One marker line of a code fence: the run of leading backticks or
/// tildes (three or more) plus the info string. A closing fence has
/// an empty info string.
fn fence_marker(line: &str) -> Option<(char, usize, &str)> {
    let t = line.trim_start();
    let bytes = t.as_bytes();
    let first = *bytes.first()?;
    if first != b'`' && first != b'~' {
        return None;
    }
    let run = bytes.iter().take_while(|&&b| b == first).count();
    if run < 3 {
        return None;
    }
    Some((first as char, run, t[run..].trim()))
}

/// Split one hard line into flow parts. Outside a code fence, a
/// `$$...$$` or `$...$` pair becomes a LaTeX span (an
/// `inline:latex` transform target). Inside a fence, on fence
/// marker lines, and for a `$` with no closing partner, the
/// dollars stay literal. The span `idx` numbering continues the
/// counter, in content order.
fn line_parts<'a>(line: &'a str, in_fence: bool, next_idx: &mut u32) -> Vec<Part<'a>> {
    if in_fence || fence_marker(line).is_some() || !line.contains('$') {
        return vec![Part::Text(line)];
    }
    let mut out: Vec<Part<'a>> = Vec::new();
    let bytes = line.as_bytes();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            i += 1;
            continue;
        }
        let display = i + 1 < bytes.len() && bytes[i + 1] == b'$';
        if display {
            let rest = &line[i + 2..];
            if let Some(off) = rest.find("$$") {
                let close = i + 2 + off;
                if close > i + 2 {
                    if start < i {
                        out.push(Part::Text(&line[start..i]));
                    }
                    out.push(Part::Latex {
                        idx: *next_idx,
                        raw: &line[i..close + 2],
                        text: &line[i + 2..close],
                    });
                    *next_idx += 1;
                    start = close + 2;
                    i = start;
                    continue;
                }
            }
            // No closing marker: the dollars stay literal.
            i += 2;
            continue;
        }
        let rest = &line[i + 1..];
        if let Some(off) = rest.find('$') {
            let close = i + 1 + off;
            if close > i + 1 {
                if start < i {
                    out.push(Part::Text(&line[start..i]));
                }
                out.push(Part::Latex {
                    idx: *next_idx,
                    raw: &line[i..close + 1],
                    text: &line[i + 1..close],
                });
                *next_idx += 1;
                start = close + 1;
                i = start;
                continue;
            }
        }
        // A lone dollar stays literal.
        i += 1;
    }
    if start < line.len() {
        out.push(Part::Text(&line[start..]));
    }
    if out.is_empty() {
        out.push(Part::Text(line));
    }
    out
}

/// The code-fence state before each hard line of `content`: true
/// inside a code fence. A miniature of the markdown fence rules:
/// three or more leading backticks or tildes opens; a matching run
/// of the same character with an empty info string closes; a fence
/// opens only outside another fence. Known divergence from the
/// highlighter: it closes on any three-backtick line, tagged or
/// not. For a tagged marker inside a fence the two disagree about
/// the inline state; the visual render is identical, and span
/// extraction and highlight only diverge for that pathological
/// content (ui-extension-plan stage 3, LaTeX eligibility note).
fn fence_states(content: &str) -> Vec<bool> {
    let content = content.trim_end_matches('\n');
    let lines: Vec<&str> = content.split('\n').collect();
    let mut states = vec![false; lines.len()];
    let mut open: Option<(char, usize)> = None;
    for (i, line) in lines.iter().enumerate() {
        states[i] = open.is_some();
        if let Some((c, run, info)) = fence_marker(line) {
            match open {
                Some((oc, orun)) => {
                    if c == oc && info.is_empty() && run >= orun {
                        open = None;
                    }
                }
                None => open = Some((c, run)),
            }
        }
    }
    states
}

/// Split message content into blocks: `fence:mermaid` code fences
/// become [`MBlock::Mermaid`] blocks; everything else (including
/// other code fences and their bodies) stays in the text flow, with
/// LaTeX spans extracted per line.
///
/// Mermaid fences are recognized by the info string `mermaid`
/// (case-insensitive), outside a code fence. All span indices are
/// numbered in content order at split time: that is what makes
/// them stable across transcript rebuilds.
fn message_blocks(content: &str) -> Vec<MBlock<'_>> {
    let content = content.trim_end_matches('\n');
    let lines: Vec<&str> = content.split('\n').collect();
    let states = fence_states(content);
    let mut blocks: Vec<MBlock> = Vec::new();
    let mut next_idx: u32 = 0;
    let mut i = 0usize;
    let mut run_start = 0usize;
    while i < lines.len() {
        let line = lines[i];
        // Inside a code fence: plain text run.
        if states[i] {
            i += 1;
            continue;
        }
        let marker = fence_marker(line);
        if let Some((c, run, info)) = marker {
            if !info.eq_ignore_ascii_case("mermaid") {
                // A non-mermaid fence: flow text, until its close.
                i += 1;
                continue;
            }
            // A mermaid fence: extract it. The body is the
            // transform target; the whole fence is the raw
            // fallback.
            if run_start < i {
                let parts: Vec<Vec<Part>> = (run_start..i)
                    .map(|j| line_parts(lines[j], states[j], &mut next_idx))
                    .collect();
                if !parts.is_empty() {
                    blocks.push(MBlock::Text { parts });
                }
            }
            let open_run = run;
            let mut raw_lines: Vec<&str> = vec![line];
            let mut text_lines: Vec<&str> = Vec::new();
            i += 1;
            loop {
                let l2 = lines[i];
                if let Some((c2, run2, info2)) = fence_marker(l2) {
                    if c2 == c && info2.is_empty() && run2 >= open_run {
                        raw_lines.push(l2);
                        i += 1;
                        break;
                    }
                }
                raw_lines.push(l2);
                text_lines.push(l2);
                i += 1;
                if i >= lines.len() {
                    // An unterminated fence runs to the end of the
                    // content: standard markdown behavior.
                    break;
                }
            }
            let raw = raw_lines.join("\n");
            let text = text_lines.join("\n");
            blocks.push(MBlock::Mermaid {
                idx: next_idx,
                raw,
                text,
            });
            next_idx += 1;
            run_start = i;
            continue;
        }
        i += 1;
    }
    if run_start < lines.len() {
        let parts: Vec<Vec<Part>> = (run_start..lines.len())
            .map(|j| line_parts(lines[j], states[j], &mut next_idx))
            .collect();
        if !parts.is_empty() {
            blocks.push(MBlock::Text { parts });
        }
    }
    blocks
}

/// Render one user/assistant message with the stage 3 span
/// extraction.
///
/// - A `fence:mermaid` block: the host requests a transform for the
///   fence body. A finished reply replaces the fence with the
///   extension's lines, guttered like the transcript. A missing
///   owner, a pending or timed-out request, or a dead extension
///   shows the raw fence (G5 fallback).
/// - An `inline:latex` span: the host requests a transform for the
///   span. A finished reply replaces the span text in place (the
///   reply lines join into one line). No owner: the raw span text
///   renders, exactly like the built-in path.
///
/// The `ext == None` path renders the content with [`wrap_markdown`],
/// byte-identical to the pre-stage-3 renderer.
fn render_message_content(
    content: &str,
    event_id: u64,
    ext: Option<&crate::ext::ExtHost>,
    wrap_w: usize,
) -> Vec<Line<'static>> {
    if ext.is_none() {
        return wrap_markdown(content, wrap_w);
    }
    let host = ext.expect("checked above");
    let blocks = message_blocks(content);
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut fence = false;
    for block in blocks {
        match block {
            MBlock::Text { parts } => {
                for line_parts in parts {
                    let mut segs: Vec<(Style, String)> = Vec::new();
                    for part in line_parts {
                        match part {
                            Part::Text(s) => {
                                segs.extend(highlight::markdown_line(s, &mut fence));
                            }
                            Part::Latex { idx, raw, text } => {
                                let req =
                                    host.request_span(event_id, idx, "inline:latex", text, wrap_w);
                                let replaced = req
                                    .and_then(|_| host.span_lines(event_id, idx))
                                    .map(|ls| {
                                        ls.iter()
                                            .map(|l| l.text.clone())
                                            .collect::<Vec<_>>()
                                            .join(" ")
                                    })
                                    .unwrap_or_else(|| raw.to_string());
                                segs.push((Style::default(), replaced));
                            }
                        }
                    }
                    // One hard line always renders at least one
                    // visual line (the pre-stage-3 invariant the
                    // event header slices `wrapped[1..]` on).
                    if segs.is_empty() {
                        out.push(Line::default());
                    } else {
                        out.extend(wrap_flow(segs, wrap_w));
                    }
                }
            }
            MBlock::Mermaid { idx, raw, text } => {
                let req = host.request_span(event_id, idx, "fence:mermaid", &text, wrap_w);
                // An empty reply erases the block: treat it as no
                // reply and show the raw fence.
                let art = req
                    .and_then(|_| host.span_lines(event_id, idx))
                    .filter(|ls| !ls.is_empty());
                match art {
                    Some(lines) => {
                        // The reply lines are width-independent; the
                        // host wraps them to the pane width with the
                        // transcript gutter, like every extension
                        // reply.
                        out.extend(ext_lines_guttered(&lines, wrap_w + GUTTER));
                    }
                    None => {
                        // The raw fence shows. A private fence state
                        // highlights the fence lines; the shared
                        // state is untouched, because the block is a
                        // balanced fence.
                        let mut private = fence;
                        for hard in raw.split('\n') {
                            let segs = highlight::markdown_line(hard, &mut private);
                            if segs.is_empty() {
                                out.push(Line::default());
                            } else {
                                out.extend(wrap_flow(segs, wrap_w));
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

/// The static help row. The quit hint comes first: it is the safety
/// relevant one, and terminal clipping always eats the right end.
pub fn help_line(running: bool) -> String {
    let run_key = if running { "Ctrl+C stop" } else { "Ctrl+R run" };
    format!(
        " q×2 quit · {run_key} · Ctrl+E edit · Enter send · Ctrl-J ⏎ · vim · y/n/e · Tab · PgUp/Dn"
    )
}

/// The status/help row content as terminal lines (one per row).
///
/// The TUI flash wins; then the status extension row (its lines or
/// the dead hint); then the built-in content. A status reply may
/// carry up to two lines (the narrow two-line layout,
/// ui-extension-plan stage 2); more than two is capped at two.
fn status_rows(
    app: &App,
    host: &crate::ext::ExtHost,
    running: bool,
    row_width: usize,
) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    if let Some(msg) = app.status() {
        return vec![Line::from(Span::styled(
            format!(" {msg}"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))];
    }
    if app.pending_name().is_some() {
        return vec![Line::from(Span::styled(
            " Enter confirm · Esc cancel · q×2 quit",
            dim,
        ))];
    }
    let last_line = app
        .active()
        .and_then(|s| app.loop_state(s))
        .and_then(|l| l.last_line.clone());
    match host.status_row() {
        crate::ext::StatusRow::Lines(lines) => {
            let rows: Vec<Line<'static>> = lines
                .iter()
                .take(2)
                .map(|l| {
                    let spans: Vec<Span<'static>> = l
                        .spans
                        .iter()
                        .map(|s| Span::styled(s.text.clone(), s.style))
                        .collect();
                    // A status row owns one reserved terminal row. A
                    // too-wide row would wrap, so the host clips it
                    // to the row width (the extension's own width is
                    // the last tick's; the terminal may have resized
                    // since).
                    Line::from(clip_spans(spans, row_width))
                })
                .collect();
            // An empty reply must not erase the row slot (the help
            // content shares it): reserve one blank row.
            if rows.is_empty() {
                vec![Line::default()]
            } else {
                rows
            }
        }
        crate::ext::StatusRow::DeadHint(hint) => vec![Line::from(Span::styled(
            format!(" {hint}"),
            Style::default().fg(Color::Red).add_modifier(Modifier::DIM),
        ))],
        crate::ext::StatusRow::Builtin => {
            // The pending handoff hint wins the built-in slot: it is
            // the action that unblocks the session. The flash still
            // wins over it (it names the result of the user's own
            // key press).
            if let Some(name) = app.pending_handoff() {
                return vec![Line::from(Span::styled(
                    format!(" context exhausted — press h to hand off to {name} · q×2 quit "),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ))];
            }
            match last_line {
                Some(l) => vec![Line::from(Span::styled(
                    format!(" » {}", trunc(&l, row_width.saturating_sub(4))),
                    dim,
                ))],
                None => vec![Line::from(Span::styled(help_line(running), dim))],
            }
        }
    }
}

/// Render the transcript into wrapped visual lines, separated by blank
/// lines. The oldest events beyond `TRANSCRIPT_EVENT_CAP` are dropped.
/// The result is cached per (events version, width, reply version)
/// by the caller.
///
/// Extension replies fold in (ui-extension-plan stage 1): when an
/// extension owns an event kind (the first extension in the composed
/// sequence that lists the kind) and a valid `lines` reply is cached
/// for the event, the extension's styled lines replace the built-in
/// render. A missing, stale, or timed-out reply falls back to the
/// built-in render (per-op G5 fallback).
pub fn build_transcript_lines(
    app: &App,
    width: usize,
    ext: Option<&crate::ext::ExtHost>,
) -> Vec<Line<'static>> {
    let names = app.call_names();
    let pending = app.oldest_pending_approval().is_some();
    let events = app.events();
    let start = events.len().saturating_sub(TRANSCRIPT_EVENT_CAP);
    let mut all: Vec<Line<'static>> = Vec::new();
    for (i, e) in events[start..].iter().enumerate() {
        // ext_status is shared UI state: suppressed from the transcript
        // by default. ext_status events add no rows, and add no blank
        // separators. The log keeps ext_status events
        // (docs/ui-extension.md 5).
        if e.kind() == EventKind::ExtStatus {
            continue;
        }
        if !all.is_empty() {
            all.push(Line::from(""));
        }
        let event_id = (start + i) as u64;
        let segs = if let Some(owner) = ext.and_then(|h| h.owner_for_kind(e.kind())) {
            match ext.unwrap().lookup_lines(owner, event_id) {
                Some(lines) => ext_lines_guttered(&lines, width),
                // No valid reply for this event: the built-in render
                // is the fallback.
                None => event_lines(e, pending, &names, width.max(GUTTER + 8), event_id, ext),
            }
        } else {
            event_lines(e, pending, &names, width.max(GUTTER + 8), event_id, ext)
        };
        all.extend(segs);
    }
    all
}

/// Extension reply lines into the transcript: the extension returns
/// width-independent styled lines; the host wraps them to the pane
/// width with the same gutter as the built-in render (docs/ui-
/// extension.md section 4).
fn ext_lines_guttered(lines: &[crate::ext::ExtLine], width: usize) -> Vec<Line<'static>> {
    let gutter = " ".repeat(GUTTER);
    let wrap_w = width.saturating_sub(GUTTER).max(4);
    let segs: Vec<(Style, String)> = lines
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| (s.style, s.text.clone())))
        .collect();
    let wrapped = wrap_styled(segs, wrap_w);
    guttered(&wrapped, &gutter)
}

/// The whole frame: bordered panel with session title, transcript,
/// optional approval banner, input line, and the status/help row.
/// When a status extension exists, its row owns that last line
/// (ui-extension-plan stage 1 layout); otherwise the built-in
/// help/status content shows there.
pub fn draw(
    f: &mut Frame,
    app: &mut App,
    cursor: &mut Option<(u16, u16)>,
    host: &crate::ext::ExtHost,
) {
    *cursor = None;
    let area = f.area();
    if area.width < 12 || area.height < 6 {
        return;
    }

    // The active session id and the status bits need no live borrow:
    // the cache rebuild below takes a mutable borrow of `app`.
    let active = app.active().cloned();
    let session_label = match &active {
        Some(s) => s.to_string(),
        None => match app.pending_name() {
            Some(n) => format!("new — {n}"),
            None => "none".to_string(),
        },
    };
    let running = active
        .as_ref()
        .map(|s| app.loop_running(s))
        .unwrap_or(false);
    let mut status_bits: Vec<Span<'static>> = vec![Span::styled(
        if running { " [running] " } else { " [idle] " },
        Style::default()
            .fg(if running {
                Color::Green
            } else {
                Color::DarkGray
            })
            .add_modifier(Modifier::BOLD),
    )];
    if app.other_running_loops() > 0 {
        status_bits.push(Span::styled(
            format!(" +{} loop", app.other_running_loops()),
            Style::default().fg(Color::Yellow),
        ));
    }

    let title_left = Line::from(vec![Span::styled(
        format!("Session: {session_label}"),
        Style::default().add_modifier(Modifier::BOLD),
    )]);
    let block = Block::bordered()
        .title(title_left)
        .title(Line::from(status_bits).right_aligned());
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Rows inside the border:
    //   transcript (fill)
    //   approval banner (1, only when pending)
    //   input line (1)
    //   help/status row (1)
    let banner = app.oldest_pending_approval().is_some();
    // The input-area frame: the `frame` extension's last valid spec,
    // or the host built-in (rounded border, thinking-level color).
    // The frame owns the border style, label, and interior height;
    // it never owns the draft content (docs/ui-extensions design:
    // the input area is customizable, not hardwired).
    let frame = host.frame_spec();
    // The interior width a draft line wraps to: the input box spans the
    // full main-interior width, its border takes two columns. Every
    // wrapping / sizing / scroll below keys off this so a long line
    // breaks at the box edge instead of running off it.
    let input_wrap_w = inner.width.saturating_sub(2) as usize;
    // Default interior: fit the draft (2..=6 display rows) so a
    // multi-line message is shown in full. A `frame` extension's
    // explicit height still overrides (the input area is customizable,
    // not hardwired).
    let input_interior = frame
        .as_ref()
        .and_then(|f| f.height)
        .unwrap_or_else(|| app.draft_lines(input_wrap_w).clamp(2, 6));
    // The bordered box is the interior rows plus a top and bottom
    // border row each.
    let input_area_h = (input_interior + 2) as u16;
    // Status/help row content, computed before the layout: the layout
    // reserves one terminal row per status line. A status extension
    // reply may carry two lines (the narrow two-line layout,
    // ui-extension-plan stage 2).
    let status_lines = status_rows(app, host, running, inner.width as usize);
    let status_n = status_lines.len() as u16;
    let constraints = if banner {
        vec![
            Constraint::Min(2),
            Constraint::Length(1),
            Constraint::Length(input_area_h),
            Constraint::Length(status_n),
        ]
    } else {
        vec![
            Constraint::Min(3),
            Constraint::Length(input_area_h),
            Constraint::Length(status_n),
        ]
    };
    let rows = ratatui::layout::Layout::vertical(constraints).split(inner);

    // transcript
    let t_area = rows[0];
    let t_width = t_area.width.saturating_sub(2) as usize;
    let h = t_area.height as usize;
    let scroll = app.scroll();
    app.set_viewport_height(h);
    let lines = app.transcript_lines(t_width, Some(host));
    let total = lines.len();
    let start = total.saturating_sub(scroll + h);
    let window = &lines[start..];
    if window.is_empty() {
        let placeholder = match app.active() {
            Some(_) => Line::from(Span::styled(
                " (no events yet — type a message below, then Ctrl+R to run the loop)",
                Style::default().fg(Color::DarkGray),
            )),
            None => Line::from(Span::styled(
                " (no session yet — type the new session name below, Enter confirms)",
                Style::default().fg(Color::DarkGray),
            )),
        };
        let p = Paragraph::new(vec![placeholder]);
        f.render_widget(p, t_area);
    } else {
        let p = Paragraph::new(window.to_vec());
        f.render_widget(p, t_area);
    }

    let mut row = 1usize;
    // approval banner
    if banner {
        let p = app.oldest_pending_approval().unwrap();
        let text = format!(
            "approval {} : {}   [y allow]  [n deny]  [e edit]",
            p.request_id,
            p.prompt
                .clone()
                .unwrap_or_else(|| "(no prompt)".to_string())
        );
        let l = Line::from(vec![Span::styled(
            text,
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )]);
        f.render_widget(Paragraph::new(l), rows[row]);
        row += 1;
    }

    // input area: a bordered, rounded-corner box showing two (or the
    // frame spec's) editor lines, colored by the thinking level. The
    // frame extension may override the border style, label, and
    // interior height; the draft content and cursor stay host-owned.
    let i_area = rows[row];
    let naming = app.pending_name().is_some();
    let border_type = frame
        .as_ref()
        .and_then(|f| f.border)
        .map(border_style)
        .unwrap_or(BorderType::Rounded);
    let border_color = frame
        .as_ref()
        .and_then(|f| f.label.as_ref())
        .and_then(|(_, s)| s.fg)
        .unwrap_or_else(|| thinking_border(app.thinking_level()));
    // The box border. A frame label replaces the built-in title.
    let title = if let Some((flabel, lstyle)) = frame.as_ref().and_then(|f| f.label.as_ref()) {
        Line::from(Span::styled(
            flabel
                .iter()
                .take(1)
                .map(|l| l.text.clone())
                .collect::<Vec<_>>()
                .join(" "),
            *lstyle,
        ))
    } else {
        Line::from(Span::styled(
            app.editor()
                .command_line_label()
                .unwrap_or_else(|| app.editor_mode_label()),
            Style::default()
                .fg(Color::Black)
                .bg(border_color)
                .add_modifier(Modifier::BOLD),
        ))
    };
    let input_block = Block::bordered()
        .border_type(border_type)
        .border_style(Style::default().fg(border_color))
        .title(title);
    let inner_i = input_block.inner(i_area);
    f.render_widget(input_block, i_area);

    // The editor lines. `naming` shows the single-line name input
    // (the session-name bar, pre-active-session). Otherwise the
    // multi-line editor, scrolled by `edit_scroll`. Keep the cursor
    // row visible in the window of `input_interior` lines.
    if !naming {
        app.editor_scroll_to_cursor(input_interior);
    }
    let scroll = app.edit_scroll();
    let ed_lines: Vec<String> = if naming {
        vec![app.pending_name().unwrap_or_default().to_string()]
    } else {
        app.editor().display(scroll, input_interior)
    };
    // One paragraph per editor line. No line-level highlight; the
    // cursor row shows a single inverted block cell at the caret
    // column so the position is always visible, and the hardware
    // cursor sits just after it.
    let cursor_row = if naming {
        0
    } else {
        let (r, _c) = app.editor().cursor();
        r.saturating_sub(app.edit_scroll())
    };
    let cursor_col = if naming {
        // One past the rendered trailing `_`: the `> ` prefix plus
        // the name plus the underscore.
        app.pending_name().map_or(3, |n| 3 + n.chars().count())
    } else {
        let (_r, c) = app.editor().cursor();
        c
    };
    // The search command line owns the input: the prompt renders in
    // the box title and the text-area cursor block stays off.
    let in_command_line = app.editor().command_line_label().is_some();
    let top = inner_i.y;
    for (j, l) in ed_lines.iter().enumerate() {
        let y = top + j as u16;
        if y >= top + inner_i.height {
            break;
        }
        let sub = ratatui::layout::Rect {
            x: inner_i.x,
            y,
            width: inner_i.width,
            height: 1,
        };
        let line = if naming && j == 0 {
            Line::from(vec![
                Span::styled("> ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(l.clone()),
                Span::styled("_", Style::default().add_modifier(Modifier::BOLD)),
            ])
        } else if j == cursor_row && !in_command_line {
            // The cursor row: render the characters up to the caret
            // in normal style, then one inverted block cell. Only
            // this one cell is inverted so the caret is always
            // visible even when the hardware cursor is not blinking.
            // The rest of the line stays plain. In the on-char
            // modes (normal, replace, visual) the block covers the
            // char under the cursor, so that char is drawn exactly
            // once (the highlighted cell); the insert caret is a
            // blank block cell that keeps the char under it.
            let (before, caret, after) =
                cursor_line_spans(l, cursor_col, app.editor().mode().cursor_on_char());
            let style = Style::default().bg(Color::White).fg(Color::Black);
            let mut spans = Vec::new();
            if !before.is_empty() {
                spans.push(Span::raw(before.iter().collect::<String>()));
            }
            spans.push(Span::styled(caret.to_string(), style));
            if !after.is_empty() {
                spans.push(Span::raw(after.iter().collect::<String>()));
            }
            Line::from(spans)
        } else {
            Line::from(Span::raw(l.clone()))
        };
        f.render_widget(Paragraph::new(line), sub);
    }
    // The cursor position: on the cursor line, on the block cell.
    // In command-line mode it lands on the prompt in the box title.
    if !app.should_quit() {
        if let Some(prompt) = app.editor().command_line_label() {
            let prompt_w = prompt.chars().count().saturating_sub(1);
            let cx = inner_i.x + 1 + prompt_w as u16;
            let cy = inner_i.y.saturating_sub(1);
            *cursor = Some((cx.min(inner_i.x + inner_i.width), cy));
        } else {
            let cy = inner_i.y + cursor_row as u16;
            // The hardware cursor lands on the block cell (the
            // visible caret). At end-of-line, `cursor_col` points
            // at the blank block cell, which is still inside the
            // row.
            let cx = inner_i.x + cursor_col as u16;
            if cy < inner_i.y + inner_i.height {
                *cursor = Some((cx.min(inner_i.x + inner_i.width), cy));
            }
        }
    }
    row += 1;

    // status/help row: the TUI flash wins; then the status extension
    // row (its lines or the dead hint); then the built-in content.
    // The status slot is one layout cell of `status_n` rows; split it
    // into single-row rects and render one line each.
    let s_area = rows[row];
    for (i, l) in status_lines.iter().enumerate() {
        if i as u16 >= s_area.height {
            break;
        }
        let sub = ratatui::layout::Rect {
            x: s_area.x,
            y: s_area.y + i as u16,
            width: s_area.width,
            height: 1,
        };
        f.render_widget(Paragraph::new(l.clone()), sub);
    }
}

/// The spans of the editor cursor row: `(before, caret, after)`.
/// `on_char` is true when the mode rests on the character at the
/// caret (normal / replace / visual): the block covers that char,
/// so it is drawn once and the line continues from the next char.
/// In insert the caret is a blank block cell and the char under it
/// stays drawn.
fn cursor_line_spans(line: &str, col: usize, on_char: bool) -> (Vec<char>, char, Vec<char>) {
    let chars: Vec<char> = line.chars().collect();
    let cc = col.min(chars.len());
    if on_char && cc < chars.len() {
        (chars[..cc].to_vec(), chars[cc], chars[cc + 1..].to_vec())
    } else {
        (chars[..cc].to_vec(), ' ', chars[cc..].to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::produce;
    use serde_json::json;

    fn join(lines: &[ratatui::text::Line]) -> String {
        lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn app_with_session(events: Vec<Event>) -> App {
        let mut app = App::new();
        app.set_active(crate::port::SessionId::new("s1"), events);
        app
    }

    #[test]
    fn wrap_flow_preserves_spaces() {
        // Spaces between plain and styled tokens must survive the
        // wrap (the transcript is a view: no text is lost).
        let text = "4. **Cursor.** While naming the cursor x is `p + n length`, i.e. one column *left* of the rendered `_` underline.";
        let app = app_with_session(vec![produce::user_message(text)]);
        let lines = build_transcript_lines(&app, 80, None);
        let joined: String = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect::<Vec<_>>()
            .join("\n");
        // The gutter prefix is stripped out: compare the content only.
        let content_only: String = joined
            .lines()
            .map(|l| l.trim_start_matches(' '))
            .collect::<Vec<_>>()
            .join("\n");
        // The gutter prefix and label are stripped; wrap breaks become
        // single spaces, so the original phrase is searchable.
        let flat: String = content_only
            .lines()
            .map(|l| l.trim())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(flat.contains("i.e. one column"), "spaces lost: {flat}");
        assert!(
            flat.contains("*left* of the rendered"),
            "italic token lost its neighbours: {flat}"
        );
    }

    #[test]
    fn semantic_lines_for_known_categories() {
        let evs = vec![
            produce::user_message("rename the file"),
            Event::parse_line(
                r#"{"v":1,"type":"assistant_message","ts":"t","content":"Use the bash tool.","tool_calls":[{"id":"c1","name":"bash","arguments":{"command":"mv a b"}}]}"#,
            )
            .unwrap(),
            Event::parse_line(
                r#"{"v":1,"type":"tool_call","ts":"t","id":"c1","name":"bash","arguments":{"command":"mv a b"}}"#,
            )
            .unwrap(),
            Event::parse_line(
                r#"{"v":1,"type":"tool_result","ts":"t","id":"c1","value":{"text":"a b\n","exit_code":0,"stdout":"a b\n","stderr":"","timed_out":false,"truncated":false},"is_error":false}"#,
            )
            .unwrap(),
        ];
        let app = app_with_session(evs);
        let joined = join(&build_transcript_lines(&app, 80, None));
        assert!(joined.contains("user"), "{joined}");
        assert!(joined.contains("rename the file"), "{joined}");
        assert!(joined.contains("assistant"), "{joined}");
        assert!(joined.contains("tool:bash"), "{joined}");
        assert!(joined.contains("exit 0"), "{joined}");
        assert!(
            joined.contains("tool call"),
            "tool-call count hint missing: {joined}"
        );
        // Tool result shows the output text, not the raw JSON envelope.
        assert!(joined.contains("a b"), "tool text missing: {joined}");
        assert!(
            !joined.contains("\"exit_code\""),
            "raw JSON leaked into the result view: {joined}"
        );
    }

    #[test]
    fn empty_assistant_content_renders_header_without_panic() {
        // FT-006: a model output that carries only tool calls has an
        // empty content string. The launch draw must not panic on the
        // `wrapped[1..]` slice of an empty vec.
        let evs = vec![
            Event::parse_line(
                r#"{"v":1,"type":"assistant_message","ts":"t","content":"","tool_calls":[{"id":"c1","name":"bash","arguments":{"command":"ls"}}]}"#,
            )
            .unwrap(),
            Event::parse_line(
                r#"{"v":1,"type":"assistant_message","ts":"t","content":"\n\n","tool_calls":[{"id":"c2","name":"read","arguments":{"path":"f"}}]}"#,
            )
            .unwrap(),
        ];
        let app = app_with_session(evs);
        let joined = join(&build_transcript_lines(&app, 80, None));
        assert!(joined.contains("assistant"), "header missing: {joined}");
        assert!(
            joined.contains("tool call"),
            "the tool-call hint must survive the empty content: {joined}"
        );
    }

    #[test]
    fn long_user_message_wraps_to_many_lines() {
        let content = format!("line one\nline two\n{}", "word ".repeat(80).trim());
        let evs = vec![produce::user_message(&content)];
        let app = app_with_session(evs);
        let lines = build_transcript_lines(&app, 60, None);
        let joined = join(&lines);
        // The full text is visible across wrapped lines, no 96-char
        // cutoff: content well past 96 chars must survive.
        assert!(
            joined.contains("word word word word word word word word"),
            "long content was truncated: {joined}"
        );
        for l in &lines {
            assert!(
                l.to_string().chars().count() <= 60,
                "visual line wider than the pane: {l:?}"
            );
        }
    }

    #[test]
    fn event_body_is_displayed_in_full() {
        let content = (0..60)
            .map(|i| format!("line {i:02}"))
            .collect::<Vec<_>>()
            .join("\n");
        let evs = vec![produce::user_message(&content)];
        let app = app_with_session(evs);
        let joined = join(&build_transcript_lines(&app, 60, None));
        // Item 1: the content is shown in full — no cap, no hint.
        assert!(
            !joined.contains("more lines"),
            "content must not fold: {joined}"
        );
        for i in 0..60 {
            assert!(
                joined.contains(&format!("line {i:02}")),
                "line {i:02} missing from the transcript"
            );
        }
    }

    /// The handoff marker renders its message and names the seeded
    /// session (correction 57). The one-key hint lives on the status
    /// row, not in the transcript.
    #[test]
    fn context_exhausted_line_names_the_handoff_session() {
        let evs = vec![
            Event::parse_line(
                r#"{"v":1,"type":"user_message","ts":"t","content":"the task"}"#,
            )
            .unwrap(),
            Event::parse_line(
                r#"{"v":1,"type":"context_exhausted","ts":"t","message":"context budget exhausted after compaction. Run the handoff.","new_session":"s1_h1"}"#,
            )
            .unwrap(),
        ];
        let app = app_with_session(evs);
        let joined = join(&build_transcript_lines(&app, 80, None));
        assert!(
            joined.contains("[context exhausted]"),
            "the marker label renders: {joined}"
        );
        assert!(
            joined.contains("handoff session: s1_h1"),
            "the seed is named"
        );
        assert!(joined.contains("the task"), "the log history still renders");
    }

    /// A marker that seeded no session (a failed summary call) shows
    /// no handoff line; the transcript still renders the marker.
    #[test]
    fn context_exhausted_line_without_a_seed_shows_no_handoff() {
        let evs = vec![Event::parse_line(
            r#"{"v":1,"type":"context_exhausted","ts":"t","message":"m","new_session":""}"#,
        )
        .unwrap()];
        let app = app_with_session(evs);
        let joined = join(&build_transcript_lines(&app, 80, None));
        assert!(joined.contains("[context exhausted]"));
        assert!(!joined.contains("handoff session:"), "no seed, no line");
    }

    #[test]
    fn long_tool_result_is_displayed_in_full() {
        let text = (0..100)
            .map(|i| format!("tool out {i:03}"))
            .collect::<Vec<_>>()
            .join("\n");
        let value = json!({ "text": text, "exit_code": 0 });
        let evs = vec![Event::parse_line(&format!(
            r#"{{"v":1,"type":"tool_result","ts":"t","id":"c9","value":{value},"is_error":false}}"#
        ))
        .unwrap()];
        let app = app_with_session(evs);
        let joined = join(&build_transcript_lines(&app, 80, None));
        assert!(
            !joined.contains("more lines"),
            "tool result must not fold: {joined}"
        );
        for i in [0, 1, 50, 99] {
            assert!(
                joined.contains(&format!("tool out {i:03}")),
                "tool line {i} missing"
            );
        }
    }

    #[test]
    fn long_error_message_is_displayed_in_full() {
        let msg = (0..50)
            .map(|i| format!("trace {i:02}"))
            .collect::<Vec<_>>()
            .join("\n");
        let evs = vec![Event::parse_line(&format!(
            r#"{{"v":1,"type":"error","ts":"t","message":{}}}"#,
            serde_json::json!(msg)
        ))
        .unwrap()];
        let app = app_with_session(evs);
        let joined = join(&build_transcript_lines(&app, 80, None));
        assert!(
            !joined.contains("more lines"),
            "error must not fold: {joined}"
        );
        for i in [0, 49] {
            assert!(
                joined.contains(&format!("trace {i:02}")),
                "error line {i} missing"
            );
        }
    }

    #[test]
    fn markdown_syntax_in_content_is_highlighted() {
        let content = "# Title\n- item\n`code` **b** *i* [t](u)\n> quote";
        let evs = vec![produce::user_message(content)];
        let app = app_with_session(evs);
        let lines = build_transcript_lines(&app, 80, None);
        let find = |needle: &str| lines.iter().position(|l| l.to_string().contains(needle));
        // Word wrapping splits a line into spans, so assert over the
        // spans of the line that holds the syntax, not global spans.
        let hl = find("# Title").expect("heading line missing");
        assert!(
            lines[hl]
                .spans
                .iter()
                .filter(|s| s.content.as_ref() == "# " || s.content.as_ref() == "Title")
                .all(|s| s.style == highlight::heading_style()),
            "heading spans not styled: {lines:?}"
        );
        let ql = find("> quote").expect("quote line missing");
        assert!(
            lines[ql]
                .spans
                .iter()
                .filter(|s| s.content.as_ref() == "> " || s.content.as_ref() == "quote")
                .all(|s| s.style == highlight::quote_style()),
            "quote spans not styled: {lines:?}"
        );
        let ml = find("item").expect("list line missing");
        assert!(lines[ml]
            .spans
            .iter()
            .any(|s| s.content.as_ref() == "-" && s.style == highlight::list_style()));
        let cl = find("`code`").expect("inline code line missing");
        assert!(lines[cl]
            .spans
            .iter()
            .any(|s| s.content.as_ref() == "`code`" && s.style == highlight::inline_code_style()));
        let bl = find("**b**").expect("bold line missing");
        assert!(lines[bl]
            .spans
            .iter()
            .any(|s| s.content.as_ref() == "**b**" && s.style == highlight::bold_style()));
        let il = find("*i*").expect("italic line missing");
        assert!(lines[il]
            .spans
            .iter()
            .any(|s| s.content.as_ref() == "*i*" && s.style == highlight::italic_style()));
        let ll = find("[t]").expect("link line missing");
        assert!(lines[ll]
            .spans
            .iter()
            .any(|s| s.content.as_ref() == "[t]" && s.style == highlight::link_style()));
        assert!(lines[ll]
            .spans
            .iter()
            .any(|s| s.content.as_ref() == "(u)" && s.style == highlight::link_url_style()));
    }

    #[test]
    fn json_tool_result_is_highlighted() {
        let value = json!({ "text": "{\"a\":1,\"s\":\"x\",\"n\":null}", "exit_code": 0 });
        let evs = vec![Event::parse_line(&format!(
            r#"{{"v":1,"type":"tool_result","ts":"t","id":"c9","value":{value},"is_error":false}}"#
        ))
        .unwrap()];
        let app = app_with_session(evs);
        let lines = build_transcript_lines(&app, 80, None);
        let spans: Vec<(Style, &str)> = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| (s.style, s.content.as_ref())))
            .collect();
        assert!(
            spans
                .iter()
                .any(|(st, t)| *t == "\"a\"" && *st == highlight::json_key_style()),
            "json key not styled: {spans:?}"
        );
        assert!(spans
            .iter()
            .any(|(st, t)| *t == "1" && *st == highlight::json_number_style()));
        assert!(spans
            .iter()
            .any(|(st, t)| *t == "null" && *st == highlight::json_null_style()));
    }

    #[test]
    fn tool_result_renders_text_not_raw_json() {
        let value = json!({
            "text": "first\nsecond\nthird",
            "exit_code": 0,
            "stdout": "first\nsecond\n",
            "stderr": "third",
            "timed_out": false,
            "truncated": false
        });
        let evs = vec![Event::parse_line(&format!(
            r#"{{"v":1,"type":"tool_result","ts":"t","id":"c9","value":{value},"is_error":false}}"#
        ))
        .unwrap()];
        let app = app_with_session(evs);
        let joined = join(&build_transcript_lines(&app, 80, None));
        assert!(joined.contains("exit 0"), "{joined}");
        // Newlines are hard breaks: each hard line of the tool text
        // lands on its own visual line.
        let lines: Vec<String> = build_transcript_lines(&app, 80, None)
            .iter()
            .map(|l| l.to_string())
            .collect();
        let i1 = lines.iter().position(|l| l.contains("first")).unwrap();
        let i2 = lines.iter().position(|l| l.contains("second")).unwrap();
        let i3 = lines.iter().position(|l| l.contains("third")).unwrap();
        assert!(
            i1 < i2 && i2 < i3,
            "hard lines must stay separate: {lines:?}"
        );
        // The envelope JSON must not be what is shown.
        assert!(
            !joined.contains("\"stdout\""),
            "raw value JSON leaked: {joined}"
        );
    }

    #[test]
    fn tool_result_error_is_red_and_flagged() {
        let value = json!({ "text": "boom", "exit_code": 2, "truncated": false });
        let evs = vec![Event::parse_line(&format!(
            r#"{{"v":1,"type":"tool_result","ts":"t","id":"c9","value":{value},"is_error":true}}"#
        ))
        .unwrap()];
        let app = app_with_session(evs);
        let joined = join(&build_transcript_lines(&app, 80, None));
        assert!(joined.contains("exit 2 (error)"), "{joined}");
        assert!(joined.contains("boom"), "{joined}");
    }

    #[test]
    fn unknown_type_renders_raw_json_with_hint() {
        // G5 case (a): unknown event type -> fallback, still in order.
        let evs = vec![
            produce::user_message("before"),
            Event::parse_line(r#"{"v":1,"type":"flux_capacitor","ts":"t","data":{"charge":9}}"#)
                .unwrap(),
            produce::user_message("after"),
        ];
        let app = app_with_session(evs);
        let joined = join(&build_transcript_lines(&app, 80, None));
        assert!(
            joined.contains("unknown event type \"flux_capacitor\""),
            "{joined}"
        );
        assert!(joined.contains("charge"), "{joined}");
        // order preserved: before -> raw -> after
        assert!(
            joined.find("before").unwrap() < joined.find("flux_capacitor").unwrap()
                && joined.find("flux_capacitor").unwrap() < joined.find("after").unwrap(),
            "{joined}"
        );
    }

    #[test]
    fn unsupported_version_renders_hint() {
        // G5 case (b): v: 99 -> raw + "newer than this TUI".
        let evs = vec![Event::parse_line(
            r#"{"v":99,"type":"user_message","ts":"t","content":"future"}"#,
        )
        .unwrap()];
        let app = app_with_session(evs);
        let joined = join(&build_transcript_lines(&app, 80, None));
        assert!(
            joined.contains("log version newer than this TUI"),
            "{joined}"
        );
        assert!(joined.contains("future"), "{joined}");
    }

    #[test]
    fn malformed_line_renders_without_crash() {
        // G5 case (c): malformed JSON line -> raw + hint.
        let evs = vec![
            produce::user_message("ok line"),
            Event::MalformedLine {
                line: "{broken".into(),
            },
        ];
        let app = app_with_session(evs);
        let joined = join(&build_transcript_lines(&app, 80, None));
        assert!(joined.contains("malformed log line"), "{joined}");
        assert!(joined.contains("{broken"), "{joined}");
    }

    #[test]
    fn missing_fields_render_placeholders() {
        // G5 case (d): known type, missing fields -> placeholders, no crash.
        let evs = vec![
            Event::parse_line(r#"{"v":1,"type":"tool_call","ts":"t"}"#).unwrap(),
            Event::parse_line(r#"{"v":1,"type":"error","ts":"t"}"#).unwrap(),
            Event::parse_line(r#"{"v":1,"type":"user_message","ts":"t"}"#).unwrap(),
        ];
        let app = app_with_session(evs);
        let joined = join(&build_transcript_lines(&app, 80, None));
        assert!(joined.contains("[missing arguments]"), "{joined}");
        assert!(joined.contains("[missing message]"), "{joined}");
        assert!(joined.contains("[missing content]"), "{joined}");
    }

    #[test]
    fn approval_request_banner_when_pending() {
        let evs = vec![
            Event::parse_line(
                r#"{"v":1,"type":"tool_call","ts":"t","id":"c1","name":"bash","arguments":{"command":"rm -rf /tmp/x"}}"#,
            )
            .unwrap(),
            Event::parse_line(
                r#"{"v":1,"type":"approval_request","ts":"t","id":"appr-1","call_id":"c1","prompt":"Allow rm -rf /tmp/x?"}"#,
            )
            .unwrap(),
        ];
        let app = app_with_session(evs);
        let pending = app.oldest_pending_approval().expect("request is pending");
        assert_eq!(pending.request_id, "appr-1");
        assert_eq!(pending.arguments, Some(json!({"command": "rm -rf /tmp/x"})),);
        let joined = join(&build_transcript_lines(&app, 80, None));
        assert!(joined.contains("[y allow] [n deny] [e edit]"), "{joined}");
    }

    #[test]
    fn help_line_fits_a_96_column_pane() {
        // The inner width of a 100-column terminal is 98; keep the
        // whole hint row short enough that the quit hint never clips.
        assert!(
            help_line(false).chars().count() <= 96,
            "idle: {}",
            help_line(false)
        );
        assert!(
            help_line(true).chars().count() <= 96,
            "running: {}",
            help_line(true)
        );
        assert!(help_line(false).starts_with(' '));
        assert!(help_line(false).contains("q×2 quit"));
    }

    #[test]
    fn wrapping_never_exceeds_width() {
        let long = "x".repeat(400);
        let evs = vec![produce::user_message(&long)];
        let app = app_with_session(evs);
        let lines = build_transcript_lines(&app, 40, None);
        for l in &lines {
            assert!(
                l.to_string().chars().count() <= 40,
                "visual line wider than the pane: {l:?}"
            );
        }
    }

    #[test]
    fn ext_status_event_adds_no_transcript_lines() {
        // Stage 0 acceptance (ui-extension-plan): an ext_status event
        // adds no rows, including the blank separator. Compare with
        // the same log without the ext_status event.
        let plain = vec![
            produce::user_message("before"),
            produce::user_message("after"),
        ];
        let app = app_with_session(plain);
        let base = build_transcript_lines(&app, 80, None);

        let with_ext = vec![
            produce::user_message("before"),
            Event::parse_line(
                r#"{"v":1,"type":"ext_status","ts":"t","id":"vim_mode","value":"insert"}"#,
            )
            .unwrap(),
            produce::user_message("after"),
        ];
        let app = app_with_session(with_ext);
        let lines = build_transcript_lines(&app, 80, None);
        assert_eq!(
            join(&base),
            join(&lines),
            "an ext_status event adds no transcript lines"
        );
        let joined = join(&lines);
        assert!(!joined.contains("ext_status"), "{joined}");
        // Two more events, interleaved: still no rows.
        let many = vec![
            produce::user_message("a"),
            Event::parse_line(r#"{"v":1,"type":"ext_status","ts":"t","id":"s1","value":"x"}"#)
                .unwrap(),
            Event::parse_line(r#"{"v":1,"type":"ext_status","ts":"t","id":"s2","value":{"k":1}}"#)
                .unwrap(),
            produce::user_message("b"),
        ];
        let app = app_with_session(many);
        assert_eq!(
            join(&build_transcript_lines(&app, 80, None)),
            join(&build_transcript_lines(
                &app_with_session(vec![produce::user_message("a"), produce::user_message("b")]),
                80,
                None,
            )),
        );
    }

    #[test]
    fn ext_lines_replace_the_builtin_render() {
        // A host whose single extension owns tool_result and has a
        // cached lines reply for event 0: the extension's styled
        // lines replace the built-in render (ui-extension-plan
        // stage 1: kind ownership in build_transcript_lines).
        use crate::config::TuiConfig;
        use crate::ext::{discover, ExtHost};
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let entry = root.join("ui_extensions").join("tr");
        std::fs::create_dir_all(&entry).unwrap();
        std::fs::write(
            entry.join("ext.toml"),
            "[ext]\ncommand = \"bash\"\nargs = []\nkinds = [\"tool_result\"]\nprotocol_v = 1\n",
        )
        .unwrap();
        let cfg = TuiConfig {
            sessions_root: root.join("sessions"),
            schemas_dir: None,
            loop_cmd: None,
            config_dir: root.clone(),
            config_path: root.join("config.toml"),
            ext_dir: Some(root.join("ui_extensions")),
            active_model: None,
        };
        let disc = discover(&cfg).unwrap();
        let host = ExtHost::new(&disc, &cfg);
        let evs = vec![
            produce::user_message("before"),
            Event::parse_line(
                r#"{"v":1,"type":"tool_result","ts":"t","id":"c1","value":{"exit_code":0},"is_error":false}"#,
            )
            .unwrap(),
        ];
        let app = app_with_session(evs);
        // No reply yet: the built-in render shows.
        let joined = join(&build_transcript_lines(&app, 80, Some(&host)));
        assert!(
            joined.contains("exit 0"),
            "the fallback is the built-in render: {joined}"
        );
        // A valid reply lands: the extension lines replace it.
        host.reply_line(
            0,
            r#"{"v":1,"op":"lines","event_id":1,"lines":[["EXT TOOL VIEW",{"fg":"green","bold":true}]]}"#,
        );
        let app = app_with_session(vec![
            produce::user_message("before"),
            Event::parse_line(
                r#"{"v":1,"type":"tool_result","ts":"t","id":"c1","value":{"exit_code":0},"is_error":false}"#,
            )
            .unwrap(),
        ]);
        let lines = build_transcript_lines(&app, 80, Some(&host));
        let joined = join(&lines);
        assert!(
            joined.contains("EXT TOOL VIEW"),
            "the reply replaces the render: {joined}"
        );
        assert!(
            !joined.contains("exit 0"),
            "the built-in render is gone: {joined}"
        );
        // The reply version folded into the transcript cache key:
        // a second rebuild picks the new lines up through App.
        let mut app2 = app_with_session(vec![
            produce::user_message("before"),
            Event::parse_line(
                r#"{"v":1,"type":"tool_result","ts":"t","id":"c1","value":{"exit_code":0},"is_error":false}"#,
            )
            .unwrap(),
        ]);
        let _ = app2.transcript_lines(80, Some(&host));
        assert!(
            app2.transcript_lines(80, Some(&host))
                .iter()
                .any(|l| l.to_string().contains("EXT TOOL VIEW")),
            "the cache rebuild folds in the extension reply"
        );
    }

    #[test]
    fn transcript_caps_events_on_huge_logs() {
        // Beyond TRANSCRIPT_EVENT_CAP the oldest events are dropped so
        // memory stays bounded.
        let many = "x".repeat(10_000);
        let evs: Vec<Event> = (0..TRANSCRIPT_EVENT_CAP + 50)
            .map(|i| {
                if i == 0 {
                    Event::parse_line(&format!(
                        r#"{{"v":1,"type":"user_message","ts":"t","content":"{many}"}}"#
                    ))
                    .unwrap()
                } else {
                    Event::parse_line(r#"{"v":1,"type":"user_message","ts":"t","content":"x"}"#)
                        .unwrap()
                }
            })
            .collect();
        let app = app_with_session(evs);
        let joined = join(&build_transcript_lines(&app, 80, None));
        assert!(!joined.contains(&many), "oldest event must be dropped");
    }

    // ── transform span extraction (stage 3) ─────────────────────
    fn parts_of<'a>(block: &MBlock<'a>) -> Vec<Vec<Part<'a>>> {
        match block {
            MBlock::Text { parts } => (*parts).clone(),
            MBlock::Mermaid { .. } => Vec::new(),
        }
    }

    #[test]
    fn message_blocks_extract_mermaid_and_latex() {
        let content = "Line one $a+b$ tail\n\n```mermaid\ngraph TD\n  A-->B\n```\n\n```bash\necho $HOME\n```\nend $$x$$ done";
        let blocks = message_blocks(content);
        // One text run before the fence, the mermaid block, and one
        // text run after, in order.
        assert_eq!(blocks.len(), 3, "blocks: {blocks:?}");
        match &blocks[1] {
            MBlock::Mermaid { idx, raw, text } => {
                assert_eq!(*idx, 1, "the mermaid span takes index 1");
                assert!(raw.starts_with("```mermaid") && raw.ends_with("```"));
                assert_eq!(text, "graph TD\n  A-->B");
            }
            other => panic!("expected a mermaid block, got {other:?}"),
        }
        // The text before the fence: the first latex span is index 0
        // (content order: it comes before the mermaid fence).
        let pre = parts_of(&blocks[0]);
        let span: Vec<Part> = pre
            .iter()
            .flatten()
            .filter_map(|p| match *p {
                Part::Latex { idx, raw, text } => {
                    assert_eq!(idx, 0, "the first latex span takes index 0");
                    assert_eq!(raw, "$a+b$");
                    assert_eq!(text, "a+b");
                    Some(Part::Text(raw))
                }
                _ => None,
            })
            .collect();
        assert_eq!(span.len(), 1, "one latex span before the fence");
        // The non-mermaid fence body stays in the text flow, and its
        // dollar pair is literal (inside a code fence).
        let post = parts_of(&blocks[2]);
        let all: String = post
            .iter()
            .flatten()
            .filter_map(|p| match p {
                Part::Text(s) => Some(s.to_string()),
                Part::Latex { .. } => None,
            })
            .collect();
        assert!(
            all.contains("echo $HOME"),
            "code fence dollars stay literal"
        );
        // The trailing `$$x$$` is a second latex span (index 2),
        // after the first took 0 and the mermaid fence took 1.
        let tail: Vec<&Part> = post
            .iter()
            .flatten()
            .filter(|p| matches!(p, Part::Latex { .. }))
            .collect();
        assert_eq!(tail.len(), 1, "the trailing span only");
        match *tail[0] {
            Part::Latex { idx, raw, text } => {
                assert_eq!(idx, 2);
                assert_eq!(raw, "$$x$$");
                assert_eq!(text, "x");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn message_blocks_mermaid_inside_code_fence_stays_text() {
        let content = "```bash\n# ```mermaid\n# graph TD\n```";
        let blocks = message_blocks(content);
        assert_eq!(blocks.len(), 1, "no extraction inside a code fence");
        match &blocks[0] {
            MBlock::Text { parts } => {
                let joined: String = parts
                    .iter()
                    .flatten()
                    .filter_map(|p| match p {
                        Part::Text(s) => Some(s.to_string()),
                        Part::Latex { .. } => None,
                    })
                    .collect();
                assert!(joined.contains("# ```mermaid"), "fence stays literal");
            }
            MBlock::Mermaid { .. } => panic!("a fenced mermaid comment must not extract"),
        }
    }

    #[test]
    fn line_parts_dollar_cases() {
        let mut idx = 0u32;
        // An unclosed dollar stays literal.
        let parts = line_parts("price is $5 only", false, &mut idx);
        assert_eq!(parts.len(), 1, "no span: {parts:?}");
        assert!(matches!(parts[0], Part::Text(_)));
        // Two pairs on one line: both become spans, in order.
        let parts = line_parts("$a$ mid $b$", false, &mut idx);
        assert_eq!(parts.len(), 3, "span-text-span: {parts:?}");
        match &parts[0] {
            Part::Latex { idx, text, .. } => {
                assert_eq!(*idx, 0);
                assert_eq!(*text, "a");
            }
            _ => panic!("expected the first span"),
        }
        match &parts[2] {
            Part::Latex { idx, text, .. } => {
                assert_eq!(*idx, 1);
                assert_eq!(*text, "b");
            }
            _ => panic!("expected the second span"),
        }
        // Display form takes the whole `$$...$$` span.
        let parts = line_parts("$$x$$ end", false, &mut idx);
        assert_eq!(parts.len(), 2, "span-text: {parts:?}");
        match &parts[0] {
            Part::Latex { idx, text, .. } => {
                assert_eq!(*idx, 2);
                assert_eq!(*text, "x");
            }
            _ => panic!("expected the display span"),
        }
        // Inside a fence: no span.
        let parts = line_parts("$a$ $b$", true, &mut idx);
        assert_eq!(parts.len(), 1, "fence lines keep dollars literal");
        // An empty inline span ($$) is a display-form opener with no
        // body: the dollars stay literal.
        let parts = line_parts("$$$$", false, &mut idx);
        assert_eq!(parts.len(), 1, "empty span is literal: {parts:?}");
    }

    #[test]
    fn render_message_content_without_ext_matches_wrap_markdown() {
        let content = "head $a+b$\n\n```mermaid\ngraph TD\n  A-->B\n```\n\n```bash\necho hi\n```";
        let plain = render_message_content(content, 7, None, 60);
        let builtin = wrap_markdown(content, 60);
        let show = |v: &Vec<Line>| v.iter().map(|l| l.to_string()).collect::<Vec<_>>();
        assert_eq!(
            show(&plain),
            show(&builtin),
            "ext None must match the built-in path"
        );
    }
}

#[cfg(test)]
mod cursor_span_tests {
    use super::cursor_line_spans;

    fn text(before: &[char], caret: char, after: &[char]) -> String {
        let mut s: String = before.iter().collect();
        s.push(caret);
        s.extend(after.iter());
        s
    }

    /// Bug: the on-char cursor used to draw the covered char a
    /// second time (`[t]this`). The covered char must appear
    /// exactly once.
    #[test]
    fn on_char_cursor_does_not_duplicate_the_covered_char() {
        // Cursor on `t` of "this" (the reported `[t]this` case).
        let (b, c, a) = cursor_line_spans("this", 0, true);
        assert_eq!(text(&b, c, &a), "this", "the covered char is drawn once");
        // Cursor on `i`: `th[i]is` must stay `this`.
        let (b, c, a) = cursor_line_spans("this", 2, true);
        assert_eq!(text(&b, c, &a), "this");
        // Last char of the line.
        let (b, c, a) = cursor_line_spans("this", 3, true);
        assert_eq!(text(&b, c, &a), "this");
    }

    #[test]
    fn insert_caret_keeps_the_char_under_it() {
        // Insert mode, caret between `b` and `c` of "abc": a blank
        // block at the caret, the char under it stays.
        let (b, c, a) = cursor_line_spans("abc", 2, false);
        assert_eq!(c, ' ');
        assert_eq!(text(&b, c, &a), "ab c");
        // Caret past the end of the line.
        let (b, c, a) = cursor_line_spans("ab", 2, false);
        assert_eq!(text(&b, c, &a), "ab ");
    }

    #[test]
    fn empty_line_caret() {
        let (_b, c, _a) = cursor_line_spans("", 0, false);
        assert_eq!(c, ' ');
        // On-char on an empty line degrades to the blank cell.
        let (_b, c, _a) = cursor_line_spans("", 0, true);
        assert_eq!(c, ' ');
    }
}
