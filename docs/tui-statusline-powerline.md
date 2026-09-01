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
