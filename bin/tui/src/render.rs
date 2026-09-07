//! Rendering: event -> terminal lines, and the frame layout.
//!
//! Rendering rules (docs/tui.md section 2.2 and 10.4):
//! - known category -> semantic pretty-print
//! - unknown `type` -> raw JSON with a hint
//! - unsupported `v` -> raw JSON with a "newer log version" hint
//! - malformed line -> raw text with a hint
//!
//! Message content (the `content` field of user and assistant
//! events) wraps across as many lines as it needs: it is displayed
//! in full, never truncated or folded (docs/
//! tui_feature_requests_from_human.md item 1). Message content gets
//! markdown syntax highlighting; tool result bodies get the folded,
//! box-wrapped, syntax-aware render of docs/tui-tool-display-port.md.
//! The log file stays the record.
//!
//! The input area is a multi-line textarea in a rounded-corner border
//! whose color tracks the active model's thinking level, and is
//! customizable by a `frame` extension (docs/ui-extensions design:
//! the input area is not hardwired into the TUI; an external process
//! owns its frame through the `frame_spec` reply, never its content).

use bon::builder;
use ratatui::layout::Constraint;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};
use ratatui::Frame;
use std::collections::HashMap;
use std::sync::Arc;

use crate::app::App;
use crate::event::{Event, EventKind};
use crate::highlight;
use crate::picker::preview::Previewer;

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
/// The `content` field of user and assistant messages has no cap:
/// it displays in full (docs/tui_feature_requests_from_human.md item
/// 1). Tool result bodies fold at render time (docs/
/// tui-tool-display-port.md).
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
        // The ellipsis owns one column of the budget: the result
        // never exceeds `max` columns.
        out = s.chars().take(max.saturating_sub(1)).collect();
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
/// `cap` lines with a hint for the remainder. The fold hint is the
/// palette `Hint` role (the pi `muted` tone), not a hard-coded gray.
fn body(
    text: &str,
    style: Style,
    cap: usize,
    wrap_w: usize,
    gutter: &str,
    hint: Style,
) -> Vec<Line<'static>> {
    let text = text.trim_end_matches('\n');
    if text.is_empty() {
        return Vec::new();
    }
    let wrapped = wrap_styled(vec![(style, text.to_string())], wrap_w);
    let dim = hint.add_modifier(Modifier::DIM);
    let mut out = Vec::with_capacity(cap + 1);
    for l in wrapped.iter().take(cap) {
        // Keep the wrapped line's styled spans (the tool-output tone) and
        // only prefix the gutter: flattening to a raw span would drop the
        // foreground and fall back to the terminal default.
        let mut spans = vec![Span::raw(gutter.to_string())];
        spans.extend(l.spans.iter().cloned());
        out.push(Line::from(spans));
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

/// The token count as a compact `k` figure: 212992 renders as
/// `212k`, 33000 as `33k`, 999 as `999`.
fn fmt_k(n: i64) -> String {
    if n >= 1000 {
        format!("{}k", (n + 500) / 1000)
    } else {
        n.to_string()
    }
}

/// One visual event block: a header line plus wrapped, capped body
/// lines, each continuation line aligned under the content gutter.
/// `event_id` is the log index of the event; `ext` enables the stage
/// 3 span extraction (transform owners may rewrite the message
/// spans in place). `None` ext renders exactly the built-in path.
/// The render state of one transcript build (docs/tui-tool-display-
/// port.md section 2, docs/tui-color-scheme.md section 3, docs/tui-
/// thinking-block.md section 4, docs/tui-pending-user-messages.md
/// stage 2): the palette, the tool display config, the global fold
/// toggle, and the thinking-block state. The render path is a pure
/// function of the events and this state.
pub struct RenderState<'a> {
    pub palette: &'a crate::color::Palette,
    pub tool_display: &'a crate::tool_display::ToolDisplay,
    /// The global tool fold/expand toggle (Ctrl+O): `true` expands
    /// every collapsed block to the full body.
    pub tool_expanded: bool,
    /// The thinking-block visibility (Ctrl+T): `false` hides every
    /// thinking block.
    pub thinking_shown: bool,
    /// The thinking-block expand state (Ctrl+X): `false` shows the
    /// collapsed header row only.
    pub thinking_expanded: bool,
}

