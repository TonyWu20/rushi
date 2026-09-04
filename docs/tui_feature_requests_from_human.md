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

## New requests (2026-09-05, follow-up: select-and-yank)

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

## New requests (2026-09-08)

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
      2026-09-03: the day-0 scope ships in `bin/tui/src/picker/`
      plus the `@` trigger in `app.rs`. Detail:
      `docs/tui-file-picker.md`. Library research:
      `docs/tui-file-picker-research.md`.
- [x] Syntax-highlight the picker preview pane and share the
      highlighter with tool-result rendering (follow-up flagged in
      `docs/tui-file-picker.md` section 9: "Code highlight is a later
      add"). Implemented 2026-09-08: `bin/tui/src/highlight.rs` now
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
- [ ] Redesign session navigation. The `Tab` / `Shift+Tab`
      bindings were freed from unconditional session cycling so
      they can serve the file picker (path completion). The
      `CycleSessions` action and `cycle_target` helper remain in
      `app.rs` as the seam for a redesigned session navigator.
      The new design should: (1) not hijack keys the user expects
      for text editing or picker completion; (2) support listing
      all sessions, not just next/prev; (3) be reachable in one
      key press. The design now lives in the `:` command palette:
      `:b` opens a fuzzy session list, `:bn` / `:bp` cycle next
      and previous. Detail: `docs/tui-command-palette.md`
      (section 7); `bin/tui/src/app.rs` `Action::CycleSessions`.

## New requests (2026-09-11)

- [ ] A `:` command palette in normal mode. The user types `:`
      in normal mode. A floating two-pane window opens: the left
      pane lists commands and settings with fuzzy filtering, the
      right pane shows help text, option pickers, and session
      metadata. Built-in commands cover toggles, the effort
      setter, session buffers (`b`, `bn`, `bp`), `new-session`,
      `edit-queue`, open editor, and quit. Extension-provided
      commands join the list through a new `commands` cap and
      `invoke` op on the extension protocol. The window reuses
      the `picker/` fuzzy ranker and floating layout. Detail:
      `docs/tui-command-palette.md`.
- [ ] Recall and edit pending user messages. `Alt + Up` pulls
      every pending message into the editor in one shot. The
      user edits the combined text and sends it. The log stays
      append-only: a new `user_message_retract` event cancels the
      originals, and the loop skips retracted ids. `:edit-queue`
      in the `:` palette is the deliberate entry point for the
      same flow. Detail:
      `docs/user-message-editing.md`.
