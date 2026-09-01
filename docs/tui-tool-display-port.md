# TUI tool display port

Status: open. The request lives in
`docs/tui_feature_requests_from_human.md` (2026-09-02 item).

## 1. Request

Full port of `pi-tool-display` for the tool result style. Wrap
every tool result in a lighter-colored box. Add the fold/expand
control, just like the extension.

Today: the built-in render (`bin/tui/src/render.rs`) and the
reference renderer (`ui_extensions/tool_result/tool_result.sh`,
Rust port `ext-rs/tool_result-rs`) paint the result body as
flat lines. No box. No fold. No expand.

Reference: `pi-tool-display` (github.com/MasuRii/pi-tool-display,
v0.5.0, pinned rev `91cef758`), installed via
`~/programming/pi-config/flake.nix`.

## 2. Parts to port

- Box: each result sits in a rounded box. A light background.
  One-cell padding. The extension renders its compact output
  inside that box.
- Fold: long output collapses to a preview. The preview shows
  the first lines only. A muted hint states the remainder and
  the key, like `... (173 more lines • Ctrl+O to expand)`.
- Expand: one global key toggles every collapsed block to the
  full output. Pi's key is `app.tools.expand`, default
  `Ctrl+O`. The expanded preview caps at
  `expandedPreviewMaxLines` (4000).
- Per-tool limits: `previewLines` 8 for read,
  `bashCollapsedLines` 10, `diffCollapsedLines` 24. Output
  modes: `hidden` / `summary` / `preview` for read and bash,
  `hidden` / `count` / `preview` for search. Presets
  `opencode`, `balanced`, `verbose`.
- Config: `config.json` plus a settings modal, like the
  extension.

## 3. Relation to the truncation request

The 2026-08-29 request
(`docs/tui-tool-result-truncation.md`) owns the content layer:
truncation of `Read` and `Write` results and the `Edit` diff.
This doc owns the style layer: the box, the fold/expand
control, and the remaining render behavior. The result is a
full port of the extension.
