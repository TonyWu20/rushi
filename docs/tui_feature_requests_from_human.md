# TUI feature requests

- [x] `content` field in the `events.jsonl` must be displayed in full, never
      truncated or folded.
- [x] Use `Ctrl + u/d` for scrolling, like vim.
- [x] Support mouse scrolling: Currently, though not claimed supported, the tui can
      be scrolled by mouse, but it is very laggy and uncontrollable, and will hang
      the process: double `q` cannot exit, no response to other key inputs.
- [x] Syntax highlight for the tool results if possible, markdown syntax highlight
      and proper rendering for `content`.

## New requests (2026-08-29)

- [ ] Truncate `Read` and `Write` tool results. Show `Edit` results as a diff.
      How-to: `pi-tool-display` (github.com/MasuRii/pi-tool-display, v0.5.0,
      pinned rev `91cef758`). It installs via
      `~/programming/pi-config/flake.nix`.
      Behavior: compact read output (preview line count, output modes),
      adaptive edit and write diffs (split or unified layout), syntax
      highlighting inside the diff, width clamping on narrow panes,
      presets opencode / balanced / verbose.
      Today `ui_extensions/tool_result/tool_result.sh` renders the full
      body ("Nothing is truncated"). A `Read` spams the whole screen.
- [ ] Stop the abuse of one gray text color across the UI.
      Show `Read` content with syntax highlighting.
      Today the reference renderer paints every body line `darkgray`
      (`ui_extensions/tool_result/tool_result.sh`). The color map lives in
      `bin/tui/src/ext.rs`. The transcript makes no highlight effort.
      Reference: pi-tool-display highlights syntax inside the diff.
- [x] The statusline extension promised powerline icons.
      The current TUI rendered none.
      `starship-statusline.ts` (pi-config, via flake.nix) draws a rounded
      powerline footer with Nerd Font glyphs `U+E0B4` / `U+E0B6`.
      The harness reference `ui_extensions/statusline/statusline.sh`
      is plain text. It has no glyphs.
      Shipped: the `statusline` extension (bash and the Rust port) now
      emits a powerline footer. Each pill is a rounded segment: the
      left cap is `U+E0B6`, the arrow between pills and the end cap
      are `U+E0B4`, and every span carries its own hex colors
      (Catppuccin Macchiato, the reference palette). The multi-span
      line shape is documented in `docs/ui-extension.md` section 4.
      A row that overflows the terminal drops its lowest-priority
      pills.
- [ ] Display and control the model's thinking (reasoning) block.
      Today `bin/model` sends `reasoning: { effort }` to the API and
      the model can emit thinking. But `bin/parse` keeps only `text`,
      `tool_calls`, `stop_reason`, and `usage`. It drops the thinking
      content. Nothing reaches the log, so the TUI cannot show it.
      Needed: capture thinking from the model response into the log,
      render it as a collapsible dimmed block, and add controls to
      toggle it and set reasoning effort.
- [ ] Fix the transient `[malformed log line]` flash on live loops.
      Root cause: `read_events` (`bin/tui/src/port_file.rs`) reads the
      whole file and splits on newline. A read that races an in-flight
      append sees the last line without its trailing newline. That
      partial segment fails `Event::parse_line` and renders as
      malformed. The persisted line is clean; all lines validate.
      Fix: in `read_events`, when the read data does not end in a
      newline, drop the final segment as in progress. Show it on the
      next read once the append lands.

## New requests (2026-08-31)

- [ ] Show user messages that wait for the busy loop. Render them
      like `pi` renders `steering` and `follow-ups`.
      Today: while the loop is busy, the TUI still accepts input.
      Enter appends a `user_message` to the log and the loop picks
      it up at its next step. The TUI shows nothing about it.
      The user does not know if the input was acknowledged, or
      when the loop will process it.
      Needed: a pending list of unconsumed `user_message` events
      per session. Show the waiting messages, with a count, like
      `pi`'s `steering` (injected at the next step) and
      `follow-ups` (processed after the loop ends).
- [ ] Show thinking content behind a toggle, like `pi`.
      Extends the 2026-08-29 item "Display and control the model's
      thinking (reasoning) block". Beyond capturing thinking into
      the log and rendering it as a collapsible dimmed block, add
      a toggle that shows or hides thinking blocks. The toggle
      follows `pi`'s thinking display.

## Pending corrections (2026-08-29)

The "never truncate" rule in item 1 applies to the `content` field
of `user_message` and `assistant_message` events. Tool result
bodies may be truncated. The code encodes the wider rule. These
fixes are recorded here and left for later:

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
