//! Palette item types, built-in command registration, and the
//! extension-command conversion (docs/tui-command-palette.md sections 5, 6, 10, 11).

use crate::ext::ExtCommand;

/// The command kind. Drives the preview-pane content and the
/// commit behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmdKind {
    /// Fire one `Action`. No arguments. Enter executes and closes.
    Run,
    /// A value with a choice. The preview pane lists options; the
    /// user selects one and Enter applies it.
    Set,
    /// Open a sub-list. The query keeps feeding the sub-list filter.
    Goto,
    /// An extension-owned command. Enter sends an `invoke` op.
    Ext,
}

/// One option of a `Set` or extension setting item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdOption {
    pub value: String,
    /// Whether this is the currently active value (marked in the
    /// preview pane).
    pub current: bool,
}

/// One palette entry.
#[derive(Debug, Clone, PartialEq)]
pub struct PaletteItem {
    /// Stable key, e.g. `"thinking-level"`, `"b"`, `"myext.reload"`.
    pub id: String,
    /// Display label shown in the list.
    pub label: String,
    pub kind: CmdKind,
    /// A short hint: the keybinding, the current value, or empty.
    pub hint: String,
    /// Help text shown in the preview pane.
    pub help: String,
    /// Non-empty only for `Set` items and extension settings.
    pub options: Vec<CmdOption>,
    /// The extension name that owns this item, or `None` for built-in.
    pub ext: Option<String>,
}

/// The thinking-level values in cycle order
/// (docs/tui-thinking-block.md section 4). The palette
/// `thinking-level` item offers these as options; `Ctrl+L` walks the
/// same order.
pub const THINKING_LEVEL_ORDER: &[&str] = &[
    "none",
    "minimal",
    "low",
    "medium",
    "high",
    "xhigh",
    "max",
];

/// The built-in palette commands (docs/tui-command-palette.md section 6).
///
/// `current_effort` is the active model's current thinking level,
/// used to mark the `thinking-level` item's current option.
pub fn builtins(current_effort: &str) -> Vec<PaletteItem> {
    let effort_opts: Vec<CmdOption> = THINKING_LEVEL_ORDER
        .iter()
        .map(|v| CmdOption {
            value: (*v).to_string(),
            current: v.eq_ignore_ascii_case(current_effort),
        })
        .collect();

    vec![
        PaletteItem {
            id: "toggle-tools".into(),
            label: "toggle-tools".into(),
            kind: CmdKind::Run,
            hint: "Ctrl+O".into(),
            help: "Toggle tool-result fold/expand (all blocks).".into(),
            options: Vec::new(),
            ext: None,
        },
        PaletteItem {
            id: "toggle-thinking".into(),
            label: "toggle-thinking".into(),
            kind: CmdKind::Run,
            hint: "Ctrl+X".into(),
            help: "Show or hide thinking blocks.".into(),
            options: Vec::new(),
            ext: None,
        },
        PaletteItem {
            id: "expand-thinking".into(),
            label: "expand-thinking".into(),
            kind: CmdKind::Run,
            hint: "Ctrl+T".into(),
            help: "Collapse or expand thinking blocks.".into(),
            options: Vec::new(),
            ext: None,
        },
        PaletteItem {
            id: "thinking-level".into(),
            label: "thinking level".into(),
            kind: CmdKind::Set,
            hint: current_effort.to_string(),
            help: format!(
                "Set the thinking level for the active model. Current: {current_effort}."
            ),
            options: effort_opts,
            ext: None,
        },
        PaletteItem {
            id: "b".into(),
            label: "b".into(),
            kind: CmdKind::Goto,
            hint: String::new(),
            help: "Open the session buffer list. Type to filter.".into(),
            options: Vec::new(),
            ext: None,
        },
        PaletteItem {
            id: "bn".into(),
            label: "bn".into(),
            kind: CmdKind::Run,
            hint: String::new(),
            help: "Cycle to the next session.".into(),
            options: Vec::new(),
            ext: None,
        },
        PaletteItem {
            id: "bp".into(),
            label: "bp".into(),
            kind: CmdKind::Run,
            hint: String::new(),
            help: "Cycle to the previous session.".into(),
            options: Vec::new(),
            ext: None,
        },
        PaletteItem {
            id: "new-session".into(),
            label: "new-session".into(),
            kind: CmdKind::Run,
            hint: String::new(),
            help: "Start a new session. Opens the name input.".into(),
            options: Vec::new(),
            ext: None,
        },
        PaletteItem {
            id: "edit-queue".into(),
            label: "edit-queue".into(),
            kind: CmdKind::Run,
            hint: String::new(),
            help: "Recall all pending user messages into the editor for editing.".into(),
            options: Vec::new(),
            ext: None,
        },
        PaletteItem {
            id: "e".into(),
            label: "e".into(),
            kind: CmdKind::Run,
            hint: "Ctrl+E".into(),
            help: "Open the external editor on the draft.".into(),
            options: Vec::new(),
            ext: None,
        },
        PaletteItem {
            id: "q".into(),
            label: "q".into(),
            kind: CmdKind::Run,
            hint: String::new(),
            help: "Quit the TUI. Loops keep running.".into(),
            options: Vec::new(),
            ext: None,
        },
    ]
}

