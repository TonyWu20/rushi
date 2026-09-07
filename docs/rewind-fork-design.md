# Session rewind and fork: the rewind event and the active path

Status: Implemented — the mechanism (event, schema, shared active-path
computation, rewind-aware `bin/assemble` projection, rewind-aware
`bin/claim` state, TUI event vocabulary and marker rendering) landed
2026-07-17. The TUI picker command (the `/tree`-style entry point)
is the next stage, specified in section 10.

## 1. Request

Let the user rewind the session to any recorded event, pi `/tree`
style: pick an event, and the state goes back to the moment before
or on it. A user-message target restores the message to the input
box, unsent; a finished-step target (tool call / agent response)
restores the moment the step completed, ready for the next step.
The session log stays append-only: the rewind appends a checkpoint
event, `bin/assemble` masks the abandoned events out of the model
context, and later rewinds can fork from or re-enter any branch.

## 2. The proposed design, restated

The user's checkpoint + mask proposal:

- The user picks an event to rewind to (before it, or on it).
- The log gains one appended event: `rewind to <target>`.
- `bin/assemble` projects the log: pick events up to the
  checkpoint, continue with the events after the rewind
  declaration; mask the events between the picked event and the
  rewind declaration — they stay in the log but never enter the
  context.
- A later rewind applies the same rule with its own target: the
  user can re-enter a forked branch, and the context is rebuilt to
  pick up that timeline.

The proposal is sound at depth one: one fork, one mask span. It
needed four refinements to hold at depth two and beyond, all found
by walking the real `bin/assemble` and `bin/claim` code. Section 4
lists the issues; sections 3, 5, 7–9 are the refined design as
implemented.

## 3. The event: `rewind`

One schema file, `schemas/events/v1/rewind.json`, picked up by the
shared glob loader (docs/phase-2-plan.md section 6):

```json
{"v":1,"type":"rewind","ts":"...","target_seq":41,"mode":"before","reason":"tui_pick"}
```

- `target_seq` — the 1-based log seq of the target event, the same
  numbering `first_kept_seq`, `--up-to`, and `last_user_seq` use.
  The log has no event ids; the seq is stable because the log is
  append-only and no path rewrites `events.jsonl`. An optional
  display id is not needed for the mechanism: seqs are the
  addressing, and the TUI already renders seqs in markers.
- `mode` — `before`: the target is excluded from the context (the
  user-message-to-input-box case); `on`: the target is included
  (the finished-step case). The mode is stored, not derived from
  the target's type: derivation would couple projection to event
  shapes and leave no escape hatch.
- The marker projects to nothing: `assemble` collects it into the
  rewind list and `claim` reads it for state; neither appends its
  fields to the model input.

Target rules (enforced by the producer; `bin/log` schema validation
covers the shape only): `target_seq >= 1`, strictly earlier than
the marker's own seq, and a settled point — a `user_message`, a
`tool_result`, or an `assistant_message` with no outstanding tool
calls. Section 7.

## 4. Issues with the proposed design (the review)

**I1. The single-gap mask breaks at nesting depth 2.**
The rule "mask between the picked event and the latest rewind
declaration" keeps the right events only when every rewind targets
the end of the timeline so far. Counter-example, log seqs:

```
1..3   branch A
4      rewind target=3      (fork B)
5..6   branch B
7      rewind target=3      (fork A')
8..9   branch A'
10     rewind target=9      (continue A')
```

At seq 10 the latest rewind masks only the gap (9, 10) — empty.
The naive rule keeps seqs 1..9, so branch B (5..6) rides branch
A''s context even though the user abandoned B two forks ago. The
mask must be computed recursively through the rewind chain: the
active path at the log end is `active(T) ∪ (R .. end]` where
`(T, R)` is the latest rewind, and `active(T)` is computed the same
way over the rewinds that precede it. Here the chain 10 → 9 →
(latest rewind < 9 is seq 7, target 3) → active(3) = [1..3] gives
`[1..3] ∪ [8..9]`: B is masked. Re-entering B is one more rewind
(target 6), and the same recursion rebuilds B''s full path while
masking A''. This is issue 1 of the design as proposed; the fix is
the active-path recursion of section 5.

