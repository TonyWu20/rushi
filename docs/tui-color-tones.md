# TUI text tones and the gray-abuse defect

Status: shipped. The request lives in
`docs/tui_feature_requests_from_human.md` (2026-08-29 item).
The built-in palette shipped in commit f652b89. The reference
renderers and the `Read` highlighting shipped in the 2026-09-03
pass (section 4).

## 1. Request

Stop the abuse of one gray text color across the UI. Show
`Read` content with syntax highlighting.

Reference: `pi-tool-display` highlights syntax inside the
diff.

## 2. Today at request time

The reference renderer paints every body line `darkgray`
(`ui_extensions-demos/tool_result/tool_result.sh`). The color map
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

## 4. Shipped (2026-09-03 pass)

- The reference renderers stop the gray abuse. The body paints in
  one muted tone (`#8f92ac`, the ToolOutput tone of the built-in
  palette), not a single darkgray:
  `ui_extensions-demos/tool_result/tool_result.sh` and the Rust port
  `ext-rs/tool_result-rs`.
- A body that is a complete JSON document gets JSON syntax
  highlighting in both reference renderers: keys, strings,
  numbers, literals, null, punctuation, in the catppuccin-
  macchiato hex the host lowers to the capability level. The
  bash reference tokenizes with an awk walk; the Rust port
  tokenizes natively.
- `Read` content gets JSON syntax highlighting in the built-in
  render: the `Read` and unknown-tool results that parse as a
  complete JSON document highlight the tokens inside the tool
  box (docs/tui-tool-display-port.md, the style layer). General
  source-code highlighting stays out of scope: the port follows
  `pi-tool-display`, which highlights JSON, diff, and bash, not
  arbitrary languages.

## 5. Pi alignment (2026-09-05 pass)

The tone and token hexes rebase to the pi `catppuccin-macchiato`
theme values (docs/tui-color-pi-alignment.md): the reference
body tone moves from `#8f92ac` to the pi `toolOutput` value
(`#cad3f5`), and the JSON token hexes move to the pi `syntax*`
role values (`#cad3f5` keys, `#a6da95` strings, `#f5a97f`
numbers and literals, `#939ab7` punctuation).
