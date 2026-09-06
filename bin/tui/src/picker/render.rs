//! The picker body renderer (section 4.4 and 6).
//!
//! `render_picker` draws the picker body into a given `Rect`. It never
//! decides where the `Rect` is: the container (the floating window in
//! section 6) owns placement. This keeps the body reusable for a
//! future inline container (section 6, Option A).
//!
//! The float layout lives in `crate::float`, shared with the command
//! palette (docs/tui-command-palette.md section 11).

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;
use bon::builder;

use super::fuzzy::Snapshot;
use super::preview::Previewer;
use super::state::PickerState;

// Re-export the shared float layout so existing imports keep working.
pub use crate::float::{compute_float_layout, FloatLayout};

/// The result count below which the preview pane auto-hides so a short
/// list keeps the full width (a `telescope` `preview_cutoff`
/// behavior).
pub const PREVIEW_CUTOFF: usize = 4;
/// The default height of the preview scroll page for `Ctrl+U` /
/// `Ctrl+D`.
pub const PREVIEW_PAGE: usize = 5;

/// Draw the picker body into a pre-computed float layout. This
/// function never decides where the float is: the container (the
/// floating window in section 6, or an inline row in a later
/// revision) computes the regions and passes them in. The hardware
/// cursor is placed on the input bar. Eight parameters, so a `bon`
/// builder (docs/coding-conventions.md).
#[builder]
pub fn render_picker<'frame>(
    f: &mut Frame<'frame>,
    state: &mut PickerState,
    snapshot: &Snapshot,
    layout: &FloatLayout,
    previewer: &dyn Previewer,
    hints: &str,
    palette: &crate::color::Palette,
    cursor: &mut Option<(u16, u16)>,
) {
    // Clamp the cursor and window to the fresh snapshot before
    // drawing (the snapshot may have shrunk since the last move).
    state.sync(snapshot.items.len());

    // Paint the entire float region with the terminal default background
    // so the transcript / input rows underneath are hidden.
    f.render_widget(Clear, layout.float);

    // The outer float border and title. An unsettled ranking shows a
    // trailing ellipsis so the user sees a fresh pass is in flight.
    let accent = palette.color(crate::color::Role::Border4);
    let border_style = Style::default().fg(accent);
    let n = snapshot.items.len();
    let pending = if snapshot.settled { "" } else { " ·" };
    // The cycled file scope (docs/tui-file-picker.md P9) tags the
    // title so the widened set is visible at a glance.
    let scope_tag = state
        .scope
        .tag()
        .map(|t| format!(" · {t}"))
        .unwrap_or_default();
    let title = format!(
        "files ({}) — @ {} — {} match{}{}{}",
        layout.orientation.label(),
        snapshot.query,
        n,
        if n == 1 { "" } else { "es" },
        pending,
        scope_tag
    );
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border_style)
        .title(Line::from(Span::styled(
            title,
            Style::default().add_modifier(Modifier::BOLD),
        )));
    f.render_widget(block, layout.float);

    // The result list.
    render_list(f, state, snapshot, layout, palette, accent);

    // The preview pane, if present.
    if let Some(preview_rect) = layout.preview {
        if let Some(item) = snapshot.items.get(state.cursor()) {
            render_preview(f, item, state, previewer, &preview_rect, palette);
        }
    }

    // The input bar: the `@query` prompt and the key hints, as a
    // plain line at the bottom of the float interior.
    let query_display = format!("@{}", snapshot.query);
    let query_len = query_display.chars().count();
    let prose = palette.color(crate::color::Role::PlainText);
    let hint_style = Style::default().fg(palette.color(crate::color::Role::Hint));
    let input_line = Line::from(vec![
        Span::styled(
            query_display.clone(),
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(prose),
        ),
        Span::styled(format!("  {hints}"), hint_style),
    ]);
    f.render_widget(Paragraph::new(input_line), layout.input);

    // The hardware cursor on the input bar, one cell past the query text.
    let caret = layout.input.x + query_len as u16;
    let max_x = layout.input.x + layout.input.width.saturating_sub(1);
    *cursor = Some((caret.min(max_x), layout.input.y));
}