/// Convert an extension's raw commands into palette items.
///
/// Each `ExtCommand` becomes a `PaletteItem` with `kind = Ext` and
/// `ext` set to the owning extension name. Settings (commands with
/// `options`) get non-empty `options` so the preview pane shows the
/// picker.
pub fn from_extension(ext_name: &str, cmds: &[ExtCommand]) -> Vec<PaletteItem> {
    cmds.iter()
        .map(|c| PaletteItem {
            id: format!("{}.{}", ext_name, c.id),
            label: c.label.clone(),
            kind: CmdKind::Ext,
            hint: c.hint.clone(),
            help: c.help.clone(),
            options: if c.options.is_empty() {
                Vec::new()
            } else {
                c.options
                    .iter()
                    .map(|o| CmdOption {
                        value: o.clone(),
                        current: false,
                    })
                    .collect()
            },
            ext: Some(ext_name.to_string()),
        })
        .collect()
}

// ── tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_count() {
        let items = builtins("medium");
        assert_eq!(items.len(), 11);
    }

    #[test]
    fn builtins_ids() {
        let items = builtins("high");
        let ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
        assert!(ids.contains(&"toggle-tools"));
        assert!(ids.contains(&"toggle-thinking"));
        assert!(ids.contains(&"expand-thinking"));
        assert!(ids.contains(&"thinking-level"));
        assert!(ids.contains(&"b"));
        assert!(ids.contains(&"bn"));
        assert!(ids.contains(&"bp"));
        assert!(ids.contains(&"new-session"));
        assert!(ids.contains(&"edit-queue"));
        assert!(ids.contains(&"e"));
        assert!(ids.contains(&"q"));
    }

    #[test]
    fn thinking_level_item_marks_current() {
        let items = builtins("high");
        let item = items.iter().find(|i| i.id == "thinking-level").unwrap();
        assert_eq!(item.kind, CmdKind::Set);
        assert_eq!(item.options.len(), 7);
        let current = item
            .options
            .iter()
            .find(|o| o.current)
            .expect("exactly one current");
        assert_eq!(current.value, "high");
    }

    #[test]
    fn thinking_level_item_hint_shows_current() {
        let items = builtins("xhigh");
        let item = items.iter().find(|i| i.id == "thinking-level").unwrap();
        assert_eq!(item.hint, "xhigh");
    }

    #[test]
    fn from_extension_builds_items() {
        let cmds = vec![
            ExtCommand {
                id: "reload".into(),
                label: "Reload myext".into(),
                is_setting: false,
                hint: String::new(),
                help: "Reload the extension".into(),
                options: Vec::new(),
            },
            ExtCommand {
                id: "theme".into(),
                label: "Theme".into(),
                is_setting: true,
                hint: "dark".into(),
                help: "Set the theme".into(),
                options: vec!["light".into(), "dark".into(), "auto".into()],
            },
        ];
        let items = from_extension("myext", &cmds);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "myext.reload");
        assert_eq!(items[0].kind, CmdKind::Ext);
        assert_eq!(items[0].ext.as_deref(), Some("myext"));
        assert!(items[0].options.is_empty());
        assert_eq!(items[1].id, "myext.theme");
        assert_eq!(items[1].options.len(), 3);
        assert!(items[1].options.iter().all(|o| !o.current));
    }
}
