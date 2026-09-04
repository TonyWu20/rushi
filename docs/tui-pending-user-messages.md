# TUI pending user messages

Status: shipped (stage 2, 2026-09-03 pass). Stage 1 shipped in
commit `fc51f71`. The request lives in
`docs/tui_feature_requests_from_human.md` (2026-08-31 item).
Stage 2, the loop-side split, shipped in the same pass.

## 1. Request

Show user messages that wait for the busy loop. Render them
like `pi` renders `steering` and `follow-ups`.

Today: while the loop is busy, the TUI still accepts input.
Enter appends a `user_message` to the log and the loop picks
it up at its next step. The TUI shows nothing about it. The
user does not know if the input was acknowledged, or when the
loop will process it.

Needed: a pending list of unconsumed `user_message` events per
session. Show the waiting messages, with a count, like `pi`'s
`steering` (injected at the next step) and `follow-ups`
(processed after the loop ends).

## 2. Design decision: two stages

Decision: ship the pending-message work in two stages. Stage 1
is a TUI-only indicator. Stage 2 adds the loop-side
`steering` / `follow-up` split.

Reason: the indicator states the delivery time to the user.
Today, every `user_message` injects at the next step. That is
`steering`, drain mode `all`. A "follow-up: processed after
the loop ends" hint is false until the loop supports the
split. A wrong hint is worse than no hint.

How `pi` splits the two queues (reference,
`pi-agent-core/src/agent.ts`):

- `steer()`: queue a message to be injected after the current
  assistant turn finishes. The loop polls the queue at the next
  step inside the running execution.
- `followUp()`: queue a message to run only after the agent
  would otherwise stop. The run delivers it as a new prompt.
- Both queues use two drain modes: `all` and `one-at-a-time`.
- The UI reads `pendingMessageCount`, `steeringMode`,
  `followUpMode`.

## 3. Stage 1 (TUI only, shipped)

- [x] Show the pending list of unconsumed `user_message` events
      per session, with a count.
- [x] Label it `steering — injected at the next step`. The
      label states today's real loop behavior.
- No loop changes. No schema changes.
- Shipped: the steering block renders between the transcript
      and the input box. Header row with the count and the
      delivery hint; up to three preview rows; a `+N more` row
      for the rest. The running label is `steering, injected at
      the next step`; the stopped label points at `Ctrl+R`.
      `App::pending_user_messages` (bin/tui/src/app.rs) and
      `pending_steering_lines` (bin/tui/src/render.rs).

## 4. Stage 2 (loop-side split, shipped)

- [x] Schema: the `queue` field, `steer` or `follow`, on
      `user_message` (`schemas/events/v1/user_message.json`). A
      missing field means `steer`. Old logs stay valid. `bin/user`
      takes `--queue follow` (the steer path leaves the field
      absent, so its logs stay unchanged). The TUI's
      `Ctrl+F`-toggled composer sends the follow queue when
      toggled.
- [x] `bin/claim`: `derive_state` reports `pending_follow_ups`
      alongside the pending tool calls. Only pending `follow`
      messages hold `idle` (with the count); a pending `steer`
      message gives `awaiting_model`. A turn boundary (an
      `assistant_message` without tool calls, an `error`, or an
      exhausted context) clears the follow count.
- [x] `scripts/turn.sh` and `scripts/step.sh`: the step exits
      `idle` only when no follow-ups wait. On `idle` with
      follow-ups the loop runs the follow turn: the model path
      with `bin/assemble --inject-follow`, one new turn per drain.
      In-progress steps of that turn do not re-inject.
- [x] `bin/assemble`: `user_event_rides` waits for the follow
      queue to reach the turn boundary; `--inject-follow`
      releases the queued follow messages into the request. The
      seq counting survives the skip.
- [x] `bin/user` and the TUI input: `bin/user --queue follow`
      writes the field; the TUI composer toggles the queue with
      `Ctrl+F` and flashes the active queue on the status row.
- [x] TUI: the two pending blocks, each with its own count and
      previews. `pending_steering_lines` becomes
      `pending_message_lines`; the steer block keeps its
      stage-1 labels and the follow block renders its own header
      (`follow-up — run after the loop stops`).

## 5. Notes

- Stage 2 ships drain mode `all` first. The `one-at-a-time`
  mode is a later refinement.
- Stage 1 ships before stage 2. Stage 2 replaces the stage-1
  label when it lands.

## Properties

Lean-style invariants for this spec (see `lean-driven-development.md`).
One property per non-trivial invariant. Each property is observable:
given an input, an output guarantee.

P1. steering-display: given unconsumed `user_message` events with
    queue `steer` or a missing `queue` field in a session, observe a
    steering block renders between the transcript and the input box,
    with a count, up to three preview rows, and a `+N more` row for
    the rest.
P2. follow-display: given unconsumed `follow`-queued user messages in
    a session, observe a separate follow-up block renders with its
    own count, previews, and the `follow-up — run after the loop
    stops` header.
P3. queue-default: given a `user_message` event without a `queue`
    field, observe it is treated as `steer` and old logs stay valid.
P4. claim-state: given a pending `follow` message with no pending
    tool calls, observe `derive_state` reports `idle` with the follow
    count; given a pending `steer` message, observe `awaiting_model`.
P5. follow-turn: given the step exits `idle` while follow-ups wait,
    observe the follow turn runs through `bin/assemble --inject-follow`
    with one new turn per drain, and a turn boundary clears the
    follow count.
P6. composer-toggle: given the user toggles the composer queue with
    Ctrl+F, observe the next draft sends to the follow queue and the
    active queue flashes on the status row.

## Verification

Each property maps to its proof. `proven` means the cited test or
script exists and passes. `open` names the blocker and what unblocks
it.

| P# | Property | Proof | Status |
|----|----------|-------|--------|
| P1 | steering-display | `steering_block_caps_the_list_and_counts_the_rest`, `steering_header_names_the_delivery` in `bin/tui/src/render.rs` | proven |
| P2 | follow-display | `follow_messages_render_their_own_block`, `follow_only_queue_shows_one_block` in `bin/tui/src/render.rs` | proven |
| P3 | queue-default | `user_event_rides_by_queue` in `bin/assemble/src/main.rs` | proven |
| P4 | claim-state | `follow_message_keeps_idle_and_counts`, `steer_message_wakes_the_model` in `bin/claim/src/main.rs` | proven |
| P5 | follow-turn | `turn_boundary_consumes_the_follow_ups` in `bin/claim/src/main.rs`, `user_event_rides_by_queue` in `bin/assemble/src/main.rs` | proven |
| P6 | composer-toggle | Blocked: no test drives the Ctrl+F composer toggle. Unblock with a test that dispatches `Key::CtrlF` and asserts `follow_queue` flips and the status-row flash fires | open |

## Gate

The acceptance commands. All must exit 0 for this spec to be proven.

```
cargo build
cargo test
```
