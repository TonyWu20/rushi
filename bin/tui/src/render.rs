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
//! Nothing here branches on loop internals; the only cross-event data is
//! the tool_call id -> name map, which is presentation (docs/tui.md 10.1).

use ratatui::layout::Constraint;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;
use std::collections::HashMap;

use crate::app::App;
use crate::event::{Event, EventKind};
use crate::highlight;

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
const TRANSCRIPT_EVENT_CAP: usize = 2000;
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
fn event_lines(
    e: &Event,
    pending: bool,
    call_names: &HashMap<String, String>,
    width: usize,
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
            let wrapped = wrap_markdown(&content, wrap_w);
            let mut spans = vec![Span::styled(format!("{LABEL}user"), label_style(Color::Cyan))];
            if let Some(first) = wrapped.first() {
                spans.push(Span::raw("  "));
                spans.extend(first.spans.iter().cloned());
            }
            out.push(Line::from(spans));
            // The content is displayed in full: no cap, no hint.
            out.extend(guttered(&wrapped[1..], &gutter));
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
                wrap_markdown(&content, wrap_w)
            };
            if let Some(first) = wrapped.first() {
                header.push(Span::raw("  "));
                header.extend(first.spans.iter().cloned());
            }
            out.push(Line::from(header));
            out.extend(guttered(&wrapped[1..], &gutter));
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
            out.extend(guttered(&wrapped[1..], &gutter));
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

/// The static help row. The quit hint comes first: it is the safety
/// relevant one, and terminal clipping always eats the right end.
pub fn help_line(running: bool) -> String {
    let run_key = if running { "Ctrl+C stop" } else { "Ctrl+R run" };
    format!(
        " q×2 quit · {run_key} · Ctrl+E edit · Enter send · y/n/e · Tab · PgUp/Dn Ctrl+U/D wheel"
    )
}

/// Render the transcript into wrapped visual lines, separated by blank
/// lines. The oldest events beyond `TRANSCRIPT_EVENT_CAP` are dropped.
/// The result is cached per (events version, width) by the caller.
pub fn build_transcript_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    let names = app.call_names();
    let pending = app.oldest_pending_approval().is_some();
    let events = app.events();
    let start = events.len().saturating_sub(TRANSCRIPT_EVENT_CAP);
    let mut all: Vec<Line<'static>> = Vec::new();
    for e in &events[start..] {
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
        let segs = event_lines(e, pending, &names, width.max(GUTTER + 8));
        all.extend(segs);
    }
    all
}

