# TUI text tones and the gray-abuse defect

Status: partial. The request lives in
`docs/tui_feature_requests_from_human.md` (2026-08-29 item).
The single-gray defect is fixed in the built-in palette. The
reference renderer and the `Read` highlighting stay open.

## 1. Request

Stop the abuse of one gray text color across the UI. Show
`Read` content with syntax highlighting.

Reference: `pi-tool-display` highlights syntax inside the
diff.

## 2. Today at request time

The reference renderer paints every body line `darkgray`
(`ui_extensions/tool_result/tool_result.sh`). The color map
lives in `bin/tui/src/ext.rs`. The transcript makes no
highlight effort.

## 3. Shipped so far

Commit `f652b89` ("Ship the [tui] color override and the
capability-aware tones"):

- The built-in palette drops the single gray. It uses three
  capability-aware tones: transcript prose (soft light gray),
  tool output (muted mauve), tool-call command text (light
  blue). Each tone quantizes to the 256 palette and falls back
  to a distinct 16-color swatch.
- A `[tui] color` setting forces the terminal capability level
  (truecolor, 256, 16, 8). Absent, detection from `COLORTERM`
  and `TERM` stands. Unknown names are a hard error at load.
- Extension hex wire colors lower to the capability level at
  storage time. The tick payload carries the level name, so
  extensions see what the TUI actually emits.

## 4. Still open

- The reference renderer still paints every body line
  `darkgray` (`ui_extensions/tool_result/tool_result.sh`, and
  the Rust port `ext-rs/tool_result-rs`).
- `Read` content has no syntax highlighting. The transcript
  highlights markdown and complete JSON documents only.
