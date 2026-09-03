//! The picker body renderer and float layout (section 4.4 and 6).
//!
//! `render_picker` draws the picker body into a given `Rect`. It never
//! decides where the `Rect` is: the container (the floating window in
//! section 6) owns placement. This keeps the body reusable for a
//! future inline container (section 6, Option A).
//!
//! The float layout is a function of the terminal size and the picker
//! state. It flips orientation at a width threshold with no key press
//! (section 6, "Orientation: wide and narrow").

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;
use bon::builder;

use super::fuzzy::Snapshot;
use super::preview::Previewer;
use super::state::PickerState;

/// The orientation of the float layout, a function of the float width
/// (section 6, "Orientation: wide and narrow").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    /// List left, preview right.
    Wide,
    /// List top, preview bottom.
    Narrow,
    /// Preview dropped; list and input in one column.
    TooNarrow,
}

/// The computed regions of the floating picker window.
#[derive(Debug, Clone)]
pub struct FloatLayout {
    /// The outer float border.
    pub float: Rect,
    /// The result list region.
    pub list: Rect,
    /// The input bar region (the `@query` line plus hints).
    pub input: Rect,
    /// The preview pane region, `None` when it is hidden.
    pub preview: Option<Rect>,
    pub orientation: Orientation,
}

/// The float width at which the layout switches to wide (list and
/// preview side by side). A config knob, not a hard constant.
pub const WIDE_MIN: u16 = 80;
/// The float width below which the preview drops out entirely.
pub const FLOAT_MIN: u16 = 50;
/// The result count below which the preview pane auto-hides so a short
/// list keeps the full width (a `telescope` `preview_cutoff`
/// behavior).
pub const PREVIEW_CUTOFF: usize = 4;
/// The default height of the preview scroll page for `Ctrl+U` /
/// `Ctrl+D`.
pub const PREVIEW_PAGE: usize = 5;

/// Compute the float layout from the terminal area and whether the
/// preview pane should show. The orientation is a pure function of the
/// float width: wide side-by-side, narrow stacked, or too narrow with
/// no preview.
pub fn compute_float_layout(term: Rect, show_preview: bool) -> FloatLayout {
    let tw = term.width as usize;
    let th = term.height as usize;
    // Center a 60% box, floored at a minimum, capped at the terminal.
    let fw = ((tw * 6) / 10).max(20).min(tw.saturating_sub(4));
    let fh = ((th * 6) / 10).max(10).min(th.saturating_sub(4));
    let x = term.x as usize + (tw.saturating_sub(fw)) / 2;
    let y = term.y as usize + (th.saturating_sub(fh)) / 2;
    let float = Rect::new(x as u16, y as u16, fw as u16, fh as u16);

    // The interior, inside the 1-cell border.
    let inner_w = float.width.saturating_sub(2);
    let inner_h = float.height.saturating_sub(2);
    let ix = float.x + 1;
    let iy = float.y + 1;

    // The input bar is one row at the bottom of the interior.
    let input_h: u16 = 1;
    let body_h = inner_h.saturating_sub(input_h);
    let input = Rect::new(ix, iy + body_h, inner_w, input_h);

    let orientation = if fw as u16 >= WIDE_MIN {
        Orientation::Wide
    } else if fw as u16 >= FLOAT_MIN {
        Orientation::Narrow
    } else {
        Orientation::TooNarrow
    };

    let preview_wanted = show_preview && orientation != Orientation::TooNarrow;

    let (list, preview) = if preview_wanted {
        match orientation {
            Orientation::Wide => {
                let gap: u16 = 1;
                let total_w = inner_w.saturating_sub(gap);
                let list_w = total_w * 55 / 100;
                let preview_w = total_w - list_w;
                let list = Rect::new(ix, iy, list_w, body_h);
                let preview = Rect::new(ix + list_w + gap, iy, preview_w, body_h);
                (list, Some(preview))
            }
            _ => {
                // Narrow: stack list above preview.
                let gap: u16 = 1;
                let total_h = body_h.saturating_sub(gap);
                let list_h = total_h * 45 / 100;
                let preview_h = total_h - list_h;
                let list = Rect::new(ix, iy, inner_w, list_h);
                let preview = Rect::new(ix, iy + list_h + gap, inner_w, preview_h);
                (list, Some(preview))
            }
        }
    } else {
        let list = Rect::new(ix, iy, inner_w, body_h);
        (list, None)
    };

    FloatLayout {
        float,
        list,
        input,
        preview,
        orientation,
    }
}

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
    let title = format!(
        "{} — @ {} — {} match{}{}",
        layout.orientation.label(),
        snapshot.query,
        n,
        if n == 1 { "" } else { "es" },
        pending
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
            let truncated = if item.label.chars().count() > max_chars {
                let head: String = item.label.chars().take(max_chars.saturating_sub(1)).collect();
                format!("{head}…")
            } else {
                item.label.clone()
            };
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
    let content = previewer.content(item);
    let pane_h = preview_rect.height as usize;
    let start = state.preview_scroll.min(content.len());
    let visible: Vec<String> = content.iter().skip(start).take(pane_h).cloned().collect();
    let lines: Vec<Line> = visible
        .into_iter()
        .map(|l| Line::from(Span::styled(l, Style::default().fg(palette.color(crate::color::Role::PlainText)))))
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

impl Orientation {
    /// A short label for the float title.
    pub fn label(self) -> &'static str {
        match self {
            Self::Wide => "files (wide)",
            Self::Narrow => "files (narrow)",
            Self::TooNarrow => "files",
        }
    }
}

// ── tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
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
}
