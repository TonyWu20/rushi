# User-message editing: queue recall and the retract event

Status: Spec, not yet built (2026-09-11). The request lives in
`docs/tui_feature_requests_from_human.md` (the 2026-09-11 item).
The `:edit-queue` palette entry is specified here. The palette
shell it plugs into is `docs/tui-command-palette.md`.

## 1. Request

Recall and edit pending user messages while the loop is busy.
Reference behavior is `pi`: `Alt + Up` pulls the queued message
back into the editor so the user can fix it before the model
consumes it. This covers both delivery queues: the steer queue
(inject at the next step) and the follow queue (run after the
loop stops), defined in `docs/tui-pending-user-messages.md`.

Two entry points share one mechanism:

- `Alt + Up` — the quick path. One keypress, no menu.
- `:edit-queue` — the `:edit-queue` command in the command
  palette, for the deliberate path.

## 2. Today (verified in code)

- Pending messages are derived from the log, not held in memory.
  `App::pending_user_messages` collects every `user_message` after
  the last `assistant_message`. `pending_steering` and
  `pending_follows` split that set on the `queue` field
  (`app.rs`).
- A message is "consumed" only when an `assistant_message` follows
  it in the log. Until then it is pending.
- The pending blocks render above the input box
  (`pending_message_lines` in `render.rs`). The user sees the count
  and previews but cannot touch the queued text.
- The log is append-only JSONL. A sent `user_message` is a fact.
  The loop reads these lines on the next step.
- There is no retraction mechanism today. Once sent, a message
  stays and the loop consumes it as-is.

## 3. The core tension and its resolution

The log is the single source of truth and is append-only. That
rule exists for cache hit rate and audit: the model already saw
the past, so the past is never rewritten in the prompt. The
pending queue, however, has not been seen by the model yet. It is
not a fact. It is editable.

Resolution: editing a pending message is not a rewrite of history.
It is a new, later fact that cancels the earlier one. We append a
retract marker and a corrected message. The log grows. Nothing is
deleted or rewritten. The append-only invariant holds. The audit
trail shows both the original and the correction, which is
strictly more useful than a silent in-place edit.

## 4. New event type: `user_message_retract`

```json
{
  "v": 1,
  "type": "user_message_retract",
  "ts": "...",
  "target": "<the retracted message id>",
  "reason": "user_edit"
}
```

- `target` is the `id` of the `user_message` being retracted.
- `reason` records why. `user_edit` is the only v1 value.
- The retracted `user_message` stays in the log. It is marked, not
  deleted.

Schema files to add under `schemas/events/v1/`:

- `user_message_retract.json` — required `v`, `type`, `ts`,
  `target`; optional `reason`.
- Update `user_message.json` to add an optional `id` field (string,
  recommended UUID v4). Existing logs without `id` remain valid.

`docs/refinement-policy.md` P1b applies: additive change, no `v`
bump, old logs stay valid. P1c defines done: a schema exists, a
producer test appends it, a consumer test reads it, and a replay
test proves an old session still renders and reduces.

## 5. The retract-and-resend flow

The mechanism is identical for both entry points.

1. The TUI collects every pending user message (steer and follow,
   in log order).
2. The TUI appends one `user_message_retract` per message. All
   retracts land before the replacement so the log order is
   consistent.
3. The TUI joins the retracted messages' text into the editor,
   one block per original message, in log order, separated by a
   blank line. The queue label is preserved per block so the user
   sees which block was steer and which was follow.
4. The user edits in the editor.
5. `Enter` sends the edited text. The TUI appends one
   `user_message` (or one per queue, if the user kept the
   original queue split). The retracted originals are now
   cancelled. The pending list re-derives to the new message.
6. `Esc` or no send: the retracts stand, the originals are
   cancelled, and the pending list is empty. The user loses the
   queued text from the queue but it is still in the log.

Bulk recall is the only v1 behavior. There is no per-message
selection. The user edits the combined text. This matches `pi`
and is the least error-prone: one operation, no index math.

## 6. Loop-side: the loop must skip retracted messages

The loop derives pending work from the log. It must treat a
retracted message as not-pending.

- `bin/claim` `derive_state`: when scanning `user_message` events,
  build a set of retracted `target` ids from the
  `user_message_retract` events. A `user_message` whose `id` is in
  that set does not advance `last_user_message_seq` and does not
  open a follow-up.
- `bin/assemble` `user_event_rides`: the same skip applies when
  deciding which user messages ride the next model input.
- The TUI's `App::pending_user_messages` applies the same skip, so
  the rendered pending blocks match what the loop will actually
  consume.

