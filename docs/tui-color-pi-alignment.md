# TUI color scheme alignment with the pi TUI

Status: shipped (2026-09-05 pass). The request: make this TUI choose
the same color on every element that shares or is equivalent to an
element in the `pi` TUI, and apply the color scheme to syntax
highlighting the way pi does.

## 1. The two palettes compared, case by case

The comparison pairs. `pi` side: the pi `catppuccin-macchiato` theme
JSON (the user's active pi theme, `~/.pi/agent/settings.json`
`"theme": "catppuccin-macchiato"`, resolved from the theme lookup
`~/.pi/agent/themes`). `this TUI` side: the `catppuccin macchiato`
scheme table (`bin/tui/src/color.rs` `catppuccin_macchiato`), before
this pass and after.

| Element (this TUI → pi element) | pi role | pi macchiato | before (this TUI) | after (this TUI) |
|---|---|---|---|---|
| transcript prose / user text / input draft (`PlainText`) | `text` / `userMessageText` | `#cad3f5` | `#cdd6f4` (the mocha `text` var, not macchiato) | `#cad3f5` |
| tool output body (`ToolOutput`) | `toolOutput` | `#cad3f5` | `#8f92ac` (a free-form muted tone) | `#cad3f5` |
| tool call title / command line (`ToolCommand`) | `toolTitle` | `#c6a0f6` | `#89dceb` (the mocha `sky` var) | `#c6a0f6` |
| user label, list bullets, accent elements (`Accent`) | `accent` | `#c6a0f6` | the `user` label was a hard-coded 16-color `Cyan` | `#c6a0f6` |
| fenced-code body, unknown language (`Code`) | `mdCodeBlock` | `#cad3f5` | `#8bd5ca` | `#cad3f5` |
| fence marker line (`Fence`) | `mdCodeBlockBorder` | `#5b6078` | `#7dcfff` (a free-form hex) | `#5b6078` |
| markdown heading (`Heading`) | `mdHeading` | `#f5bde6` | `#babcfc` (a free-form hex) | `#f5bde6` |
| blockquote (`Quote`) | `mdQuote` | `#a5adcb` | `#7c7f96` (a free-form hex) | `#a5adcb` |
| list marker (`List`) | `mdListBullet` | `#c6a0f6` | `#fab387` (the mocha `peach` var) | `#c6a0f6` |
| inline code (`InlineCode`) | `mdCode` | `#91d7e3` | `#f0c674` (a free-form hex) | `#91d7e3` |
| link text (`Link`) | `mdLink` | `#8aadf4` | `#74c7ec` (the mocha `sapphire` var) | `#8aadf4` |
| link URL (`LinkUrl`) | `mdLinkUrl` | `#a5adcb` | `#7c7f96` | `#a5adcb` |
| thinking block content (`Thinking`) | `thinkingText` | `#b8c0e0` | `#b8c0e0` (already aligned) | `#b8c0e0` |
| fold/expand hint (`Hint`) | `muted` | `#a5adcb` | `#7c7f96` | `#a5adcb` |
| status/help row (`Status`) | `dim` | `#8087a2` | `#7c7f96` | `#8087a2` |
| error accent (`Error`) | `error` | `#ed8796` | `#ed8796` (aligned) | `#ed8796` |
| success accent (`Success`) | `success` | `#a6da95` | `#a6e3a1` (the mocha `green` var) | `#a6da95` |
| warning accent, approval banner (`Warning`, new role) | `warning` | `#eed49f` | a hard-coded 16-color `Yellow` | `#eed49f` |
| added diff line (`DiffAdded`, new role) | `toolDiffAdded` | `#a6da95` | the `Success` role (`#a6e3a1`) | `#a6da95` |
| removed diff line (`DiffRemoved`, new role) | `toolDiffRemoved` | `#ed8796` | the `Error` role (aligned) | `#ed8796` |
| diff context line (`DiffContext`, new role) | `toolDiffContext` | `#a5adcb` | no role | `#a5adcb` |
| input border, thinking 0 (`Border0`) | `thinkingOff` | `#8087a2` | `#7c7f96` | `#8087a2` |
| input border, thinking 1 (low) (`Border1`) | `thinkingLow` | `#8bd5ca` | `#74c7ec` | `#8bd5ca` |
| input border, thinking 2 (medium) (`Border2`) | `thinkingMedium` | `#a6da95` | `#89dceb` | `#a6da95` |
| input border, thinking 3 (high) (`Border3`) | `thinkingHigh` | `#eed49f` | `#a6e3a1` | `#eed49f` |
| input border, thinking 4+ (highest) (`Border4`) | `thinkingXhigh` | `#f5a97f` | `#f0c674` | `#f5a97f` |
| tool-result box, running (`ToolBoxBg`) | `toolPendingBg` | `#363a4f` | `#363a4f` (aligned) | `#363a4f` |
| tool-result box, success (`ToolBoxBgSuccess`, new role) | `toolSuccessBg` | `#363a4f` | no role (one box bg for all states) | `#363a4f` |
| tool-result box, error (`ToolBoxBgError`, new role) | `toolErrorBg` | `#363a4f` | no role | `#363a4f` |
| transient flash line (the `Copied`-style
  confirmations) | the pi flash line: a neutral
  inverted tone, no color role | — | a hard-coded 16-color `Cyan` | `Hint` (the pi `muted` value) |

