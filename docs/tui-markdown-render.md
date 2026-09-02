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
