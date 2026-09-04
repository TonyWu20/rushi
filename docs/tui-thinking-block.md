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

## Properties

Lean-style invariants for this spec (see `lean-driven-development.md`).
One property per non-trivial invariant. Each property is observable:
given an input, an output guarantee.

P1. reasoning-capture: given a model response that carries a
    reasoning item, observe the item (content, id, status, summary)
    is captured and the reasoning array is forwarded onto the
    `assistant_message`.
P2. reasoning-replay: given an `assistant_message` carrying
    reasoning items, observe `bin/assemble` replays the items into
    the next model request in the full form, and the compact form
    drops them.
P3. block-render: given an `assistant_message` with `reasoning`
    content, observe the TUI renders a collapsible thinking block
    above the message body in the thinking tone.
P4. block-entries: given reasoning entries that are typed
    `reasoning_text` and plain-text entries from older logs, observe
    the block renders both.
P5. toggle-controls: given the user presses Ctrl+T (collapse/expand),
    Ctrl+X (show/hide), or Ctrl+L (cycle effort), observe the block
    visibility and expansion state change and the input-border color
    follows the new level.

## Verification

Each property maps to its proof. `proven` means the cited test or
script exists and passes. `open` names the blocker and what unblocks
it.

| P# | Property | Proof | Status |
|----|----------|-------|--------|
| P1 | reasoning-capture | `chat_response_captures_reasoning_content`, `convert_to_chat_attaches_reasoning_content` in `bin/model/src/main.rs` | proven |
| P2 | reasoning-replay | `full_items_send_reasoning_item_verbatim`, `compact_items_drop_reasoning` in `bin/assemble/src/main.rs` | proven |
| P3 | block-render | `thinking_block_renders_above_the_assistant_message` in `bin/tui/src/render.rs` | proven |
| P4 | block-entries | `thinking_text_accepts_typed_and_typeless_entries` in `bin/tui/src/render.rs` | proven |
| P5 | toggle-controls | Blocked: no test drives the Ctrl+T / Ctrl+X / Ctrl+L key handlers. Unblock with a test that dispatches each key and asserts `thinking_expanded`, `thinking_shown`, and the config write-back | open |

## Gate

The acceptance commands. All must exit 0 for this spec to be proven.

```
cargo build
cargo test
```