#[builder]
fn event_lines<'a>(
    e: &'a Event,
    pending: bool,
    call_details: &'a HashMap<String, (String, serde_json::Value)>,
    result_ids: &'a std::collections::HashSet<String>,
    width: usize,
    event_id: u64,
    ext: Option<&'a crate::ext::ExtHost>,
    state: &'a RenderState<'a>,
    loop_running: bool,
    compaction_last_open: bool,
) -> Vec<Line<'static>> {
    let gutter = " ".repeat(GUTTER);
    let wrap_w = width.saturating_sub(GUTTER).max(4);
    let label_style = |fg: Color| Style::default().fg(fg).add_modifier(Modifier::BOLD);
    let palette = state.palette;
    // The muted tone of the hint rows and the error accents: the
    // palette roles, lowered to the capability level
    // (docs/tui-color-tones.md).
    let dim = palette.style(crate::color::Role::Hint, Modifier::DIM);
    // Command/output bodies: the capability-aware muted tones, distinct
    // from the transcript prose color (docs/tui-color-tones.md: the
    // reference-tone fix). The colors are palette roles.
    let output = palette.style(crate::color::Role::ToolOutput, Modifier::empty());
    // The command text of a tool call: a capability-aware light tone
    // (`tool_command`), lighter than the result so the command and its
    // output read as two different voices.
    let command = palette.style(crate::color::Role::ToolCommand, Modifier::empty());
    // Unstyled transcript prose: the capability-aware plain-text
    // color (`plain_text`), not the terminal default.
    let prose = palette.style(crate::color::Role::PlainText, Modifier::empty());

    let mut out: Vec<Line<'static>> = Vec::new();
    match e.kind() {
        EventKind::UserMessage => {
            let content = e
                .get_str("content")
                .unwrap_or("[missing content]")
                .to_string();
            let wrapped = render_message_content(&content, event_id, ext, wrap_w, prose, palette);
            let mut spans = vec![Span::styled(
                format!("{LABEL}user"),
                // The pi accent tone (the pi-tool-display user box
                // title), not a hard-coded cyan.
                label_style(palette.color(crate::color::Role::Accent)),
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
            // The thinking block first: the model's own reasoning items,
            // captured into the log by the loop, render above the
            // message body and the tool calls that follow, so the
            // transcript reads thinking, then the actions (docs/tui-
            // thinking-block.md section 4: the reasoning content shows
            // above the message body).
            // pi-aligned keymap: `Ctrl+T` collapses or expands
            // the block (the pi `app.thinking.toggle`). Collapsed it is a
            // one-line label row; expanded it is the full reasoning text.
            // `Ctrl+X` hides or shows the block entirely. The color is
            // the lighter thinking tone (docs/tui-color-tones.md), not a
            // dimmed gray.
            if state.thinking_shown {
                let reasoning = e.get("reasoning").and_then(|v| v.as_array());
                if let Some(text) = thinking_text(reasoning) {
                    let thinking_style =
                        palette.style(crate::color::Role::Thinking, Modifier::empty());
                    if state.thinking_expanded {
                        let header = vec![Span::styled(
                            format!("{LABEL}thinking"),
                            thinking_style,
                        )];
                        out.push(Line::from(header));
                        let wrapped = wrap_thinking(&text, wrap_w, palette, thinking_style);
                        out.extend(guttered(&wrapped, &gutter));
                    } else {
                        // The collapsed row: a one-line pi-style label with
                        // the expand hint, not the full reasoning text.
                        out.push(Line::from(Span::styled(
                            format!("{LABEL}thinking \u{2026} (Ctrl+T to expand)"),
                            thinking_style,
                        )));
                    }
                }
            }
            let content = e.get_str("content").unwrap_or("").to_string();
            let tool_calls = e.get("tool_calls").and_then(|v| v.as_array());
            let mut header = vec![Span::styled(
                format!("{LABEL}assistant"),
                // The pi `toolTitle` tone: the assistant's actions
                // show as tool calls in pi, titled in `toolTitle`.
                label_style(palette.color(crate::color::Role::ToolCommand)),
            )];
            if let Some(calls) = tool_calls {
                if !calls.is_empty() {
                    header.push(Span::styled(
                        format!(
                            " ({} tool call{})",
                            calls.len(),
                            if calls.len() == 1 { "" } else { "s" }
                        ),
                        dim,
                    ));
                }
            }
            let wrapped = if content.is_empty() {
                Vec::new()
            } else {
                render_message_content(&content, event_id, ext, wrap_w, prose, palette)
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
            let id = e.get_str("id").unwrap_or("");
            // A bash call whose result follows merges into the
            // result box: the box body opens with the `$ <command>`
            // line, so the separate call line would repeat the
            // command. A call without a result yet keeps its line:
            // the command is the only view of a running tool.
            let merged = name == "bash" && result_ids.contains(id);
            if !merged {
                let args = e
                    .get("arguments")
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "[missing arguments]".to_string());
                out.push(Line::from(vec![
                    // The pi `toolTitle` tone for the tool name, the
                    // muted tone for the arguments.
                    Span::styled(
                        format!("{LABEL}tool:{name}"),
                        label_style(palette.color(crate::color::Role::ToolCommand)),
                    ),
                    Span::styled(
                        format!(" {}", trunc(&args, wrap_w.max(20))),
                        dim,
                    ),
                ]));
                // The command of a bash call is the interesting part;
                // show it as its own dim line instead of raw JSON
                // noise.
                if name == "bash" {
                    if let Some(cmd) = e
                        .get("arguments")
                        .and_then(|a| a.get("command"))
                        .and_then(|c| c.as_str())
                    {
                        out.extend(body(
                            cmd,
                            command,
                            TOOL_CALL_BODY_LINES,
                            wrap_w,
                            &gutter,
                            dim,
                        ));
                    }
                }
            }
        }
        EventKind::ToolResult => {
            let id = e.get_str("id").unwrap_or("?");
            // The call details hold the tool name and the call
            // arguments (the write diff needs the write `content`
            // argument; docs/tui-tool-result-truncation.md).
            let (name, args) = call_details
                .get(id)
                .cloned()
                .unwrap_or_else(|| (id.to_string(), serde_json::Value::Null));
            let value = e.get("value");
            let err = e.get_bool("is_error").unwrap_or(false);
            let status = result_status(value, err);
            // The pi `error` accent (not a hard-coded red) on a failed
            // result; the muted tone on a success.
            let status_style = if err {
                palette.style(crate::color::Role::Error, Modifier::BOLD)
            } else {
                dim
            };
            // The result body in its lighter box (docs/tui-tool-
            // display-port.md section 2, the box): the body is the
            // tool-specific compact output (docs/tui-tool-result-
            // truncation.md section 1, the content layer), folded to
            // the output mode's lines, with the global Ctrl+O
            // expansion to the full body. The box top border carries
            // the `tool:<name>  <status>` title, so no separate
            // header line above the box. An error title keeps the
            // red accent through the title style.
            let value_ref = value.unwrap_or(&serde_json::Value::Null);
            // The body content budget: the box inner width minus the
            // left padding cell. The box truncates overflow with a
            // trailing ellipsis, so the content fills the panel
            // instead of leaving dead columns (the 2026-09-03 user
            // directive: truncate, never wrap).
            let body_w = width.saturating_sub(3);
            let mut body = crate::tool_display::body_rows()
                .tool(&name)
                .value(value_ref)
                .call_args(&args)
                .err(err)
                .cfg(state.tool_display)
                .palette(palette)
                .expanded(state.tool_expanded)
                .width(body_w)
                .call();
            // The JSON-document body (docs/tui-color-tones.md): a
            // read result whose content is a complete JSON document,
            // or an unknown tool whose result is JSON, keeps the
            // JSON token colors instead of the plain code tone.
            let body_text = value_ref.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let known = matches!(name.as_str(), "read" | "write" | "edit" | "bash" | "list");
            if (name == "read" || !known)
                && highlight::looks_like_json(body_text) {
                    body = crate::tool_display::json_body_rows(
                        &name,
                        value_ref,
                        state.tool_display,
                        palette,
                        state.tool_expanded,
                        body_w,
                    );
                }
            let title = format!("tool:{name}  {status}");
            let title_style = if err { Some(status_style) } else { None };
            let rows = crate::tool_display::box_rows(
                &title,
                &body,
                width,
                palette,
                title_style.as_ref(),
                err,
            );
            for row in rows {
                let spans: Vec<Span<'static>> =
                    row.into_iter().map(|(s, t)| Span::styled(t, s)).collect();
                out.push(Line::from(spans));
            }
        }
        EventKind::ApprovalRequest => {
            let id = e.get_str("id").unwrap_or("?");
            let prompt = e.get_str("prompt").unwrap_or("[no prompt]").to_string();
            // The pi `warning` accent (not a hard-coded yellow).
            let st = palette.style(crate::color::Role::Warning, Modifier::BOLD);
            let mut line = vec![
                Span::styled(format!("[approval {id}] "), st),
                Span::styled(trunc(&prompt, wrap_w.max(20)), output),
            ];
            if pending {
                line.push(Span::styled("  [y allow] [n deny] [e edit]", st));
            }
            out.push(Line::from(line));
        }
        EventKind::Approval => {
            let id = e.get_str("id").unwrap_or("?");
            let decision = e.get_str("decision").unwrap_or("?");
            let edited = e.get("arguments").is_some();
            let mut spans = vec![Span::styled(
                format!("[approval {id}] -> {decision}"),
                // The pi `success` accent on an allow, not a
                // hard-coded green.
                Style::default().fg(palette.color(crate::color::Role::Success)),
            )];
            if edited {
                spans.push(Span::styled(" (edited arguments)", dim));
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
        EventKind::UserMessageRetract => {
            let target = e.get_str("target").unwrap_or("?");
            let reason = e.get_str("reason").unwrap_or("");
            let reason_part = if reason.is_empty() { String::new() } else { format!(" ({reason})") };
            out.push(Line::from(Span::styled(
                format!("{LABEL}[retracted] target={target}{reason_part}"),
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
            // The pi `warning` accent (not a hard-coded yellow).
            let st = palette.style(crate::color::Role::Warning, Modifier::BOLD);
            let mut spans = vec![Span::styled(format!("{LABEL}[context exhausted]"), st)];
            let wrapped = if msg.is_empty() {
                Vec::new()
            } else {
                wrap_styled(vec![(prose, msg)], wrap_w)
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
            let wrapped = wrap_styled(vec![(prose, msg)], wrap_w);
            let mut spans = vec![Span::styled(
                format!("{LABEL}[error]"),
                // The pi `error` accent (not a hard-coded red).
                palette.style(crate::color::Role::Error, Modifier::BOLD),
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
        // The in-session auto-compact markers (docs/auto-compact-plan.md
        // section 4.6). The started line shows the trigger and the
        // scale while the summary call runs. The slot says whether
        // this marker is the last open one: a closed marker, or an
        // open marker superseded by a later one, renders dimmed.
        // The renderer closes the first open marker. The last open
        // marker with the loop process not running renders the
        // interrupted form.
        EventKind::CompactionStarted => {
            let reason = e.get_str("reason").unwrap_or("threshold").to_string();
            let tokens = e.get_i64("tokens_before").unwrap_or(0);
            let style = if compaction_last_open && loop_running {
                // The pi `warning` accent on the live marker.
                palette.style(crate::color::Role::Warning, Modifier::BOLD)
            } else {
                dim
            };
            let text = if compaction_last_open && !loop_running {
                format!("{LABEL}compacting (interrupted)")
            } else {
                format!("{LABEL}compacting ({reason}): {} tokens", fmt_k(tokens))
            };
            out.push(Line::from(Span::styled(text, style)));
        }
        EventKind::CompactionSummary => {
            let reason = e.get_str("reason").unwrap_or("threshold").to_string();
            let before = e.get_i64("tokens_before").unwrap_or(0);
            let after = e.get_i64("tokens_after").unwrap_or(0);
            let first_kept = e.get_i64("first_kept_seq").unwrap_or(0);
            let summary = e.get_str("summary").unwrap_or("").to_string();
            // The pi `warning` accent (not a hard-coded yellow).
            let st = palette.style(crate::color::Role::Warning, Modifier::BOLD);
            let spans = vec![Span::styled(
                format!(
                    "{LABEL}context compacted ({reason}): {} to {} tokens, keeping events from seq {first_kept}",
                    fmt_k(before),
                    fmt_k(after)
                ),
                st,
            )];
            out.push(Line::from(spans));
            // The summary body rides under the gutter, available in
            // the expand mechanism like the error body.
            if !summary.is_empty() {
                let wrapped = wrap_styled(vec![(prose, summary)], wrap_w);
                if !wrapped.is_empty() {
                    out.extend(guttered(&wrapped, &gutter));
                }
            }
        }
        EventKind::CompactionFailed => {
            let reason = e.get_str("reason").unwrap_or("overflow").to_string();
            let detail = e.get_str("detail").unwrap_or("").to_string();
            // The pi `error` accent (not a hard-coded red).
            let st = palette.style(crate::color::Role::Error, Modifier::BOLD);
            let mut spans = vec![Span::styled(
                format!("{LABEL}compaction failed ({reason})"),
                st,
            )];
            if !detail.is_empty() {
                let wrapped = wrap_styled(vec![(prose, detail)], wrap_w);
                if let Some(first) = wrapped.first() {
                    spans.push(Span::raw("  "));
                    spans.extend(first.spans.iter().cloned());
                }
                out.push(Line::from(spans));
                if !wrapped.is_empty() {
                    out.extend(guttered(&wrapped[1..], &gutter));
                }
            } else {
                out.push(Line::from(spans));
            }
        }
        EventKind::Rewind => {
            // The fork marker (docs/rewind-fork-design.md): a past
            // branch point, rendered dim like a closed marker. The
            // event projects to nothing in the model context; it
            // names the target seq and the mode (`before` restores
            // the target user message to the input box).
            let target = e.get_i64("target_seq").unwrap_or(0);
            let mode = e.get_str("mode").unwrap_or("on");
            out.push(Line::from(Span::styled(
                format!("{LABEL}rewound to seq {target} ({mode})"),
                dim,
            )));
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
                    dim,
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
                    dim,
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
                // The pi `error` accent, dimmed (not a hard-coded red).
                palette.style(crate::color::Role::Error, Modifier::DIM),
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
/// The expanded thinking block text (docs/tui-thinking-block.md
/// section 4): the raw reasoning text in the thinking tone. One
/// exception: a run of consecutive `|` table lines draws as the
/// box-drawing grid, the same rule as `wrap_markdown_p` (the
/// 2026-09-03 user report: tables inside a thinking block lost
/// their fixed column widths).
fn wrap_thinking(
    text: &str,
    wrap_w: usize,
    palette: &crate::color::Palette,
    style: Style,
) -> Vec<Line<'static>> {
    let hard_lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<Line<'static>> = Vec::new();
    let border_style = palette.style(crate::color::Role::Hint, Modifier::DIM);
    let mut i = 0usize;
    while i < hard_lines.len() {
        if highlight::is_table_row(hard_lines[i]) {
            let mut block: Vec<String> = Vec::new();
            while i < hard_lines.len() && highlight::is_table_row(hard_lines[i]) {
                block.push(hard_lines[i].to_string());
                i += 1;
            }
            let grid = highlight::table_grid(&block, wrap_w, palette);
            for row in grid {
                let spans: Vec<Span<'static>> = row
                    .into_iter()
                    .map(|(s, t)| {
                        // The cell text takes the thinking tone; the
                        // border runs keep the hint style.
                        let s = if s == border_style { s } else { style };
                        Span::styled(t, s)
                    })
                    .collect();
                out.push(Line::from(spans));
            }
            continue;
        }
        let line = hard_lines[i];
        i += 1;
        if line.is_empty() {
            out.push(Line::default());
            continue;
        }
        out.extend(wrap_styled(vec![(style, line.to_string())], wrap_w));
    }
    out
}

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

/// The marker-free markdown render (docs/tui-markdown-render.md):
/// the styles in, the markers out. The fence state spans the hard
/// lines; consecutive table rows draw as a box-drawing grid
/// (the grid clamps to the pane width); a trailing newline drops
/// like every other body render. Segments of one hard line wrap
/// continuously. `base` is the foreground of the unstyled
/// plain-text runs; `palette` the color roles.
fn wrap_markdown_p(
    text: &str,
    wrap_w: usize,
    palette: &crate::color::Palette,
    base: Style,
) -> Vec<Line<'static>> {
    let hard_lines: Vec<&str> = text.trim_end_matches('\n').split('\n').collect();
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut fence = false;
    let mut i = 0usize;
    while i < hard_lines.len() {
        let line = hard_lines[i];
        // A table block: consecutive `|`-separated rows. The block
        // draws as one grid table, clamped to the pane width
        // (docs/tui-markdown-render.md section 3: the column width
        // rule on a narrow pane).
        if highlight::is_table_row(line) {
            let mut block: Vec<String> = Vec::new();
            while i < hard_lines.len() && highlight::is_table_row(hard_lines[i]) {
                block.push(hard_lines[i].to_string());
                i += 1;
            }
            let grid = highlight::table_grid(&block, wrap_w, palette);
            for row in grid {
                let spans: Vec<Span<'static>> =
                    row.into_iter().map(|(s, t)| Span::styled(t, s)).collect();
                out.push(Line::from(spans));
            }
            continue;
        }
        i += 1;
        if line.is_empty() {
            out.push(Line::default());
            continue;
        }
        let segs = with_plain_base(highlight::md_line(line, &mut fence, palette), base);
        out.extend(wrap_flow(segs, wrap_w));
    }
    out
}

/// The thinking text of one `assistant_message` reasoning array
/// (docs/tui-thinking-block.md section 4): the item `summary` texts
/// first (the provider's own summary), the `content` reasoning-text
/// entries when the summary is empty. Malformed items drop; the
/// rest ride on, like the capture in commit `61cde02`.
fn thinking_text(reasoning: Option<&Vec<serde_json::Value>>) -> Option<String> {
    let items = reasoning?;
    let mut parts: Vec<String> = Vec::new();
    for item in items.iter().filter(|i| i.is_object()) {
        // The summary first: the provider's own summary of the
        // reasoning, when it carries text.
        let summary_text: String = item
            .get("summary")
            .and_then(|s| s.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|s| s.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        if !summary_text.is_empty() {
            parts.push(summary_text);
            continue;
        }
        // The raw reasoning-text entries of the content array.
        // A `reasoning_text` entry and a plain text entry both carry
        // the reasoning text (older session logs store the plain
        // shape; docs/tui-thinking-block.md section 4).
        let content_text: String = item
            .get("content")
            .and_then(|c| c.as_array())
            .map(|arr| {
                arr.iter()
                    .filter(|e| match e.get("type").and_then(|t| t.as_str()) {
                        Some(t) => t == "reasoning_text",
                        None => true,
                    })
                    .filter_map(|e| e.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        if !content_text.is_empty() {
            parts.push(content_text);
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

/// Give the style-free segments of a highlight flow a `base`
/// foreground so plain text never falls back to the terminal's
/// default foreground (the "all gray" defect: the default color
/// depends on the emulator, and on the iPad Blink app it reads as
/// purple). A segment that carries any fg or modifier — a heading,
/// a quote marker, a JSON token — is not the plain text and keeps
/// its own style.
fn with_plain_base(segs: Vec<(Style, String)>, base: Style) -> Vec<(Style, String)> {
    segs.into_iter()
        .map(|(s, t)| (if s == Style::default() { base } else { s }, t))
        .collect()
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

/// The rendered text of one hard line of a text block: the text
/// parts in order, the latex spans by their span text. Used for
/// the grid-table detection: a run of consecutive table lines in
/// one text block draws as a box-drawing grid, the same rule as
/// [`wrap_markdown_p`] (docs/tui-markdown-render.md section 1).
fn block_line_text(parts: &[crate::render::Part]) -> String {
    parts
        .iter()
        .map(|p| match p {
            crate::render::Part::Text(s) => *s,
            crate::render::Part::Latex { text, .. } => *text,
        })
        .collect::<Vec<_>>()
        .concat()
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
/// - A table block: a run of consecutive `|`-separated lines draws
///   as a box-drawing grid, like the `ext == None` path (the
///   2026-09-03 report: the block path showed the raw pipes).
///
/// `base` is the foreground of the unstyled plain-text runs (the
/// capability-aware prose color); styled runs keep their own
/// styles. The `ext == None` path renders the content with
/// [`wrap_markdown_p`] over the same base.
fn render_message_content(
    content: &str,
    event_id: u64,
    ext: Option<&crate::ext::ExtHost>,
    wrap_w: usize,
    base: Style,
    palette: &crate::color::Palette,
) -> Vec<Line<'static>> {
    if ext.is_none() {
        return wrap_markdown_p(content, wrap_w, palette, base);
    }
    let host = ext.expect("checked above");
    let blocks = message_blocks(content);
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut fence = false;
    for block in blocks {
        match block {
            MBlock::Text { parts } => {
                let mut li = 0usize;
                while li < parts.len() {
                    // The grid table: a run of consecutive
                    // table lines, clamped to the pane width,
                    // like wrap_markdown_p.
                    let text = block_line_text(&parts[li]);
                    if highlight::is_table_row(&text) {
                        let mut block: Vec<String> = Vec::new();
                        while li < parts.len() {
                            let t = block_line_text(&parts[li]);
                            if !highlight::is_table_row(&t) {
                                break;
                            }
                            block.push(t);
                            li += 1;
                        }
                        let grid = highlight::table_grid(&block, wrap_w, palette);
                        for row in grid {
                            let spans: Vec<Span<'static>> = row
                                .into_iter()
                                .map(|(s, t)| Span::styled(t, s))
                                .collect();
                            out.push(Line::from(spans));
                        }
                        continue;
                    }
                    let line_parts = &parts[li];
                    li += 1;
                    let mut segs: Vec<(Style, String)> = Vec::new();
                    for part in line_parts {
                        match part {
                            Part::Text(s) => {
                                segs.extend(with_plain_base(
                                    highlight::md_line(s, &mut fence, palette),
                                    base,
                                ));
                            }
                            Part::Latex { idx, raw, text } => {
                                let req = host.request_span(
                                    event_id, *idx, "inline:latex", text, wrap_w,
                                );
                                let replaced = req
                                    .and_then(|_| host.span_lines(event_id, *idx))
                                    .map(|ls| {
                                        ls.iter()
                                            .map(|l| l.text.clone())
                                            .collect::<Vec<_>>()
                                            .join(" ")
                                    })
                                    .unwrap_or_else(|| raw.to_string());
                                segs.push((base, replaced));
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
                            let segs = with_plain_base(
                                highlight::md_line(hard, &mut private, palette),
                                base,
                            );
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

/// The loop-phase display state of the active session
/// (docs/tui-model-wait-indicator.md section 2). One of four values,
/// derived from two inputs: the last `loop_phase` value in the log
/// and the loop-running bit. The state is a pure function of the
/// log and the bit, so it survives a TUI restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseState {
    /// The loop is not running. Bit `[idle]`, the working row
    /// stays blank.
    Idle,
    /// The loop runs with no marker, or a value outside
    /// `wait` / `tools`. Bit `[running]`, the row shows
    /// `Working...`.
    RunningUnknown,
    /// The loop runs, the last marker value is `wait`. Bit
    /// `[wait]`, the row shows the wait for the model response.
    Wait,
    /// The loop runs, the last marker value is `tools`. Bit
    /// `[tools]`, the row shows the tool run.
    Tools,
}

/// The title bit text per phase state (docs/tui-model-wait-indicator.md
/// section 2). A running loop names its phase; an idle loop or an
/// unknown marker keeps the plain bit.
pub fn phase_bit(state: PhaseState) -> &'static str {
    match state {
        PhaseState::Idle => " [idle] ",
        PhaseState::RunningUnknown => " [running] ",
        PhaseState::Wait => " [wait] ",
        PhaseState::Tools => " [tools] ",
    }
}

/// Derive the phase state from the last marker value and the running
/// bit (docs/tui-model-wait-indicator.md section 2). The value comes
/// from the O(1) per-id map; a missing marker or a value outside
/// the two known strings maps to `RunningUnknown`.
pub fn phase_state(app: &App, running: bool) -> PhaseState {
    if !running {
        return PhaseState::Idle;
    }
    match app
        .ext_statuses()
        .get(crate::app::LOOP_PHASE_STATUS_ID)
        .and_then(|v| v.as_str())
    {
        Some("wait") => PhaseState::Wait,
        Some("tools") => PhaseState::Tools,
        _ => PhaseState::RunningUnknown,
    }
}

/// The wait span between the marker timestamp and `now`, in whole
/// seconds: `Ns` under 60 s, `Mm SSs` at 60 s and up. A negative
/// span (the loop host clock runs behind the TUI) clamps to `0s`.
/// An unparseable timestamp yields `None`: the working row keeps
/// the label and drops the span (docs/tui-model-wait-indicator.md
/// section 4).
pub fn wait_span_text(ts: &str, now: chrono::DateTime<chrono::Utc>) -> Option<String> {
    let marked = chrono::DateTime::parse_from_rfc3339(ts)
        .ok()?
        .with_timezone(&chrono::Utc);
    let secs = now.signed_duration_since(marked).num_seconds().max(0);
    if secs < 60 {
        Some(format!("{secs}s"))
    } else {
        Some(format!("{m}m {s}s", m = secs / 60, s = secs % 60))
    }
}

/// The braille spinner frames of the working row, in cycle order.
/// The row shows one frame per redraw; the main loop redraws about
/// every 100 ms, so the cycle runs at 100 ms per frame.
const WORKING_SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// The spinner frame index for `now`: the wall-clock milliseconds
/// over the frame interval. The index is a pure function of time, so
/// no animation state lives in the App.
fn spinner_frame(now: &chrono::DateTime<chrono::Utc>) -> &'static str {
    let idx = (now.timestamp_millis() / 100) % WORKING_SPINNER_FRAMES.len() as i64;
    WORKING_SPINNER_FRAMES[idx as usize]
}

/// The working row text above the input box
/// (docs/tui-model-wait-indicator.md section 3): the phase with the
/// wait span from the marker timestamp. The `wait` and `tools`
/// states carry the span; an unparseable timestamp drops the span
/// and keeps the label. The idle state owns no text: the row stays
/// blank.
fn working_row_text(
    state: PhaseState,
    ts: Option<&str>,
    now: &chrono::DateTime<chrono::Utc>,
) -> Option<String> {
    match state {
        PhaseState::Idle => None,
        PhaseState::RunningUnknown => Some("Working...".to_string()),
        PhaseState::Wait => {
            let label = "waiting for model";
            match ts.and_then(|t| wait_span_text(t, *now)) {
                Some(span) => Some(format!("{label} · {span}")),
                None => Some(label.to_string()),
            }
        }
        PhaseState::Tools => {
            let label = "tools running";
            match ts.and_then(|t| wait_span_text(t, *now)) {
                Some(span) => Some(format!("{label} · {span}")),
                None => Some(label.to_string()),
            }
        }
    }
}

/// The working row above the input box
/// (docs/tui-model-wait-indicator.md section 3, after the `pi`
/// working indicator): the spinner frame in the thinking-level
/// border color, then the phase text in dim. The caller draws the
/// row only while the loop runs. The idle state yields no text:
/// no row, and the transcript absorbs its place.
fn working_row(app: &App, running: bool, now: &chrono::DateTime<chrono::Utc>) -> Line<'static> {
    let state = phase_state(app, running);
    let Some(text) = working_row_text(state, app.loop_phase_ts(), now) else {
        return Line::default();
    };
    let frame = Span::styled(
        format!("{} ", spinner_frame(now)),
        Style::default().fg(app.palette().thinking_border(app.thinking_level())),
    );
    // The phase text in the pi `dim` tone (not a hard-coded gray).
    let body = Span::styled(
        format!(" {text}"),
        app.palette().style(crate::color::Role::Status, Modifier::empty()),
    );
    Line::from(vec![frame, body])
}

/// The live stream block for an in-progress model response
/// (docs/tui-streaming-response.md §6.3).
///
/// Shows the header with an ellipsis ("…") while the stream is open
/// and "· done" once the done line arrives. The body shows the tail
/// of the accumulated content: the thinking tail and the response text
/// share one window (the last `max_body_lines` rows, thinking above
/// text), so the block grows with the content and its height never
/// shrinks when the response text starts — the thinking slides out as
/// the text arrives, not as a sudden collapse. (Any partial tool-call
/// arguments render when no content has arrived yet.) A blinking
/// block cursor marks the end of the live text.
///
/// The block sits right after the existing messages (the transcript),
/// above the model status indicator (working row); it does not scroll
/// with the transcript. The app clears it when the matching log event
/// lands or the loop stops.
///
/// The block grows with the arriving content: the body rows are
/// bounded by `max_body_lines` (the caller bounds it to a fraction of
/// the viewport height, so a long response extends the block without
/// stealing the whole screen; the transcript absorbs the rest).
fn stream_block_lines(app: &App, width: usize, max_body_lines: usize) -> Vec<Line<'static>> {
    let buf = match app.stream_buf() {
        Some(b) => b,
        None => return Vec::new(),
    };
    let palette = app.palette();
    let prose = palette.style(crate::color::Role::PlainText, Modifier::empty());
    let dim = palette.style(crate::color::Role::Status, Modifier::DIM);
    let label_style = Style::default()
        .fg(palette.color(crate::color::Role::ToolCommand))
        .add_modifier(Modifier::BOLD);
    let gutter = " ".repeat(GUTTER);
    let wrap_w = width.saturating_sub(GUTTER).max(4);

    let mut out: Vec<Line<'static>> = Vec::new();

    // Header row.
    let suffix = if buf.done { " · done" } else { " …" };
    out.push(Line::from(vec![Span::styled(
        format!("{LABEL}assistant{suffix}"),
        label_style,
    )]));

    let mut body_lines: Vec<Line<'static>> = Vec::new();

    let has_thinking = app.thinking_shown() && !buf.reasoning.is_empty();
    let has_text = !buf.text.is_empty();

    // Thinking renders above the response text (the natural order is
    // thinking → response). Thinking and text share one content
    // window: when the response starts, the thinking is not suddenly
    // collapsed — the block keeps its height and the oldest thinking
    // lines slide out as the text arrives, so the view never jumps
    // (no flicker at the thinking → text transition).
    let thinking_style = palette.style(crate::color::Role::Thinking, Modifier::empty());
    let mut thinking_tail: Vec<Line<'static>> = Vec::new();
    let mut shows_thinking_label = false;
    if has_thinking && max_body_lines > 0 {
        let mut ids: Vec<&String> = buf.reasoning.keys().collect();
        ids.sort();
        let thinking_text = ids
            .iter()
            .filter_map(|id| buf.reasoning.get(*id))
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if !thinking_text.is_empty() {
            shows_thinking_label = true;
            thinking_tail = wrap_thinking(&thinking_text, wrap_w, palette, thinking_style);
        }
    }

    // Text block: the accumulated response text, wrapped with the same
    // markdown/syntax path used for the settled message.
    let text_tail: Vec<Line<'static>> = if has_text {
        wrap_markdown_p(&buf.text, wrap_w, palette, prose)
    } else {
        Vec::new()
    };

    // The shared content window: the last `max_body_lines` rows (one
    // row reserved for the pinned "thinking" label when thinking is
    // shown). While the content fits, the block grows with it; once
    // full, the oldest rows (the front of the thinking) scroll out as
    // the text grows. The block height never shrinks at the
    // thinking → text transition, so the transcript above it does not
    // lurch and the view does not flicker.
    let window = max_body_lines.saturating_sub(usize::from(shows_thinking_label));
    if window > 0 {
        let combined = thinking_tail.len() + text_tail.len();
        let drop = combined.saturating_sub(window);
        let think_take = thinking_tail.len().saturating_sub(drop);
        let text_drop = drop.saturating_sub(thinking_tail.len());
        let text_take = text_tail.len().saturating_sub(text_drop);
        if shows_thinking_label {
            body_lines.push(Line::from(vec![Span::styled(
                "thinking".to_string(),
                thinking_style,
            )]));
        }
        body_lines.extend(thinking_tail[thinking_tail.len() - think_take..].iter().cloned());
        body_lines.extend(text_tail[text_tail.len() - text_take..].iter().cloned());
    }

    // Partial tool-call arguments: one dim line per call.
    if body_lines.is_empty() {
        let mut ids: Vec<&String> = buf.tool_args.keys().collect();
        ids.sort();
        for id in ids {
            let Some((name, args)) = buf.tool_args.get(id) else {
                continue;
            };
            let name_disp = if name.is_empty() { id.as_str() } else { name.as_str() };
            let budget = max_body_lines.saturating_sub(body_lines.len());
            if budget == 0 {
                break;
            }
            let shown = trunc(args, wrap_w.saturating_sub(12));
            body_lines.push(Line::from(vec![
                Span::styled(
                    format!("{gutter}tool:{name_disp}"),
                    label_style,
                ),
                Span::styled(format!(" {shown}"), dim),
            ]));
        }
    }

    // Apply the gutter to text/thinking body lines that lack it.
    let mut content: Vec<Line<'static>> = body_lines
        .into_iter()
        .map(|l| {
            // The wrap helpers already prefix the gutter when the wrap
            // width accounts for it. If the first span does not start
            // with the gutter, prepend it.
            let has_gutter = l
                .spans
                .first()
                .map_or(false, |s| s.content.starts_with(&gutter));
            if has_gutter {
                l
            } else {
                let mut spans = vec![Span::styled(gutter.clone(), Style::default())];
                spans.extend(l.spans);
                Line::from(spans)
            }
        })
        .collect();

    // Blinking cursor on the last content line while the stream is open.
    if !buf.done && content.is_empty() {
        content.push(Line::from(vec![
            Span::styled(gutter.clone(), dim),
            Span::styled("▊", dim),
        ]));
    } else if !buf.done && !content.is_empty() {
        let now = chrono::Utc::now();
        let blink = (now.timestamp_millis() / 500) % 2 == 0;
        let cursor = if blink { "▊" } else { " " };
        let last = content.last_mut().unwrap();
        last.spans.push(Span::styled(cursor, dim));
    }

    out.extend(content);
    out
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
    // The built-in status text in the pi `dim` tone (not a
    // hard-coded gray).
    let dim = app
        .palette()
        .style(crate::color::Role::Status, Modifier::empty());
    if let Some(msg) = app.status() {
        // The pi flash line: a neutral confirmation tone, not a hard
        // coded cyan. The scheme `Hint` role (the pi `muted` value)
        // carries it, bold for the emphasis (docs/tui-color-pi-
        // alignment.md).
        let hint = app
            .palette()
            .style(crate::color::Role::Hint, Modifier::BOLD);
        return vec![Line::from(Span::styled(
            format!(" {msg}"),
            hint,
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
            // The pi `error` accent, dimmed (not a hard-coded red).
            app.palette().style(crate::color::Role::Error, Modifier::DIM),
        ))],
        crate::ext::StatusRow::Builtin => {
            // The pending handoff hint wins the built-in slot: it is
            // the action that unblocks the session. The flash still
            // wins over it (it names the result of the user's own
            // key press).
            if let Some(name) = app.pending_handoff() {
                return vec![Line::from(Span::styled(
                    format!(" context exhausted — press h to hand off to {name} · q×2 quit "),
                    // The pi `warning` accent (not a hard-coded yellow).
                    app.palette()
                        .style(crate::color::Role::Warning, Modifier::BOLD),
                ))];
            }
            // The running loop names its phase in the reserved
            // working row above the input box, not here
            // (docs/tui-model-wait-indicator.md section 3): the
            // statusline extension owns this slot, and the built-in
            // fallback shows the handoff hint, the last loop line,
            // or the help row.
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

/// Resolve the transcript text width with the bar and the browse
/// gutter reserved (sections 3 and 4.3). The width drives the wrap,
/// the wrap the total, the total the gutter width: the loop runs to
/// the fixpoint, capped at three passes.
fn resolve_transcript_width(
    app: &mut crate::app::App,
    host: &crate::ext::ExtHost,
    t_width: usize,
    bar_w: usize,
    browse_active: bool,
) -> (usize, usize) {
    // (text_w, gutter_w). The gutter hint seeds from the last
    // rendered total; a missing layout reads as zero.
    let mut gutter_w = if browse_active {
        crate::browse::gutter_width(app.browse_layout_total())
    } else {
        0
    };
    let mut text_w = t_width.saturating_sub(bar_w).saturating_sub(gutter_w);
    let mut total = app.transcript_lines(text_w, Some(host)).len();
    if browse_active {
        for _ in 0..2 {
            let g = crate::browse::gutter_width(total);
            if g == gutter_w {
                break;
            }
            gutter_w = g;
            text_w = t_width.saturating_sub(bar_w).saturating_sub(gutter_w);
            total = app.transcript_lines(text_w, Some(host)).len();
        }
    }
    (text_w, gutter_w)
}

/// The browse-mode window lines (sections 4.3 and 7.3): the gutter
/// prefix on every row, the cursorline highlight and the caret block
/// on the cursor row, the search highlight on the match rows. The
/// styles are owned values: the caller precomputes them so no
/// palette borrow crosses the lines borrow.
#[allow(clippy::too_many_arguments)]
fn browse_window_lines(
    lines: &[Line<'static>],
    start: usize,
    h: usize,
    gutter_w: usize,
    cursor: (usize, usize),
    hl: &std::collections::HashSet<usize>,
    active_match: Option<(usize, usize)>,
    dim: Style,
    accent: Style,
    cursor_bg: Color,
    match_style: Style,
    active_style: Style,
) -> Vec<Line<'static>> {
    let (cl, cc) = cursor;
    lines
        .iter()
        .skip(start)
        .take(h)
        .enumerate()
        .map(|(i, l)| {
            let abs = start + i;
            let is_cursor = abs == cl;
            let line_matched = hl.contains(&abs);
            let active_line = active_match.is_some_and(|m| m.0 == abs);
            // The gutter: the absolute number at the cursor, the
            // relative distance elsewhere (section 4.3), right-
            // aligned in `gutter_w` cells (the digit count plus one
            // trailing space).
            let num = crate::browse::gutter_number(cl, abs);
            let gutter_style = if is_cursor {
                accent
            } else {
                dim
            };
            let gutter_span =
                Span::styled(format!("{num:>width$}", width = gutter_w), gutter_style);
            if is_cursor {
                // The cursor row: the low-contrast background across
                // the row (section 4.3), the caret block at the col.
                // A matched cursor row keeps the accent tone.
                let fg_override = if active_line {
                    Some(active_style)
                } else if line_matched {
                    Some(match_style)
                } else {
                    None
                };
                let mut spans = vec![gutter_span];
                spans.extend(caret_spans(l, cc, cursor_bg, fg_override));
                Line::from(spans)
            } else if active_line || line_matched {
                // The match rows flatten to the highlight tone
                // (section 7.3); the current match takes the accent
                // tone.
                let style = if active_line { active_style } else { match_style };
                let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
                Line::from(vec![gutter_span, Span::styled(text, style)])
            } else {
                let mut spans = vec![gutter_span];
                spans.extend(l.spans.iter().cloned());
                Line::from(spans)
            }
        })
        .collect()
}

/// The caret block at col `cc`: the split span keeps its styling, the
/// cell inverts, the tail keeps its styling. A col past the rendered
/// line draws the block on the line-end blank (section 4.1).
fn caret_spans(
    l: &Line<'static>,
    cc: usize,
    bg: Color,
    fg_override: Option<Style>,
) -> Vec<Span<'static>> {
    let patch = |s: &Span| {
        let mut st = s.style.patch(Style::default().bg(bg));
        if let Some(o) = fg_override {
            st = o.patch(st);
        }
        st
    };
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut rest = cc;
    for s in &l.spans {
        if rest == 0 {
            out.push(Span::styled(s.content.clone(), patch(s)));
            continue;
        }
        let n = s.content.chars().count();
        if rest < n {
            let chars: Vec<char> = s.content.chars().collect();
            let pre: String = chars[..rest].iter().collect();
            let at = chars[rest];
            let post: String = chars[rest + 1..].iter().collect();
            out.push(Span::styled(pre, patch(s)));
            let mut caret = Style::default()
                .bg(Color::Black)
                .fg(Color::White)
                .add_modifier(Modifier::REVERSED);
            if let Some(o) = fg_override {
                caret = o.patch(caret);
            }
            out.push(Span::styled(at.to_string(), caret));
            out.push(Span::styled(post, patch(s)));
            rest = 0;
        } else {
            out.push(Span::styled(s.content.clone(), patch(s)));
            rest -= n;
        }
    }
    if rest > 0 {
        out.push(
            Span::styled(
                " ",
                Style::default()
                    .bg(Color::Black)
                    .fg(Color::White)
                    .add_modifier(Modifier::REVERSED),
            ),
        );
    }
    out
}

/// The position bar (section 3): one column at the right edge of the
/// transcript. The track draws in the dim tone, the thumb in the
/// normal transcript tone, the tail and cursor markers in the accent
/// tone (the `Border4` role, docs/tui-color-scheme.md section 6).
#[allow(clippy::too_many_arguments)]
fn draw_position_bar(
    f: &mut Frame,
    t_area: &ratatui::layout::Rect,
    total: usize,
    scroll: usize,
    cursor_line: Option<usize>,
    track_c: Color,
    thumb_c: Color,
    mark_c: Color,
) {
    let h = t_area.height as usize;
    let g = match crate::browse::bar_geometry(total, h, scroll, cursor_line) {
        Some(g) => g,
        None => return,
    };
    for row in 0..h {
        let mut ch = '·';
        let mut color = track_c;
        if row >= g.thumb_top && row < g.thumb_top + g.thumb_h {
            ch = '█';
            color = thumb_c;
        }
        if row == g.tail_cell {
            ch = '▼';
            color = mark_c;
        }
        if g.cursor_cell == Some(row) {
            ch = '▶';
            color = mark_c;
        }
        let rect = ratatui::layout::Rect {
            x: t_area.x + t_area.width - 1,
            y: t_area.y + row as u16,
            width: 1,
            height: 1,
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                ch.to_string(),
                Style::default().fg(color),
            ))),
            rect,
        );
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
    let details = app.call_details();
    let pending = app.oldest_pending_approval().is_some();
    let events = app.events();
    let start = events.len().saturating_sub(TRANSCRIPT_EVENT_CAP);
    // The tool_result ids of the visible window: a tool_call whose
    // result follows merges into the result box (the call line drops
    // for the tools whose result carries the call info).
    let result_ids: std::collections::HashSet<String> = events[start..]
        .iter()
        .filter(|e| e.kind() == EventKind::ToolResult)
        .filter_map(|e| e.get_str("id").map(String::from))
        .collect();
    // The render state (docs/tui-tool-display-port.md section 2, the
    // config part, plus the fold and thinking toggles): the palette,
    // the tool display config, and the app's toggle states.
    let state = RenderState {
        palette: app.palette(),
        tool_display: app.tool_display(),
        tool_expanded: app.tool_expanded(),
        thinking_shown: app.thinking_shown(),
        thinking_expanded: app.thinking_expanded(),
    };
    // The loop supervision (docs/auto-compact-plan.md section 4.6):
    // the transcript session's loop process running bit.
    let running = app.active().is_some_and(|s| app.loop_running(s));
    // The last open compaction marker: a `compaction_started` with
    // no later `compaction_summary` or `compaction_failed` in the
    // visible window. A restarted loop may add more open markers;
    // the renderer closes the first one, and only the last open
    // marker renders the live or interrupted form.
    let mut last_open: Vec<bool> = vec![false; events.len() - start];
    let open_started: Vec<usize> = (0..events.len() - start)
        .filter(|&i| events[start + i].kind() == EventKind::CompactionStarted)
        .filter(|&i| {
            !(start + i + 1..events.len()).any(|j| {
                matches!(
                    events[j].kind(),
                    EventKind::CompactionSummary | EventKind::CompactionFailed
                )
            })
        })
        .collect();
    if let Some(&i) = open_started.last() {
        last_open[i] = true;
    }
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
                None => {
                    let builder = event_lines()
                        .e(e)
                        .pending(pending)
                        .call_details(&details)
                        .result_ids(&result_ids)
                        .width(width.max(GUTTER + 8))
                        .event_id(event_id)
                        .state(&state)
                        .loop_running(running)
                        .compaction_last_open(
                            last_open.get(i).copied().unwrap_or(false),
                        );
                    if let Some(h) = ext {
                        builder.ext(h).call()
                    } else {
                        builder.call()
                    }
                }
            }
        } else {
            let builder = event_lines()
                .e(e)
                .pending(pending)
                .call_details(&details)
                .result_ids(&result_ids)
                .width(width.max(GUTTER + 8))
                .event_id(event_id)
                .state(&state)
                .loop_running(running)
                .compaction_last_open(last_open.get(i).copied().unwrap_or(false));
            if let Some(h) = ext {
                builder.ext(h).call()
            } else {
                builder.call()
            }
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

/// The waiting-message cap of one pending block: the header row
/// plus the listed rows stay inside five terminal rows. The rest
/// counts in the `+N more` row. The full content renders in the
/// transcript when the loop answers the message.
const PENDING_MESSAGE_CAP: usize = 3;

/// The pending-message blocks of the active session (docs/tui-
/// pending-user-messages.md stage 2): the two delivery queues, each
/// with its count and its previews. The stage-1 steering block is
/// replaced by the pair: the steer queue lists the messages that
/// inject at the next step (or wait for a loop start when stopped),
/// and the follow queue lists the messages that run after the loop
/// would stop, as new turns. Each preview row owns one terminal
/// row: previews truncate to the width.
pub fn pending_message_lines(app: &App, running: bool, row_width: usize) -> Vec<Line<'static>> {
    let steer = app.pending_steering();
    let follow = app.pending_follows();
    if steer.is_empty() && follow.is_empty() {
        return Vec::new();
    }
    let prose = app
        .palette()
        .style(crate::color::Role::PlainText, Modifier::empty());
    let mut out: Vec<Line<'static>> = Vec::new();
    // The steer block: the stage-1 label states the delivery the
    // loop actually performs. Running: injected at the next step.
    // Stopped: the messages wait for a loop start.
    if !steer.is_empty() {
        let (label, dim) = if running {
            ("steering — injected at the next step", false)
        } else {
            ("no loop running — waiting for Ctrl+R run", true)
        };
        out.extend(pending_block_lines(
            &steer,
            label,
            dim,
            running,
            row_width,
            &prose,
            app.palette(),
        ));
    }
    // The follow block: the messages run after the loop would stop,
    // as new turns.
    if !follow.is_empty() {
        out.extend(pending_block_lines(
            &follow,
            "follow-up — run after the loop stops",
            false,
            running,
            row_width,
            &prose,
            app.palette(),
        ));
    }
    out
}

/// One pending block: the header row (the count and the delivery
/// hint), the preview rows capped at [`PENDING_MESSAGE_CAP`], and
/// the `+N more` row for the rest. `waiting` dims the header of a
/// stopped-loop steer block: the messages wait, nothing injects.
fn pending_block_lines(
    pending: &[&crate::event::Event],
    label: &str,
    waiting: bool,
    running: bool,
    row_width: usize,
    prose: &Style,
    palette: &crate::color::Palette,
) -> Vec<Line<'static>> {
    let n = pending.len();
    let what = if n == 1 {
        "1 message".to_string()
    } else {
        format!("{n} messages")
    };
    // The header is ` {what} waiting — {label}`. The label owns its
    // full width; the what part gets the rest. A zero budget drops
    // the what part (the ellipsis would own a column the row does
    // not have).
    let label_w = label.chars().count();
    let what_budget = row_width.saturating_sub(label_w + 4);
    let what_cell = if what_budget == 0 {
        String::new()
    } else {
        trunc(&format!("{what} waiting"), what_budget)
    };
    let mut out: Vec<Line<'static>> = vec![Line::from(Span::styled(
        format!(" {what_cell} — {label}"),
        if waiting {
            palette.style(crate::color::Role::Hint, Modifier::BOLD | Modifier::DIM)
        } else {
            palette.style(crate::color::Role::Status, Modifier::BOLD)
        },
    ))];
    for (i, ev) in pending.iter().enumerate().take(PENDING_MESSAGE_CAP) {
        let content = ev
            .get_str("content")
            .unwrap_or("")
            .lines()
            .next()
            .unwrap_or("");
        let prefix = format!("  {}. ", i + 1);
        let max = row_width
            .saturating_sub(prefix.chars().count())
            .saturating_sub(1);
        out.push(Line::from(Span::styled(
            format!("{prefix}{}", trunc(content, max)),
            *prose,
        )));
    }
    if n > PENDING_MESSAGE_CAP {
        out.push(Line::from(Span::styled(
            format!("  +{} more", n - PENDING_MESSAGE_CAP),
            palette.style(crate::color::Role::Hint, Modifier::DIM),
        )));
    }
    let _ = running;
    out
}

/// The input box title (the box top border text).
///
/// The search prompt wins: in command-line mode the host renders the
/// prompt (`/pat█`) even when a frame extension labels the frame, so
/// the typed pattern stays visible (the host keeps its modal-state
/// render, docs/ui-extension.md section 10 and docs/vim-editor-design.md
/// section 7). Otherwise a frame label replaces the built-in mode
/// label. Without a frame label, the mode label shows
/// (`[NORMAL]`, `[d-PENDING]`, ...).
fn input_box_title(
    editor: &crate::vim_editor::Editor,
    mode_label: &str,
    frame: &Option<crate::ext::FrameSpec>,
    border_color: Color,
    browse_prompt: Option<&str>,
) -> Line<'static> {
    // The browse command line owns the title while it is open
    // (docs/tui-conversation-browsing.md sections 4.4 and 7.3):
    // the `:N` goto and the `/` / `?` pattern, like the editor's
    // search prompt.
    if let Some(p) = browse_prompt {
        return Line::from(Span::styled(
            format!("{p}█"),
            Style::default()
                .fg(Color::Black)
                .bg(border_color)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(prompt) = editor.command_line_label() {
        Line::from(Span::styled(
            prompt,
            Style::default()
                .fg(Color::Black)
                .bg(border_color)
                .add_modifier(Modifier::BOLD),
        ))
    } else if let Some((flabel, _)) = frame.as_ref().and_then(|f| f.label.as_ref()) {
        // Each label line is a list of styled spans; the title is the
        // first line's spans, in order.
        Line::from(
            flabel
                .first()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| Span::styled(s.text.clone(), s.style))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
        )
    } else {
        Line::from(Span::styled(
            mode_label.to_string(),
            Style::default()
                .fg(Color::Black)
                .bg(border_color)
                .add_modifier(Modifier::BOLD),
        ))
    }
}

/// The whole frame: bordered panel with session title, transcript,
/// optional approval banner, the reserved working row, the input
/// line, and the status/help row.
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
    // The loop-phase bit (docs/tui-model-wait-indicator.md): a
    // running loop names its phase (`[wait]`, `[tools]`); an
    // idle loop or an unknown marker keeps the plain bit.
    let phase = phase_state(app, running);
    // The palette colors as owned values: the palette borrow must
    // end before the mutable app borrows below (the viewport and
    // transcript lines).
    let success_fg = app.palette().color(crate::color::Role::Success);
    let status_fg = app.palette().color(crate::color::Role::Status);
    let warning_fg = app.palette().color(crate::color::Role::Warning);
    // The loop-phase bit in the pi accents: `success` while the loop
    // runs, the `dim` tone when idle (not hard-coded swatches).
    let mut status_bits: Vec<Span<'static>> = vec![Span::styled(
        phase_bit(phase),
        Style::default()
            .fg(if running { success_fg } else { status_fg })
            .add_modifier(Modifier::BOLD),
    )];
    if app.other_running_loops() > 0 {
        status_bits.push(Span::styled(
            format!(" +{} loop", app.other_running_loops()),
            // The pi `warning` accent for the extra running loops.
            Style::default().fg(warning_fg),
        ));
    }
    let title_left = Line::from(vec![Span::styled(
        format!("Session: {session_label}"),
        Style::default().add_modifier(Modifier::BOLD),
    )]);
    let block = Block::bordered()
        .title(title_left)
        .title(Line::from(status_bits).right_aligned())
        .border_style(Style::default());
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Rows inside the border:
    //   transcript (fill)
    //   waiting messages (0..=4, only when messages wait)
    //   approval banner (1, only when pending)
    //   working row (1, while the loop runs: spinner + phase)
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
    // The waiting-message block owns one layout cell for its rows
    // (docs/tui_feature_requests_from_human.md 2026-08-31, stage 1).
    let pending_rows = pending_message_lines(app, running, inner.width as usize);
    // The live stream block (docs/tui-streaming-response.md section
    // 6.3): pinned right after the existing messages (the transcript)
    // and above the model status indicator while a model response
    // streams. Empty while no model call is in flight. The block
    // grows with the arriving content: the body is bounded to half
    // the viewport height, so a long response extends the block
    // without stealing the whole screen (the transcript's Min(2)
    // absorbs the rest).
    let stream_max_body = inner.height as usize / 2;
    let stream_lines = stream_block_lines(app, inner.width as usize, stream_max_body);
    let mut constraints: Vec<Constraint> = vec![Constraint::Min(2)];
    // The live stream block owns one layout cell per row it renders
    // (docs/tui-streaming-response.md section 6.3): it sits right
    // after the existing messages, above the model status indicator.
    // No cell when no model response is streaming.
    if !stream_lines.is_empty() {
        constraints.push(Constraint::Length(stream_lines.len() as u16));
    }
    if !pending_rows.is_empty() {
        constraints.push(Constraint::Length(pending_rows.len() as u16));
    }
    if banner {
        constraints.push(Constraint::Length(1));
    }
    // The working row above the input box: the loop-phase spinner
    // and text while the loop runs (docs/tui-model-wait-indicator.md
    // section 3). No row when idle: the transcript absorbs it. The
    // input box shifts one row at the loop start/stop transition,
    // like the `pi` working indicator.
    if running {
        constraints.push(Constraint::Length(1));
    }
    // The host-reserved row above the input box (docs/ui-extension.md
    // section 4, `row` capability): the row owner's last valid
    // `row_spec` lines, one layout cell per line. No cell when no row
    // extension is installed, or when the owner's content is empty
    // (the bare TUI shows no row).
    let row_lines = host.row_spec().unwrap_or_default();
    let row_cells: Vec<Line<'static>> = row_lines
        .iter()
        .map(|l| {
            Line::from(
                l.spans
                    .iter()
                    .map(|s| Span::styled(s.text.clone(), s.style))
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    for _ in &row_cells {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Length(input_area_h));
    constraints.push(Constraint::Length(status_n));
    let rows = ratatui::layout::Layout::vertical(constraints).split(inner);

    // transcript
    let t_area = rows[0];
    let t_width = t_area.width.saturating_sub(2) as usize;
    let h = t_area.height as usize;
    let scroll0 = app.scroll();
    app.set_viewport_height(h);
    // The position bar show decision (section 3): browse shows it
    // always; normal mode shows it when the view left the tail.
    // While shown, the bar owns the rightmost transcript column.
    let browse_active = app.browse_ref().active();
    let bar_shown = browse_active || scroll0 > 0;
    let bar_w = if bar_shown { 1 } else { 0 };
    // The text width with the bar and the browse gutter reserved
    // (sections 3 and 4.3): the width drives the wrap, the wrap the
    // total, the total the gutter width: iterate to the fixpoint.
    let (text_w, gutter_w) = resolve_transcript_width(app, host, t_width, bar_w, browse_active);
    // The total and the browse layout sync run before the lines
    // borrow: the cache holds the lines, so no app borrow may stay
    // live while the app mutates (the palette owned values above,
    // same pattern).
    let total = app.transcript_lines(text_w, Some(host)).len();
    let mut scroll = scroll0;
    if browse_active {
        let grew = app.take_events_grew();
        app.browse().sync(total, h, &mut scroll, grew);
        app.set_scroll(scroll);
    }
    let start = total.saturating_sub(scroll + h);
    // The cursor col clamps to the visible cursor line length
    // (section 4.1): a transient lines read, released before the
    // mutation.
    if browse_active {
        let (cl, _) = app.browse_ref().line_col();
        if cl >= start && cl < start + h {
            let len = {
                let ls = app.transcript_lines(text_w, Some(host));
                ls[cl].spans.iter().map(|s| s.content.chars().count()).sum::<usize>()
            };
            app.browse().clamp_col(len);
        }
    }
    // The press-path layout: the line texts and the width the
    // browse motions and the search read.
    if browse_active {
        let texts: Vec<String> = app
            .transcript_lines(text_w, Some(host))
            .iter()
            .map(ToString::to_string)
            .collect();
        app.set_browse_layout(total, h, text_w, texts);
    }
    // The owned browse draw inputs: the cursor, the match-line
    // cache, the highlight styles. The cache clone is one pass per
    // frame; the styles own their colors, so the palette borrow
    // ends before the lines borrow.
    let cursor_pos = app.browse_ref().line_col();
    let (hl, active_match): (
        std::collections::HashSet<usize>,
        Option<(usize, usize)>,
    ) = if browse_active {
        app.browse_highlight(total)
    } else {
        (std::collections::HashSet::new(), None)
    };
    let pl = app.palette();
    let dim_style = pl.style(crate::color::Role::Status, Modifier::empty());
    let accent_style = pl.style(crate::color::Role::Border4, Modifier::empty());
    let cursor_bg = pl.color(crate::color::Role::Status);
    let match_style = pl.style(crate::color::Role::Hint, Modifier::BOLD);
    let active_style = pl.style(crate::color::Role::Border4, Modifier::BOLD);
    let track_c = pl.color(crate::color::Role::Status);
    let thumb_c = pl.color(crate::color::Role::PlainText);
    let mark_c = pl.color(crate::color::Role::Border4);
    let lines = app.transcript_lines(text_w, Some(host));
    let window = &lines[start..];
    if window.is_empty() {
        let placeholder = match app.active() {
            // The placeholder in the pi `dim` tone (not a hard-coded gray).
            Some(_) => Line::from(Span::styled(
                " (no events yet — type a message below, then Ctrl+R to run the loop)",
                Style::default().fg(status_fg),
            )),
            None => Line::from(Span::styled(
                " (no session yet — type the new session name below, Enter confirms)",
                Style::default().fg(status_fg),
            )),
        };
        let p = Paragraph::new(vec![placeholder]);
        f.render_widget(p, t_area);
    } else {
        let draw_lines = if browse_active {
            browse_window_lines(
                lines,
                start,
                h,
                gutter_w,
                cursor_pos,
                &hl,
                active_match,
                dim_style,
                accent_style,
                cursor_bg,
                match_style,
                active_style,
            )
        } else {
            window.to_vec()
        };
        let p = Paragraph::new(draw_lines);
        f.render_widget(p, t_area);
        // The position bar: one column at the right edge (section 3).
        if bar_shown {
            let cursor_line = if browse_active {
                let (cl, _) = cursor_pos;
                if cl >= start && cl < start + h {
                    Some(cl)
                } else {
                    None
                }
            } else {
                None
            };
            draw_position_bar(
                f,
                &t_area,
                total,
                scroll,
                cursor_line,
                track_c,
                thumb_c,
                mark_c,
            );
        }
    }

    let mut row = 1usize;
    // Live stream block (docs/tui-streaming-response.md section 6.3):
    // the in-progress model response, rendered right after the
    // existing messages, above the model status indicator (working
    // row). It does not scroll with the transcript; the settle event
    // moves the text into the transcript and clears the block.
    if !stream_lines.is_empty() {
        for (i, l) in stream_lines.iter().enumerate() {
            if i as u16 >= rows[row].height {
                break;
            }
            let sub = ratatui::layout::Rect {
                x: rows[row].x,
                y: rows[row].y + i as u16,
                width: rows[row].width,
                height: 1,
            };
            f.render_widget(Paragraph::new(l.clone()), sub);
        }
        row += 1;
    }

    // waiting messages: the steering list of the active session
    // (docs/tui_feature_requests_from_human.md 2026-08-31, stage 1).
    // One layout cell for the block; one row per line, like the
    // status rows.
    if !pending_rows.is_empty() {
        let p_area = rows[row];
        for (i, l) in pending_rows.iter().enumerate() {
            if i as u16 >= p_area.height {
                break;
            }
            let sub = ratatui::layout::Rect {
                x: p_area.x,
                y: p_area.y + i as u16,
                width: p_area.width,
                height: 1,
            };
            f.render_widget(Paragraph::new(l.clone()), sub);
        }
        row += 1;
    }

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
        // The banner background is the pi `warning` accent (not a
        // hard-coded yellow); the text keeps the black-on-color chip
        // style.
        let l = Line::from(vec![Span::styled(
            text,
            Style::default()
                .fg(Color::Black)
                .bg(warning_fg)
                .add_modifier(Modifier::BOLD),
        )]);
        f.render_widget(Paragraph::new(l), rows[row]);
        row += 1;
    }

    // working row: shown while the loop runs. The spinner and the
    // phase text. No row when idle (docs/tui-model-wait-indicator.md
    // section 3).
    if running {
        let now = chrono::Utc::now();
        f.render_widget(Paragraph::new(working_row(app, running, &now)), rows[row]);
        row += 1;
    }

    // The host-reserved row above the input box (docs/ui-extension.md
    // section 4, `row` capability): the row owner's content, one cell
    // per line. No cell when no row extension is installed, or when
    // the owner's content is empty (the bare TUI shows no row).
    for l in &row_cells {
        f.render_widget(Paragraph::new(l.clone()), rows[row]);
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
        .unwrap_or_else(|| app.palette().thinking_border(app.thinking_level()));
    // The draft text's own color: the capability-aware prose color,
    // not the terminal default (color.rs: the "all gray by default"
    // complaint). The inverted caret cell is unchanged.
    let prose = app
        .palette()
        .style(crate::color::Role::PlainText, Modifier::empty());
    // The box border: the search prompt wins over the frame label
    // (the host keeps its modal-state render when a frame extension
    // owns the chrome, docs/ui-extension.md section 10); a frame
    // label otherwise replaces the built-in mode label.
    // The mode label first: it is a value, so the call below keeps
    // no second borrow of `app` (the editor borrow is the only one).
    let mode_label = app.editor_mode_label();
    // The browse command line prompt, when the overlay owns the
    // input (docs/tui-conversation-browsing.md section 4.4).
    let browse_prompt = if app.browse_ref().active() {
        app.browse_ref().prompt()
    } else {
        None
    };
    let title = input_box_title(
        &*app.editor(),
        &mode_label,
        &frame,
        border_color,
        browse_prompt.as_deref(),
    );
    let input_block = Block::bordered()
        .border_type(border_type)
        .border_style(Style::default().fg(border_color))
        .title(title);
    let inner_i = input_block.inner(i_area);
    f.render_widget(input_block, i_area);

    // The editor lines. `naming` shows the single-line name input
    // (the session-name bar, pre-active-session). Otherwise the
    // multi-line editor, scrolled by `edit_scroll`. Long lines wrap at
    // the box edge (`input_wrap_w`); the box shows a window of
    // `input_interior` display rows, and the cursor stays in view.
    if !naming {
        app.editor_scroll_to_cursor(input_interior, input_wrap_w);
    }
    let scroll = app.edit_scroll();
    let ed_lines: Vec<String> = if naming {
        vec![app.pending_name().unwrap_or_default().to_string()]
    } else {
        app.editor()
            .display_rows(scroll, input_interior, input_wrap_w)
            .into_iter()
            .map(|r| r.text)
            .collect()
    };
    // One paragraph per editor line. No line-level highlight; the
    // cursor row shows a single inverted block cell at the caret
    // column so the position is always visible, and the hardware
    // cursor sits just after it. In wrapped mode the cursor row/col
    // are display-row based: the cursor may sit on a wrapped fragment
    // of a long line.
    let cursor_row = if naming {
        0
    } else {
        app.editor()
            .cursor_display(input_wrap_w)
            .0
            .saturating_sub(app.edit_scroll())
    };
    let cursor_col = if naming {
        // One past the rendered trailing `_`: the `> ` prefix plus
        // the name plus the underscore.
        app.pending_name().map_or(3, |n| 3 + n.chars().count())
    } else {
        app.editor().cursor_display(input_wrap_w).1
    };
    // The search command line owns the input: the prompt renders in
    // the box title and the text-area cursor block stays off. The
    // browse command line (docs/tui-conversation-browsing.md
    // section 4.4) does the same: the caret parks on the prompt.
    let in_command_line = app.editor().command_line_label().is_some()
        || (app.browse_ref().active() && app.browse_ref().typing());
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
                Span::styled(l.clone(), prose),
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
                spans.push(Span::styled(before.iter().collect::<String>(), prose));
            }
            spans.push(Span::styled(caret.to_string(), style));
            if !after.is_empty() {
                spans.push(Span::styled(after.iter().collect::<String>(), prose));
            }
            Line::from(spans)
        } else {
            Line::from(Span::styled(l.clone(), prose))
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
        // The browse caret (docs/tui-conversation-browsing.md
        // section 4.1): the block cell over the transcript row, or
        // the box title while the browse command line types.
        if app.browse_ref().active() {
            if let Some(prompt) = app.browse_ref().prompt() {
                let prompt_w = prompt.chars().count() + 1;
                let cx = inner_i.x + 1 + prompt_w as u16;
                let cy = inner_i.y.saturating_sub(1);
                *cursor = Some((cx.min(inner_i.x + inner_i.width), cy));
            } else {
                let (cl, cc) = app.browse_ref().line_col();
                if cl >= start && cl < start + h {
                    let cx = t_area.x + gutter_w as u16 + cc as u16;
                    let cy = t_area.y + (cl - start) as u16;
                    *cursor = Some((cx, cy));
                }
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

    // The @ picker float (docs/tui-file-picker.md). Drawn last so it
    // overlays the transcript and input rows. The picker and the
    // browse overlay never coexist (section 5.3).
    let picker_open = app.picker_ref().open;
    if picker_open {
        let snap: Arc<crate::picker::fuzzy::Snapshot> = app
            .picker_matcher_ref()
            .as_ref()
            .map(|m| m.snapshot())
            .unwrap_or_else(|| {
                Arc::new(crate::picker::fuzzy::Snapshot {
                    items: Vec::new(),
                    query: String::new(),
                    settled: false,
                })
            });
        let previewer = crate::picker::preview::FilePreviewer::new(50);
        let show_preview = previewer.enabled()
            && app.picker_ref().preview_visible(
                snap.items.len(),
                crate::picker::render::PREVIEW_CUTOFF,
            );
        let layout = crate::picker::render::compute_float_layout(f.area(), show_preview);

        // Keep the visible window in sync with the pane height so
        // Ctrl-J/Ctrl-K paging stays within the rendered list area
        // (the list block's two border rows are not visible content).
        let rows = layout.list.height.saturating_sub(2).max(1) as usize;
        app.picker().visible = rows;

        let hints = "enter ok · esc keep · ctrl-j/ctrl-k move · ctrl-p preview · ctrl-i scope";
        // Clone the palette so the mutable picker borrow below does not
        // overlap an immutable borrow of the same app.
        let palette = app.palette().clone();
        crate::picker::render::render_picker()
            .f(f)
            .state(app.picker())
            .snapshot(&snap)
            .layout(&layout)
            .previewer(&previewer)
            .hints(hints)
            .palette(&palette)
            .cursor(cursor)
            .call();
    }

    // The `:` command-palette float (docs/tui-command-palette.md).
    // Drawn last, over the transcript and input rows, just like the
    // picker. The palette and the picker never coexist (section 12).
    if app.palette_state().open {
        let items = app.palette_ranked();
        let show_preview = app.palette_state().preview_visible(
            items.len(),
            crate::picker::render::PREVIEW_CUTOFF,
        );
        let layout = crate::float::compute_float_layout(f.area(), show_preview);
        let rows = layout.list.height.saturating_sub(2).max(1) as usize;
        let palette = app.palette().clone();
        let pstate = app.palette_state_mut();
        pstate.visible = rows;
        pstate.sync(items.len());
        crate::palette::render::render_palette()
            .f(f)
            .state(pstate)
            .items(&items)
            .layout(&layout)
            .palette(&palette)
            .cursor(cursor)
            .call();
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
    #[test]
    fn tool_result_box_rows_stay_inside_the_width() {
        // The narrow-pane clamp: a long tool result must not push any
        // box row past the requested width (no terminal wrap).
        let long = "x".repeat(80);
        let value = serde_json::json!({ "text": format!("{long}\nsecond line"), "exit_code": 0 });
        let ev = Event::parse_line(&format!(
            r#"{{"v":1,"type":"tool_result","ts":"t","id":"c9","value":{value},"is_error":false}}"#
        ))
        .unwrap();
        let app = app_with_session(vec![ev]);
        for width in [60, 69, 80, 120] {
            let lines = build_transcript_lines(&app, width, None);
            for l in &lines {
                let w: usize = l.spans.iter().map(|s| s.content.chars().count()).sum();
                assert!(
                    w <= width,
                    "a transcript row of {w} cols overflows a {width}-col pane: {l:?}"
                );
            }
        }
    }

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

    /// Release the whole pace queue (docs/tui-streaming-response.md
    /// section 6.5): the render tests assert on settled content, not
    /// on the pacing.
    fn settle_pace(app: &mut App) {
        while app.stream_pending_chars() > 0 {
            app.pump_stream_pacing();
        }
    }

    /// An editor in the search command line, with `ab` typed.
    fn command_line_editor() -> crate::vim_editor::Editor {
        let mut e = crate::vim_editor::Editor::new();
        e.set_text("alpha beta");
        e.press(crate::app::Key::Esc);
        e.press(crate::app::Key::Char('/'));
        e.press(crate::app::Key::Char('a'));
        e.press(crate::app::Key::Char('b'));
        e
    }

    /// One drawn frame on a test backend: the buffer holds every
    // cell, so the bar and gutter rows assert on real draw calls
    /// (the section 9 mutation gate).
    fn draw_frame(app: &mut App, width: u16, height: u16) -> ratatui::backend::TestBackend {
        let (host, _tmp) = empty_host();
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut term = ratatui::Terminal::new(backend).expect("test backend");
        let _ = term.draw(|f| {
            let mut cursor = None;
            draw(f, app, &mut cursor, &host);
        });
        term.backend().clone()
    }

    /// The count of buffer cells whose character passes `pred`.
    fn cell_count(backend: &ratatui::backend::TestBackend, pred: impl Fn(char) -> bool) -> usize {
        let buf = backend.buffer();
        let area = buf.area();
        let mut n = 0;
        for y in 0..area.height {
            for x in 0..area.width {
                if let Some(cell) = buf.cell((x, y)) {
                    if pred(cell.symbol().chars().next().unwrap_or(' ')) {
                        n += 1;
                    }
                }
            }
        }
        n
    }

    /// Twenty user messages: well past one viewport of transcript
    /// lines, with no block glyphs in the content.
    fn long_session_app() -> App {
        let evs: Vec<Event> = (0..20).map(|i| produce::user_message(&format!("message {i}"))).collect();
        app_with_session(evs)
    }

    // The position bar rows of section 9 (the draw-level rows).
    #[test]
    fn bar_hides_at_the_tail() {
        // "bar hidden at the tail": normal mode, `scroll = 0` — no
        // bar column; the text width is the full pane.
        let mut app = long_session_app();
        app.set_viewport_height(24);
        let b = draw_frame(&mut app, 80, 30);
        assert_eq!(
            cell_count(&b, |c| matches!(c, '█' | '▼' | '▶')),
            0,
            "no bar column at the tail"
        );
    }

    #[test]
    fn bar_shows_on_scroll_back() {
        // "bar on scroll-back": normal mode, `scroll > 0` — one
        // right-edge column, the thumb plus the tail marker.
        let mut app = long_session_app();
        app.set_viewport_height(24);
        app.scroll_up(10);
        let b = draw_frame(&mut app, 80, 30);
        assert!(
            cell_count(&b, |c| c == '█') >= 1,
            "the thumb shows on the right edge"
        );
        assert_eq!(cell_count(&b, |c| c == '▼'), 1, "the tail marker shows");
        assert_eq!(cell_count(&b, |c| c == '▶'), 0, "no cursor marker outside browse");
    }

    #[test]
    fn bar_shows_in_browse_with_the_cursor_marker() {
        // "bar in browse": browse mode, `scroll = 0` — the bar
        // shows, and the cursor marker sits on the cursor line (the
        // last line after `G`).
        let mut app = long_session_app();
        app.set_viewport_height(24);
        app.editor().press(crate::app::Key::Esc);
        app.press(crate::app::Key::Char('s'));
        app.press(crate::app::Key::Char('s'));
        assert!(app.browse_ref().active());
        // Prime the press-path layout, then drive `G`: the cursor
        // to the last line, the view to the tail.
        app.set_browse_layout(200, 24, 60, vec!["line".to_string(); 200]);
        let mut sc = app.scroll();
        app.browse().sync(200, 24, &mut sc, false);
        app.set_scroll(sc);
        assert!(app.press(crate::app::Key::Char('G')).is_empty());
        assert_eq!(app.scroll(), 0, "the view is at the tail");
        let b = draw_frame(&mut app, 80, 30);
        assert_eq!(cell_count(&b, |c| c == '▶'), 1, "the cursor marker shows");
        assert!(cell_count(&b, |c| c == '█') >= 1, "the thumb shows in browse");
    }

    /// True when any buffer row, read left to right, holds `needle`.
    fn buffer_contains(backend: &ratatui::backend::TestBackend, needle: &str) -> bool {
        let buf = backend.buffer();
        let area = buf.area();
        for y in 0..area.height {
            let mut row = String::new();
            for x in 0..area.width {
                if let Some(cell) = buf.cell((x, y)) {
                    row.push_str(cell.symbol());
                }
            }
            if row.contains(needle) {
                return true;
            }
        }
        false
    }

    #[test]
    fn browse_content_follows_the_scroll() {
        // Regression: the browse window shows the lines `start..start+h`
        // of the transcript, not the first `h` lines. The content
        // must follow the view, like the gutter and the bar.
        let mut app = long_session_app();
        app.set_viewport_height(24);
        app.editor().press(crate::app::Key::Esc);
        app.press(crate::app::Key::Char('s'));
        app.press(crate::app::Key::Char('s'));
        assert!(app.browse_ref().active());
        // The first browse frame primes the press-path layout.
        let _ = draw_frame(&mut app, 80, 30);
        // `gg`: the cursor to the top, the view to the top.
        assert!(app.press(crate::app::Key::Char('g')).is_empty());
        assert!(app.press(crate::app::Key::Char('g')).is_empty());
        let top = draw_frame(&mut app, 80, 30);
        assert!(
            buffer_contains(&top, "message 0"),
            "the top view shows the first message"
        );
        // `G`: the cursor to the last line, the view to the tail.
        assert!(app.press(crate::app::Key::Char('G')).is_empty());
        let tail = draw_frame(&mut app, 80, 30);
        assert!(
            buffer_contains(&tail, "message 19"),
            "the tail view shows the last message"
        );
        assert!(
            !buffer_contains(&tail, "message 5"),
            "the middle messages scrolled out of the view"
        );
    }

    #[test]
    fn the_gutter_numbers_show_in_browse() {
        // The gutter rows of section 4.3: in browse mode the left
        // gutter shows the absolute number at the cursor, the
        // relative distance elsewhere; outside browse there is no
        // gutter.
        let mut app = long_session_app();
        app.set_viewport_height(24);
        app.editor().press(crate::app::Key::Esc);
        // Outside browse: no gutter digits crowd the left edge.
        let plain = draw_frame(&mut app, 80, 30);
        app.press(crate::app::Key::Char('s'));
        app.press(crate::app::Key::Char('s'));
        assert!(app.browse_ref().active());
        let in_browse = draw_frame(&mut app, 80, 30);
        let digits_plain = cell_count(&plain, |c| c.is_ascii_digit());
        let digits_browse = cell_count(&in_browse, |c| c.is_ascii_digit());
        assert!(
            digits_browse > digits_plain,
            "the gutter adds the line numbers ({digits_browse} > {digits_plain})"
        );
        let _ = plain;
    }

    /// A frame spec whose label is the mode text `label`.
    fn frame_with_label(label: &str) -> Option<crate::ext::FrameSpec> {
        use crate::ext::{ExtLine, FrameSpec};
        Some(FrameSpec {
            border: None,
            label: Some((
                vec![ExtLine::styled(label, Style::default())],
                Style::default(),
            )),
            height: None,
        })
    }

    #[test]
    fn thinking_block_renders_above_the_assistant_message() {
        // The 2026-09-02 report: the thinking content showed after the
        // assistant message and the tool results. The transcript reads
        // thinking first, then the actions (docs/tui-thinking-block.md
        // section 4: the reasoning content shows above the message body).
        let reasoning = json!([{
            "type": "reasoning",
            "id": "rs_1",
            "status": "completed",
            "content": [{"type": "reasoning_text", "text": "consider the options first"}],
            "summary": [],
            "encrypted_content": null
        }]);
        let line = format!(
            r#"{{"v":1,"type":"assistant_message","ts":"t","content":"hello","tool_calls":[],"stop_reason":"stop","usage":{{"input_tokens":10,"output_tokens":2}},"reasoning":{reasoning}}}"#
        );
        let ev = Event::parse_line(&line).unwrap();
        let app = app_with_session(vec![ev]);
        let lines = build_transcript_lines(&app, 80, None);
        let joined = join(&lines);
        let think = joined.find("thinking").unwrap_or(usize::MAX);
        let asst = joined.find("assistant").unwrap_or(usize::MAX);
        assert!(
            think < asst,
            "the thinking row must come before the assistant row:\n{joined}"
        );
    }

    #[test]
    fn input_box_title_prompt_wins_over_the_frame_label() {
        // The typed prompt must stay visible when a frame extension
        // owns the chrome (docs/ui-extension.md section 10): the
        // host keeps its modal-state render in the box title.
        let e = command_line_editor();
        let frame = frame_with_label("[COMMAND]");
        let title = input_box_title(&e, "[COMMAND]", &frame, Color::DarkGray, None);
        assert_eq!(title.to_string(), "/ab\u{2588}");
        // No frame label: the prompt still shows (the built-in case).
        let none: Option<crate::ext::FrameSpec> = None;
        let title = input_box_title(&e, "[COMMAND]", &none, Color::DarkGray, None);
        assert_eq!(title.to_string(), "/ab\u{2588}");
    }

    #[test]
    fn input_box_title_browse_prompt_wins() {
        // The browse command line owns the box title (docs/tui-
        // conversation-browsing.md section 4.4), over the editor
        // prompt and the frame label.
        let e = command_line_editor();
        let frame = frame_with_label("[COMMAND]");
        let title =
            input_box_title(&e, "[COMMAND]", &frame, Color::DarkGray, Some("/err"));
        assert_eq!(title.to_string(), "/err\u{2588}");
    }

    #[test]
    fn input_box_title_frame_label_wins_outside_command_line() {
        // Outside command-line mode the frame label replaces the
        // built-in mode label, like before the prompt fix.
        let mut e = crate::vim_editor::Editor::new();
        e.set_text("abc");
        e.press(crate::app::Key::Esc); // normal mode
        let frame = frame_with_label("[COMMAND]");
        let title = input_box_title(&e, "[NORMAL]", &frame, Color::DarkGray, None);
        assert_eq!(title.to_string(), "[COMMAND]");
        // No frame label: the built-in mode label shows.
        let none: Option<crate::ext::FrameSpec> = None;
        let title = input_box_title(&e, "[NORMAL]", &none, Color::DarkGray, None);
        assert_eq!(title.to_string(), "[NORMAL]");
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
        // The marker-free render (docs/tui-markdown-render.md): the
        // italic word shows without its stars.
        assert!(
            flat.contains("left of the rendered"),
            "italic word lost its neighbours: {flat}"
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

    /// The compact markers render their lines (docs/auto-compact-plan.md
    /// section 4.6). The started line shows the trigger and the
    /// scale. The summary line names the boundary. The failed line
    /// shows the detail.
    #[test]
    fn compaction_marker_lines_render() {
        let evs = vec![
            Event::parse_line(
                r#"{"v":1,"type":"compaction_started","ts":"t","reason":"threshold","tokens_before":212992}"#,
            )
            .unwrap(),
            Event::parse_line(
                r#"{"v":1,"type":"compaction_summary","ts":"t","summary":"the summary of the old region","first_kept_seq":312,"reason":"threshold","tokens_before":212992,"tokens_after":33000,"read_files":[],"modified_files":[]}"#,
            )
            .unwrap(),
            Event::parse_line(
                r#"{"v":1,"type":"compaction_failed","ts":"t","reason":"overflow","last_user_seq":41,"attempts":2,"detail":"the model returned an error stop"}"#,
            )
            .unwrap(),
        ];
        let app = app_with_session(evs);
        let joined = join(&build_transcript_lines(&app, 80, None));
        assert!(
            joined.contains("compacting (threshold): 213k tokens"),
            "the started line shows the trigger and the scale: {joined}"
        );
        assert!(
            joined.contains(
                "context compacted (threshold): 213k to 33k tokens, keeping events from seq 312"
            ),
            "the summary line names the boundary: {joined}"
        );
        assert!(
            joined.contains("the summary of the old region"),
            "the summary body rides under the gutter: {joined}"
        );
        assert!(
            joined.contains("compaction failed (overflow)"),
            "the failed line shows: {joined}"
        );
        assert!(
            joined.contains("the model returned an error stop"),
            "the detail rides with the failed line: {joined}"
        );
    }

    /// An open compaction marker with the loop process not running
    /// renders the interrupted form. The closed markers render the
    /// plain form.
    #[test]
    fn open_compaction_marker_renders_interrupted_when_the_loop_is_dead() {
        let evs = vec![
            Event::parse_line(
                r#"{"v":1,"type":"compaction_started","ts":"t","reason":"threshold","tokens_before":212992}"#,
            )
            .unwrap(),
            Event::parse_line(
                r#"{"v":1,"type":"compaction_summary","ts":"t","summary":"s","first_kept_seq":42,"reason":"threshold","tokens_before":212992,"tokens_after":33000,"read_files":[],"modified_files":[]}"#,
            )
            .unwrap(),
            // The open marker: no summary or failed after it.
            Event::parse_line(
                r#"{"v":1,"type":"compaction_started","ts":"t","reason":"overflow","tokens_before":99000}"#,
            )
            .unwrap(),
        ];
        let app = app_with_session(evs);
        let joined = join(&build_transcript_lines(&app, 80, None));
        // The transcript session has no running loop process: the
        // last open marker renders the interrupted form, and the
        // closed one renders the plain line.
        assert!(
            joined.contains("compacting (interrupted)"),
            "the open marker with the loop dead renders interrupted: {joined}"
        );
        assert!(
            joined.contains("compacting (threshold): 213k tokens"),
            "the closed marker renders the plain form: {joined}"
        );
    }

    /// The fork marker renders its dim line naming the target and
    /// the mode (docs/rewind-fork-design.md section 6). Missing
    /// fields degrade to placeholders, never to a crash (G5).
    #[test]
    fn rewind_marker_line_renders() {
        let evs = vec![
            Event::parse_line(
                r#"{"v":1,"type":"rewind","ts":"t","target_seq":41,"mode":"before"}"#,
            )
            .unwrap(),
            Event::parse_line(
                r#"{"v":1,"type":"rewind","ts":"t","target_seq":7,"mode":"on","reason":"tui_pick"}"#,
            )
            .unwrap(),
        ];
        let app = app_with_session(evs);
        let joined = join(&build_transcript_lines(&app, 80, None));
        assert!(
            joined.contains("rewound to seq 41 (before)"),
            "the marker names the target and mode: {joined}"
        );
        assert!(
            joined.contains("rewound to seq 7 (on)"),
            "the second marker renders too: {joined}"
        );
        // The degraded form: a marker without the optional fields.
        let bare = vec![Event::parse_line(r#"{"v":1,"type":"rewind","ts":"t"}"#).unwrap()];
        let app = app_with_session(bare);
        let joined = join(&build_transcript_lines(&app, 80, None));
        assert!(
            joined.contains("rewound to seq 0 (on)"),
            "missing fields degrade to placeholders: {joined}"
        );
    }

    #[test]
    fn long_tool_result_folds_to_the_preview_cap() {
        // The rescoped rule (docs/tui-tool-result-truncation.md
        // section 4): the result no longer displays in full. The
        // unknown tool's body folds to the preview cap, with the
        // fold hint naming the remainder and the expand key.
        let text = (0..100)
            .map(|i| format!("tool out {i:03}"))
            .collect::<Vec<_>>()
            .join("\n");
        let value = json!({ "text": text, "exit_code": 0 });
        let evs = vec![Event::parse_line(&format!(
            r#"{{"v":1,"type":"tool_result","ts":"t","id":"c9","value":{value},"is_error":false}}"#
        ))
        .unwrap()];
        let app = app_with_session(evs.clone());
        let joined = join(&build_transcript_lines(&app, 80, None));
        assert!(
            joined.contains("92 more lines"),
            "the fold hint states the remainder: {joined}"
        );
        assert!(joined.contains("tool out 000"), "the first line shows");
        assert!(
            !joined.contains("tool out 099"),
            "the collapsed preview holds the first lines only: {joined}"
        );
        // The global expand toggle (Ctrl+O) opens the full body, up
        // to the expanded cap.
        let mut app = app_with_session(evs);
        app.press(crate::app::Key::CtrlO);
        let joined = join(&build_transcript_lines(&app, 80, None));
        assert!(
            joined.contains("tool out 099"),
            "the expanded state shows the body: {joined}"
        );
        assert!(
            !joined.contains("more lines"),
            "the expanded state drops the fold hint: {joined}"
        );
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
    fn markdown_content_renders_without_the_markers() {
        // The marker-free render (docs/tui-markdown-render.md): the
        // styles in, the markers out. The list bullet stays a
        // visible bullet; the `#`, `>`, emphasis stars, and the
        // backticks drop; the link text shows and the URL stays.
        let content = "# Title\n- item\n`code` **b** *i* [t](u)\n> quote";
        let evs = vec![produce::user_message(content)];
        let app = app_with_session(evs);
        let lines = build_transcript_lines(&app, 80, None);
        let palette = app.palette();
        let find = |needle: &str| lines.iter().position(|l| l.to_string().contains(needle));
        // Word wrapping splits a line into spans, so assert over the
        // spans of the line that holds the syntax, not global spans.
        let hl = find("Title").expect("heading line missing");
        assert!(
            !lines[hl].to_string().contains('#'),
            "the hash run drops: {:?}",
            lines[hl]
        );
        assert!(lines[hl].spans.iter().any(|s| s.content.as_ref() == "Title"
            && s.style.fg
                == palette
                    .style(crate::color::Role::Heading, Modifier::BOLD)
                    .fg));
        let ql = find("quote").expect("quote line missing");
        assert!(
            !lines[ql].to_string().trim_start().starts_with('>'),
            "the > marker drops: {:?}",
            lines[ql]
        );
        let ul = find("item").expect("list line missing");
        assert!(
            lines[ul]
                .spans
                .iter()
                .any(|s| s.content.as_ref() == "-" && s.style.add_modifier.contains(Modifier::BOLD)),
            "the list bullet stays a visible bullet: {:?}",
            lines[ul]
        );
        let cl = find("code").expect("code line missing");
        assert!(
            !lines[cl].to_string().contains('`'),
            "the backticks drop: {:?}",
            lines[cl]
        );
        assert!(lines[cl].spans.iter().any(|s| s.content.as_ref() == "code"));
        assert!(lines[cl]
            .spans
            .iter()
            .any(|s| s.content.as_ref() == "b"
                && s.style.add_modifier.contains(Modifier::BOLD)),
        "the bold word shows without the stars: {:?}",
            lines[cl]);
        assert!(
            lines[cl].spans.iter().any(|s| s.content.as_ref() == "i"
                && s.style.add_modifier.contains(Modifier::UNDERLINED)),
            "the italic word shows without the stars: {:?}",
            lines[cl]
        );
        // The link line shows the link text and the URL, without
        // the [ ] ( ) markers.
        let link_style = palette.style(crate::color::Role::Link, Modifier::UNDERLINED);
        let url_style = palette.style(crate::color::Role::LinkUrl, Modifier::DIM);
        let ll = lines
            .iter()
            .position(|l| {
                l.spans
                    .iter()
                    .any(|s| s.content.as_ref() == "t" && s.style == link_style)
            })
            .expect("link line missing");
        assert!(
            lines[ll]
                .spans
                .iter()
                .any(|s| s.content.as_ref() == "u" && s.style == url_style),
            "the URL shows on the link line: {:?}",
            lines[ll]
        );
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
        // The box cells carry the light box background; the token
        // colors are the foregrounds. Compare fg + modifiers, not
        // the full style (the bg is the box role). Each cell is
        // padded to the pane width: trim before comparing. The
        // expected styles are the palette roles of `json_line_p`.
        let p = app.palette();
        let key = p.style(crate::color::Role::SyntaxVariable, Modifier::empty());
        let num = p.style(crate::color::Role::SyntaxNumber, Modifier::empty());
        let nul = p.style(crate::color::Role::SyntaxNumber, Modifier::empty());
        let spans: Vec<(Style, &str)> = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| (s.style, s.content.as_ref())))
            .collect();
        let hit = |needle: &str, want: Style| {
            spans.iter().any(|(st, t)| {
                t.trim() == needle && st.fg == want.fg && st.add_modifier == want.add_modifier
            })
        };
        assert!(hit("\"a\"", key), "json key not styled: {spans:?}");
        assert!(hit("1", num), "json number not styled: {spans:?}");
        assert!(hit("null", nul), "json null not styled: {spans:?}");
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
    fn bash_call_merges_into_the_result_box() {
        // The bash tool_call line drops when its result follows:
        // the result box body opens with the `$ <command>` line, so
        // the separate call line would repeat the command.
        let result = Event::parse_line(
            r#"{"v":1,"type":"tool_result","ts":"t","id":"c9","value":{"text":"$ make\ndone","exit_code":0},"is_error":false}"#,
        )
        .unwrap();
        let call = Event::parse_line(
            r#"{"v":1,"type":"tool_call","ts":"t","id":"c9","name":"bash","arguments":{"command":"make"}}"#,
        )
        .unwrap();
        let app = app_with_session(vec![call, result]);
        let joined = join(&build_transcript_lines(&app, 80, None));
        assert!(
            joined.contains("$ make"),
            "the box body keeps the command line: {joined}"
        );
        assert!(
            !joined.contains("{\"command\":\"make\"}"),
            "the merged call line must not repeat the raw args: {joined}"
        );
        // A call without a result yet keeps its own line: the
        // command is the only view of a running tool.
        let solo = app_with_session(vec![Event::parse_line(
            r#"{"v":1,"type":"tool_call","ts":"t","id":"c9","name":"bash","arguments":{"command":"make"}}"#,
        )
        .unwrap()]);
        let joined = join(&build_transcript_lines(&solo, 80, None));
        assert!(
            joined.contains("{\"command\":\"make\"}"),
            "a pending call shows its arguments: {joined}"
        );
    }

    #[test]
    fn tool_result_box_carries_the_title_in_the_border() {
        // No standalone header line above the box: the top border
        // carries `tool:<name>  <status>` (the error accent keeps
        // the red bold through the title style). The call event
        // resolves the tool name.
        let call = Event::parse_line(
            r#"{"v":1,"type":"tool_call","ts":"t","id":"c9","name":"bash","arguments":{"command":"true"}}"#,
        )
        .unwrap();
        let ev = Event::parse_line(
            r#"{"v":1,"type":"tool_result","ts":"t","id":"c9","value":{"text":"x","exit_code":0},"is_error":false}"#,
        )
        .unwrap();
        let lines: Vec<String> =
            build_transcript_lines(&app_with_session(vec![call, ev]), 80, None)
                .iter()
                .map(|l| l.to_string())
                .collect();
        let i = lines
            .iter()
            .position(|l| l.contains("tool:bash"))
            .expect("the tool row");
        assert!(
            lines[i].starts_with('\u{250c}'),
            "the title lives in the box top border, not a header line: {lines:?}"
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
            color: None,
            color_scheme: None,
            custom_schemes: std::collections::HashMap::new(),
            tool_display: crate::tool_display::ToolDisplay::preset(
                crate::tool_display::Preset::OpenCode,
            ),
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
        let base = Style::default();
        let palette = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let plain = render_message_content(content, 7, None, 60, base, &palette);
        let builtin = wrap_markdown_p(content, 60, &palette, base);
        let show = |v: &Vec<Line>| v.iter().map(|l| l.to_string()).collect::<Vec<_>>();
        assert_eq!(
            show(&plain),
            show(&builtin),
            "ext None must match the built-in path"
        );
    }

    #[test]
    fn ext_path_draws_the_grid_table() {
        // The 2026-09-03 report: the ext-active block path showed
        // the raw `|` pipes. The consecutive table lines now draw
        // as a grid, like the ext-none path (docs/tui-markdown-
        // render.md section 1).
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let cfg = crate::config::TuiConfig {
            sessions_root: root.join("sessions"),
            schemas_dir: None,
            loop_cmd: None,
            config_dir: root.clone(),
            config_path: root.join("config.toml"),
            ext_dir: None,
            active_model: None,
            color: None,
            color_scheme: None,
            custom_schemes: std::collections::HashMap::new(),
            tool_display: crate::tool_display::ToolDisplay::preset(
                crate::tool_display::Preset::OpenCode,
            ),
        };
        let disc = crate::ext::discover(&cfg).unwrap();
        let host = crate::ext::ExtHost::new(&disc, &cfg);
        let palette = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let content = "before\n| a | b |\n| - | - |\n| 1 | 2 |\nafter";
        let lines =
            render_message_content(content, 1, Some(&host), 60, Style::default(), &palette);
        let text: String = lines
            .iter()
            .map(|l| l.iter().map(|s| s.content.to_string()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains('┌'), "the grid border shows: {text}");
        assert!(
            text.lines().all(|l| !l.trim_start().starts_with('|')),
            "no raw pipe line survives: {text}"
        );
        assert!(text.contains("before"), "the text before the table: {text}");
        assert!(text.contains("after"), "the text after the table: {text}");
    }

    #[test]
    fn steering_block_is_empty_when_nothing_waits() {
        let app = app_with_session(vec![
            produce::user_message("hi"),
            Event::parse_line(r#"{"v":1,"type":"assistant_message","ts":"t","content":"yo"}"#)
                .unwrap(),
        ]);
        assert!(pending_message_lines(&app, true, 80).is_empty());
    }

    #[test]
    fn steering_header_names_the_delivery() {
        let app = app_with_session(vec![produce::user_message("fix the test")]);
        let running = join(&pending_message_lines(&app, true, 80));
        assert!(running.contains("1 message waiting"), "{running}");
        assert!(
            running.contains("steering — injected at the next step"),
            "{running}"
        );
        assert!(running.contains("fix the test"), "{running}");

        let stopped = join(&pending_message_lines(&app, false, 80));
        assert!(stopped.contains("Ctrl+R run"), "{stopped}");
        assert!(
            !stopped.contains("steering"),
            "the stopped header must not promise the next step: {stopped}"
        );
    }

    #[test]
    fn steering_block_caps_the_list_and_counts_the_rest() {
        let evs: Vec<Event> = (0..6)
            .map(|i| produce::user_message(&format!("msg {i}")))
            .collect();
        let app = app_with_session(evs);
        let joined = join(&pending_message_lines(&app, true, 80));
        assert!(joined.contains("6 messages waiting"), "{joined}");
        assert!(joined.contains("msg 0"), "{joined}");
        assert!(joined.contains("msg 2"), "{joined}");
        assert!(!joined.contains("msg 3"), "{joined}");
        assert!(joined.contains("+3 more"), "{joined}");
    }

    #[test]
    fn steering_rows_stay_inside_the_width() {
        // Every row owns one terminal row: no row may wrap past the
        // reserved width.
        let app = app_with_session(vec![produce::user_message(&"x".repeat(200))]);
        let lines = pending_message_lines(&app, true, 40);
        assert!(!lines.is_empty());
        for l in &lines {
            let w: usize = l.spans.iter().map(|s| s.content.chars().count()).sum();
            assert!(w <= 40, "row is {w} columns, the row owns 40");
        }
    }

    /// The follow queue renders its own block, separate from the
    /// steer queue (docs/tui-pending-user-messages.md stage 2, the
    /// TUI part): each list keeps its own count and previews.
    #[test]
    fn follow_messages_render_their_own_block() {
        let app = app_with_session(vec![
            produce::user_message("steer me"),
            produce::user_message_follow("later"),
        ]);
        let joined = join(&pending_message_lines(&app, true, 80));
        assert!(
            joined.contains("steering — injected at the next step"),
            "{joined}"
        );
        assert!(joined.contains("steer me"), "{joined}");
        assert!(
            joined.contains("follow-up — run after the loop stops"),
            "{joined}"
        );
        assert!(joined.contains("later"), "{joined}");
        // Each block carries its own count: one steer, one follow.
        let counts: Vec<&str> = joined
            .split('\n')
            .filter(|l| l.contains("message waiting"))
            .collect();
        assert_eq!(counts.len(), 2, "one header per queue: {joined}");
    }

    /// A follow-only queue shows no steer block: the follow header
    /// stands alone with its count.
    #[test]
    fn follow_only_queue_shows_one_block() {
        let app = app_with_session(vec![produce::user_message_follow("later")]);
        let joined = join(&pending_message_lines(&app, true, 80));
        assert!(!joined.contains("steering"), "{joined}");
        assert!(joined.contains("1 message waiting"), "{joined}");
        assert!(
            joined.contains("follow-up — run after the loop stops"),
            "{joined}"
        );
    }

    // ── loop-phase indicator (docs/tui-model-wait-indicator.md) ──

    struct PhaseDummyHandle;
    impl crate::port::LoopHandle for PhaseDummyHandle {
        fn stop(&self) {}
        fn wait_exit(&self) -> i32 {
            0
        }
        fn take_lines(
            &self,
        ) -> Option<tokio::sync::mpsc::UnboundedReceiver<crate::port::LoopLine>> {
            None
        }
    }

    /// One `loop_phase` marker event with the given raw `ts` and value.
    fn phase_marker(ts: &str, value: &str) -> Event {
        Event::parse_line(&format!(
            r#"{{"v":1,"type":"ext_status","ts":"{ts}","id":"loop_phase","value":"{value}"}}"#,
            ts = ts,
            value = value
        ))
        .unwrap()
    }

    /// Attach a running loop with one output line, so the status row
    /// has a last line and the running bit is set.
    fn attach_running_loop(app: &mut App, sid: &str) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<crate::port::LoopLine>();
        app.attach_loop(
            crate::port::SessionId::new(sid),
            Box::new(PhaseDummyHandle),
            rx,
        );
        tx.send(crate::port::LoopLine::Stdout("loop out".into()))
            .unwrap();
        app.drain_loop_lines();
    }

    /// An extension host with no extensions: the built-in row owns
    /// the slot.
    fn empty_host() -> (crate::ext::ExtHost, tempfile::TempDir) {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = crate::config::TuiConfig {
            sessions_root: tmp.path().join("sessions"),
            schemas_dir: None,
            loop_cmd: None,
            config_dir: tmp.path().to_path_buf(),
            config_path: tmp.path().join("config.toml"),
            ext_dir: Some(tmp.path().join("ui_extensions")),
            active_model: None,
            color: None,
            color_scheme: None,
            custom_schemes: std::collections::HashMap::new(),
            tool_display: crate::tool_display::ToolDisplay::preset(
                crate::tool_display::Preset::OpenCode,
            ),
        };
        let disc = crate::ext::discover(&cfg).unwrap();
        let host = crate::ext::ExtHost::new(&disc, &cfg);
        (host, tmp)
    }

    fn fmt_ts(t: chrono::DateTime<chrono::Utc>) -> String {
        t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    }

    #[test]
    fn phase_state_table() {
        // The four display states (docs/tui-model-wait-indicator.md
        // section 2): idle, running-unknown, wait, tools.
        let app = app_with_session(vec![]);
        assert_eq!(phase_state(&app, false), PhaseState::Idle);
        assert_eq!(
            phase_state(&app, true),
            PhaseState::RunningUnknown,
            "no marker: unknown"
        );

        let app = app_with_session(vec![phase_marker("t", "weird")]);
        assert_eq!(
            phase_state(&app, true),
            PhaseState::RunningUnknown,
            "a value outside the two: unknown"
        );

        let app = app_with_session(vec![phase_marker("t", "wait")]);
        assert_eq!(phase_state(&app, true), PhaseState::Wait);
        assert_eq!(
            phase_state(&app, false),
            PhaseState::Idle,
            "a stopped loop is idle, marker or not"
        );

        let app = app_with_session(vec![phase_marker("t", "tools")]);
        assert_eq!(phase_state(&app, true), PhaseState::Tools);

        // A non-string value is outside the two: unknown.
        let obj = Event::parse_line(
            r#"{"v":1,"type":"ext_status","ts":"t","id":"loop_phase","value":{"a":1}}"#,
        )
        .unwrap();
        let app = app_with_session(vec![obj]);
        assert_eq!(phase_state(&app, true), PhaseState::RunningUnknown);
    }

    #[test]
    fn phase_bit_text_per_state() {
        assert_eq!(phase_bit(PhaseState::Idle), " [idle] ");
        assert_eq!(phase_bit(PhaseState::RunningUnknown), " [running] ");
        assert_eq!(phase_bit(PhaseState::Wait), " [wait] ");
        assert_eq!(phase_bit(PhaseState::Tools), " [tools] ");
    }

    #[test]
    fn wait_span_text_formats_and_clamps() {
        // N is whole seconds between the marker timestamp and now:
        // Ns under 60 s, Mm SSs at 60 s and up; a negative span
        // clamps to 0s; an unparseable timestamp yields None.
        let now = chrono::Utc::now();
        let span = |s: i64| wait_span_text(&fmt_ts(now - chrono::Duration::seconds(s)), now);
        assert_eq!(span(0), Some("0s".to_string()));
        assert_eq!(span(59), Some("59s".to_string()));
        assert_eq!(span(60), Some("1m 0s".to_string()));
        // The conformance row: a 90 s marker reads 1m 30s.
        assert_eq!(span(90), Some("1m 30s".to_string()));
        // A future marker (the loop host clock runs behind the TUI)
        // clamps to 0s.
        let future = now + chrono::Duration::seconds(30);
        assert_eq!(wait_span_text(&fmt_ts(future), now), Some("0s".to_string()));
        assert_eq!(wait_span_text("not-a-timestamp", now), None);
        assert_eq!(wait_span_text("", now), None);
    }

    #[test]
    fn working_row_shows_the_wait() {
        // The reserved row above the input box: the spinner frame, the
        // label, and the wait span since the marker.
        let ts = fmt_ts(chrono::Utc::now() - chrono::Duration::seconds(5));
        let mut app = app_with_session(vec![phase_marker(&ts, "wait")]);
        attach_running_loop(&mut app, "s1");
        let now = chrono::Utc::now();
        let joined = join(&[working_row(&app, true, &now)]);
        assert!(
            joined.contains("waiting for model · 5s"),
            "the wait row shows: {joined}"
        );
        // The timer left the statusline slot: the built-in row shows
        // the last loop line, not the wait.
        let (host, _keep) = empty_host();
        let slot = join(&status_rows(&app, &host, true, 80));
        assert!(
            !slot.contains("waiting for model"),
            "the slot is free: {slot}"
        );
        assert!(slot.contains("loop out"), "the last line shows: {slot}");
    }

    #[test]
    fn working_row_shows_the_tools_run() {
        let ts = fmt_ts(chrono::Utc::now() - chrono::Duration::seconds(75));
        let mut app = app_with_session(vec![phase_marker(&ts, "tools")]);
        attach_running_loop(&mut app, "s1");
        let now = chrono::Utc::now();
        let joined = join(&[working_row(&app, true, &now)]);
        assert!(
            joined.contains("tools running · 1m 15s"),
            "the tools row shows: {joined}"
        );
    }

    #[test]
    fn working_row_is_blank_when_idle() {
        // The loop is stopped: the bit shows [idle], the row stays
        // blank, even with a marker in the log. The row stays
        // reserved: the input box never shifts.
        let ts = fmt_ts(chrono::Utc::now() - chrono::Duration::seconds(5));
        let app = app_with_session(vec![phase_marker(&ts, "wait")]);
        let now = chrono::Utc::now();
        assert_eq!(
            join(&[working_row(&app, false, &now)]),
            "",
            "the idle row is blank"
        );
    }

    #[test]
    fn working_row_shows_working_for_an_unknown_value() {
        // A marker value outside wait/tools: the bit shows
        // [running], the row shows the generic Working... text.
        let mut app = app_with_session(vec![phase_marker("t", "weird")]);
        attach_running_loop(&mut app, "s1");
        let now = chrono::Utc::now();
        let joined = join(&[working_row(&app, true, &now)]);
        assert!(
            joined.contains("Working..."),
            "the generic text shows: {joined}"
        );
        assert!(!joined.contains("waiting for model"), "no wait: {joined}");
    }

    #[test]
    fn working_row_drops_the_span_on_an_unparseable_ts() {
        // The marker value is wait but its ts fails to parse: the
        // row keeps the label and drops the span.
        let mut app = app_with_session(vec![phase_marker("t", "wait")]);
        attach_running_loop(&mut app, "s1");
        let now = chrono::Utc::now();
        let joined = join(&[working_row(&app, true, &now)]);
        assert!(
            joined.contains("waiting for model"),
            "the label keeps: {joined}"
        );
        assert!(!joined.contains(" · "), "the span drops: {joined}");
    }

    #[test]
    fn spinner_frame_cycles_over_time() {
        // The frame is a pure function of the wall clock: two times
        // 100 ms apart show adjacent frames in the cycle.
        let now = chrono::Utc::now();
        let a = spinner_frame(&now);
        let b = spinner_frame(&(now + chrono::Duration::milliseconds(100)));
        let idx = |f: &str| WORKING_SPINNER_FRAMES.iter().position(|x| x == &f).unwrap();
        assert_eq!(
            idx(b),
            (idx(a) + 1) % WORKING_SPINNER_FRAMES.len(),
            "the frame advances by one per interval"
        );
    }
    /// P7 (missing-file): with no live stream buffer, the block is empty.
    #[test]
    fn stream_block_empty_when_no_buffer() {
        let app = app_with_session(Vec::new());
        assert!(stream_block_lines(&app, 80, 5).is_empty());
    }

    /// P4 (live-block): accumulated text renders in the live block, with a
    /// header row. The text is wrapped with the same path as a settled
    /// message, so it never exceeds the pane width.
    #[test]
    fn stream_block_shows_accumulated_text() {
        let mut app = app_with_session(Vec::new());
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".model-stream");
        std::fs::write(
            &path,
            [
                serde_json::json!({"kind": "text", "delta": "Hello "}),
                serde_json::json!({"kind": "text", "delta": "world"}),
            ]
            .iter()
            .map(|v| format!("{v}\n"))
            .collect::<String>(),
        )
        .unwrap();
        app.refresh_stream(&path);
        settle_pace(&mut app);

        let lines = stream_block_lines(&app, 80, 5);
        let text: String = lines.iter().map(|l| l.to_string()).collect();
        assert!(
            text.contains("assistant"),
            "header row should label the live response: {text:?}"
        );
        assert!(
            text.contains("Hello world"),
            "accumulated text must render in the block: {text:?}"
        );
    }

    /// P4 (done marker): once the `done` line is read, the header drops the
    /// in-progress ellipsis and shows the done marker.
    #[test]
    fn stream_block_marks_done_when_done_line_read() {
        let mut app = app_with_session(Vec::new());
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".model-stream");
        std::fs::write(
            &path,
            [
                serde_json::json!({"kind": "text", "delta": "hi"}),
                serde_json::json!({"kind": "done", "stop_reason": "stop"}),
            ]
            .iter()
            .map(|v| format!("{v}\n"))
            .collect::<String>(),
        )
        .unwrap();
        app.refresh_stream(&path);
        settle_pace(&mut app);

        let lines = stream_block_lines(&app, 80, 5);
        let text: String = lines.iter().map(|l| l.to_string()).collect();
        assert!(
            text.contains("done"),
            "the done marker should show once the done line is read: {text:?}"
        );
    }

    /// P4 (thinking): when no output text has arrived but reasoning has,
    /// the live block shows the in-progress thinking tail.
    #[test]
    fn stream_block_shows_thinking_when_no_text() {
        let mut app = app_with_session(Vec::new());
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".model-stream");
        std::fs::write(
            &path,
            serde_json::json!({"kind": "reasoning", "item_id": "r1", "delta": "let me think..."}).to_string() + "\n",
        )
        .unwrap();
        app.refresh_stream(&path);
        settle_pace(&mut app);

        let lines = stream_block_lines(&app, 80, 5);
        assert!(!lines.is_empty(), "the live block should show the thinking tail");
        let text: String = lines.iter().map(|l| l.to_string()).collect();
        assert!(
            text.contains("let me think"),
            "the in-progress reasoning should render: {text:?}"
        );
    }

    /// P4 (tool-call deltas): partial tool-call arguments render as one dim
    /// line per call, with the tool name shown as soon as it is known.
    #[test]
    fn stream_block_shows_partial_tool_calls() {
        let mut app = app_with_session(Vec::new());
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".model-stream");
        std::fs::write(
            &path,
            serde_json::json!({"kind": "tool_call_delta", "call_id": "c1", "name": "bash", "args_delta": "ls -la"}).to_string() + "\n",
        )
        .unwrap();
        app.refresh_stream(&path);
        settle_pace(&mut app);

        let lines = stream_block_lines(&app, 80, 5);
        let text: String = lines.iter().map(|l| l.to_string()).collect();
        assert!(
            text.contains("tool:bash"),
            "the partial tool call should render with its name: {text:?}"
        );
        assert!(
            text.contains("ls -la"),
            "the partial arguments should render: {text:?}"
        );
    }

    /// Growth: the block grows with the arriving content up to
    /// `max_body_lines` — past the old fixed 5-row cap. When the
    /// content exceeds the budget, the tail wins.
    #[test]
    fn stream_block_grows_with_content_within_budget() {
        let mut app = app_with_session(Vec::new());
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".model-stream");
        let text = (1..=12).map(|i| format!("L{i:02}")).collect::<Vec<_>>().join("\n");
        std::fs::write(
            &path,
            format!("{}\n", serde_json::json!({"kind": "text", "delta": text})),
        )
        .unwrap();
        app.refresh_stream(&path);
        settle_pace(&mut app);

        // A comfortable budget: all 12 content rows render (header +
        // 12) — the block grew past the old fixed cap.
        let lines = stream_block_lines(&app, 80, 30);
        let joined: String = lines.iter().map(|l| l.to_string()).collect();
        assert_eq!(lines.len(), 13, "header + 12 content rows: {joined:?}");
        assert!(joined.contains("L01"), "the head stays: {joined:?}");
        assert!(joined.contains("L12"), "the tail shows: {joined:?}");

        // A tight budget: exactly 3 content rows, the tail wins.
        let lines = stream_block_lines(&app, 80, 3);
        let joined: String = lines.iter().map(|l| l.to_string()).collect();
        assert_eq!(lines.len(), 4, "header + 3 content rows: {joined:?}");
        assert!(joined.contains("L10"), "the tail shows: {joined:?}");
        assert!(
            !joined.contains("L01"),
            "the head scrolled out: {joined:?}"
        );
    }

    /// Growth: while no output text has arrived, the full budget goes
    /// to the thinking tail.
    #[test]
    fn stream_block_thinking_grows_within_budget() {
        let mut app = app_with_session(Vec::new());
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".model-stream");
        let reasoning = (1..=20)
            .map(|i| format!("think L{i:02}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(
            &path,
            format!(
                "{}\n",
                serde_json::json!({"kind": "reasoning", "item_id": "r1", "delta": reasoning})
            ),
        )
        .unwrap();
        app.refresh_stream(&path);
        settle_pace(&mut app);

        // A budget that fits: header + label + all 20 thinking rows.
        let lines = stream_block_lines(&app, 80, 40);
        let joined: String = lines.iter().map(|l| l.to_string()).collect();
        assert_eq!(lines.len(), 22, "header + label + 20 rows: {joined:?}");
        assert!(
            joined.contains("think L01"),
            "the head of the reasoning stays: {joined:?}"
        );

        // A tight budget: label + the 3-row tail.
        let lines = stream_block_lines(&app, 80, 4);
        let joined: String = lines.iter().map(|l| l.to_string()).collect();
        assert_eq!(lines.len(), 5, "header + 4 content rows: {joined:?}");
        assert!(joined.contains("think L20"), "the tail shows: {joined:?}");
        assert!(
            !joined.contains("think L01"),
            "the head scrolled out: {joined:?}"
        );
    }

    /// Transition (flicker fix): when the response text starts, the
    /// thinking tail and the text share one sliding window instead of
    /// the thinking collapsing to a 2-line summary. The block height
    /// never shrinks at the transition — the thinking simply slides
    /// out as the text grows.
    #[test]
    fn stream_block_thinking_and_text_share_the_window() {
        let mut app = app_with_session(Vec::new());
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".model-stream");
        let reasoning = (1..=10)
            .map(|i| format!("think L{i:02}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(
            &path,
            [
                serde_json::json!({"kind": "reasoning", "item_id": "r1", "delta": reasoning}),
                serde_json::json!({"kind": "text", "delta": "alpha\nbeta\ngamma"}),
            ]
            .iter()
            .map(|v| format!("{v}\n"))
            .collect::<String>(),
        )
        .unwrap();
        app.refresh_stream(&path);
        settle_pace(&mut app);

        // Tight budget: the last 7 content rows (1 reserved for the
        // pinned label) of the combined 13 content rows — thinking tail
        // 4 + text 3.
        let lines = stream_block_lines(&app, 80, 8);
        let joined: String = lines.iter().map(|l| l.to_string()).collect();
        assert_eq!(lines.len(), 9, "header + label + 7 content rows: {joined:?}");
        assert!(
            joined.contains("think L07"),
            "the thinking tail survives the transition: {joined:?}"
        );
        assert!(
            !joined.contains("think L06"),
            "the oldest thinking rows slid out: {joined:?}"
        );
        assert!(joined.contains("gamma"), "the text tail shows: {joined:?}");

        // A budget that fits: header + label + all 13 content rows.
        let lines = stream_block_lines(&app, 80, 20);
        let joined: String = lines.iter().map(|l| l.to_string()).collect();
        assert_eq!(lines.len(), 15, "header + label + 13 content rows: {joined:?}");
        assert!(
            joined.contains("think L01"),
            "the head of the reasoning stays when it fits: {joined:?}"
        );
    }

    /// Anti-flicker property: the block height at the thinking → text
    /// transition never shrinks (the old 2-line compression caused a
    /// sudden multi-row collapse = the reported view flicker).
    #[test]
    fn stream_block_height_never_shrinks_when_text_starts() {
        let make_app = |reasoning: &str, text: &str| -> crate::app::App {
            let mut app = app_with_session(Vec::new());
            let tmp = tempfile::tempdir().unwrap();
            let path = tmp.path().join(".model-stream");
            let mut out = String::new();
            out.push_str(&format!(
                "{}\n",
                serde_json::json!({"kind": "reasoning", "item_id": "r1", "delta": reasoning})
            ));
            if !text.is_empty() {
                out.push_str(&format!(
                    "{}\n",
                    serde_json::json!({"kind": "text", "delta": text})
                ));
            }
            std::fs::write(&path, out).unwrap();
            app.refresh_stream(&path);
            settle_pace(&mut app);
            app
        };

        let reasoning = (1..=10)
            .map(|i| format!("think L{i:02}"))
            .collect::<Vec<_>>()
            .join("\n");

        // Thinking only (the frame before the transition).
        let app_think_only = make_app(&reasoning, "");
        let h_think_only = stream_block_lines(&app_think_only, 80, 12).len();

        // The first text row arrives.
        let app_first_text = make_app(&reasoning, "w01");
        let h_first_text = stream_block_lines(&app_first_text, 80, 12).len();

        // A long text stream (past the window cap).
        let text_long = (1..=30).map(|i| format!("w{i:02}")).collect::<Vec<_>>().join("\n");
        let app_long_text = make_app(&reasoning, &text_long);
        let h_long_text = stream_block_lines(&app_long_text, 80, 12).len();

        assert_eq!(
            h_think_only,
            12,
            "header + label + 10 thinking rows at budget 12"
        );
        assert!(
            h_first_text >= h_think_only,
            "the transition must not shrink the block ({h_first_text} < {h_think_only})"
        );
        assert!(
            h_long_text >= h_think_only,
            "a long text must not shrink the block ({h_long_text} < {h_think_only})"
        );
        assert_eq!(
            h_long_text,
            h_first_text,
            "once full, the window height stays fixed"
        );
    }

    /// A zero budget (a degenerate viewport) renders the header row
    /// only.
    #[test]
    fn stream_block_zero_budget_shows_header_only() {
        let mut app = app_with_session(Vec::new());
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".model-stream");
        std::fs::write(
            &path,
            [
                serde_json::json!({"kind": "text", "delta": "hi"}),
                serde_json::json!({"kind": "done", "stop_reason": "stop"}),
            ]
            .iter()
            .map(|v| format!("{v}\n"))
            .collect::<String>(),
        )
        .unwrap();
        app.refresh_stream(&path);
        settle_pace(&mut app);
        let lines = stream_block_lines(&app, 80, 0);
        assert_eq!(lines.len(), 1, "the header row only: {lines:?}");
    }

    /// Position (docs/tui-streaming-response.md section 6.3): the live
    /// block renders right after the existing messages, above the
    /// model status indicator (the working row).
    #[test]
    fn stream_block_sits_between_transcript_and_working_row() {
        let ts = fmt_ts(chrono::Utc::now() - chrono::Duration::seconds(2));
        let mut evs: Vec<Event> =
            (0..20).map(|i| produce::user_message(&format!("message {i}"))).collect();
        evs.push(phase_marker(&ts, "wait"));
        let mut app = app_with_session(evs);
        attach_running_loop(&mut app, "s1");
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".model-stream");
        std::fs::write(
            &path,
            format!(
                "{}\n",
                serde_json::json!({"kind": "text", "delta": "streaming reply"})
            ),
        )
        .unwrap();
        app.refresh_stream(&path);
        settle_pace(&mut app);

        let b = draw_frame(&mut app, 80, 30);
        let row_of = |needle: &str| -> u16 {
            let buf = b.buffer();
            let area = buf.area();
            (0..area.height)
                .find(|&y| {
                    let mut row = String::new();
                    for x in 0..area.width {
                        if let Some(cell) = buf.cell((x, y)) {
                            row.push_str(cell.symbol());
                        }
                    }
                    row.contains(needle)
                })
                .unwrap_or_else(|| panic!("no buffer row contains {needle:?}"))
        };
        let msg = row_of("message 19");
        let stream = row_of("assistant");
        let work = row_of("waiting for model");
        assert!(
            msg < stream,
            "the stream block sits below the existing messages (msg={msg} stream={stream})"
        );
        assert!(
            stream < work,
            "the stream block sits above the model status indicator (stream={stream} work={work})"
        );
    }

}

#[cfg(test)]
mod cursor_span_tests {
    use super::cursor_line_spans;
    use super::thinking_text;
    use super::wrap_thinking;
    use ratatui::style::Modifier;
    use ratatui::text::Span;
    use serde_json::json;

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

    #[test]
    fn thinking_text_accepts_typed_and_typeless_entries() {
        // The typed `reasoning_text` entries and the plain text
        // entries of older logs both carry the reasoning text.
        let typed: Vec<serde_json::Value> = vec![json!({
            "content": [
                {"type": "reasoning_text", "text": "typed"},
                {"type": "image", "text": "nope"}
            ]
        })];
        assert_eq!(thinking_text(Some(&typed)).as_deref(), Some("typed"));
        let typeless: Vec<serde_json::Value> = vec![json!({"content": [{"text": "plain"}]})];
        assert_eq!(thinking_text(Some(&typeless)).as_deref(), Some("plain"));
        // The summary wins over the content entries.
        let summary: Vec<serde_json::Value> = vec![json!({
            "summary": [{"text": "summed"}],
            "content": [{"text": "plain"}]
        })];
        assert_eq!(thinking_text(Some(&summary)).as_deref(), Some("summed"));
        // A text-less item renders nothing.
        let empty: Vec<serde_json::Value> = vec![json!({"content": [{"type": "image"}]})];
        assert_eq!(thinking_text(Some(&empty)), None);
    }

    #[test]
    fn thinking_block_tables_draw_as_a_fixed_width_grid() {
        // The 2026-09-03 user report: a table inside an expanded
        // thinking block kept its raw `|` pipes and lost the fixed
        // column widths. The grid replaces the run of table lines.
        let palette = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let style = palette.style(crate::color::Role::Thinking, Modifier::empty());
        let text = "preamble\n| Level | Border |\n|---|---|\n| 0 | gray |\n| 1 | blue |\nafter";
        let lines = wrap_thinking(text, 60, &palette, style);
        let text: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        let joined = text.join("\n");
        assert!(joined.contains('┌'), "the grid border: {joined:?}");
        assert!(joined.contains("Level"), "{joined:?}");
        assert!(joined.contains("gray"), "{joined:?}");
        // Every grid row has the same width: no clipped column.
        let widths: Vec<usize> = text
            .iter()
            .filter(|l| l.contains('│') || l.contains('─'))
            .map(|l| l.chars().count())
            .collect();
        let first = widths.first().copied().unwrap_or(0);
        assert!(
            !widths.is_empty() && widths.iter().all(|w| *w == first),
            "grid rows keep one width: {widths:?}"
        );
        // The prose lines survive around the grid.
        assert!(text.iter().any(|l| l.contains("preamble")), "{joined:?}");
        assert!(text.iter().any(|l| l.contains("after")), "{joined:?}");
        // The cell text uses the thinking tone, not plain text.
        let cell_line = lines
            .iter()
            .find(|l| l.to_string().contains("gray"))
            .unwrap();
        let spans: Vec<&Span<'static>> = cell_line.iter().collect();
        let cell = spans.iter().find(|s| s.content.contains("gray")).unwrap();
        assert_eq!(cell.style, style, "the cell keeps the thinking tone");
    }

}
