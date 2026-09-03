# TUI feature requests

Slim index. One line per request. Each item links its detail
doc. The checkbox tracks the request state: open, partial, or
shipped. Detail, root cause, spec, and shipped notes live in
the linked doc.

## Original requests

- [x] The `content` field in the `events.jsonl` must be shown in
      full. Never truncate or fold.
- [x] Use `Ctrl + u/d` for scrolling, like vim.
- [x] Mouse scroll support. Shipped: was laggy, uncontrollable,
      and hung the process. A double `q` could not exit.
- [x] Syntax highlighting for tool results. Markdown
      highlighting and rendering for `content`.

## New requests (2026-08-29)

- [x] Truncate `Read` and `Write` tool results. Show `Edit`
      results as a diff. Shipped: the `pi-tool-display` content
      and style port (2026-09-03 pass). Detail:
      `docs/tui-tool-result-truncation.md`.
- [x] Stop the abuse of one gray text color across the UI.
      Show `Read` content with syntax highlighting. Shipped:
      the built-in palette dropped the single gray (commit
      f652b89); the reference renderers and the `Read`
      highlighting shipped in the 2026-09-03 pass. Detail:
      `docs/tui-color-tones.md`.
- [x] The statusline extension should draw a powerline footer.
      Detail: `docs/tui-statusline-powerline.md`.
- [x] Display and control the model's thinking (reasoning)
      block. Shipped: the capture into the log (commit 61cde02);
      the render, the toggles, and the effort control in the
      2026-09-03 pass. Detail: `docs/tui-thinking-block.md`.
- [x] Fix the transient `[malformed log line]` flash on live
      loops. Detail: `docs/tui-malformed-line-flash.md`.

## New requests (2026-08-31)

- [x] Show user messages that wait for the busy loop. Render
      them like `pi`'s `steering` and `follow-ups`. Shipped:
      stage 1, the TUI steering block (commit fc51f71); stage 2,
      the loop-side split, in the 2026-09-03 pass. Detail:
      `docs/tui-pending-user-messages.md`.
- [x] Show thinking content behind a toggle, like `pi`.
      Shipped with the thinking block in the 2026-09-03 pass
      (`Ctrl+T` show/hide, `Ctrl+X` collapse/expand). Detail:
      `docs/tui-thinking-block.md`.

## New requests (2026-09-01)

- [x] Show the loop phase while the loop runs. Detail:
      `docs/tui-model-wait-indicator.md`.

## New requests (2026-09-02)

- [x] Full port of `pi-tool-display` for the tool result style:
      the lighter box, the fold/expand control. Shipped in the
      2026-09-03 pass. Detail: `docs/tui-tool-display-port.md`.
- [x] Render the markdown in user and assistant messages
      without the syntax markers. `|` tables draw as proper
      grid tables. Shipped in the 2026-09-03 pass. Detail:
      `docs/tui-markdown-render.md`.
- [x] The TUI colors accept a custom scheme. Use `catppuccin
      macchiato` as the first internal color scheme. Shipped in
      the 2026-09-03 pass. Detail:
      `docs/tui-color-scheme.md`.

## Pending corrections (2026-08-29)

- [x] Rescope the "never truncate" comment to user and assistant
      messages. Six spots, one per file. Shipped in the
      2026-09-03 pass. Detail:
      `docs/tui-tool-result-truncation.md` (section 4).

## New requests (2026-09-04)

- [x] Truncate the tool result box overflow, never wrap it.
      Shipped in the 2026-09-04 pass: the cut marks the overflow
      with a trailing ellipsis. Detail:
      `docs/tui-tool-result-truncation.md` (section 5).
- [x] Wrap the bash command text in the box on a narrow
      terminal. Shipped in the 2026-09-04 pass: the `$`
      command line word-wraps to the pane width; the output
      lines keep the truncation. Detail:
      `docs/tui-tool-result-truncation.md` (section 5).
- [x] Draw the `|` tables inside the thinking block as a
      fixed-width grid, like the message tables. Shipped in the
      2026-09-04 pass (`wrap_thinking` in `render.rs`).
- [x] Keep the message table columns at a fixed width. Shipped in
      the 2026-09-04 pass: `table_grid` pads every cell to the
      column width.

## New requests (2026-09-05)

- [x] A scrolling bar at the right edge of the transcript
      pane. It shows the view position over the whole log.
      It appears when the user begins to scroll away from the
      tail, and it stays up in the browsing mode. Highest UX
      priority with the `gg`/`G` jump below: in the current
      TUI a scroll or `Ctrl+U/D` loses the position sense, and
      the walk back to the latest position is confusing and
      tiring. Shipped in the 2026-09-05 pass: the one-column
      bar shows on scroll-back and in browse mode, with the
      thumb, the tail marker, and the cursor marker. Detail:
      `docs/tui-conversation-browsing.md` (section 3).
- [x] A conversation browsing mode over the session log.
      Entry: a double `s`, under the same two conditions as
      the `q q` exit path: the input area is empty and the
      editor is in normal mode. `h j k l` move the cursor on
      the log, `Ctrl+U/D` move half a screen, `<count>j/k`
      move n lines, `<count>h/l` move n columns, `:N` (with
      the `j`/`k` suffix accepted) goes to line N, and
      `gg`/`G` go to the top and the end of the log. While
      active, the line-number gutter shows the absolute
      number at the cursor and relative numbers on the other
      lines. The detail behavior matches the neovim configured
      on this machine. Whether the mode takes a regex search
      on the log is discussed in the detail doc. The
      gutter-side question: left gutter, right bar (section
      4.3 of the detail doc). Highest priority: the bar plus
      `gg`/`G`. Shipped in the 2026-09-05 pass: the mode, the
      gutter, the counts, the `:N` goto, and the stage-2 regex
      search with the highlight and the `N` view restore. Detail:
      `docs/tui-conversation-browsing.md`.
