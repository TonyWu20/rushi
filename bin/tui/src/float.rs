//! Shared floating-window layout used by the picker and the command
//! palette (docs/tui-command-palette.md section 11).
//!
//! The layout is a pure function of the terminal area and whether the
//! preview pane should show. It flips orientation at width thresholds
//! with no key press.

use ratatui::layout::Rect;

/// The orientation of the float layout, a function of the float width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    /// List left, preview right.
    Wide,
    /// List top, preview bottom.
    Narrow,
    /// Preview dropped; list and input in one column.
    TooNarrow,
}

impl Orientation {
    /// A short orientation label for the float title.
    pub fn label(self) -> &'static str {
        match self {
            Self::Wide => "wide",
            Self::Narrow => "narrow",
            Self::TooNarrow => "compact",
        }
    }
}

/// The computed regions of a floating window.
#[derive(Debug, Clone)]
pub struct FloatLayout {
    /// The outer float border.
    pub float: Rect,
    /// The list / content region.
    pub list: Rect,
    /// The input bar region.
    pub input: Rect,
    /// The preview pane region, `None` when it is hidden.
    pub preview: Option<Rect>,
    pub orientation: Orientation,
}

/// The float width at which the layout switches to wide (list and
/// preview side by side).
pub const WIDE_MIN: u16 = 80;
/// The float width below which the preview drops out entirely.
pub const FLOAT_MIN: u16 = 50;

/// Compute the float layout from the terminal area and whether the
/// preview pane should show.
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

// ── tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
}