/// Shorten a result-list label to fit `max_chars` characters (P10 in
/// `docs/tui-file-picker.md`).
///
/// For a path label that has directory levels, the leading levels are
/// collapsed into a `...` prefix so the tail of the path stays
/// visible (`.../a/b/src/app.rs`): the largest suffix of path
/// components that fits with the `.../` prefix is kept. A label that
/// fits is returned unchanged. A label without directory levels, or
/// one still too wide with only `.../` plus the file name, falls
/// back to the plain head truncation with a trailing `…`.
pub fn abbreviate_path(label: &str, max_chars: usize) -> String {
    if label.chars().count() <= max_chars {
        return label.to_string();
    }
    // Empty components (a leading `/` on absolute labels) carry no
    // directory, so they never count as a level to collapse.
    let comps: Vec<&str> = label.split('/').filter(|c| !c.is_empty()).collect();
    if comps.len() >= 2 {
        // Growing suffixes: the first (smallest start index) that
        // fits with the `.../` prefix is the largest tail that fits.
        for start in 0..comps.len() {
            let candidate = format!(".../{}", comps[start..].join("/"));
            if candidate.chars().count() <= max_chars {
                return candidate;
            }
        }
    }
    let head: String = label.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{head}…")
}

fn render_list(
    f: &mut Frame,
    state: &PickerState,
    snapshot: &Snapshot,
    layout: &FloatLayout,
    palette: &crate::color::Palette,
    accent: ratatui::style::Color,
) {
    let list_w = layout.list.width as usize;
    let max_chars = list_w.saturating_sub(4);
    let end = (state.top() + state.visible).min(snapshot.items.len());
    let lines: Vec<Line> = (state.top()..end)
        .map(|i| {
            let item = &snapshot.items[i];
            let is_cursor = i == state.cursor();
            let marker = if is_cursor { "❯ " } else { "  " };
            let marker_style = if is_cursor {
                Style::default()
                    .bg(accent)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(palette.color(crate::color::Role::Hint))
            };
            let label_style = if is_cursor {
                Style::default()
                    .bg(accent)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(palette.color(crate::color::Role::PlainText))
            };
            // P10: when the path label is too wide for the list
            // column, collapse the leading directory levels into a
            // `...` prefix so the tail of the path stays visible
            // (docs/tui-file-picker.md P10). The budget follows the
            // column, which differs with the preview pane on or off.
            let truncated = abbreviate_path(&item.label, max_chars);
            Line::from(vec![
                Span::styled(marker, marker_style),
                Span::styled(truncated, label_style),
            ])
        })
        .collect();
    let list_block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(palette.color(crate::color::Role::Status)))
        .title(Line::from(Span::styled(
            "results",
            Style::default().fg(palette.color(crate::color::Role::Hint)),
        )));
    f.render_widget(Paragraph::new(lines).block(list_block), layout.list);
}

fn render_preview(
    f: &mut Frame,
    item: &crate::picker::items::PickerItem,
    state: &PickerState,
    previewer: &dyn Previewer,
    preview_rect: &Rect,
    palette: &crate::color::Palette,
) {
    let header = previewer.header(item).unwrap_or_else(|| item.label.clone());
    let content = previewer.content(item, palette);
    let pane_h = preview_rect.height as usize;
    let start = state.preview_scroll.min(content.len());
    // Plain (default-style) segments get the pane's plain-text tone;
    // highlighted segments keep their palette syntax styles.
    let plain_base = palette.style(crate::color::Role::PlainText, Modifier::empty());
    let lines: Vec<Line> = content
        .iter()
        .skip(start)
        .take(pane_h)
        .map(|segs| {
            if segs.is_empty() {
                Line::from("")
            } else {
                let spans: Vec<Span> = segs
                    .iter()
                    .map(|(s, t)| {
                        let style = if *s == Style::default() { plain_base } else { *s };
                        Span::styled(t.clone(), style)
                    })
                    .collect();
                Line::from(spans)
            }
        })
        .collect();
    let preview_block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(palette.color(crate::color::Role::Status)))
        .title(Line::from(Span::styled(
            header,
            Style::default().fg(palette.color(crate::color::Role::Hint)),
        )));
    f.render_widget(Paragraph::new(lines).block(preview_block), *preview_rect);
}

// ── tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::float::Orientation;
    use crate::picker::items::PickerItem;
    use crate::picker::preview::FilePreviewer;

    fn sample_snapshot(n: usize) -> Snapshot {
        Snapshot {
            items: (0..n)
                .map(|i| PickerItem {
                    label: format!("src/file_{i:03}.rs"),
                    value: format!("src/file_{i:03}.rs"),
                    payload: format!("/repo/src/file_{i:03}.rs"),
                })
                .collect(),
            query: "file".into(),
            settled: true,
        }
    }

    #[test]
    fn wide_layout_splits_side_by_side() {
        let term = Rect::new(0, 0, 160, 40);
        let layout = compute_float_layout(term, true);
        assert_eq!(layout.orientation, Orientation::Wide);
        let p = layout.preview.as_ref().expect("preview present in wide");
        assert!(layout.list.x < p.x, "list is left of preview");
        assert_eq!(layout.list.y, p.y, "list and preview share the top row");
    }

    #[test]
    fn narrow_layout_stacks_vertically() {
        let term = Rect::new(0, 0, 90, 30);
        let layout = compute_float_layout(term, true);
        assert_eq!(layout.orientation, Orientation::Narrow);
        let p = layout.preview.as_ref().expect("preview present in narrow");
        assert!(layout.list.y < p.y, "list is above preview");
    }

    #[test]
    fn too_narrow_drops_preview() {
        let term = Rect::new(0, 0, 40, 20);
        let layout = compute_float_layout(term, true);
        assert_eq!(layout.orientation, Orientation::TooNarrow);
        assert!(layout.preview.is_none());
    }

    #[test]
    fn preview_cutoff_hides_pane() {
        let term = Rect::new(0, 0, 160, 40);
        // Three items is below PREVIEW_CUTOFF(4): no preview even wide.
        let show = 3 >= PREVIEW_CUTOFF;
        assert!(!show, "three items is under the cutoff");
        let layout = compute_float_layout(term, false);
        assert!(layout.preview.is_none());
    }

    #[test]
    fn float_is_centered() {
        let term = Rect::new(0, 0, 100, 30);
        let layout = compute_float_layout(term, true);
        let fw = layout.float.width as usize;
        let expected_x = (100 - fw) / 2;
        assert_eq!(layout.float.x as usize, expected_x, "float is centered");
    }

    #[test]
    fn orientation_flips_on_resize() {
        let wide = compute_float_layout(Rect::new(0, 0, 160, 30), true);
        assert_eq!(wide.orientation, Orientation::Wide);
        let narrow = compute_float_layout(Rect::new(0, 0, 90, 30), true);
        assert_eq!(narrow.orientation, Orientation::Narrow);
    }

    #[test]
    fn render_picker_does_not_panic_on_empty() {
        let app_palette = crate::color::Palette::builtin(crate::color::Level::detect());
        let snap = sample_snapshot(0);
        let mut state = PickerState::new();
        state.open("", 10);
        let previewer = FilePreviewer::new(50);
        // Zero items is below the cutoff, so no preview pane.
        let layout = compute_float_layout(Rect::new(0, 0, 120, 40), false);
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut term = ratatui::Terminal::new(backend).expect("backend");
        let mut cursor = None;
        let _ = term.draw(|f| {
            render_picker()
                .f(f)
                .state(&mut state)
                .snapshot(&snap)
                .layout(&layout)
                .previewer(&previewer)
                .hints("esc close")
                .palette(&app_palette)
                .cursor(&mut cursor)
                .call();
        });
        assert!(cursor.is_some(), "the cursor is placed on the input bar");
    }

    #[test]
    fn render_picker_draws_list_and_preview() {
        let snap = sample_snapshot(10);
        let mut state = PickerState::new();
        state.open("file", 5);
        let previewer = FilePreviewer::new(50);
        let palette = crate::color::Palette::builtin(crate::color::Level::detect());
        // Ten items is above the cutoff and the terminal is wide enough,
        // so the preview pane is present and side-by-side.
        let layout = compute_float_layout(Rect::new(0, 0, 160, 40), true);
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 40)).expect("backend");
        let mut cursor = None;
        let _ = term.draw(|f| {
            render_picker()
                .f(f)
                .state(&mut state)
                .snapshot(&snap)
                .layout(&layout)
                .previewer(&previewer)
                .hints("esc close")
                .palette(&palette)
                .cursor(&mut cursor)
                .call();
        });
        let buf = term.backend().buffer();
        // The buffer content is a flat slice of cells, one per column
        // then the next row, so joining every symbol keeps each row's
        // text contiguous for substring asserts.
        let joined: String = buf
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(joined.contains("src/file_000"), "the top result shows");
        assert!(joined.contains("results"), "the list pane title shows");
        assert!(cursor.is_some());
    }

    // ── P10: long-path abbreviation (`abbreviate_path`) ─────────────

    #[test]
    fn abbrev_keeps_fitting_labels_unchanged() {
        assert_eq!(abbreviate_path("a/b/src/app.rs", 14), "a/b/src/app.rs", "an exact fit is unchanged");
        assert_eq!(abbreviate_path("a/b/src/app.rs", 20), "a/b/src/app.rs");
        assert_eq!(abbreviate_path("file.rs", 8), "file.rs");
    }

    #[test]
    fn abbrev_collapses_leading_parent_levels() {
        // The P10 example: the largest tail that fits the column is
        // kept, the dropped parent levels become a `...` prefix.
        assert_eq!(
            abbreviate_path("x/y/z/a/b/src/app.rs", 18),
            ".../a/b/src/app.rs"
        );
        // A 74-char path at the 47-char preview-on column budget
        // collapses the leading levels; the tail stays visible.
        let long = "very/long/path/segment/that/goes/way/beyond/any/sane/terminal/width/app.rs";
        assert_eq!(
            abbreviate_path(long, 47),
            ".../way/beyond/any/sane/terminal/width/app.rs"
        );
        // A tighter column collapses more levels.
        assert_eq!(
            abbreviate_path(long, 30),
            ".../sane/terminal/width/app.rs"
        );
    }

    #[test]
    fn abbrev_handles_absolute_labels() {
        // A leading `/` is not a directory level: the result must not
        // gain a doubled `.../` prefix.
        assert_eq!(
            abbreviate_path("/home/user/proj/src/app.rs", 19),
            ".../proj/src/app.rs"
        );
    }

    #[test]
    fn abbrev_falls_back_to_head_truncation() {
        // No directory levels: plain head truncation with a trailing
        // ellipsis, as before P10.
        assert_eq!(
            abbreviate_path("averyveryverylongfilename.txt", 10),
            "averyvery…"
        );
        // Directory levels exist, but even `.../` plus the file name
        // is wider than the budget: the head truncation still wins.
        assert_eq!(
            abbreviate_path("ab/veryverylongfilename.txt", 10),
            "ab/veryve…"
        );
    }

    #[test]
    fn render_picker_abbreviates_long_paths_in_narrow_list_column() {
        let palette = crate::color::Palette::builtin(crate::color::Level::detect());
        let long = "very/long/path/segment/that/goes/way/beyond/any/sane/terminal/width/app.rs";
        let snap = Snapshot {
            items: vec![
                PickerItem {
                    label: long.to_string(),
                    value: long.to_string(),
                    payload: "/repo/app.rs".into(),
                },
                PickerItem {
                    label: "src/file_001.rs".into(),
                    value: "src/file_001.rs".into(),
                    payload: "/repo/src/file_001.rs".into(),
                },
            ],
            query: "app".into(),
            settled: true,
        };
        let previewer = FilePreviewer::new(50);

        // Preview pane ON: the list column is 51 wide, a 47-char
        // budget. The 74-char path collapses its leading levels.
        let layout = compute_float_layout(Rect::new(0, 0, 160, 40), true);
        let mut state = PickerState::new();
        state.open("app", 5);
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 40)).expect("backend");
        let mut cursor = None;
        let _ = term.draw(|f| {
            render_picker()
                .f(f)
                .state(&mut state)
                .snapshot(&snap)
                .layout(&layout)
                .previewer(&previewer)
                .hints("esc close")
                .palette(&palette)
                .cursor(&mut cursor)
                .call();
        });
        let joined: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            joined.contains(".../way/beyond/any/sane/terminal/width/app.rs"),
            "with the preview pane on the narrow list column collapses the leading levels: {joined:?}"
        );

        // Preview pane OFF: the list column is 94 wide, a 90-char
        // budget. The full 74-char path fits and shows unabbreviated.
        let layout = compute_float_layout(Rect::new(0, 0, 160, 40), false);
        let mut state = PickerState::new();
        state.open("app", 5);
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 40)).expect("backend");
        let mut cursor = None;
        let _ = term.draw(|f| {
            render_picker()
                .f(f)
                .state(&mut state)
                .snapshot(&snap)
                .layout(&layout)
                .previewer(&previewer)
                .hints("esc close")
                .palette(&palette)
                .cursor(&mut cursor)
                .call();
        });
        let joined: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            joined.contains(long),
            "with the preview pane off the full path fits the column and shows unabbreviated: {joined:?}"
        );
    }
}