**I2. There is no event id to point at.**
The proposal says "event-id or index-based". The log has no ids:
seqs are the only stable, cheap addressing, and every existing
mechanism (`first_kept_seq`, `--up-to`, `last_user_seq`) already
counts 1-based non-empty log lines. A rewind therefore stores a
seq, not an id.

**I3. A masked-or-kept context may strand a tool call.**
Rewinding to a user message picked mid-step (a steer message typed
while a step was running) leaves the step''s `function_call` items
in the context with their `function_call_output` masked out. The
Responses API rejects that shape. The settled-target rule of
section 7 keeps this out of producer picks; `assemble` enforces it
defensively (P4): a rewind whose context strands a call is ignored
with a stderr warning, and the branch re-projects linear — the same
degrade-or-skip discipline as a corrupt compaction boundary.

**I4. The compaction boundary owns the old region.**
A rewind target below `first_kept_seq` points at events the
boundary already replaced by the summary framing. The refined rule
is structural, not a special case: the boundary filter runs first,
the active-path mask runs second over the projected region, so a
pre-boundary target simply degrades to the region head. The
framing item still leads; the estimator''s framing-aware path is
unchanged.

**I5. `claim` owes work from masked events as-is.**
The state machine walks the raw log. After a rewind to seq 1, an
unresolved tool call at seq 3 still sits in the log: `claim`
reports `awaiting_tool_result` and the loop re-dispatches the call
the user just abandoned. The rewind arm clears every pending list
(`pending_tool_calls`, `pending_follow_ups`, the open approval
wait) and sets the owed state from the target: a `tool_result`
target in `on` mode owes the model call that continues the finished
step (`awaiting_model` — "prepare to take the next step"); every
other target is idle. `last_user_message_seq` keeps counting the
raw log: the compaction re-arm compares "have we seen a newer user
message than the failed marker", which the raw count answers.

**I6. The projection''s side channels must follow the mask.**
`drop_pairs` (FT-008 self-priming), `drop_last_assistant_group`
(overflow retry), and `estimate_request_tokens` (the budget gate)
all consumed the pre-mask region. A masked pair must be out of the
drop set (it is out of the context); the last assistant group is
the last group of the masked context; the estimate is the masked
context. All three now take the masked lists.

**I7. The `usage_input` anchor stays valid.**
`estimate_request_tokens` anchors on the last measured
`usage.input_tokens` in the kept region. After a rewind, that
measurement was taken on a request whose input was exactly the
active-path prefix at that event — the same prefix the rebuilt
request carries — so the anchor is still the right one. No change.

**I8. The log shows every branch.**
The transcript must not present masked events as if they were the
conversation. v1 (implemented): the `rewind` marker renders a dim
branch line (`rewound to seq N (mode)`) like the compaction
markers, and the transcript shows the full log — masked events
remain visible, which is the source of truth. The active-path
filter and the `/tree`-style picker are the next stage (section
10), matching the staged convention of `docs/tui-file-picker.md`.

**I9. Without the schema file the marker is rejected at the door.**
`bin/log` and the TUI port validate against the schema vocabulary;
an unknown `type` is a hard rejection. The `rewind.json` schema is
therefore part of the mechanism, not optional. The shared
validator''s subset enforces `const`/`enum`/`required`/`properties`/
`items`; `minimum` and the earlier-than-self rule are producer-
enforced, documented on the schema like `first_kept_seq`.

**I10. `tools.jsonl` needs no change.**
The tool log is keyed by call id, and a forked branch generates
fresh call ids. Masked results stay readable by their ids; the
slim-index pointers in trimmed events are unaffected.

