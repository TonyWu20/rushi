//! Tool result display: the port of `pi-tool-display`
//! (docs/tui-tool-display-port.md) plus the content layer of
//! docs/tui-tool-result-truncation.md.
//!
//! The request in one line: wrap every tool result in a lighter
//! box, fold long output to a preview, and one global key expands
//! every collapsed block. The content layer owns what shows and how
//! many lines: `Read` and `Write` results truncate to a preview,
//! `Edit` results render as a diff, and `bash` output folds to a
//! collapsed line count.
//!
//! Reference: `pi-tool-display` (github.com/MasuRii/pi-tool-display,
//! v0.5.0, pinned rev `91cef758`), the config shape and the presets
//! ported here:
//!
//! - Per-tool limits: `previewLines` 8 for read,
//!   `bashCollapsedLines` 10, `diffCollapsedLines` 24.
//! - Output modes: `hidden` / `summary` / `preview` for read and
//!   bash, `hidden` / `count` / `preview` for search.
//! - Presets: `opencode` (default), `balanced`, `verbose`.
//! - Fold: the preview shows the first lines only; a muted hint
//!   states the remainder and the expand key, like
//!   `... (173 more lines • Ctrl+O to expand)`.
//! - Expand: one global key (`Ctrl+O`) toggles every collapsed
//!   block to the full output; the expanded preview caps at
//!   `expandedPreviewMaxLines` (4000).
//!
//! This module is pure: the render call in render.rs feeds it the
//! event value, the config state, and the pane width. It returns
//! styled segments. No I/O, no decision logic.

use bon::builder;
use ratatui::style::{Color, Modifier, Style};

/// The tool-result presets of the port
/// (docs/tui-tool-display-port.md section 2). The `opencode` preset
/// is the default: read content shows a syntax-highlighted preview,
/// search stays hidden, bash collapses to the first 10 lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    /// The default preset: read shows a syntax-highlighted
    /// preview, search hidden, bash collapsed to the first 10
    /// lines.
    OpenCode,
    /// Compact summaries: read line count, search match total, bash
    /// line count.
    Balanced,
    /// Larger previews: read and search show 12 preview lines, bash
    /// 20.
    Verbose,
}

/// Parse one preset name from the config. Case-insensitive.
pub fn parse_preset(s: &str) -> Option<Preset> {
    Some(match s.trim().to_ascii_lowercase().as_str() {
        "opencode" => Preset::OpenCode,
        "balanced" => Preset::Balanced,
        "verbose" => Preset::Verbose,
        _ => return None,
    })
}

/// The output mode of one tool's result body. `Hidden` shows no body
/// lines; `Summary` one muted count line; `Preview` the first lines
/// of the body plus the fold hint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputMode {
    Hidden,
    Summary,
    Preview,
}

pub fn parse_output_mode(s: &str) -> Option<OutputMode> {
    Some(match s.trim().to_ascii_lowercase().as_str() {
        "hidden" => OutputMode::Hidden,
        "summary" => OutputMode::Summary,
        "preview" => OutputMode::Preview,
        _ => return None,
    })
}

/// The search output mode of the port. `Count` shows the match
/// total instead of a line summary; `Hidden` and `Preview` behave as
/// in [`OutputMode`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchMode {
    Hidden,
    Count,
    Preview,
}

pub fn parse_search_mode(s: &str) -> Option<SearchMode> {
    Some(match s.trim().to_ascii_lowercase().as_str() {
        "hidden" => SearchMode::Hidden,
        "count" => SearchMode::Count,
        "preview" => SearchMode::Preview,
        _ => return None,
    })
}

/// The diff layout of `Edit` and `Write` results
/// (docs/tui-tool-result-truncation.md: the adaptive diff).
/// `Auto` picks split at wide panes and unified at narrow ones;
/// `Split` and `Unified` force the layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffView {
    Auto,
    Split,
    Unified,
}

pub fn parse_diff_view(s: &str) -> Option<DiffView> {
    Some(match s.trim().to_ascii_lowercase().as_str() {
        "auto" => DiffView::Auto,
        "split" => DiffView::Split,
        "unified" => DiffView::Unified,
        _ => return None,
    })
}

/// The tool-result display config of one TUI run
/// (docs/tui-tool-display-port.md section 2: the config part). The
/// preset value plus per-field overrides; the overrides win.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolDisplay {
    pub read_mode: OutputMode,
    pub search_mode: SearchMode,
    pub bash_mode: OutputMode,
    /// The preview line count of read results (`previewLines`, 8).
    pub preview_lines: usize,
    /// The collapsed line count of bash output
    /// (`bashCollapsedLines`, 10).
    pub bash_collapsed_lines: usize,
    /// The collapsed line count of edit/write diffs
    /// (`diffCollapsedLines`, 24).
    pub diff_collapsed_lines: usize,
    /// The expanded preview cap (`expandedPreviewMaxLines`, 4000).
    pub expanded_preview_max_lines: usize,
    /// The diff layout mode (`auto`, `split`, `unified`).
    pub diff_view: DiffView,
}

impl ToolDisplay {
    /// The value table of a preset (docs/tui-tool-display-port.md
    /// section 2: the presets). `Custom` is the `opencode` value
    /// table: a user override of a preset field is a `Custom`
    /// preset, not the default.
    pub fn preset(p: Preset) -> Self {
        match p {
            Preset::OpenCode => Self {
                read_mode: OutputMode::Preview,
                search_mode: SearchMode::Hidden,
                bash_mode: OutputMode::Preview,
                preview_lines: 8,
                bash_collapsed_lines: 10,
                diff_collapsed_lines: 24,
                expanded_preview_max_lines: 4000,
                diff_view: DiffView::Auto,
            },
            Preset::Balanced => Self {
                read_mode: OutputMode::Summary,
                search_mode: SearchMode::Count,
                bash_mode: OutputMode::Summary,
                preview_lines: 8,
                bash_collapsed_lines: 10,
                diff_collapsed_lines: 24,
                expanded_preview_max_lines: 4000,
                diff_view: DiffView::Auto,
            },
            Preset::Verbose => Self {
                read_mode: OutputMode::Preview,
                search_mode: SearchMode::Preview,
                bash_mode: OutputMode::Preview,
                preview_lines: 12,
                bash_collapsed_lines: 20,
                diff_collapsed_lines: 24,
                expanded_preview_max_lines: 4000,
                diff_view: DiffView::Auto,
            },
        }
    }

    /// The split threshold of the `auto` diff layout: split when the
    /// pane is at least this wide, unified below it (the reference
    /// `diffSplitMinWidth` 120, in half the two-pane width).
    pub const DIFF_SPLIT_MIN_WIDTH: usize = 60;

    /// The layout of the `auto` mode at `width` columns: split when
    /// the pane fits two panes of the diff plus the divider.
    pub fn diff_layout(&self, width: usize) -> DiffView {
        match self.diff_view {
            DiffView::Auto => {
                if width >= Self::DIFF_SPLIT_MIN_WIDTH * 2 {
                    DiffView::Split
                } else {
                    DiffView::Unified
                }
            }
            other => other,
        }
    }
}

// ── the fold/expand render ──────────────────────────────────────

/// One visual row of the tool-result body: one or more styled
/// segments (the split diff row carries the left pane, the divider,
/// and the right pane). One row is one terminal line of the box;
/// the box border lines around it are one row each.
pub type BodyRow = Vec<(Style, String)>;

