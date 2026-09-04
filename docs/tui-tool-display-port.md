# TUI tool display port

Status: shipped (2026-09-03 pass). The request lives in
`docs/tui_feature_requests_from_human.md` (2026-09-02 item).
See section 4 for what shipped and where.

## 1. Request

Full port of `pi-tool-display` for the tool result style. Wrap
every tool result in a lighter-colored box. Add the fold/expand
control, just like the extension.

Today: the built-in render (`bin/tui/src/render.rs`) and the
reference renderer (`ui_extensions-demos/tool_result/tool_result.sh`,
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

## 4. Shipped (2026-09-03 pass)

The port lives in `bin/tui/src/tool_display.rs` plus the
`ToolResult` render pass in `bin/tui/src/render.rs`:

- **Box**: each result sits in a rounded box with the box-state
  background role (`tool_box_bg_success` on success, the
  `tool_box_bg_error` role on failure; docs/tui-color-pi-
  alignment.md: the pi `toolSuccessBg` / `toolErrorBg` values),
  one cell of padding, the command header row with the exit
  status. The top border carries the `tool:<name>  <status>`
  title (the red bold accent on an error), so no separate header
  line sits above the box.
- **Call/result merge**: a bash tool_call whose result follows
  drops its own line: the result box body opens with the
  `$ <command>` line, so the separate call line would repeat the
  command. A call without a result yet keeps its line: the
  command is the only view of a running tool.
- **Fold**: long output collapses to the preview. The muted hint
  states the remainder and the key, `... (N more lines •
  Ctrl+O to expand)`.
- **Expand**: `Ctrl+O` toggles every collapsed block to the
  full output, capped at `expanded_max_lines` (4000).
- **Per-tool limits**: `preview_lines` 8 (read, search, the
  generic tool), `bash_collapsed_lines` 10, `diff_collapsed_lines`
  24. Output modes, the extension names: `hidden` / `summary` /
  `preview` for read and bash, `hidden` / `count` / `preview`
  for search. `hidden` shows no body; `summary` keeps the line or
  count summary line.
- **Presets**: `opencode`, `balanced`, `verbose`, the extension
  values. An override switches the effective preset to
  `custom`.
- **Config**: `[tui] tool_display` in `config.toml` (docs/
  tui.md section 13.2). The extension's settings modal is not
  built: the TUI config is the toml file, edited by the user or
  by the `Ctrl+L`-style in-place edits.
- **Diff layout**: `Edit` results render as a diff (before and
  after). `diff_view` `auto` switches split at
  `DIFF_SPLIT_MIN_WIDTH` (120 columns), `unified` and `split`
  force a layout. The added and removed lines color through the
  `diff_added` / `diff_removed` roles (the pi `toolDiffAdded` /
  `toolDiffRemoved` values, docs/tui-color-pi-alignment.md). The
  JSON and bash bodies syntax-highlight inside the box
  (docs/tui-color-tones.md section 4): the JSON tokens color
  through the `syntax*` roles, the pi scope mapping.

## Properties

Lean-style invariants for this spec (see `lean-driven-development.md`).

P1. result-box: given a finished tool result, observe a rounded box with the success or error background role, one cell of padding, and the `tool:<name> <status>` title on the top border.
P2. call-merge: given a bash `tool_call` whose result follows, observe the call line drop and the box open with the `$ <command>` line. A call with no result keeps its own line.
P3. fold: given a result longer than the tool preview cap, observe a collapsed preview with a muted `N more lines` hint naming `Ctrl+O`.
P4. expand: given a collapsed block, observe `Ctrl+O` open every block to the full output capped at `expanded_max_lines`.
P5. preset-override: given a per-tool override in `[tui.tool_display]`, observe the effective preset become `custom` and the named per-tool limits apply.

## Verification

| P# | Property | Proof | Status |
|----|----------|-------|--------|
| P1 | result-box | `box_rows_draw_the_rounded_panel` in `bin/tui/src/tool_display.rs`; `tool_result_box_carries_the_title_in_the_border` in `bin/tui/src/render.rs` | proven |
| P2 | call-merge | `bash_call_merges_into_the_result_box` in `bin/tui/src/render.rs` | proven |
| P3 | fold | `read_preview_folds_to_the_preview_lines` in `bin/tui/src/tool_display.rs`; `long_tool_result_folds_to_the_preview_cap` in `bin/tui/src/render.rs` | proven |
| P4 | expand | `expand_overrides_the_hidden_and_summary_modes` in `bin/tui/src/tool_display.rs` | proven |
| P5 | preset-override | `preset_and_mode_parsing`, `preset_value_tables` in `bin/tui/src/tool_display.rs` | proven |

## Gate

The acceptance commands. All must exit 0 for this spec to be proven.

```
cargo build
cargo test
```