**I11. Rewinding breaks the provider prefix cache.**
The rebuilt request''s prefix differs from the abandoned branch''s,
so prompt caching misses once per fork. Inherent to any rewind,
pi included; no design change.

**I12. Guards.** A no-op rewind (target equal to the current active
tail) and rewinds on terminal sessions (`context_exhausted`,
terminal `error`) are refused by the producer, not the log: the
log stays the source of truth and stays append-only.

## 5. The active path (the algorithm)

Shared in `crates/rushi/src/rewind.rs` (the one home, like
`logline` and `event_validation`):

- `parse_rewind_event(event, seq)` — one log line into a
  `RewindRef { seq, target, before }`. Malformed values (zero or
  future target, missing or bad mode) yield `None`; the projection
  degrades as if the marker were absent.
- `active_ranges(end, rewinds) -> Vec<(lo, hi)>` — the active path
  of the prefix ending at seq `end`, as disjoint ascending
  inclusive ranges. Definition: no rewind at or before `end`
  gives `[1..end]`; with `(S, T, m)` the last one, the path is
  `active(T_eff) ∪ [S+1..end]`, `T_eff = T` (on) or `T-1`
  (before). The recursion strictly decreases `end`
  (`T_eff < S ≤ end`), so it terminates; the chain is the sequence
  of rewinds the log forked through, which is exactly what
  re-entering a branch means.
- `seq_in_ranges(s, ranges)` — the membership test the
  projection''s filter uses.

`bin/assemble` applies it after the boundary filter: the projected
events are kept only where their seq is active. If the kept events
strand a call (I3), the outermost marker is popped, warned, and the
projection recomputed — until clean or marker-free. `bin/claim`
consumes the markers for state (I5).

## 6. TUI (implemented)

- `EventKind::Rewind` joins the vocabulary; `produce::rewind`
  builds the marker (G3: producers build typed envelopes).
- The transcript renders the marker''s dim line
  (`rewound to seq N (mode)`), missing fields degrade to
  placeholders (G5).

## 7. Settled targets (producer rule)

A pick is offered only at a settled point: a `user_message`
(mode `before` — restore to the input box), a `tool_result` (mode
`on`), or an `assistant_message` with no outstanding calls (mode
`on`). "Outstanding" is computed over the active path, not the raw
log. This keeps every projected context call-complete without the
defensive path of I3 ever firing; the check stays in `assemble`
because the log is the source of truth and can be edited by hand.

## 8. Interaction with compaction, one example

Log: boundary `first_kept_seq = 7` (events 1..6 summarized), then
a fork. Rewind `target_seq = 3` (inside the compacted region): the
boundary filter drops 1..6 before the mask runs, so the target
degrades to seq 7, the region head, and the context is the framing
item plus the active region from 7. Rewind `target_seq = 9`
(inside the kept region): the mask masks the span `(9, R]`
normally; the framing still leads, and the estimator''s
framing-aware from-scratch path applies. Both behaviors fall out
of the filter order; no compaction-aware rule in the mask.

## 9. Verification (implemented)

- `crates/rushi/src/rewind.rs` tests: the recursion, the depth-2
  counter-example, the depth-3 chain composition, branch re-entry,
  before/at-seq-1 edges, the parse guards.
- `bin/assemble` tests: the mask on single forks, nested forks
  (B masked), branch re-entry (A'' masked), the P4 ignore-and-
  warn path, the dangling-call check.
- `bin/claim` tests: the rewind settles the session (masked calls
  owe nothing), a `tool_result` target owes the model call,
  `before` mode never does, masked follow-ups and approval waits
  clear, a new-branch message reopens the loop.
- `bin/tui` tests: the kind round-trips, the producer envelope,
  the marker line renders and degrades.
- `scripts/e2e-rewind.sh`: drives the real `log`, `claim`, and
  `assemble` binaries over a synthetic forked session.