The no-scheme default is now the `catppuccin macchiato` scheme
(the reference pi theme, the one the user runs). The pi built-in
`dark` theme stays as the fallback for an unset role in a user
scheme table (element by element: `text` `#d4d4d4`, `toolOutput`
`#808080`, `toolTitle` `#d4d4d4`, `mdCodeBlock` `#b5bd68`, the
three box backgrounds `#282832` / `#283228` / `#3c2828`, and so
on). Every value lowers to the active capability level, as before.

## 2. Syntax highlighting: how pi applies the scheme, and how this
TUI now does

pi colors language syntax with highlight.js. The active theme
supplies nine `syntax*` color roles. A static scope map
(`buildCliHighlightTheme`, pi `dist/modes/interactive/theme/
theme.js`) feeds the scope names to the role colors: `keyword` and
`name` to `syntaxKeyword`, `built_in` / `class` / `type` to
`syntaxType`, `literal` and `number` to `syntaxNumber`, `string` and
`regexp` to `syntaxString`, `comment` to `syntaxComment`,
`function` and `title` to `syntaxFunction`, `variable` / `params` /
`attr` to `syntaxVariable`, `operator` to `syntaxOperator`,
`punctuation` and `tag` to `syntaxPunctuation`. A fence with an
unknown language paints the whole block in one `mdCodeBlock` color,
no auto-detect.

JSON gets no dedicated pi roles. The highlight.js JSON grammar
classes a key as `attr` (hence `syntaxVariable`), a string value as
`string` (hence `syntaxString`), a number as `number` (hence
`syntaxNumber`), `true` / `false` / `null` as `literal` (hence
`syntaxNumber`, one color for the three literals), and the braces,
colons, commas and brackets as `punctuation` (hence
`syntaxPunctuation`).

`pi-tool-display` reuses the same `highlightCode` entry point for
its diff lines: the language comes from the file extension, the
token colors come from the active theme's `syntax*` roles, and the
diff row backgrounds tint the `toolSuccessBg` value (a 12% green or
red mix, a 26% inline-emphasis mix). No hex colors live in the
extension.

This TUI ports the structure, not the highlighter:

- The palette grows the nine `syntax*` roles (`SyntaxComment`
  through `SyntaxPunctuation`). A scheme table can carry the pi
  theme `syntax*` values verbatim.
- The JSON token walk (`bin/tui/src/highlight.rs` `json_line_p`)
  colors through the `syntax*` roles, the pi scope mapping: keys
  through `SyntaxVariable`, strings through `SyntaxString`, numbers
  and the three literals through `SyntaxNumber`, punctuation through
  `SyntaxPunctuation`. No extra modifiers (pi's token colors carry
  none; the old bold keys, bold literals and dim `null` drop).
- Fenced-code bodies keep the `Code` role (`mdCodeBlock`): the
  unknown-language fallback tone, the pi rule. Per-language token
  highlighting stays out of scope, as of
  docs/tui-color-tones.md section 4 (the port follows
  `pi-tool-display`, which highlights JSON, diff and bash, not
  arbitrary languages).
- The diff bodies color added lines through `DiffAdded` and removed
  lines through `DiffRemoved` (`bin/tui/src/tool_display.rs`), the
  pi `toolDiff*` roles. Context lines render no role yet: the
  `Edit` diff shows removed plus added lines only. `DiffContext`
  exists in the table for a scheme to set.

## 3. Where each element changed

- `bin/tui/src/color.rs`: the 38-role table. The `catppuccin
  macchiato` scheme carries the pi `catppuccin-macchiato` theme
  values, and is the no-scheme default (the reference pi theme the
  user runs). The pi `dark` theme is the fallback for an unset role
  in a user scheme table. The six `Json*` roles retire in favor of
  the nine `Syntax*` roles. `Warning`, `Accent`, `Diff*` and the two
  box-state backgrounds join the role list.
- `bin/tui/src/highlight.rs`: `json_line_p` colors through the
  `Syntax*` roles, no modifiers.
- `bin/tui/src/tool_display.rs`: diff lines through `DiffAdded` /
  `DiffRemoved`; the bash box command opener line through
  `ToolCommand` (the pi `toolTitle` tone), the output lines through
  `ToolOutput`; the search and generic bodies through `ToolOutput`. The box background picks the pi box-state
  role by result state (`box_bg` now takes the `err` flag).
