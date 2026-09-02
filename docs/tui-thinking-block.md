# TUI thinking block

Status: shipped. The request lives in
`docs/tui_feature_requests_from_human.md` (2026-08-29 item,
extended by the 2026-08-31 item). The capture shipped in commit
`61cde02`. The render, the toggles, and the effort control
shipped in the 2026-09-03 pass.

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

## 4. Shipped (2026-09-03 pass)

- The TUI renders the thinking block: the `reasoning` content of
  an `assistant_message` shows above the message body. The block
  renders for the typed `reasoning_text` content entries and for
  the plain text entries of older logs. Collapsed, one label row;
  expanded, the full reasoning text in the lighter thinking tone
  (the pi `subtext1` color, not a dim gray).
- `Ctrl+T` collapses or expands the thinking blocks (the pi
  `app.thinking.toggle` keymap; the 2026-08-31 toggle extension).
  `Ctrl+X` shows or hides them entirely.
- `Ctrl+L` cycles the active model's `reasoning_effort` through
  `none, minimal, low, medium, high, xhigh, max`. The TUI edits
  `[model.<active>]` in `config.toml` in place, comments kept,
  and creates the table when absent. The input-border color
  follows the new level.
