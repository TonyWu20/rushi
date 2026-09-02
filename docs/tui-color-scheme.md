# TUI color scheme

Status: shipped (2026-09-03 pass). The request lives in
`docs/tui_feature_requests_from_human.md` (2026-09-02 item).
See section 6 for what shipped.

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

## 6. Shipped (2026-09-03 pass)

The design questions of section 4, answered in
`bin/tui/src/color.rs` and `bin/tui/src/config.rs`:

- **The role list**: the 28-role `Role` enum. The three built-in
  tones, the markdown and JSON highlight styles, the thinking
  block, the fold/expand hint, the error and success accents, the
  five thinking-level border colors, the built-in status row, and
  the tool box background.
- **The custom scheme format**: a config table, not an extension
  payload. A `[tui.custom_schemes.<name>]` table maps role names
  to `#rgb` or `#rrggbb` hex. The values lower to the active
  capability level, as the extension hex wire colors do. A
  partial table overlays the built-in palette: an unset role
  keeps its current value. An unknown role name is a hard error
  at load.
- **The scheme switch point**: config load. The user names the
  scheme in `[tui] color_scheme`. The first internal scheme is
  `catppuccin-macchiato`, the built-in value of the 2026-08-29
  palette. Absent, the built-in tones stand.
- The thinking-level border colors join the palette as roles
  (`Border0` to `Border4`), so a scheme recolors the border
  too.
