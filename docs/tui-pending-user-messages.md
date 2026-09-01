# TUI pending user messages

Status: stage 1 shipped (commit `fc51f71`), stage 2 open. The
request lives in `docs/tui_feature_requests_from_human.md`
(2026-08-31 item).

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

## 4. Stage 2 (loop-side split, open)

- [ ] Schema: add `queue: "steer" | "follow"` to `user_message`.
      A missing field means `steer`. Old logs stay valid.
- [ ] `bin/claim`: pending `steer` gives `awaiting_model`. Only
      pending `follow` gives `idle` plus a `pending_follow_ups`
      count.
- [ ] `scripts/turn.sh`: do not break on `idle` when follow-ups
      are pending. Continue the loop. They run as a new turn.
- [ ] `bin/assemble`: inject `steer` messages into the in-flight
      step. Inject `follow` messages only on a turn restart.
- [ ] `bin/user` and the TUI input: pick the queue while the
      loop is busy.
- [ ] TUI: render both pending lists, each with a count.
      Replace the stage-1 label.

## 5. Notes

- Stage 2 ships drain mode `all` first. The `one-at-a-time`
  mode is a later refinement.
- Stage 1 ships before stage 2. Stage 2 replaces the stage-1
  label when it lands.