/// The whole frame: bordered panel with session title, transcript,
/// optional approval banner, input line, and the status/help row.
pub fn draw(f: &mut Frame, app: &mut App, cursor: &mut Option<(u16, u16)>) {
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
    let constraints = if banner {
        vec![
            Constraint::Min(2),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ]
    } else {
        vec![
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(1),
        ]
    };
    let rows = ratatui::layout::Layout::vertical(constraints).split(inner);

    // transcript
    let t_area = rows[0];
    let t_width = t_area.width.saturating_sub(2) as usize;
    let h = t_area.height as usize;
    let scroll = app.scroll();
    app.set_viewport_height(h);
    let lines = app.transcript_lines(t_width);
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

    // input line
    let i_area = rows[row];
    let naming = app.pending_name().is_some();
    let prefix = if naming { " new session: " } else { " > " };
    let text = if naming {
        app.pending_name().unwrap_or_default().to_string()
    } else {
        app.draft().to_string()
    };
    let mut spans = vec![Span::styled(
        prefix,
        Style::default().add_modifier(Modifier::BOLD),
    )];
    spans.push(Span::raw(text.clone()));
    if naming {
        spans.push(Span::styled(
            "_",
            Style::default().add_modifier(Modifier::BOLD),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), i_area);
    if !app.should_quit() {
        let x = if naming {
            // One past the rendered `_` underline.
            (prefix.chars().count() + text.chars().count() + 1)
                .min(i_area.width as usize)
        } else {
            (2 + text.chars().count()).min(i_area.width as usize)
        } as u16;
        *cursor = Some((i_area.x + x, i_area.y));
    }
    row += 1;

    // status/help row
    let s_area = rows[row];
    let status_line: Line = match app.status() {
        Some(msg) => Line::from(Span::styled(
            format!(" {msg}"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        None if app.pending_name().is_some() => Line::from(Span::styled(
            " Enter confirm · Esc cancel · q×2 quit",
            Style::default().fg(Color::DarkGray),
        )),
        None => {
            let last = active
                .as_ref()
                .and_then(|s| app.loop_state(s))
                .and_then(|l| l.last_line.clone());
            match last {
                Some(l) => Line::from(Span::styled(
                    format!(
                        " » {}",
                        trunc(&l, (s_area.width as usize).saturating_sub(4))
                    ),
                    Style::default().fg(Color::DarkGray),
                )),
                None => Line::from(Span::styled(
                    help_line(running),
                    Style::default().fg(Color::DarkGray),
                )),
            }
        }
    };
    f.render_widget(Paragraph::new(status_line), s_area);
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
        let lines = build_transcript_lines(&app, 80);
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
        let joined = join(&build_transcript_lines(&app, 80));
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
    fn long_user_message_wraps_to_many_lines() {
        let content = format!("line one\nline two\n{}", "word ".repeat(80).trim());
        let evs = vec![produce::user_message(&content)];
        let app = app_with_session(evs);
        let lines = build_transcript_lines(&app, 60);
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
        let joined = join(&build_transcript_lines(&app, 60));
        // Item 1: the content is shown in full — no cap, no hint.
        assert!(!joined.contains("more lines"), "content must not fold: {joined}");
        for i in 0..60 {
            assert!(
                joined.contains(&format!("line {i:02}")),
                "line {i:02} missing from the transcript"
            );
        }
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
        let joined = join(&build_transcript_lines(&app, 80));
        assert!(!joined.contains("more lines"), "tool result must not fold: {joined}");
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
        let joined = join(&build_transcript_lines(&app, 80));
        assert!(!joined.contains("more lines"), "error must not fold: {joined}");
        for i in [0, 49] {
            assert!(joined.contains(&format!("trace {i:02}")), "error line {i} missing");
        }
    }

    #[test]
    fn markdown_syntax_in_content_is_highlighted() {
        let content = "# Title\n- item\n`code` **b** *i* [t](u)\n> quote";
        let evs = vec![produce::user_message(content)];
        let app = app_with_session(evs);
        let lines = build_transcript_lines(&app, 80);
        let find = |needle: &str| lines.iter().position(|l| l.to_string().contains(needle));
        // Word wrapping splits a line into spans, so assert over the
        // spans of the line that holds the syntax, not global spans.
        let hl = find("# Title").expect("heading line missing");
        assert!(lines[hl]
            .spans
            .iter()
            .filter(|s| s.content.as_ref() == "# " || s.content.as_ref() == "Title")
            .all(|s| s.style == highlight::heading_style()),
            "heading spans not styled: {lines:?}");
        let ql = find("> quote").expect("quote line missing");
        assert!(lines[ql]
            .spans
            .iter()
            .filter(|s| s.content.as_ref() == "> " || s.content.as_ref() == "quote")
            .all(|s| s.style == highlight::quote_style()),
            "quote spans not styled: {lines:?}");
        let ml = find("item").expect("list line missing");
        assert!(lines[ml].spans
            .iter()
            .any(|s| s.content.as_ref() == "-" && s.style == highlight::list_style()));
        let cl = find("`code`").expect("inline code line missing");
        assert!(lines[cl].spans
            .iter()
            .any(|s| s.content.as_ref() == "`code`" && s.style == highlight::inline_code_style()));
        let bl = find("**b**").expect("bold line missing");
        assert!(lines[bl].spans
            .iter()
            .any(|s| s.content.as_ref() == "**b**" && s.style == highlight::bold_style()));
        let il = find("*i*").expect("italic line missing");
        assert!(lines[il].spans
            .iter()
            .any(|s| s.content.as_ref() == "*i*" && s.style == highlight::italic_style()));
        let ll = find("[t]").expect("link line missing");
        assert!(lines[ll].spans
            .iter()
            .any(|s| s.content.as_ref() == "[t]" && s.style == highlight::link_style()));
        assert!(lines[ll].spans
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
        let lines = build_transcript_lines(&app, 80);
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
        let joined = join(&build_transcript_lines(&app, 80));
        assert!(joined.contains("exit 0"), "{joined}");
        // Newlines are hard breaks: each hard line of the tool text
        // lands on its own visual line.
        let lines: Vec<String> = build_transcript_lines(&app, 80)
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
        let joined = join(&build_transcript_lines(&app, 80));
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
        let joined = join(&build_transcript_lines(&app, 80));
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
        let joined = join(&build_transcript_lines(&app, 80));
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
        let joined = join(&build_transcript_lines(&app, 80));
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
        let joined = join(&build_transcript_lines(&app, 80));
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
        let joined = join(&build_transcript_lines(&app, 80));
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
        let lines = build_transcript_lines(&app, 40);
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
        let base = build_transcript_lines(&app, 80);

        let with_ext = vec![
            produce::user_message("before"),
            Event::parse_line(
                r#"{"v":1,"type":"ext_status","ts":"t","id":"vim_mode","value":"insert"}"#,
            )
            .unwrap(),
            produce::user_message("after"),
        ];
        let app = app_with_session(with_ext);
        let lines = build_transcript_lines(&app, 80);
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
            Event::parse_line(
                r#"{"v":1,"type":"ext_status","ts":"t","id":"s1","value":"x"}"#,
            )
            .unwrap(),
            Event::parse_line(
                r#"{"v":1,"type":"ext_status","ts":"t","id":"s2","value":{"k":1}}"#,
            )
            .unwrap(),
            produce::user_message("b"),
        ];
        let app = app_with_session(many);
        assert_eq!(
            join(&build_transcript_lines(&app, 80)),
            join(&build_transcript_lines(&app_with_session(vec![
                produce::user_message("a"),
                produce::user_message("b"),
            ]), 80)),
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
        let joined = join(&build_transcript_lines(&app, 80));
        assert!(!joined.contains(&many), "oldest event must be dropped");
    }
}
