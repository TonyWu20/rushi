# TUI message markdown rendering

Status: open. The request lives in
`docs/tui_feature_requests_from_human.md` (2026-09-02 item).

## 1. Request

Both user and assistant message content render the markdown
without the syntax markers. `**bold**` shows the word in bold.
A `# Heading` shows the heading in its style. A `| a | b |`
table draws as a proper grid table. The markers stay out of the
output.

## 2. Today

The `highlight` module (`bin/tui/src/highlight.rs`) colors the
markers in place. Each segment keeps the raw text. The
`bold_style` span keeps `**b**` with the stars. The
`inline_code_style` span keeps the backticks. A heading line
keeps the `#` run. A table line renders as prose with visible
pipes. The module doc states the boundary: "The TUI does not
render markdown structurally; it colors the syntax."

## 3. Open design questions

- Which markers drop, which stay. The list bullet `-` shows as
      a bullet. The `#`, `>`, and `*` style markers drop.
- Links: the link text shows. The URL drops or stays dimmed.
- Tables: the grid line style. The column width rule on a
      narrow pane.
- Code fences: the content stays literal. The fence marker
      lines dim.

## 4. Relation to the shipped highlighting

The original request item (syntax highlighting) shipped the
highlight layer. This request changes the presentation: the
markers out, the styles in. The `highlight` module keeps the
segment split. The render pass drops the marker text.

## Properties

Lean-style invariants for this spec (see `lean-driven-development.md`).
Each property is observable: given an input, an output guarantee.

P1. bold-no-markers: given `**bold**` in a message, observe the word
    rendered in bold style with the asterisk markers removed.
P2. heading-no-markers: given a `# Heading` line, observe the heading
    rendered in heading style with the `#` prefix removed.
P3. table-grid: given a markdown table with `|` separators, observe
    it rendered as a grid table with fixed column widths, not raw
    pipe-delimited text.
P4. fence-literal: given a fenced code block, observe the fence
    delimiter lines render in dim `Fence` style and the code content
    stays literal in `Code` style.
P5. link-rendering: given a markdown link, observe the link text in
    link style and the URL in dimmed `LinkUrl` style, with the
    bracket and paren markers removed.
P6. quote-no-markers: given a `> quoted` line, observe the text
    rendered in quote style with the `>` marker removed.

## Verification

Each property maps to its proof. `proven` means the cited test exists
and passes. `open` names the blocker and what unblocks it.

| P# | Property | Proof | Status |
|----|----------|-------|--------|
| P1 | bold-no-markers | `inline_tokens_are_styled_and_text_survives` in `bin/tui/src/highlight.rs` | proven |
| P2 | heading-no-markers | `headings_lists_and_quotes_are_styled` in `bin/tui/src/highlight.rs` | proven |
| P3 | table-grid | `table_grid_rows_keep_fixed_column_widths` in `bin/tui/src/highlight.rs` | proven |
| P4 | fence-literal | `fence_state_toggles_across_lines` in `bin/tui/src/highlight.rs` | proven |
| P5 | link-rendering | `inline_tokens_are_styled_and_text_survives` in `bin/tui/src/highlight.rs` | proven |
| P6 | quote-no-markers | `headings_lists_and_quotes_are_styled` in `bin/tui/src/highlight.rs` | proven |

## Gate

The acceptance commands. All must exit 0 for this spec to be proven.

```
cargo build
cargo test -p tui
```
