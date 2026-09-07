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
      highlighting and rendering for `content`. Shipped: the
      `Read` body drives the shared `CodeHighlighter` from
      `bin/tui/src/highlight.rs` (the same engine the picker
      preview pane uses, `docs/tui-file-picker.md` section 9). The
      fix (2026-09-05) threads the tool call arguments into
      `read_body` so the file path — and therefore the language —
      resolves even though the log stores the path in the call
      argument (`file_path`), not the result value.

## New requests (2026-08-29)

- [x] Truncate `Read` and `Write` tool results. Show `Edit`
      results as a diff. Shipped: the `pi-tool-display` content
      and style port (2026-09-02 pass). Detail:
      `docs/tui-tool-result-truncation.md`.
- [x] Stop the abuse of one gray text color across the UI.
      Show `Read` content with syntax highlighting. Shipped:
      the built-in palette dropped the single gray (commit
      f652b89); the reference renderers and the `Read`
      highlighting shipped in the 2026-09-02 pass. Detail:
      `docs/tui-color-tones.md`.
- [x] The statusline extension should draw a powerline footer.
      Detail: `docs/tui-statusline-powerline.md`.
- [x] Display and control the model's thinking (reasoning)
      block. Shipped: the capture into the log (commit 61cde02);
      the render, the toggles, and the effort control in the
      2026-09-02 pass. Detail: `docs/tui-thinking-block.md`.
- [x] Fix the transient `[malformed log line]` flash on live
      loops. Detail: `docs/tui-malformed-line-flash.md`.

## Pending corrections (2026-08-29)

- [x] Rescope the "never truncate" comment to user and assistant
      messages. Six spots, one per file. Shipped in the
      2026-09-02 pass. Detail:
      `docs/tui-tool-result-truncation.md` (section 4).

## New requests (2026-08-31)

- [x] Show user messages that wait for the busy loop. Render
      them like `pi`'s `steering` and `follow-ups`. Shipped:
      stage 1, the TUI steering block (commit fc51f71); stage 2,
      the loop-side split, in the 2026-09-02 pass. Detail:
      `docs/tui-pending-user-messages.md`.
- [x] Show thinking content behind a toggle, like `pi`.
      Shipped with the thinking block in the 2026-09-02 pass
      (`Ctrl+T` show/hide, `Ctrl+X` collapse/expand). Detail:
      `docs/tui-thinking-block.md`.

## New requests (2026-09-01)

- [x] Show the loop phase while the loop runs. Detail:
      `docs/tui-model-wait-indicator.md`.

## New requests (2026-09-02)

- [x] Full port of `pi-tool-display` for the tool result style:
      the lighter box, the fold/expand control. Shipped in the
      2026-09-02 pass. Detail: `docs/tui-tool-display-port.md`.
- [x] Render the markdown in user and assistant messages
      without the syntax markers. `|` tables draw as proper
      grid tables. Shipped in the 2026-09-02 pass. Detail:
      `docs/tui-markdown-render.md`.
- [x] The TUI colors accept a custom scheme. Use `catppuccin
macchiato` as the first internal color scheme. Shipped in
      the 2026-09-03 pass. Detail:
      `docs/tui-color-scheme.md`.
- [x] Truncate the tool result box overflow, never wrap it.
      Shipped in the 2026-09-02 pass: the cut marks the overflow
      with a trailing ellipsis. Detail:
      `docs/tui-tool-result-truncation.md` (section 5).
- [x] Wrap the bash command text in the box on a narrow
      terminal. Shipped in the 2026-09-02 pass: the `$`
      command line word-wraps to the pane width; the output
      lines keep the truncation. Detail:
      `docs/tui-tool-result-truncation.md` (section 5).
- [x] Draw the `|` tables inside the thinking block as a
      fixed-width grid, like the message tables. Shipped in the
      2026-09-02 pass (`wrap_thinking` in `render.rs`).
- [x] Keep the message table columns at a fixed width. Shipped in
      the 2026-09-02 pass: `table_grid` pads every cell to the
      column width.

## New requests (2026-09-03)

- [x] A scrolling bar at the right edge of the transcript
      pane. It shows the view position over the whole log.
      It appears when the user begins to scroll away from the
      tail, and it stays up in the browsing mode. Highest UX
      priority with the `gg`/`G` jump below: in the current
      TUI a scroll or `Ctrl+U/D` loses the position sense, and
      the walk back to the latest position is confusing and
      tiring. Shipped in the 2026-09-03 pass: the one-column
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
      `gg`/`G`. Shipped in the 2026-09-03 pass: the mode, the
      gutter, the counts, the `:N` goto, and the stage-2 regex
      search with the highlight and the `N` view restore. Detail:
      `docs/tui-conversation-browsing.md`.