- `bin/tui/src/render.rs`: the hard-coded 16-color swatches move to
  palette roles. The `user` label takes `Accent`, the `assistant`
  and `tool:` labels take `ToolCommand`, the approval and compaction
  lines take `Warning`, the error lines take `Error`, the status
  row, working row and placeholders take `Status`, the fold hint
  takes `Hint`, the approval banner background takes `Warning`, and
  the transient flash line takes `Hint` bold (the pi flash is a
  neutral inverted tone, not a hard-coded cyan).
- `ui_extensions/frame/frame.sh`: the thinking-level border table
  emits the pi macchiato `thinking*` hex values (`#8087a2`,
  `#8bd5ca`, `#a6da95`, `#eed49f`, `#f5a97f`), the same table as
  the scheme `border0`..`border4` roles and the reference statusline
  palette. The host lowers the hex at storage
  (`bin/tui/src/ext.rs` `lower_frame_spec`).
- `ui_extensions/statusline/statusline.sh`: unchanged. The powerline
  palette was already the macchiato palette: `base` `#24273a`,
  `surface0` `#363a4f`, `mauve` `#c6a0f6`, `surface1` `#494d64`,
  `text` `#cad3f5`, `mantle` `#1e2030`.
- `ui_extensions-demos/tool_result/tool_result.sh` and
  `ext-rs/tool_result-rs`: the body tone and the JSON token hexes
  move to the pi macchiato values (`#cad3f5` body, `#cad3f5` keys,
  `#a6da95` strings, `#f5a97f` numbers and literals, `#939ab7`
  punctuation).

## 4. The modifier difference

pi renders markdown emphasis with modifiers (bold, italic,
underline), and its token colors carry no extra modifiers. This TUI
keeps its marker-free presentation modifiers (bold headings,
underlined links, dimmed quotes, dimmed fold hints). The alignment
covers color; the modifiers stay as the presentation pass defines
them (docs/tui-markdown-render.md).

## 5. Verification

- `cargo test -p tui`: the role table, the scheme hexes and the
  lowering all assert the pi values (the `color::tests` module).
- The frame table and the statusline table read as the same
  `thinking*` and macchiato hexes as the scheme tables above.
- The no-scheme default asserts the macchiato values
  (`no_scheme_default_is_the_macchiato_scheme`); the pi `dark`
  fallback asserts its own values
  (`builtin_palette_mirrors_the_pi_dark_theme`).

## Properties

Lean-style invariants for this spec (see `lean-driven-development.md`).
Each property is observable: given an input, an output guarantee.

P1. macchiato-values: given the `catppuccin macchiato` scheme, observe
    its role table carry the pi `catppuccin-macchiato` theme values
    verbatim, role by role.
P2. default-scheme: given no named scheme in the config, observe the
    TUI load the `catppuccin macchiato` scheme as the default.
P3. pi-dark-fallback: given a user scheme table with an unset role,
    observe that role fall back to the pi built-in `dark` theme value.
P4. syntax-roles: given a complete JSON document body, observe keys
    paint through `SyntaxVariable`, strings through `SyntaxString`,
    numbers and literals through `SyntaxNumber`, punctuation through
    `SyntaxPunctuation`, with no extra modifiers.
P5. diff-roles: given an `Edit` tool diff body, observe added lines
    paint through `DiffAdded` and removed lines through
    `DiffRemoved`.
P6. code-block-fallback: given a fenced-code body with an unknown
    language, observe the block paint in one `Code` role color with no
    auto-detected token colors.

## Verification

Each property maps to its proof. `proven` means the cited test exists
and passes. `open` names the blocker and what unblocks it.

| P# | Property | Proof | Status |
|----|----------|-------|--------|
| P1 | macchiato-values | `macchiato_scheme_maps_every_role` in `bin/tui/src/color.rs` | proven |
| P2 | default-scheme | `no_scheme_default_is_the_macchiato_scheme` in `bin/tui/src/color.rs` | proven |
| P3 | pi-dark-fallback | `builtin_palette_mirrors_the_pi_dark_theme` in `bin/tui/src/color.rs` | proven |
| P4 | syntax-roles | `json_line_styles_keys_strings_numbers_literals`, `looks_like_json_only_accepts_complete_documents` in `bin/tui/src/highlight.rs` | proven |
| P5 | diff-roles | `edit_diff_shows_removed_and_added` in `bin/tui/src/tool_display.rs` | proven |
| P6 | code-block-fallback | `code_highlighter_unknown_lang_is_plain` in `bin/tui/src/highlight.rs` | proven |

## Gate

The acceptance commands. All must exit 0 for this spec to be proven.

```
cargo build
cargo test -p tui
```