/// The body rows of one tool result at the display state, as styled
/// segments for the word-wraper. `expanded` is the global fold/
/// expand toggle (the `Ctrl+O` key): collapsed shows the mode's
/// lines, expanded shows the body up to `expanded_preview_max_lines`.
///
/// The body is the tool-specific compact output
/// (docs/tui-tool-result-truncation.md section 1: the content
/// layer). The fold state owns how many lines of it show.
#[builder]
pub fn body_rows(
    tool: &str,
    value: &serde_json::Value,
    call_args: Option<&serde_json::Value>,
    err: bool,
    cfg: &ToolDisplay,
    palette: &crate::color::Palette,
    expanded: bool,
    width: usize,
) -> Vec<BodyRow> {
    let out = Style::default().fg(palette.color(crate::color::Role::ToolOutput));
    let hint = Style::default()
        .fg(palette.color(crate::color::Role::Hint))
        .add_modifier(Modifier::DIM);
    let diff_added = Style::default().fg(palette.color(crate::color::Role::DiffAdded));
    let diff_removed = Style::default().fg(palette.color(crate::color::Role::DiffRemoved));
    let error = Style::default().fg(palette.color(crate::color::Role::Error));
    let command = Style::default().fg(palette.color(crate::color::Role::ToolCommand));
    let code = Style::default().fg(palette.color(crate::color::Role::Code));
    match tool {
        "read" => {
            let builder = read_body()
                .value(value)
                .cfg(cfg)
                .hint(&hint)
                .code(&code)
                .palette(palette)
                .expanded(expanded)
                .width(width);
            if let Some(ca) = call_args {
                builder.call_args(ca).call()
            } else {
                builder.call()
            }
        }
        "write" => {
            let builder = write_body()
                .value(value)
                .cfg(cfg)
                .out(&out)
                .hint(&hint)
                .diff_added(&diff_added)
                .expanded(expanded)
                .width(width);
            if let Some(ca) = call_args {
                builder.call_args(ca).call()
            } else {
                builder.call()
            }
        }
        "edit" => edit_body()
            .value(value)
            .cfg(cfg)
            .out(&out)
            .hint(&hint)
            .diff_added(&diff_added)
            .diff_removed(&diff_removed)
            .expanded(expanded)
            .width(width)
            .call(),
        "bash" => bash_body()
            .value(value)
            .cfg(cfg)
            .palette(palette)
            .out(&out)
            .hint(&hint)
            .error(&error)
            .command(&command)
            .err(err)
            .expanded(expanded)
            .width(width)
            .call(),
        "list" => search_body(value, cfg, &out, &hint, expanded, width),
        _ => generic_body(value, cfg, &out, &hint, expanded, width),
    }
}

/// The body content plan of one output mode at the fold state.
/// The global expand (the `Ctrl+O` key) overrides every mode
/// (the port spec of the module header): a hidden or summary block
/// shows its body lines, capped by the expanded cap. A summary
/// line is the one muted count row; no body keeps the mode's hint
/// row only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BodyPlan {
    /// No body lines: the mode's hint row only.
    NoBody,
    /// One muted count row.
    SummaryLine,
    /// The body lines, capped by the fold state.
    Lines,
}

fn body_plan(mode: OutputMode, expanded: bool) -> BodyPlan {
    if expanded {
        return BodyPlan::Lines;
    }
    match mode {
        OutputMode::Hidden => BodyPlan::NoBody,
        OutputMode::Summary => BodyPlan::SummaryLine,
        OutputMode::Preview => BodyPlan::Lines,
    }
}

/// The body plan of one search mode at the fold state: the search
/// `count` mode maps to the summary line, `hidden` to no body,
/// `preview` to the lines. The global expand overrides, like
/// [`body_plan`].
fn search_body_plan(mode: SearchMode, expanded: bool) -> BodyPlan {
    if expanded {
        return BodyPlan::Lines;
    }
    match mode {
        SearchMode::Hidden => BodyPlan::NoBody,
        SearchMode::Count => BodyPlan::SummaryLine,
        SearchMode::Preview => BodyPlan::Lines,
    }
}
/// The folded-preview hint row: `... (N more lines • Ctrl+O to expand)`.
/// `N` is the hidden line count. When the global expand is on, the
/// key hint drops (the block is already expanded). The hint
/// shortens on narrow panes: the key hint drops first, then the
/// count sentence to the ellipsis run.
fn fold_hint(
    remaining: usize,
    expanded: bool,
    hint: &Style,
    width: usize,
) -> Option<(Style, String)> {
    if remaining == 0 {
        return None;
    }
    let unit = if remaining == 1 { "line" } else { "lines" };
    let full = format!("... ({remaining} more {unit} • Ctrl+O to expand)");
    let short = format!("... ({remaining} more {unit})");
    let text = if expanded || full.chars().count() > width {
        if short.chars().count() <= width {
            short
        } else {
            // The pane is too narrow for the count itself: show the
            // ellipsis run only, never a clipped sentence.
            "...".to_string()
        }
    } else {
        full
    };
    Some((*hint, text))
}

/// The body rows of a `Read` result (the truncation request of
/// docs/tui-tool-result-truncation.md section 1). The log value
/// carries `{text}` (the fuller tool shape adds `path` and
/// `total_lines`); the body is the compact read output: the line
/// count, then the output-mode lines of the text. Collapsed, the
/// preview shows `preview_lines` (8) lines of the text; expanded,
/// up to the expanded cap.
/// The file path for language detection resolves `value.path`
/// first, then the call argument `file_path` (the log stores the
/// path in the call arguments, not the result value).
/// The read tool prefixes each content line with its 1-based file
/// line number (`"12: "`). Split that prefix off so the syntax
/// highlighter only sees file content. Lines without a digit-colon
/// prefix (annotation lines like `(5 lines omitted)` or
/// `(End of file ...)`) come back unsplit with a `None` number.
fn split_line_number(line: &str) -> (Option<&str>, &str) {
    match line.find(':') {
        Some(pos) if pos > 0 && line[..pos].bytes().all(|b| b.is_ascii_digit()) => {
            let rest = line[pos + 1..].strip_prefix(' ').unwrap_or(&line[pos + 1..]);
            (Some(&line[..pos]), rest)
        }
        _ => (None, line),
    }
}

