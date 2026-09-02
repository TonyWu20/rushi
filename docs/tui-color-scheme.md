# TUI color scheme

Status: open. The request lives in
`docs/tui_feature_requests_from_human.md` (2026-09-02 item).

## 1. Request

The TUI colors accept a custom scheme. `catppuccin macchiato`
serves as the first internal color scheme. A user selects a
named scheme or supplies a custom one.

## 2. Today

`bin/tui/src/color.rs` holds the capability level: `truecolor`,
`256`, `16`, `8`. Detection reads `COLORTERM` and `TERM`. The
`[tui] color` config forces the level (`docs/tui.md` section
13.2). Three built-in tones lower to the level: transcript
prose, tool output, tool command. The `highlight` module adds
the markdown and JSON role styles. The thinking-level border
palette holds five fixed colors. The statusline extension
spans carry Catppuccin Macchiato hexes
(`docs/tui-statusline-powerline.md`). No scheme concept exists.
Every role holds a fixed color.

## 3. Parts

- Named internal schemes. The first is `catppuccin macchiato`.
      A scheme maps every color role to a hex value.
- Custom scheme input. The user supplies role-to-hex values.
      The values lower to the active capability level, as the
      extension hex wire colors do today.
- Selection. The user names the scheme in the config. The
      default keeps the current built-in palette.

## 4. Open design questions

- The role list: the three built-in tones, the highlight
      styles, the thinking-level border palette, the built-in
      statusline row.
- The custom scheme format: a config table, an extension
      payload, or both.
- The scheme switch point: config load, or a live key.

## 5. Relation to the tone request

The 2026-08-29 tone request (`docs/tui-color-tones.md`) fixed
the single-gray defect in the built-in palette. This request
generalizes the palette into selectable schemes. The
capability lowering applies to every scheme.
