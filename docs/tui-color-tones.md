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

## Properties

Lean-style invariants for this spec (see `lean-driven-development.md`).
Each property is observable: given an input, an output guarantee.

P1. three-tones: given a terminal at any capability level, observe the
    built-in palette paint three distinct tones: transcript prose,
    tool output, and tool-call command text.
P2. gray-elimination: given the built-in palette, observe no body text
    role paint a single shared darkgray; the three tones are visually
    distinct.
P3. capability-lowering: given a tone color and the active capability
    level, observe the tone quantize to that level's palette, with a
    distinct 16-color swatch at the 16-color level.
P4. color-override: given a `[tui] color` value, observe the TUI force
    that capability level; absent the setting, observe detection from
    `COLORTERM` and `TERM`.
P5. unknown-level-rejection: given an unknown `[tui] color` value,
    observe a hard error at config load.
P6. json-highlight: given a tool-result body that is a complete JSON
    document, observe the body tokens paint with the JSON syntax
    roles.

## Verification

Each property maps to its proof. `proven` means the cited test exists
and passes. `open` names the blocker and what unblocks it.

| P# | Property | Proof | Status |
|----|----------|-------|--------|
| P1 | three-tones | `plain_text_palette`, `tool_output_palette`, `tool_command_palette` in `bin/tui/src/color.rs` | proven |
| P2 | gray-elimination | `plain_text_palette`, `tool_output_palette`, `tool_command_palette` in `bin/tui/src/color.rs` | proven |
| P3 | capability-lowering | `lowering_quantizes_rgb_at_256`, `lowering_snaps_rgb_at_16`, `lowering_keeps_rgb_at_truecolor` in `bin/tui/src/color.rs` | proven |
| P4 | color-override | `cfg_override_table`, `detection_table` in `bin/tui/src/color.rs` | proven |
| P5 | unknown-level-rejection | open: no test asserts that an unknown `[tui] color` value is rejected. Add a `color_unknown_level_rejected` test in `bin/tui/src/config.rs` that writes a config with `color = "bogus"` and asserts the load error mentions unknown value. | open |
| P6 | json-highlight | `json_line_styles_keys_strings_numbers_literals`, `looks_like_json_only_accepts_complete_documents` in `bin/tui/src/highlight.rs` | proven |

## Gate

The acceptance commands. All must exit 0 for this spec to be proven.

```
cargo build
cargo test -p tui
```