#[builder]
fn read_body(
    value: &serde_json::Value,
    call_args: Option<&serde_json::Value>,
    cfg: &ToolDisplay,
    hint: &Style,
    code: &Style,
    palette: &crate::color::Palette,
    expanded: bool,
    width: usize,
) -> Vec<BodyRow> {
    let text = value.get("text").and_then(|t| t.as_str()).unwrap_or("");
    let total = value.get("total_lines").and_then(|t| t.as_u64());
    let lines: Vec<&str> = text.lines().collect();
    let count = total.map(|n| n as usize).unwrap_or(lines.len());
    let mut rows: Vec<BodyRow> = Vec::new();
    match body_plan(cfg.read_mode, expanded) {
        BodyPlan::NoBody => {
            rows.push(vec![(*hint, format!("↳ {count} lines hidden"))]);
        }
        BodyPlan::SummaryLine => {
            let n = if count == 1 { "1 line" } else { "lines" };
            rows.push(vec![(*hint, format!("↳ {count} {n}"))]);
        }
        BodyPlan::Lines => {
            let cap = if expanded {
                cfg.expanded_preview_max_lines
            } else {
                cfg.preview_lines
            };
            let remaining = lines.len().saturating_sub(cap);
            // The shared syntax-highlight entry point: the language is
            // detected from the read path; unknown types stay plain.
            // This is the same engine the picker preview pane uses
            // (docs/tui-file-picker.md section 9). The log stores the
            // path in the call arguments (`file_path`), not the result
            // value, so resolve value first, then the call arguments.
            let path = value
                .get("path")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| {
                    call_args
                        .and_then(|a| a.get("file_path").or_else(|| a.get("path")))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or_default();
            let lang = crate::highlight::language_from_path(&path);
            let mut hl = crate::highlight::CodeHighlighter::new();
            for raw in lines.iter().take(cap) {
                let (num, content) = split_line_number(raw);
                let mut row: BodyRow = Vec::new();
                if let Some(n) = num {
                    row.push((*code, format!("{n}: ")));
                }
                if num.is_some() {
                    for (st, s) in hl.line(content, lang, palette) {
                        // Plain runs take the tool code tone; syntax
                        // segments keep the palette syntax colors.
                        let st = if st == Style::default() { *code } else { st };
                        row.push((st, s));
                    }
                } else {
                    // No line-number prefix: keep the legacy plain
                    // rendering for annotation lines.
                    row.push((*code, raw.to_string()));
                }
                rows.push(row);
            }
            if let Some(h) = fold_hint(remaining, expanded, hint, width) {
                rows.push(vec![h]);
            }
        }
    }
    rows
}

/// The body of a `Write` result: the adaptive write diff of
/// docs/tui-tool-result-truncation.md. The result value carries the
/// summary (`{text, path, operation, bytes}`); the written content
/// is the call argument `content` (the TUI holds the call
/// arguments through `call_args`). The diff shows the new content
/// as added lines (the pi `toolDiffAdded` color), collapsed to
/// `diff_collapsed_lines`. The summary row names the size, like the
/// reference write summary.
#[builder]
fn write_body(
    value: &serde_json::Value,
    call_args: Option<&serde_json::Value>,
    cfg: &ToolDisplay,
    out: &Style,
    hint: &Style,
    diff_added: &Style,
    expanded: bool,
    width: usize,
) -> Vec<BodyRow> {
    let path = value.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let operation = value
        .get("operation")
        .and_then(|v| v.as_str())
        .unwrap_or("create");
    let bytes = value.get("bytes").and_then(|v| v.as_u64()).unwrap_or(0);
    let content = call_args
        .and_then(|a| a.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");
    let content_lines: Vec<&str> = content.lines().collect();
    let mut rows: Vec<BodyRow> = Vec::new();
    // The write summary: the line count and the byte size, inline,
    // like the reference write summary (the adaptive write diff).
    rows.push(vec![(
        *hint,
        format!(
            "↳ {} {} lines, {} bytes",
            match operation {
                "update" => "overwrite",
                _ => "create",
            },
            content_lines.len(),
            bytes
        ),
    )]);
    if !path.is_empty() {
        rows.push(vec![(*out, path.to_string())]);
    }
    if !content_lines.is_empty() {
        let cap = if expanded {
            cfg.expanded_preview_max_lines
        } else {
            cfg.diff_collapsed_lines
        };
        let remaining = content_lines.len().saturating_sub(cap);
        for l in content_lines.iter().take(cap) {
            rows.push(vec![(*diff_added, format!("+ {l}"))]);
        }
        if let Some(h) = fold_hint(remaining, expanded, hint, width) {
            rows.push(vec![h]);
        }
    }
    rows
}

/// The body of an `Edit` result: the adaptive edit diff of
/// docs/tui-tool-result-truncation.md. The value carries `before`
/// (the old string) and `after` (the new string). The unified
/// layout colors the removed lines in the pi `toolDiffRemoved`
/// role and the added lines in the pi `toolDiffAdded` role; the
/// split layout shows the two sides in one wide row. Collapsed to
/// `diff_collapsed_lines`.
#[builder]
fn edit_body(
    value: &serde_json::Value,
    cfg: &ToolDisplay,
    out: &Style,
    hint: &Style,
    diff_added: &Style,
    diff_removed: &Style,
    expanded: bool,
    width: usize,
) -> Vec<BodyRow> {
    let path = value.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let before: Vec<&str> = value
        .get("before")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .lines()
        .collect();
    let after: Vec<&str> = value
        .get("after")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .lines()
        .collect();
    let replace_all = value
        .get("replace_all")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mut rows: Vec<BodyRow> = Vec::new();
    // The diff stats row, like the reference diff presentation:
    // `↳ diff +A -R` (the added and removed line counts).
    rows.push(vec![(
        *hint,
        format!("↳ diff +{} -{}", after.len(), before.len()),
    )]);
    if !path.is_empty() {
        rows.push(vec![(*out, path.to_string())]);
    }
    if replace_all {
        rows.push(vec![(*hint, "replace_all".to_string())]);
    }
    let cap = if expanded {
        cfg.expanded_preview_max_lines
    } else {
        cfg.diff_collapsed_lines
    };
    let total = before.len() + after.len();
    let layout = cfg.diff_layout(width);
    match layout {
        DiffView::Split => {
            // The row is `│` + ` left ` + ` │` + ` right ` + `│`. The
            // three cell groups must fit the inner width, so each
            // pane owns (width - 6) / 2: two panes, two leading
            // spaces, and the divider column.
            let pane = width.saturating_sub(6) / 2;
            let mut i = 0usize;
            let mut shown = 0usize;
            while (i < before.len() || i < after.len()) && shown < cap {
                let b = before.get(i).copied().unwrap_or("");
                let a = after.get(i).copied().unwrap_or("");
                let b_txt = if b.is_empty() {
                    String::new()
                } else {
                    format!("- {b}")
                };
                let a_txt = if a.is_empty() {
                    String::new()
                } else {
                    format!("+ {a}")
                };
                let b_style = if b.is_empty() {
                    *out
                } else {
                    *diff_removed
                };
                let a_style = if a.is_empty() {
                    *out
                } else {
                    *diff_added
                };
                // One split row: the left pane, the divider column,
                // the right pane. Each pane clamps to its half of
                // the width (the narrow-pane width clamp of the port).
                rows.push(vec![
                    (b_style, clamp_col(b_txt, pane)),
                    (*out, "│".to_string()),
                    (a_style, clamp_col(a_txt, pane)),
                ]);
                shown += 1;
                i += 1;
            }
            let remaining = total.saturating_sub(shown * 2);
            if let Some(h) = fold_hint(remaining / 2, expanded, hint, width) {
                rows.push(vec![h]);
            }
        }
        DiffView::Auto | DiffView::Unified => {
            let mut shown = 0usize;
            for l in before.iter() {
                if shown >= cap {
                    break;
                }
                rows.push(vec![(*diff_removed, format!("- {l}"))]);
                shown += 1;
            }
            for l in after.iter() {
                if shown >= cap {
                    break;
                }
                rows.push(vec![(*diff_added, format!("+ {l}"))]);
                shown += 1;
            }
            let remaining = total.saturating_sub(shown);
            if let Some(h) = fold_hint(remaining, expanded, hint, width) {
                rows.push(vec![h]);
            }
        }
    }
    rows
}

/// The body of a `bash` result: the collapsed bash output of the
/// port. The value is `{text, exit_code, stdout, stderr, ...}`; the
/// body is the model-facing `text` (the command line, the output,
/// the exit line), collapsed to `bash_collapsed_lines` (10).
/// `Hidden` shows no body; `Summary` one line-count line;
/// `Preview` the first lines.
#[builder]
fn bash_body(
    value: &serde_json::Value,
    cfg: &ToolDisplay,
    palette: &crate::color::Palette,
    out: &Style,
    hint: &Style,
    error: &Style,
    command: &Style,
    err: bool,
    expanded: bool,
    width: usize,
) -> Vec<BodyRow> {
    let text = value.get("text").and_then(|t| t.as_str()).unwrap_or("");
    let lines: Vec<&str> = text.lines().collect();
    let so = value.get("stdout").and_then(|s| s.as_str()).unwrap_or("");
    let se = value.get("stderr").and_then(|s| s.as_str()).unwrap_or("");
    let out_count = so.lines().count() + se.lines().count();
    let mut rows: Vec<BodyRow> = Vec::new();
    match body_plan(cfg.bash_mode, expanded) {
        BodyPlan::NoBody => {
            rows.push(vec![(*hint, "↳ output hidden".to_string())]);
        }
        BodyPlan::SummaryLine => {
            let n = if out_count == 1 { "1 line" } else { "lines" };
            rows.push(vec![(*hint, format!("↳ {out_count} {n} returned"))]);
            if err {
                rows.push(vec![(*error, "↳ command failed".to_string())]);
            }
        }
        BodyPlan::Lines => {
            let cap = if expanded {
                cfg.expanded_preview_max_lines
            } else {
                cfg.bash_collapsed_lines
            };
            let remaining = lines.len().saturating_sub(cap);
            // The line styles: the `$ <command>` opener in the pi
            // `toolTitle` tone (the `command` style), the output
            // lines in the pi `toolOutput` tone. A failed command
            // paints every line in the error accent.
            for (i, l) in lines.iter().enumerate().take(cap) {
                let st = if err {
                    *error
                } else if i == 0 {
                    *command
                } else {
                    *out
                };
                if i == 0 {
                    // The command line: the bash result text opens
                    // with the `$ <command>` line. On a narrow pane
                    // it wraps to the pane width, not a truncated
                    // line (the user request of the 2026-09-04
                    // pass). The wrapped pieces keep the line style.
                    for piece in wrap_hard_line(l, width) {
                        rows.push(vec![(st, piece)]);
                    }
                } else {
                    rows.push(vec![(st, l.to_string())]);
                }
            }
            if let Some(h) = fold_hint(remaining, expanded, hint, width) {
                rows.push(vec![h]);
            }
        }
    }
    let _ = out;
    let _ = palette;
    rows
}

/// The body of a `list` (search) result. The value is
/// `{text, path, type: "directory", count}`. The modes: `hidden`
/// shows no body, `count` the entry total, `preview` the first
/// lines of the listing.
fn search_body(
    value: &serde_json::Value,
    cfg: &ToolDisplay,
    out: &Style,
    hint: &Style,
    expanded: bool,
    width: usize,
) -> Vec<BodyRow> {
    let text = value.get("text").and_then(|t| t.as_str()).unwrap_or("");
    let count = value
        .get("count")
        .and_then(|c| c.as_u64())
        .map(|n| n as usize)
        .unwrap_or_else(|| text.lines().count());
    let lines: Vec<&str> = text.lines().collect();
    let mut rows: Vec<BodyRow> = Vec::new();
    match search_body_plan(cfg.search_mode, expanded) {
        BodyPlan::NoBody => {
            rows.push(vec![(*hint, format!("↳ {count} entries hidden"))]);
        }
        BodyPlan::SummaryLine => {
            let n = if count == 1 { "1 entry" } else { "entries" };
            rows.push(vec![(*hint, format!("↳ {count} {n}"))]);
        }
        BodyPlan::Lines => {
            let cap = if expanded {
                cfg.expanded_preview_max_lines
            } else {
                cfg.preview_lines
            };
            let remaining = lines.len().saturating_sub(cap);
            for l in lines.iter().take(cap) {
                // The listing lines are plain text, the pi
                // `toolOutput` tone, not code.
                rows.push(vec![(*out, l.to_string())]);
            }
            if let Some(h) = fold_hint(remaining, expanded, hint, width) {
                rows.push(vec![h]);
            }
        }
    }
    rows
}

/// The body of a tool outside the known set: the compact generic
/// output. Collapsed shows the first lines (the `preview` cap of
/// the reference `generic` rendering); expanded, the body up to the
/// expanded cap. The body is the result text (the tool's `text`
/// field, the reference body precedence of docs/tui.md 13.1), the
/// pi `toolOutput` tone.
fn generic_body(
    value: &serde_json::Value,
    cfg: &ToolDisplay,
    out: &Style,
    hint: &Style,
    expanded: bool,
    width: usize,
) -> Vec<BodyRow> {
    let text = value.get("text").and_then(|t| t.as_str()).unwrap_or("");
    let lines: Vec<&str> = text.lines().collect();
    let mut rows: Vec<BodyRow> = Vec::new();
    let cap = if expanded {
        cfg.expanded_preview_max_lines
    } else {
        cfg.preview_lines
    };
    let remaining = lines.len().saturating_sub(cap);
    for l in lines.iter().take(cap) {
        rows.push(vec![(*out, l.to_string())]);
    }
    if let Some(h) = fold_hint(remaining, expanded, hint, width) {
        rows.push(vec![h]);
    }
    rows
}

/// The JSON-document body (docs/tui-color-tones.md): the result
/// text is a complete JSON document. The body lines keep the JSON
/// token colors (`highlight::json_line_p`), and the same fold caps
/// as the tool's output mode: the read preview cap for `read`, the
/// generic preview cap otherwise. The fold hint states the
/// remainder.
pub fn json_body_rows(
    tool: &str,
    value: &serde_json::Value,
    cfg: &ToolDisplay,
    palette: &crate::color::Palette,
    expanded: bool,
    width: usize,
) -> Vec<BodyRow> {
    let text = value.get("text").and_then(|t| t.as_str()).unwrap_or("");
    let lines: Vec<&str> = text.lines().collect();
    let hint = Style::default()
        .fg(palette.color(crate::color::Role::Hint))
        .add_modifier(Modifier::DIM);
    // The cap: the read preview cap for read results, the generic
    // preview cap otherwise; the expanded state lifts both to the
    // expanded cap. A hidden or summary mode shows no body lines.
    let cap = if expanded {
        cfg.expanded_preview_max_lines
    } else if tool == "read" {
        match cfg.read_mode {
            OutputMode::Preview => cfg.preview_lines,
            _ => 0,
        }
    } else {
        cfg.preview_lines
    };
    let mut rows: Vec<BodyRow> = Vec::new();
    let remaining = lines.len().saturating_sub(cap);
    for l in lines.iter().take(cap) {
        rows.push(crate::highlight::json_line_p(l, palette));
    }
    if let Some(h) = fold_hint(remaining, expanded, &hint, width) {
        rows.push(vec![h]);
    }
    rows
}

/// Clamp one split-diff column to `pane` columns: the overflow
/// drops with a trailing ellipsis. An empty column stays empty.
fn clamp_col(text: String, pane: usize) -> String {
    if pane == 0 {
        return String::new();
    }
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= pane {
        return text;
    }
    let mut out: String = chars[..pane.saturating_sub(1)].iter().collect();
    out.push('…');
    out
}

/// Word-wrap one hard line to `width` columns. The wrap breaks on
/// spaces. A word longer than the width splits at the width. A line
/// that already fits returns as one piece. The command line of a
/// bash result wraps on a narrow pane instead of truncating
/// (the user request of the 2026-09-04 pass).
fn wrap_hard_line(line: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![line.to_string()];
    }
    if line.chars().count() <= width {
        return vec![line.to_string()];
    }
    let mut out: Vec<String> = Vec::new();
    let mut cur: String = String::new();
    let flush = |cur: &mut String, out: &mut Vec<String>| {
        if !cur.is_empty() {
            out.push(std::mem::take(cur));
        }
    };
    for word in line.split(' ') {
        let wlen = word.chars().count();
        if wlen > width {
            // A word wider than the pane: hard-split it, and move
            // to a fresh piece per split.
            if !cur.is_empty() {
                flush(&mut cur, &mut out);
            }
            let chars: Vec<char> = word.chars().collect();
            let mut i = 0usize;
            while i < chars.len() {
                let end = (i + width).min(chars.len());
                out.push(chars[i..end].iter().collect());
                i = end;
            }
            continue;
        }
        let need = if cur.is_empty() { wlen } else { cur.chars().count() + 1 + wlen };
        if need > width {
            flush(&mut cur, &mut out);
        }
        if cur.is_empty() {
            cur.push_str(word);
        } else {
            cur.push(' ');
            cur.push_str(word);
        }
    }
    flush(&mut cur, &mut out);
    out
}

// ── the box (docs/tui-tool-display-port.md section 2: the box) ──

/// The background of a tool-result box, per the pi box-state role
/// (docs/tui-color-pi-alignment.md): the `toolBoxBgSuccess` /
/// `toolBoxBgError` palette role, lowered to the active capability
/// level. One cell of padding, the rounded corners.
pub fn box_bg(palette: &crate::color::Palette, err: bool) -> Color {
    if err {
        palette.color(crate::color::Role::ToolBoxBgError)
    } else {
        palette.color(crate::color::Role::ToolBoxBgSuccess)
    }
}

/// The rows of the rounded tool-result box: the top border with the
/// title text, the body rows (one cell of left padding, the border
/// cells of the row), and the bottom border. Every segment carries
/// the box-state background (`err` picks the pi `toolErrorBg` role,
/// otherwise the `toolSuccessBg` role) so the box reads as one
/// lighter panel on the transcript. `width` is the box width in
/// columns.
///
/// `title_style` restyles the title run of the top border (the
/// result status accent, like the red bold of an error status);
/// `None` keeps the border style.
///
/// One box row is one terminal line: the top border row is three
/// segments (the left border cell, the title, the close run); a
/// body row is one or three segments (the left border cell, the
/// body segments, the right border cell), or a pad when the body
/// row is empty.
pub fn box_rows(
    title: &str,
    body: &[BodyRow],
    width: usize,
    palette: &crate::color::Palette,
    title_style: Option<&Style>,
    err: bool,
) -> Vec<BodyRow> {
    let bg = box_bg(palette, err);
    let border = Style::default()
        .fg(palette.color(crate::color::Role::Hint))
        .bg(bg);
    let inner_w = width.saturating_sub(2).max(1);
    // The title row: `┌` + one-space padding + the title + the
    // `─` run to the close. The title clamps to the inner width
    // and keeps its own style (the error accent) when given.
    let title_chars: Vec<char> = title.chars().take(inner_w.saturating_sub(1)).collect();
    let title_text: String = title_chars.iter().collect();
    let pad_run = inner_w.saturating_sub(title_text.chars().count() + 1);
    let title_style = title_style
        .cloned()
        .map(|s| s.bg(bg))
        .unwrap_or(border);
    let top: Vec<(Style, String)> = vec![
        (border, "┌ ".to_string()),
        (title_style, title_text.clone()),
        (
            border,
            std::iter::repeat_n('─', pad_run)
                .chain(std::iter::once('┐'))
                .collect(),
        ),
    ];
    let mut out: Vec<BodyRow> = Vec::new();
    out.push(top);
    // The body rows: `│` + one-space padding + the row, padded to
    // the inner width. A body row may hold several segments (the
    // split diff panes). Each segment keeps its own clamped width:
    // the segments share the inner width left to right, and the
    // last segment pads to the right border. A segment that still
    // overflows is truncated with a trailing ellipsis, never pushed
    // past the border, and never wrapped to the next line (the
    // 2026-09-03 user directive).
    for row in body {
        let mut cells: Vec<(Style, String)> = Vec::new();
        cells.push((border, "│".to_string()));
        let mut used = 0usize;
        let n = row.len();
        for (idx, (st, text)) in row.iter().enumerate() {
            let st = (*st).bg(bg);
            let is_last = idx + 1 == n;
            // One leading space plus the segment text, clamped to
            // the inner columns still free.
            let avail = inner_w.saturating_sub(used);
            let mut cell = format!(" {text}");
            let want = cell.chars().count();
            if want > avail {
                if avail == 0 {
                    cell = String::new();
                } else if avail == 1 {
                    cell = "…".to_string();
                } else {
                    // Keep `avail - 1` columns and mark the cut
                    // with the ellipsis (one column).
                    let cut: String = cell.chars().take(avail - 1).collect();
                    cell = format!("{cut}…");
                }
            }
            used = used.saturating_add(cell.chars().count());
            if is_last {
                // The last segment owns the padding to the right
                // border: the box rows stay one terminal line.
                cell.push_str(&" ".repeat(inner_w.saturating_sub(used)));
            }
            cells.push((st, cell));
        }
        // An empty body row still fills the panel: pad to the
        // inner width so the borders stay one cell apart.
        if row.is_empty() {
            cells.push((Style::default().bg(bg), " ".repeat(inner_w)));
        }
        cells.push((border, "│".to_string()));
        out.push(cells);
    }
    let bottom: String = std::iter::once('└')
        .chain(std::iter::repeat_n('─', inner_w))
        .chain(std::iter::once('┘'))
        .collect();
    out.push(vec![(border, bottom)]);
    out
}
#[cfg(test)]
mod tests {
    use super::*;

    /// The joined text of one body row: the segments in order.
    fn row_text(row: &BodyRow) -> String {
        row.iter()
            .map(|(_, t)| t.as_str())
            .collect::<Vec<_>>()
            .join("")
    }

    /// The joined text of every body row.
    fn rows_text(rows: &[BodyRow]) -> Vec<String> {
        rows.iter().map(row_text).collect()
    }

    #[test]
    fn preset_value_tables() {
        let o = ToolDisplay::preset(Preset::OpenCode);
        assert_eq!(o.read_mode, OutputMode::Preview);
        assert_eq!(o.search_mode, SearchMode::Hidden);
        assert_eq!(o.bash_mode, OutputMode::Preview);
        assert_eq!(o.preview_lines, 8);
        assert_eq!(o.bash_collapsed_lines, 10);
        assert_eq!(o.diff_collapsed_lines, 24);
        assert_eq!(o.expanded_preview_max_lines, 4000);

        let b = ToolDisplay::preset(Preset::Balanced);
        assert_eq!(b.read_mode, OutputMode::Summary);
        assert_eq!(b.search_mode, SearchMode::Count);
        assert_eq!(b.bash_mode, OutputMode::Summary);

        let v = ToolDisplay::preset(Preset::Verbose);
        assert_eq!(v.read_mode, OutputMode::Preview);
        assert_eq!(v.search_mode, SearchMode::Preview);
        assert_eq!(v.bash_mode, OutputMode::Preview);
        assert_eq!(v.preview_lines, 12);
        assert_eq!(v.bash_collapsed_lines, 20);
    }

    #[test]
    fn preset_and_mode_parsing() {
        assert_eq!(parse_preset("opencode"), Some(Preset::OpenCode));
        assert_eq!(parse_preset("BALANCED"), Some(Preset::Balanced));
        assert_eq!(parse_preset("verbose"), Some(Preset::Verbose));
        assert_eq!(parse_preset("weird"), None);
        assert_eq!(parse_output_mode("hidden"), Some(OutputMode::Hidden));
        assert_eq!(parse_output_mode("summary"), Some(OutputMode::Summary));
        assert_eq!(parse_output_mode("preview"), Some(OutputMode::Preview));
        assert_eq!(parse_output_mode("nope"), None);
        assert_eq!(parse_search_mode("count"), Some(SearchMode::Count));
        assert_eq!(parse_diff_view("auto"), Some(DiffView::Auto));
        assert_eq!(parse_diff_view("split"), Some(DiffView::Split));
        assert_eq!(parse_diff_view("unified"), Some(DiffView::Unified));
    }

    #[test]
    fn fold_hint_format_and_clamp() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let hint = Style::default().fg(p.color(crate::color::Role::Hint));
        let h = fold_hint(173, false, &hint, 80).expect("a hint");
        assert_eq!(h.1, "... (173 more lines \u{2022} Ctrl+O to expand)");
        // The singular unit.
        let h = fold_hint(1, false, &hint, 80).expect("a hint");
        assert_eq!(h.1, "... (1 more line \u{2022} Ctrl+O to expand)");
        // The expanded state drops the key hint.
        let h = fold_hint(173, true, &hint, 80).expect("a hint");
        assert_eq!(h.1, "... (173 more lines)");
        // A narrow pane shortens the hint: the key drops first.
        let h = fold_hint(173, false, &hint, 30).expect("a hint");
        assert!(!h.1.contains("Ctrl+O"), "{h:?}");
        assert!(h.1.chars().count() <= 30, "{h:?}");
        // Zero remaining: no hint.
        assert!(fold_hint(0, false, &hint, 80).is_none());
    }

    #[test]
    fn read_summary_shows_the_line_count() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let mut cfg = ToolDisplay::preset(Preset::Balanced);
        cfg.read_mode = OutputMode::Summary;
        let value = serde_json::json!({"text": "a\nb\nc", "total_lines": 342});
        let rows = body_rows()
            .tool("read")
            .value(&value)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(false)
            .width(80)
            .call();
        let texts = rows_text(&rows);
        assert!(texts.iter().any(|t| t.contains("342 lines")), "{texts:?}");
    }

    #[test]
    fn read_preview_folds_to_the_preview_lines() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let cfg = ToolDisplay::preset(Preset::OpenCode);
        let text = (0..50)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let value = serde_json::json!({"text": text, "total_lines": 50});
        let rows = body_rows()
            .tool("read")
            .value(&value)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(false)
            .width(80)
            .call();
        // The opencode preset now previews read content (8 lines)
        // with syntax highlighting; the fold hint states the rest.
        let texts = rows_text(&rows);
        let shown = texts
            .iter()
            .filter(|l| l.starts_with("line "))
            .count();
        assert_eq!(shown, 8, "the preview shows 8 lines: {texts:?}");
        assert!(
            texts.iter().any(|t| t.contains("42 more lines")),
            "the fold hint states the remainder: {texts:?}"
        );
        // The preview mode of the balanced preset shows 8 lines.
        let cfg = ToolDisplay::preset(Preset::Balanced);
        let cfg = ToolDisplay {
            read_mode: OutputMode::Preview,
            ..cfg
        };
        let rows = body_rows()
            .tool("read")
            .value(&value)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(false)
            .width(80)
            .call();
        let shown = rows_text(&rows);
        assert_eq!(
            shown.iter().filter(|l| l.starts_with("line ")).count(),
            8,
            "the preview shows 8 lines: {shown:?}"
        );
        assert!(
            shown.iter().any(|l| l.contains("42 more lines")),
            "the fold hint states the remainder: {shown:?}"
        );
        // The expanded state shows up to the cap, no hint.
        let rows = body_rows()
            .tool("read")
            .value(&value)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(true)
            .width(80)
            .call();
        let shown = rows_text(&rows);
        assert_eq!(
            shown.iter().filter(|l| l.starts_with("line ")).count(),
            50,
            "the expanded state shows the body: {shown:?}"
        );
        assert!(!shown.iter().any(|l| l.contains("more lines")));
    }

    #[test]
    fn read_preview_highlights_known_language() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let cfg = ToolDisplay::preset(Preset::Balanced);
        let cfg = ToolDisplay {
            read_mode: OutputMode::Preview,
            ..cfg
        };
        let text = "1: let x = 42; // note\n2: ";
        let value = serde_json::json!({
            "text": text,
            "path": "/tmp/sample.rs",
            "total_lines": 2,
        });
        let rows = body_rows()
            .tool("read")
            .value(&value)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(false)
            .width(80)
            .call();
        let shown = rows_text(&rows);
        assert!(
            shown.iter().any(|l| l.contains("let x = 42")),
            "the first body row shows the rust line: {shown:?}"
        );
        // The keyword "let" gets the palette syntax-keyword color.
        let kw = p.style(crate::color::Role::SyntaxKeyword, Modifier::empty());
        let found_kw = rows.iter().any(|row| {
            row.iter()
                .any(|(st, t)| t == "let" && *st == kw)
        });
        assert!(found_kw, "the 'let' keyword is colored: {rows:?}");
        // The number "42" gets the palette syntax-number color.
        let num = p.style(crate::color::Role::SyntaxNumber, Modifier::empty());
        let found_num = rows.iter().any(|row| {
            row.iter()
                .any(|(st, t)| t == "42" && *st == num)
        });
        assert!(found_num, "the literal 42 is colored: {rows:?}");
        // The comment "// note" gets the palette syntax-comment color.
        let cmt = p.style(crate::color::Role::SyntaxComment, Modifier::DIM);
        let found_cmt = rows.iter().any(|row| {
            row.iter()
                .any(|(st, t)| t.contains("note") && *st == cmt)
        });
        assert!(found_cmt, "the comment is colored: {rows:?}");
    }

    #[test]
    fn read_preview_highlights_from_call_args() {
        // The log stores the read path in the call arguments, not the
        // result value (the value carries only `text`). The language
        // must still resolve from `call_args.file_path`.
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let cfg = ToolDisplay::preset(Preset::Balanced);
        let cfg = ToolDisplay {
            read_mode: OutputMode::Preview,
            ..cfg
        };
        let value = serde_json::json!({"text": "1: let x = 42; // note\n2: "});
        let args =
            serde_json::json!({"file_path": "/tmp/sample.rs"});
        let rows = body_rows()
            .tool("read")
            .value(&value)
            .call_args(&args)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(false)
            .width(80)
            .call();
        let shown = rows_text(&rows);
        assert!(
            shown.iter().any(|l| l.contains("let x = 42")),
            "the first body row shows the rust line: {shown:?}"
        );
        // The keyword "let" gets the palette syntax-keyword color even
        // though the value carries no `path` (the call args supply it).
        let kw = p.style(crate::color::Role::SyntaxKeyword, Modifier::empty());
        let found_kw = rows.iter().any(|row| {
            row.iter()
                .any(|(st, t)| t == "let" && *st == kw)
        });
        assert!(found_kw, "the 'let' keyword is colored from call args: {rows:?}");
        let cmt = p.style(crate::color::Role::SyntaxComment, Modifier::DIM);
        let found_cmt = rows.iter().any(|row| {
            row.iter()
                .any(|(st, t)| t.contains("note") && *st == cmt)
        });
        assert!(found_cmt, "the comment is colored from call args: {rows:?}");
    }

    #[test]
    fn read_value_path_wins_over_call_args() {
        // When the result value carries a `path`, it wins over the
        // call argument: the value is the record for the read.
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let cfg = ToolDisplay::preset(Preset::Balanced);
        let cfg = ToolDisplay {
            read_mode: OutputMode::Preview,
            ..cfg
        };
        let value = serde_json::json!({
            "text": "1: def f():\n2:     pass",
            "path": "/tmp/a.py",
        });
        let args = serde_json::json!({"file_path": "/tmp/b.rs"});
        let rows = body_rows()
            .tool("read")
            .value(&value)
            .call_args(&args)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(false)
            .width(80)
            .call();
        // The python keyword "def" colors through the python family,
        // not the rust keyword set: proves the value path won.
        let kw = p.style(crate::color::Role::SyntaxKeyword, Modifier::empty());
        let found_def = rows.iter().any(|row| {
            row.iter().any(|(st, t)| t == "def" && *st == kw)
        });
        assert!(
            found_def,
            "python 'def' is a keyword: {rows:?}"
        );
    }

    #[test]
    fn read_preview_no_path_stays_plain() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let cfg = ToolDisplay::preset(Preset::Balanced);
        let cfg = ToolDisplay {
            read_mode: OutputMode::Preview,
            ..cfg
        };
        let value = serde_json::json!({"text": "1: hello\n2: world"});
        let rows = body_rows()
            .tool("read")
            .value(&value)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(false)
            .width(80)
            .call();
        let shown = rows_text(&rows);
        assert!(
            shown.iter().any(|l| l.contains("1: hello")),
            "line 1 is shown: {shown:?}"
        );
        // Without a path there is no language, so every run stays in
        // the code tone (no syntax colors applied).
        let code = p.style(crate::color::Role::Code, Modifier::empty());
        for row in &rows {
            for (st, _) in row.iter() {
                assert!(
                    *st == code || *st == hint_style(&p),
                    "unexpected style in plain read body: {st:?}"
                );
            }
        }
    }

    /// The muted hint style, used by the plain-read assertion.
    fn hint_style(p: &crate::color::Palette) -> Style {
        Style::default()
            .fg(p.color(crate::color::Role::Hint))
            .add_modifier(Modifier::DIM)
    }

    #[test]
    fn edit_diff_shows_removed_and_added() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let cfg = ToolDisplay::preset(Preset::Balanced);
        let value = serde_json::json!({
            "text": "The file f has been updated successfully.",
            "path": "f",
            "before": "old one\nold two",
            "after": "new one\nnew two\nnew three",
            "replace_all": false
        });
        let rows = body_rows()
            .tool("edit")
            .value(&value)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(false)
            .width(80)
            .call();
        let texts = rows_text(&rows);
        // The stats line, like the reference diff presentation.
        assert!(
            texts.iter().any(|t| t.contains("+3 -2")),
            "the stats line shows the added and removed counts: {texts:?}"
        );
        assert!(texts.iter().any(|t| t == "- old one"), "{texts:?}");
        assert!(texts.iter().any(|t| t == "+ new three"), "{texts:?}");
    }

    #[test]
    fn write_diff_shows_the_content_as_added_lines() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let cfg = ToolDisplay::preset(Preset::Balanced);
        let value = serde_json::json!({
            "text": "Successfully wrote 5 bytes to f.",
            "path": "f",
            "operation": "create",
            "bytes": 5
        });
        let args = serde_json::json!({"file_path": "f", "content": "abc\ndef\n"});
        let rows = body_rows()
            .tool("write")
            .value(&value)
            .call_args(&args)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(false)
            .width(80)
            .call();
        let texts = rows_text(&rows);
        assert!(
            texts.iter().any(|t| t.contains("2 lines, 5 bytes")),
            "the write summary names the size: {texts:?}"
        );
        assert!(texts.iter().any(|t| t.starts_with("+ abc")), "{texts:?}");
        assert!(texts.iter().any(|t| t.starts_with("+ def")), "{texts:?}");
    }

    #[test]
    fn bash_collapsed_preview() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let cfg = ToolDisplay::preset(Preset::OpenCode);
        let text = format!(
            "$ ls\n{}",
            (0..30)
                .map(|i| format!("file{i}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let value = serde_json::json!({
            "text": text,
            "exit_code": 0,
            "stdout": text,
            "stderr": "",
            "timed_out": false,
            "truncated": false
        });
        let rows = body_rows()
            .tool("bash")
            .value(&value)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(false)
            .width(80)
            .call();
        let texts = rows_text(&rows);
        // The first line is the command line; the preview holds the
        // next 9 of the 10 collapsed lines.
        assert_eq!(
            texts.iter().filter(|l| l.starts_with("file")).count(),
            9,
            "the collapsed bash preview shows the first 10 lines (the command line plus 9 output lines): {texts:?}"
        );
        assert!(
            texts.iter().any(|l| l.contains("21 more lines")),
            "{texts:?}"
        );
    }

    #[test]
    fn search_count_mode() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let mut cfg = ToolDisplay::preset(Preset::Balanced);
        cfg.search_mode = SearchMode::Count;
        let value =
            serde_json::json!({"text": "a\nb\nc\nd", "path": "d", "type": "directory", "count": 4});
        let rows = body_rows()
            .tool("list")
            .value(&value)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(false)
            .width(80)
            .call();
        let texts = rows_text(&rows);
        assert!(
            texts.iter().any(|t| t.contains("4 entries")),
            "the count mode names the total: {texts:?}"
        );
    }

    #[test]
    fn expand_overrides_the_hidden_and_summary_modes() {
        // The port spec of the module header: one global key (the
        // Ctrl+O toggle) expands every collapsed block to the full
        // output. A hidden or summary mode shows its body lines
        // when the global expand is on.
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let cfg = ToolDisplay::preset(Preset::OpenCode);
        // read previews by default in opencode: collapsed shows the
        // body lines (up to the cap), expanded shows the full body.
        let value = serde_json::json!({"text": "a\nb\nc\nd", "total_lines": 4});
        let collapsed = rows_text(&body_rows()
            .tool("read")
            .value(&value)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(false)
            .width(80)
            .call());
        let shown = collapsed
            .iter()
            .filter(|l| l.starts_with('a') || l.starts_with('b')
                || l.starts_with('c') || l.starts_with('d'))
            .count();
        assert_eq!(shown, 4, "all four lines fit in the preview: {collapsed:?}");
        let expanded = rows_text(&body_rows()
            .tool("read")
            .value(&value)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(true)
            .width(80)
            .call());
        assert_eq!(
            expanded
                .iter()
                .filter(|l| l.starts_with("a") || l.starts_with("b"))
                .count(),
            2,
            "expanded hidden mode shows the body lines: {expanded:?}"
        );
        // bash summary by default in balanced: expanded shows lines.
        let mut bcfg = ToolDisplay::preset(Preset::Balanced);
        bcfg.bash_mode = OutputMode::Summary;
        let bvalue =
            serde_json::json!({"text": "$ ls\nf1\nf2", "exit_code": 0, "stdout": "f1\nf2\n"});
        let collapsed = rows_text(&body_rows()
            .tool("bash")
            .value(&bvalue)
            .err(false)
            .cfg(&bcfg)
            .palette(&p)
            .expanded(false)
            .width(80)
            .call());
        assert!(
            collapsed.iter().any(|l| l.contains("2 lines returned")),
            "collapsed summary mode keeps the count: {collapsed:?}"
        );
        let expanded = rows_text(&body_rows()
            .tool("bash")
            .value(&bvalue)
            .err(false)
            .cfg(&bcfg)
            .palette(&p)
            .expanded(true)
            .width(80)
            .call());
        assert!(
            expanded.iter().any(|l| l == "f1"),
            "expanded summary mode shows the body lines: {expanded:?}"
        );
        // search count by default in balanced: expanded shows lines.
        let mut scfg = ToolDisplay::preset(Preset::Balanced);
        scfg.search_mode = SearchMode::Count;
        let svalue =
            serde_json::json!({"text": "f1\nf2", "path": "d", "type": "directory", "count": 2});
        let expanded = rows_text(&body_rows()
            .tool("list")
            .value(&svalue)
            .err(false)
            .cfg(&scfg)
            .palette(&p)
            .expanded(true)
            .width(80)
            .call());
        assert!(
            expanded.iter().any(|l| l == "f1"),
            "expanded count mode shows the listing: {expanded:?}"
        );
    }

    #[test]
    fn box_rows_never_exceed_the_width() {
        // The narrow-pane width clamp of the port: no box row may
        // wrap past the reserved terminal width. The three-segment
        // split diff row is the worst case: two panes, the divider,
        // and the borders must fit the box width exactly.
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let value = serde_json::json!({"before": "a\nb\nc", "after": "a\nx\ny"});
        let mut split = ToolDisplay::preset(Preset::OpenCode);
        split.diff_view = DiffView::Split;
        let rows = body_rows()
            .tool("edit")
            .value(&value)
            .err(false)
            .cfg(&split)
            .palette(&p)
            .expanded(false)
            .width(120)
            .call();
        let boxed = box_rows("tool:edit  ok", &rows, 120, &p, None, false);
        for r in &boxed {
            let w: usize = r.iter().map(|(_, t)| t.chars().count()).sum();
            assert_eq!(w, 120, "a box row overflows the wide pane: {w}");
        }
        // The narrow pane: the auto layout switches to unified, the
        // rows stay single-segment, nothing wraps.
        let cfg = ToolDisplay::preset(Preset::OpenCode);
        let rows = body_rows()
            .tool("edit")
            .value(&value)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(false)
            .width(60)
            .call();
        let boxed = box_rows("tool:edit  ok", &rows, 60, &p, None, false);
        for r in &boxed {
            let w: usize = r.iter().map(|(_, t)| t.chars().count()).sum();
            assert!(w <= 60, "a box row overflows the narrow pane: {w}");
        }
    }

    #[test]
    fn wrap_hard_line_fits_and_wraps_and_splits() {
        // A line that fits returns as one piece.
        assert_eq!(wrap_hard_line("hi there", 40), vec!["hi there"]);
        // A space wrap: the pieces stay within the width, the words
        // stay whole.
        let pieces = wrap_hard_line("alpha beta gamma delta", 11);
        assert!(pieces.iter().all(|p| p.chars().count() <= 11),
            "a piece overflows: {pieces:?}");
        assert_eq!(pieces.join(" "), "alpha beta gamma delta",
            "the wrap keeps the words: {pieces:?}");
        // A word wider than the pane splits at the width.
        let big = wrap_hard_line(&"x".repeat(50), 20);
        assert_eq!(big.len(), 3, "50 over 20 is three pieces: {big:?}");
        assert_eq!(big[0].chars().count(), 20);
        assert_eq!(big[2].chars().count(), 10);
        // Zero width returns the line untouched: the box border
        // handles the empty pane.
        assert_eq!(wrap_hard_line("abc", 0), vec!["abc"]);
    }

    #[test]
    fn bash_body_wraps_the_command_line_on_a_narrow_pane() {
        // The user request of the 2026-09-04 pass: the bash command
        // text wraps to the next line on a narrow terminal, not a
        // truncated row. The output lines keep the hard-line
        // behavior (the box truncates them).
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let cfg = ToolDisplay::preset(Preset::OpenCode);
        let cmd = "make -C /home/dev/project build --jobs 8 --target release";
        let value = serde_json::json!({
            "text": format!("$ {cmd}\nline one\nline two"),
            "exit_code": 0, "stdout": "", "stderr": "",
        });
        // A wide pane: the command fits, one row.
        let wide = body_rows()
            .tool("bash")
            .value(&value)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(false)
            .width(120)
            .call();
        assert_eq!(wide.len(), 3, "command plus two lines: {wide:?}");
        assert_eq!(row_text(&wide[0]), format!("$ {cmd}"));
        // A narrow pane: the command wraps to several rows; the
        // output rows stay single.
        let narrow = body_rows()
            .tool("bash")
            .value(&value)
            .err(false)
            .cfg(&cfg)
            .palette(&p)
            .expanded(false)
            .width(24)
            .call();
        let head: Vec<String> = narrow.iter().take(4).map(row_text).collect();
        let joined: String = head.join("\n");
        assert!(
            joined.contains("$ make -C\n/home/dev/project build"),
            "the command wraps at a word boundary: {joined:?}"
        );
        for r in &narrow[..4] {
            assert!(row_text(r).chars().count() <= 24,
                "a wrapped row overflows the pane: {narrow:?}");
        }
    }

    #[test]
    fn box_rows_mark_the_truncated_overflow_with_an_ellipsis() {
        // The 2026-09-03 user directive: overflow is truncated with
        // a trailing ellipsis, never wrapped to the next line. The
        // box row keeps the box width exactly.
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let long = "a".repeat(500);
        let rows: Vec<BodyRow> = vec![vec![(Style::default(), long.clone())]];
        let boxed = box_rows("tool:read  ok", &rows, 80, &p, None, false);
        // The body row sits between the top and the bottom border.
        assert_eq!(boxed.len(), 3, "the box has three rows: {boxed:?}");
        let body = &boxed[1];
        let w: usize = body.iter().map(|(_, t)| t.chars().count()).sum();
        assert_eq!(w, 80, "the body row keeps the box width: {w}");
        let text = row_text(body);
        // The row is `│` + 78 inner columns + `│`. The cut sits at
        // the last inner column: one space, 76 content columns, the
        // ellipsis. The right border closes the row.
        let chars: Vec<char> = text.chars().collect();
        assert_eq!(chars[78], '…', "the cut marks the overflow: {text:?}");
        assert_eq!(chars[79], '│', "the border closes the row: {text:?}");
        // The three-segment split row: the middle segment that
        // overflows the remaining columns carries the ellipsis,
        // the row still fits the width.
        let seg = "b".repeat(200);
        let rows3: Vec<BodyRow> =
            vec![vec![
                (Style::default(), "left".to_string()),
                (Style::default(), "│".to_string()),
                (Style::default(), seg),
            ]];
        let boxed3 = box_rows("tool:edit  ok", &rows3, 60, &p, None, false);
        let b3 = &boxed3[1];
        let w3: usize = b3.iter().map(|(_, t)| t.chars().count()).sum();
        assert_eq!(w3, 60, "the split row keeps the box width: {w3}");
        let t3 = row_text(b3);
        assert!(
            t3.contains("…"),
            "the overflow marks the cut with the ellipsis: {t3:?}"
        );
    }

    #[test]
    fn box_rows_draw_the_rounded_panel() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let body: Vec<BodyRow> = vec![
            vec![(Style::default(), "line one".to_string())],
            vec![(Style::default(), "line two".to_string())],
        ];
        let rows = box_rows("tool:read  ok", &body, 30, &p, None, false);
        // One terminal line per row: the top border row, then the
        // body rows (border + body + border cells), then the bottom.
        assert_eq!(rows.len(), 4, "top + 2 body rows + bottom");
        let top = rows_text(&[rows[0].clone()]);
        assert!(
            top[0].starts_with('\u{250c}') && top[0].ends_with('\u{2510}'),
            "{top:?}"
        );
        let bottom = rows_text(&[rows[3].clone()]);
        assert!(
            bottom[0].starts_with('\u{2514}') && bottom[0].ends_with('\u{2518}'),
            "{bottom:?}"
        );
        let r1 = rows_text(&[rows[1].clone()]);
        assert!(r1[0].contains("line one"), "{r1:?}");
        assert!(
            r1[0].starts_with('\u{2502}') && r1[0].ends_with('\u{2502}'),
            "{r1:?}"
        );
        // Every cell of the box carries the light background.
        for row in &rows {
            for (st, _) in row {
                assert!(st.bg.is_some(), "the box cells keep the background: {st:?}");
            }
        }
    }

    #[test]
    fn diff_layout_auto_switches_at_the_threshold() {
        let cfg = ToolDisplay::preset(Preset::Balanced);
        assert_eq!(cfg.diff_layout(120), DiffView::Split, "wide: split");
        assert_eq!(cfg.diff_layout(119), DiffView::Unified, "narrow: unified");
        let mut forced = ToolDisplay::preset(Preset::Balanced);
        forced.diff_view = DiffView::Unified;
        assert_eq!(
            forced.diff_layout(200),
            DiffView::Unified,
            "forced: unified"
        );
    }

    #[test]
    fn clamp_col_elides_the_overflow() {
        let s = "x".repeat(50);
        let c = clamp_col(s, 10);
        assert_eq!(c.chars().count(), 10, "{c:?}");
        assert!(c.ends_with('\u{2026}'), "{c:?}");
        assert_eq!(clamp_col(String::new(), 10), "");
    }
}
