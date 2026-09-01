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

- [ ] Truncate `Read` and `Write` tool results. Show `Edit`
      results as a diff. Detail: `docs/tui-tool-result-truncation.md`.
- [ ] Stop the abuse of one gray text color across the UI.
      Show `Read` content with syntax highlighting. Detail:
      `docs/tui-color-tones.md`.
- [x] The statusline extension should draw a powerline footer.
      Detail: `docs/tui-statusline-powerline.md`.
- [ ] Display and control the model's thinking (reasoning)
      block. Detail: `docs/tui-thinking-block.md`.
- [x] Fix the transient `[malformed log line]` flash on live
      loops. Detail: `docs/tui-malformed-line-flash.md`.

## New requests (2026-08-31)

- [ ] Show user messages that wait for the busy loop. Render
      them like `pi`'s `steering` and `follow-ups`. Detail:
      `docs/tui-pending-user-messages.md`.
- [ ] Show thinking content behind a toggle, like `pi`.
      Detail: `docs/tui-thinking-block.md`.

## New requests (2026-09-01)

- [x] Show the loop phase while the loop runs. Detail:
      `docs/tui-model-wait-indicator.md`.

## New requests (2026-09-02)

- [ ] Full port of `pi-tool-display` for the tool result style:
      the lighter box, the fold/expand control. Detail:
      `docs/tui-tool-display-port.md`.

## Pending corrections (2026-08-29)

- [ ] Rescope the "never truncate" comment to user and assistant
      messages. Six spots, one per file. Detail:
      `docs/tui-tool-result-truncation.md` (section 4).
