# TUI thinking block

Status: partial. The request lives in
`docs/tui_feature_requests_from_human.md` (2026-08-29 item,
extended by the 2026-08-31 item). The capture is shipped. The
render and the controls stay open.

## 1. Request (2026-08-29)

Display and control the model's thinking (reasoning) block.

Today `bin/model` sends `reasoning: { effort }` to the API and
the model can emit thinking. But `bin/parse` keeps only `text`,
`tool_calls`, `stop_reason`, and `usage`. It drops the thinking
content. Nothing reaches the log, so the TUI cannot show it.

Needed: capture thinking from the model response into the log,
render it as a collapsible dimmed block, and add controls to
toggle it and set reasoning effort.

## 2. Extension (2026-08-31)

Show thinking content behind a toggle, like `pi`. Beyond
capturing thinking into the log and rendering it as a
collapsible dimmed block, add a toggle that shows or hides
thinking blocks. The toggle follows `pi`'s thinking display.

## 3. Shipped so far: the capture

Commit `61cde02`:

- `bin/model` captures the reasoning item from the model
  response (content, encrypted_content, id, status, summary).
- `bin/parse` forwards the reasoning array onto
  `assistant_message`.
- `bin/assemble` replays the item into the next model request.
  The compact form drops the item.
- The schema accepts the field
  (`schemas/events/v1/assistant_message.json`).

## 4. Still open

- Render the thinking block in the TUI as a collapsible dimmed
  block.
- Add the show/hide toggle (the 2026-08-31 extension).
- Add the reasoning-effort control.