## Properties

Lean-style invariants for this spec (see `lean-driven-development.md`).

P1. fork-mask: given a log with a rewind at seq S, target T, and events after S, observe the assembled input exclude every event in (T_eff, S] and include [1..T_eff] plus every event after S.
P2. nested-fork: given a fork log of depth two or more (A, B, A'), observe the assembled input on the A' tail exclude branch B — the single-gap rule''s counter-example; at depth three the composition still masks every abandoned intermediate span.
P3. branch-reentry: given a rewind to a forked branch''s tail, observe the assembled input equal that branch''s full active path, with the sibling branch masked.
P4. no-stranded-pair: given a rewind whose context would strand a tool call without its result, or a result without its call, observe the marker ignored (stderr warning), the branch re-projected linear, and every function_call and function_call_output in the input paired with its twin.
P5. boundary-degrade: given a rewind target below the boundary''s first_kept_seq, observe the compacted region staying out of the input — the target degrades to the region head, the framing item still leads, and a mask that would strand a pair across the boundary drops the marker under P4.
P6. claim-settled: given a log ending in a rewind, observe claim report idle (or awaiting_model for an on-mode tool_result target) with empty pending tool calls, follow-ups, and approval waits.
P7. append-only: given a rewind, observe the log gain exactly one line and every earlier byte unchanged (the FT-005 write discipline is the one writer).

## Verification

| P# | Property | Proof | Status |
|----|----------|-------|--------|
| P1 | fork-mask | `mask_masks_the_abandoned_branch` in `bin/assemble/src/main.rs`; `on_mode_includes_the_target` in `crates/rushi/src/rewind.rs` | proven |
| P2 | nested-fork | `mask_nested_forks_mask_the_intermediate_branch` in `bin/assemble`; `nested_forks_mask_the_abandoned_branch` and `depth_three_forks_compose_through_the_chain` in `crates/rushi/src/rewind.rs`; the nested (B) and depth-3 (F) scenarios of `scripts/e2e-rewind.sh` | proven |
| P3 | branch-reentry | `mask_reentering_a_branch_rebuilds_its_path` in `bin/assemble`; `reentering_a_branch_rebuilds_its_path` in `crates/rushi/src/rewind.rs` | proven |
| P4 | no-stranded-pair | `mask_ignores_a_rewind_that_strands_a_call` and `pair_stranding_check_both_sides` in `bin/assemble` | proven |
| P5 | boundary-degrade | The filter order in `main` (boundary before mask); observed by the `e2e-rewind` boundary scenarios when a boundary precedes the fork | proven |
| P6 | claim-settled | the six `rewind_*` tests in `bin/claim/src/main.rs` | proven |
| P7 | append-only | `LogLine::commit` is the only writer (FT-005); `scripts/e2e-rewind.sh` asserts the prefix hash is unchanged after the append | proven |

## Gate

Gate: clean for the mechanism — the TUI picker stage (section 10)
is tracked separately.

```
cargo build
cargo test
scripts/e2e-rewind.sh
scripts/verify-specs.sh
```

## 10. Next stage: the TUI picker

Spec, not yet built. The `/tree`-style entry point:

- A palette command (`rewind`) opens the existing picker over the
  active-path events, offering only settled points (section 7).
  Each row shows the seq, the event kind, and a one-line preview;
  branch markers annotate the rows.
- Pick confirms with a summary (target, mode, the branch being
  abandoned). The TUI appends the marker through the existing
  validated append path and, for a `before`-mode user-message
  target, restores the text to the editor, unsent; the restored
  text persists across a reattach like the other pending state.
- The transcript gains an active-path toggle: masked events dimmed
  and non-scrolling, the branch structure visible (pi `/tree`).
- For an `on`-mode `tool_result` target, the TUI spawns the loop
  when one is not running: claim already reports `awaiting_model`,
  so the step that continues the finished step is the one the
  loop owes.
