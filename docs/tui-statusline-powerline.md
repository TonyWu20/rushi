# TUI statusline powerline footer

Status: shipped (2026-08-29 request, commit `9f9d4eb`). The
request lives in `docs/tui_feature_requests_from_human.md`
(2026-08-29 item).

## 1. Request

The statusline extension promised powerline icons. The current
TUI rendered none.

`starship-statusline.ts` (pi-config, via flake.nix) draws a
rounded powerline footer with Nerd Font glyphs `U+E0B4` /
`U+E0B6`. The harness reference
`ui_extensions/statusline/statusline.sh` is plain text. It has
no glyphs.

## 2. Shipped

The `statusline` extension (bash and the Rust port) now emits
a powerline footer. Each pill is a rounded segment: the left
cap is `U+E0B6`, the arrow between pills and the end cap are
`U+E0B4`, and every span carries its own hex colors (Catppuccin
Macchiato, the reference palette). The multi-span line shape
is documented in `docs/ui-extension.md` section 4. A row that
overflows the terminal drops its lowest-priority pills.

## Properties

Lean-style invariants for this spec (see `lean-driven-development.md`).
One property per non-trivial invariant. Each property is observable:
given an input, an output guarantee.

P1. powerline-shape: given a statusline footer with one or more
    pills, observe each pill renders as a rounded segment: the left
    cap is `U+E0B6`, the arrow between pills and the end cap are
    `U+E0B4`.
P2. per-span-color: given a powerline footer with multiple spans,
    observe every span carries its own hex colors from the
    Catppuccin Macchiato reference palette.
P3. overflow-drop: given a footer row that overflows the terminal
    width, observe the row drops its lowest-priority pills.

## Verification

Each property maps to its proof. `proven` means the cited test or
script exists and passes. `open` names the blocker and what unblocks
it.

| P# | Property | Proof | Status |
|----|----------|-------|--------|
| P1 | powerline-shape | `ext_statusline_real`, `ext_rus` in `scripts/tui-pty-smoke.py` (the powerline footer renders with pills under both the bash reference and the Rust port) | proven |
| P2 | per-span-color | Blocked: no test inspects the per-span SGR hex codes. Unblock with a pty SGR-classification check (as in `scripts/capture-thinking-border.py`) that asserts the per-span hex colors | open |
| P3 | overflow-drop | Blocked: the 80-column pty scenarios in `scripts/tui-pty-smoke.py` do not reach the drop case because the pills fit. Unblock with a pty scenario narrower than the combined pill width that asserts the model pill drops | open |

## Gate

The acceptance commands. All must exit 0 for this spec to be proven.

```
cargo build
cargo test
python3 scripts/tui-pty-smoke.py
```