Because the retract is appended after the target, and the loop
processes the log in order, a step that has already consumed the
original is unaffected. Retraction only matters for messages that
have not been consumed yet, which is exactly the pending set.

## 7. Interaction with compaction

Compaction shadows compacted ranges and re-derives the request from
a checkpoint. It never deletes log lines.
`docs/deepseek-harness-compaction-research.md` and
`docs/auto-compact-plan.md` describe the shadow model. Therefore
the `user_message_retract` and the retracted `user_message` both
survive compaction in the log, and their `id` references stay
valid. No special handling is needed. The retract is only
meaningful while the target is still pending, which is always
before any compaction boundary that would shadow it.

## 8. No gate on loop state

Editing a pending message is allowed whether the loop is running,
idle, or stopped. The common case is the model mid-call: the user
sent a message, sees a mistake, and wants to fix it before the
next step injects it. Gating on "loop not running" would block
that case. There is no gate. The retract is always safe because it
only affects messages the model has not yet consumed.

## 9. TUI-side changes

- `key_input` in `main.rs` maps `Alt + Up` to a new
  `Key::AltUp`. The `:edit-queue` command is a `Run` item in the
  command palette (`docs/tui-command-palette.md` section 9).
- `press` in `app.rs` handles `AltUp` and the `edit-queue` commit
  by emitting a new `Action::RecallQueue`.
- `Action::RecallQueue` in `main.rs`: append the retract events,
  set the editor text to the joined retracted messages, focus the
  editor. Flash the recalled queue type and count.
- Render retracted messages dimmed with a "retracted" marker,
  consistent with the superseded-marker handling in `render.rs`.

## 10. Build plan

- Step 0: schema. Add `user_message_retract.json`, add optional
  `id` to `user_message.json`. Producer test appends a retract and
  a corrected message; consumer test reads the pair.
- Step 1: loop skip. `bin/claim` and `bin/assemble` skip
  retracted ids. Unit tests: a retracted steer message does not
  set `awaiting_model`; a retracted follow message does not open a
  follow-up.
- Step 2: TUI derivation. `pending_user_messages` and its two
  splits skip retracted ids. The pending blocks no longer show
  retracted messages.
- Step 3: recall flow. `Key::AltUp`, `Action::RecallQueue`, the
  join-and-load into the editor, and the re-send. The `:edit-queue`
  palette entry dispatches to the same path.
- Step 4: replay. A session log containing a retract re-renders
  and re-reduces to the same idle state as a fresh session with
  the corrected message.

## 11. Open items

- Per-message recall: the user picks one queued message instead of
  all. Requires a selection UI on the pending blocks. Later add.
- `bin/user` generating an `id` so CLI-sent messages can be
  retracted. Later add.
- A `:undo` that re-appends the retracted text as a fresh
  `user_message`. Later add; the text is always recoverable from
  the log.

## Properties

Lean-style invariants for this spec (see `lean-driven-development.md`).

P1. retract-append: given a recall of all pending messages, observe one `user_message_retract` per target appended before the replacement, keeping the log order consistent.
P2. recall-edit: given a recall, observe the retracted text join into the editor with one block per original message and the queue label kept per block.
P3. loop-skip-retracted: given a log where a `user_message` id is in the retracted set, observe `claim` and `assemble` treat it as not-pending, opening no follow-up and sending no model input.
P4. re-derivation: given a retracted message, observe the TUI pending blocks exclude it so they match what the loop will consume.
P5. send-cancels: given a recall then `Enter` on edited text, observe the edited `user_message` append and the retracted originals cancel. Given `Esc`, observe the retracts stand and the pending list empty.

## Verification

| P# | Property | Proof | Status |
|----|----------|-------|--------|
| P1 | retract-append | Blocked: the `user_message_retract` schema and TUI append exist, but no producer test appends a retract plus a corrected message. Unblock with the step-0 producer and consumer test in `bin/tui`. | open |
| P2 | recall-edit | Blocked: no test asserts the joined-block editor load. Unblock with an `app.rs` test for the `RecallQueue` join. | open |
| P3 | loop-skip-retracted | Blocked: `bin/claim` and `bin/assemble` do not yet skip retracted ids. Unblock when the step-1 skip logic lands with the unit tests in section 10. | open |
| P4 | re-derivation | Blocked: the retracted-id skip in `bin/tui/src/app.rs` has no test. Unblock with a test that a retracted pending message does not render. | open |
| P5 | send-cancels | Blocked: no test covers send-after-recall and Esc-after-recall. Unblock with an `app.rs` test. | open |

## Gate

Gate: blocked — the loop-side retract skip and the recall and re-derivation tests are not yet implemented.

```
cargo build
cargo test
```
