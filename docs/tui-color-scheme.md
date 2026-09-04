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
      default is the `catppuccin macchiato` scheme (the reference
      pi theme).

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

- **The role list**: the 38-role `Role` enum. The three built-in
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
  scheme in `[tui] color_scheme`. The default is the
  `catppuccin macchiato` scheme (the reference pi theme).
  Absent a named scheme, the TUI loads it.
- The thinking-level border colors join the palette as roles
  (`Border0` to `Border4`), so a scheme recolors the border
  too.

## 7. Pi alignment (2026-09-05 pass)

The role table rebases to the `pi` TUI element colors
(docs/tui-color-pi-alignment.md): the 28-role list grows to 38
(the nine `syntax*` roles replace the six `Json*` roles; `Accent`,
`Warning`, the three `Diff*` and the two box-state backgrounds
join the list). The `catppuccin macchiato` scheme carries the pi
`catppuccin-macchiato` theme JSON values verbatim, role by role, and
is the no-scheme default (the reference pi theme the user runs).
The pi built-in `dark` theme is the fallback for an unset role in a
user scheme table. Partial custom tables still overlay role by role.

## Properties

Lean-style invariants for this spec (see `lean-driven-development.md`).
Each property is observable: given an input, an output guarantee.

P1. role-completeness: given the built-in palette, observe every role in
    the 38-role `Role` enum maps to a hex value.
P2. capability-lowering: given a scheme hex value and an active
    capability level, observe the color lower to that level.
P3. custom-overlay: given a partial custom scheme table, observe unset
    roles keep the built-in value and set roles use the custom value.
P4. unknown-role-error: given a custom scheme table naming an unknown
    role, observe a hard error at config load.
P5. default-scheme: given no named scheme in the config, observe the TUI
    load the `catppuccin macchiato` scheme.
P6. named-scheme-select: given `[tui] color_scheme` naming a known
    scheme, observe that scheme's role table resolve at the active level.

## Verification

Each property maps to its proof. `proven` means the cited test exists
and passes. `open` names the blocker and what unblocks it.

| P# | Property | Proof | Status |
|----|----------|-------|--------|
| P1 | role-completeness | `builtin_role_table_is_complete` in `bin/tui/src/color.rs` | proven |
| P2 | capability-lowering | `lowering_quantizes_rgb_at_256`, `lowering_snaps_rgb_at_16` in `bin/tui/src/color.rs` | proven |
| P3 | custom-overlay | `custom_scheme_overlays_the_builtins` in `bin/tui/src/color.rs` | proven |
| P4 | unknown-role-error | `unknown_scheme_role_rejected` in `bin/tui/src/config.rs` | proven |
| P5 | default-scheme | `no_scheme_default_is_the_macchiato_scheme` in `bin/tui/src/color.rs` | proven |
| P6 | named-scheme-select | `color_scheme_selects_the_builtin_name` in `bin/tui/src/config.rs`, `named_scheme_resolves_at_the_level` in `bin/tui/src/color.rs` | proven |

## Gate

The acceptance commands. All must exit 0 for this spec to be proven.

```
cargo build
cargo test -p tui
```