- [ ] Select-and-yank in the browse mode. The browse mode is
      the natural fit for vim `Visual` mode: select text on
      the rendered transcript and yank it to a register. The
      yanked text is pasteable into the draft (`p` in the
      editor) so the user can quote anything from the
      conversation to ask the agent about it. The `y`
      operator cooperates with the existing browse motions:
      `yw` (word), `y$` (line end), `yG` (last line),
      `<n>yy` (n lines), and the `i` / `a` text objects
      (inside double quotes, single quotes, parentheses,
      square brackets, braces, angle brackets). No new
      dependency: the `vim_editor.rs` motion and text-object
      primitives are reused. The register store is shared
      between the editor and the browse overlay. Detail:
      `docs/tui-conversation-browsing.md` (section 11).
- [x] A file picker on the `@` trigger. The user types `@` in
      the input box. A candidate file list opens. It re-ranks as
      the user types. The picked path inserts into the draft.
      Fuzzy search is on from day 0. The candidate window is a
      reusable completion widget, not a one-off. The display is the
      floating spawned window, chosen for the file content preview
      pane. The preview saves opening the target in a second tmux
      pane or shell session. The float adapts to the terminal width:
      the preview sits on the right in a wide terminal and on the
      bottom in a narrow one. Spec and layout decision committed
      (`docs/tui-file-picker.md`, commit 5b6ac59). Implemented
      2026-09-04: the day-0 scope ships in `bin/tui/src/picker/`
      plus the `@` trigger in `app.rs`. Detail:
      `docs/tui-file-picker.md`. Library research:
      `docs/tui-file-picker-research.md`.

## New requests (2026-09-04)

