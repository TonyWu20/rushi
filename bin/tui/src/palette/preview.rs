//! The `CommandPreviewer`: renders the preview-pane content for a
//! highlighted palette item (docs/tui-command-palette.md sections 11,
//! 12).
//!
//! - `Run` items show their help text.
//! - `Set` items list their options, marking the current value with
//!   `*` and the user's selection with `>`.
//! - `Goto` items show help text; the sub-list replaces the left pane.
//! - `Ext` items show the extension's help text; settings list their
//!   options the same way `Set` items do.
//!
//! Session-buffer items (the `b` Goto sub-list) carry their metadata
//! in `help` as multi-line text the previewer renders verbatim.

use crate::highlight::Seg;
use crate::palette::items::{CmdKind, PaletteItem};
use ratatui::style::Modifier;

/// Render the preview-pane content for one highlighted item.
///
/// Returns a list of lines; each line is a list of `(style, text)`
/// segments. The caller places the pane and applies scroll.
pub fn render_preview(
    item: &PaletteItem,
    option_cursor: usize,
    palette: &crate::color::Palette,
) -> Vec<Vec<Seg>> {
    match item.kind {
        CmdKind::Set | CmdKind::Ext if !item.options.is_empty() => {
            option_list(item, option_cursor, palette)
        }
        _ => {
            // Run, Goto, and Ext-without-options: show help text.
            help_lines(item, palette)
        }
    }
}

/// Lines for a `Set` / Ext-setting item: one line per option, the
/// current value marked with `*`, the selected option marked with
/// `>`.
fn option_list(
    item: &PaletteItem,
    option_cursor: usize,
    palette: &crate::color::Palette,
) -> Vec<Vec<Seg>> {
    let plain =
        palette.style(crate::color::Role::PlainText, Modifier::empty());
    let dim = palette.style(crate::color::Role::Hint, Modifier::DIM);
    let accent =
        palette.style(crate::color::Role::Accent, Modifier::BOLD);

    let mut out: Vec<Vec<Seg>> = Vec::new();
    for (i, opt) in item.options.iter().enumerate() {
        let selected = i == option_cursor;
        let style = if selected {
            accent
        } else if opt.current {
            dim
        } else {
            plain
        };
        let mark = if selected {
            ">"
        } else if opt.current {
            "*"
        } else {
            " "
        };
        let text = format!("{mark} {}", opt.value);
        out.push(vec![(style, text)]);
    }
    if item.help.is_empty() {
        out
    } else {
        let mut lines = out;
        lines.push(Vec::new()); // blank separator
        lines.extend(help_lines(item, palette));
        lines
    }
}

/// Lines for help-text items. One `Seg` per line; blank line for an
/// empty help.
fn help_lines(
    item: &PaletteItem,
    palette: &crate::color::Palette,
) -> Vec<Vec<Seg>> {
    let plain =
        palette.style(crate::color::Role::PlainText, Modifier::empty());
    let help = if item.help.is_empty() {
        String::new()
    } else {
        item.help.clone()
    };
    let mut out: Vec<Vec<Seg>> = Vec::new();
    for line in help.lines() {
        out.push(vec![(plain, line.to_string())]);
    }
    if out.is_empty() {
        out.push(Vec::new());
    }
    out
}

// ── tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::items::CmdOption;
    use crate::palette::items::CmdKind;

    fn palette() -> crate::color::Palette {
        crate::color::Palette::builtin(crate::color::Level::Rgb)
    }

    #[test]
    fn set_item_lists_options_and_marks_current() {
        let item = PaletteItem {
            id: "thinking-level".into(),
            label: "thinking level".into(),
            kind: CmdKind::Set,
            hint: "high".into(),
            help: "Pick a level".into(),
            options: vec![
                CmdOption { value: "low".into(), current: false },
                CmdOption { value: "high".into(), current: true },
            ],
            ext: None,
        };
        let lines = render_preview(&item, 1, &palette());
        let joined: Vec<String> = lines
            .iter()
            .map(|l| l.iter().map(|s| s.1.as_str()).collect::<Vec<_>>().join(""))
            .collect();
        // Two option lines, then a blank, then the help line.
        assert!(joined[0].contains("low"));
        assert!(joined[1].contains("high"));
        assert!(joined[1].contains(">"), "selected option is marked");
    }

    #[test]
    fn run_item_shows_help() {
        let item = PaletteItem {
            id: "q".into(),
            label: "q".into(),
            kind: CmdKind::Run,
            hint: String::new(),
            help: "Quit the TUI.".into(),
            options: Vec::new(),
            ext: None,
        };
        let lines = render_preview(&item, 0, &palette());
        let joined: String = lines
            .iter()
            .map(|l| l.iter().map(|s| s.1.as_str()).collect::<Vec<_>>().join(""))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("Quit the TUI."));
    }
}
