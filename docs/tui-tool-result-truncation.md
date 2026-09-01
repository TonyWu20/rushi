# TUI tool result truncation

Status: open. The request lives in
`docs/tui_feature_requests_from_human.md` (2026-08-29 item).

## 1. Request

Truncate `Read` and `Write` tool results. Show `Edit` results as
a diff.

How-to: port the `pi-tool-display` extension
(github.com/MasuRii/pi-tool-display, v0.5.0, pinned rev
`91cef758`). It installs via `~/programming/pi-config/flake.nix`.

Behavior to port:

- Compact read output: preview line count, output modes.
- Adaptive edit and write diffs: split or unified layout.
- Syntax highlighting inside the diff.
- Width clamping on narrow panes.
- Presets `opencode`, `balanced`, `verbose`.

## 2. Today

`ui_extensions/tool_result/tool_result.sh` renders the full
body. Its header says "Nothing is truncated". A `Read` spams
the whole screen. The Rust port (`ext-rs/tool_result-rs`)
repeats the same rule.

## 3. Relation to the style request

The 2026-09-02 request (`docs/tui-tool-display-port.md`) owns
the style layer: the lighter box, the fold/expand control. This
doc owns the content layer: what shows, how many lines, and
the diff layout. The two may ship together or apart.

## 4. Pending corrections (2026-08-29)

The "never truncate" rule in original request item 1 applies to
the `content` field of `user_message` and `assistant_message`
events. Tool result bodies may be truncated. The code encodes
the wider rule. These rescopes are recorded here and left for
later:

- [ ] `bin/tui/src/render.rs` module header: "Text content (user/
      assistant messages, tool output) ... never truncated or
      folded". Rescope to user and assistant messages.
- [ ] `bin/tui/src/render.rs` `TOOL_CALL_BODY_LINES` comment: "The
      `content` field and tool result text have no cap". Rescope.
- [ ] `bin/tui/src/render.rs` `result_text` doc comment: "Nothing is
      hidden ... (item 1: no truncation)". Rescope.
- [ ] `ui_extensions/tool_result/tool_result.sh` header: "Nothing is
      truncated: the body is shown in full". Rescope.
- [ ] `ext-rs/tool_result-rs/src/main.rs` header: same text.
      Rescope.
- [ ] `ui_extensions/README.md` tool_result row: "styled header
      plus the full body". Annotate the truncation request above.