- [x] Syntax-highlight the picker preview pane and share the
      highlighter with tool-result rendering (follow-up flagged in
      `docs/tui-file-picker.md` section 9: "Code highlight is a later
      add"). Implemented 2026-09-04: `bin/tui/src/highlight.rs` now
      exposes a reusable `CodeHighlighter` plus `language_from_path`
      and a one-shot `highlight_text_lines` entry point. The picker
      preview pane (`picker/preview.rs`) and the `Read` tool-result
      body (`tool_display.rs` `read_body`) both drive it through the
      shared `Palette`/`Role` system. Unknown languages and binary
      files render plain. No new dependencies: a hand-rolled
      per-language tokenizer. Tree-sitter was evaluated and deferred:
      C-FFI grammar builds are disproportionate for a preview pane and
      an inline tool-result body. Revisit if highlight quality
      demands it.
- [x] Redesign session navigation. The `Tab` / `Shift+Tab`
      bindings were freed from unconditional session cycling so
      they can serve the file picker (path completion). The
      `CycleSessions` action and `cycle_target` helper remain in
      `app.rs` as the seam for a redesigned session navigator.
      The new design should: (1) not hijack keys the user expects
      for text editing or picker completion; (2) support listing
      all sessions, not just next/prev; (3) be reachable in one
      key press. The design now lives in the `:` command palette:
      `:b` opens a fuzzy session list, `:bn` / `:bp` cycle next
      and previous. Shipped with the `:` command palette in the
      2026-09-05 pass (commit 156c01e): `:b` opens the fuzzy
      session list, `:bn` / `:bp` cycle next and previous.
      Detail: `docs/tui-command-palette.md` (section 7);
      `bin/tui/src/app.rs` `Action::CycleSessions`.
- [x] A `:` command palette in normal mode. The user types `:`
      in normal mode. A floating two-pane window opens: the left
      pane lists commands and settings with fuzzy filtering, the
      right pane shows help text, option pickers, and session
      metadata. Built-in commands cover toggles, the effort
      setter, session buffers (`b`, `bn`, `bp`), `new-session`,
      `edit-queue`, open editor, and quit. Extension-provided
      commands join the list through a new `commands` cap and
      `invoke` op on the extension protocol. The window reuses
      the `picker/` fuzzy ranker and floating layout. Shipped in
      the 2026-09-05 pass (commit 156c01e): the two-pane float
      on the `picker/` ranker, the built-ins (`toggle-tools`,
      `toggle-thinking`, `expand-thinking`, `thinking-level`,
      `b`/`bn`/`bp`, `new-session`, `edit-queue`, `e`, `q`),
      and the extension `commands` cap with the `invoke` op
      (`ext.rs` `command_items`). Detail:
      `docs/tui-command-palette.md`.
- [x] Recall and edit pending user messages. `Alt + Up` pulls
      every pending message into the editor in one shot. The
      user edits the combined text and sends it. The log stays
      append-only: a new `user_message_retract` event cancels the
      originals, and the loop skips retracted ids. `:edit-queue`
      in the `:` palette is the deliberate entry point for the
      same flow. Shipped in the 2026-09-05 pass (commit
      156c01e): `Alt+Up` and `:edit-queue` both run
      `Action::RecallQueue`, which pulls the pending queue into
      the editor and emits the `user_message_retract` events;
      `pending_user_messages` filters the retracted ids so the
      loop skips them. Detail:
      `docs/user-message-editing.md`.

## New requests (2026-09-06)

- [ ] Accept `ctrl+z` the standard keybinding that send our tui to
      background jobs in the shell.
- [ ] The current markdown table rendering of the messages cannot correctly
      distinguish if `|` is used as the table column marker or written as part of the
      text or code, e.g. the closure syntax in Rust `.map(|e| ...)`/`.unwrap_or(|e| ...)`
- [x] Stream rendering of the model response. Shipped 2026-09-13: the
      `harness` loop owns `sessions/<n>/.model-stream`, the `model` binary writes one
      JSON line per SSE delta, and the TUI polls the file each frame to render a live
      block that settles into the transcript on the `assistant_message` event.
      See `docs/tui-streaming-response.md` and `lean/TuiStreamSpec.lean`.
      2026-09-06 audit: the TUI and harness consumer paths checked out; the single
      defect was producer-side — `bin/model` buffered the whole SSE body before
      writing any channel line, so the live block showed nothing while the response
      was in flight. Fixed with the incremental `SseParser` in
      `bin/model/src/main.rs`; in-flight growth is proven by
      `bin/model/tests/stream_channel.rs` and re-verified against the live endpoint.
      Lean spec clean (`scripts/lean-gate.sh`, zero `sorry`).
- [x] The thinking→text transition flickered: when the response text
      began, the thinking block collapsed from its full grown height to a
      fixed 2-line summary, so the block shrunk ~16 rows and the view
      jumped. Shipped 2026-09-13: the thinking tail and response text now
      share one sliding window (`stream_block_lines`, `bin/tui/src/render.rs`)
      — the oldest thinking rows slide out as text grows instead of a sudden
      collapse, so the block height never shrinks at the transition.
      Tests: `stream_block_thinking_and_text_share_the_window`,
      `stream_block_height_never_shrinks_when_text_starts`.
- [x] Buffer the response text and render it at a smooth, steady frame rate
      instead of jumping in whole-delta bursts. Shipped 2026-09-13: the TUI
      now queues arriving deltas into a FIFO pace queue and releases
      `max(1, backlog/15)` characters per frame (`App::pump_stream_pacing`,
      `bin/tui/src/app.rs`); the main loop ticks at ~60 FPS (16 ms poll)
      while a response is in flight and falls back to 100 ms when idle.
      `done` drains the queue in one shot so the final text settles promptly.
      Tests: `stream_pacing_*`, `clear_stream_drops_the_pace_queue`.
      Note: both changes are binary-side; the running TUI must be restarted
      (rebuild `target/release/tui`) to pick them up.
- [ ] Remove `assistant`, `user`, `tool:xxx` markers. Remove the indent of
      messages. Wrap the user message with the same box used for tool results.
- [ ] When in browse mode, updates from model response should not flush the
      screen to the latest position of the conversation.
- [x] The `@` picker respects `.gitignore` by default, but sometimes the
      human needs to point at ignored files or directories (e.g. a
      specific session in `@sessions`). Shipped 2026-09-06: `Ctrl+I`
      in the picker cycles the file scope — default (hidden and
      git-ignored excluded) → show git-ignored → also show hidden →
      back to default on the third press. `FileScope` in
      `bin/tui/src/picker/items.rs`, the state cycle in
      `bin/tui/src/picker/state.rs` (P9). In a standard terminal
      `Ctrl+I` is the same byte as `Tab` (0x09), so the picker binds
      `Tab` to the cycle too. Detail: `docs/tui-file-picker.md`.
- [x] When a file path in the `@` picker result list is too long to
      fit the column, trim the leading parent directory levels and
      replace them with `...` so the tail of the path stays visible
      (e.g. `.../a/b/src/app.rs`), with the preview pane on or off.
      Shipped 2026-09-06: `abbreviate_path` in
      `bin/tui/src/picker/render.rs` — the list column keeps the
      largest suffix of the path that fits the column budget, and
      the budget follows the column width, which differs with the
      preview pane on or off (P10 in `docs/tui-file-picker.md`).

## New requests (2026-09-07)

- [ ] Bug: `tool:edit` results always show `diff +0 -0`. Evidence session:
      `sessions/goal-ux-impl`
- [ ] `tool:edit` shows diff in vertica split when terminal is wide, horizontal
      split when terminal is narrow.
