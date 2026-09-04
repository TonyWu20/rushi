# TUI tool result truncation

Status: shipped (2026-09-03 pass). The request lives in
`docs/tui_feature_requests_from_human.md` (2026-08-29 item).
The content-layer work shipped with the `pi-tool-display` port
(docs/tui-tool-display-port.md): the preview caps, the diff
layout, the fold/expand control, the presets, the config.

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

`ui_extensions-demos/tool_result/tool_result.sh` renders the full
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
the wider rule. These rescopes shipped in the 2026-09-03 pass:

- [x] `bin/tui/src/render.rs` module header: "Text content (user/
      assistant messages, tool output) ... never truncated or
      folded". Rescoped to user and assistant messages; tool
      result bodies fold at render time.
- [x] `bin/tui/src/render.rs` `TOOL_CALL_BODY_LINES` comment: "The
      `content` field and tool result text have no cap". Rescoped
      to the `content` field of user and assistant messages; tool
      result bodies fold at render time.
- [x] `bin/tui/src/render.rs` `result_text` doc comment: "Nothing is
      hidden ... (item 1: no truncation)". Rescoped: the
      extraction takes the whole value; the cap applies at render
      time.
- [x] `ui_extensions-demos/tool_result/tool_result.sh` header: "Nothing is
      truncated: the body is shown in full". Rescoped: the body is
      still shown in full (this reply protocol has no fold
      control); the no-truncation promise now covers the `content`
      field of user and assistant messages only.
- [x] `ext-rs/tool_result-rs/src/main.rs` header: same text.
      Rescoped as the bash reference.
- [x] `ui_extensions/README.md` tool_result row: "styled header
      plus the full body". Annotated with the truncation request
      above and the gray-abuse fix.

## 5. Box overflow and the bash command wrap (2026-09-04 pass)

Two user directives closed the box width model:

- [x] Overflow truncates, it never wraps. A body row that still
      overflows the box inner width cuts at the border. The cut
      marks the overflow with a trailing ellipsis (`box_rows`,
      one reserved column). The body content budget is the box
      inner width minus the left padding cell (`width - 3` in
      `render.rs`), so the panel fills instead of leaving dead
      columns.
- [x] The bash command line wraps on a narrow pane. The merged
      bash box opens with the `$ <command>` line. On a narrow
      terminal that line word-wraps to the pane width instead of
      truncating (`wrap_hard_line` in `tool_display.rs`, applied
      to the first body line of `bash_body`). The output lines
      keep the hard-line truncation above.

Verification: `wrap_hard_line_fits_and_wraps_and_splits`,
`bash_body_wraps_the_command_line_on_a_narrow_pane`, and
`box_rows_mark_the_truncated_overflow_with_an_ellipsis` in
`bin/tui/src/tool_display.rs`. A 60-col PTY run shows the
command on two box rows and the long output row with the
ellipsis at the border.

## Properties

Lean-style invariants for this spec (see `lean-driven-development.md`).

P1. content-never-truncated: given `user_message` or `assistant_message` content, observe the TUI display it in full with no cap.
P2. tool-result-folds: given a tool result body, observe it fold at render time to the tool preview cap while the message content field stays whole.
P3. overflow-truncates: given a body row wider than the box inner width, observe the row cut at the border with a trailing ellipsis, never wrap.
P4. command-wraps: given a merged bash box on a narrow pane, observe the `$ <command>` line word-wrap to the pane width while output lines keep hard-line truncation.

## Verification

| P# | Property | Proof | Status |
|----|----------|-------|--------|
| P1 | content-never-truncated | `event_body_is_displayed_in_full` in `bin/tui/src/render.rs` | proven |
| P2 | tool-result-folds | `long_tool_result_folds_to_the_preview_cap` in `bin/tui/src/render.rs` | proven |
| P3 | overflow-truncates | `box_rows_mark_the_truncated_overflow_with_an_ellipsis`, `box_rows_never_exceed_the_width` in `bin/tui/src/tool_display.rs` | proven |
| P4 | command-wraps | `bash_body_wraps_the_command_line_on_a_narrow_pane`, `wrap_hard_line_fits_and_wraps_and_splits` in `bin/tui/src/tool_display.rs` | proven |

## Gate

The acceptance commands. All must exit 0 for this spec to be proven.

```
cargo build
cargo test
```
