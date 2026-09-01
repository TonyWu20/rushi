# Auto-compact plan: design review

Status: review of `docs/auto-compact-plan.md` (proposed). Scope: design
correctness and YAGNI shortcuts that hide real damage. Evidence: the
repo code, `config.toml`, and the pi 0.84.2 source in the nix store.

## 1. Verified fact base

### 1.1 Repo facts (plan section 2)

All repo claims check out. Key evidence:

- `turn.sh` loops until `claim` reports `idle` or `exhausted`. No
  step cap (correction 51). `scripts/turn.sh:13-23`.
- `claim` has a `_ => {}` branch for unknown types. `error` maps to
  `idle`. `bin/claim/src/main.rs:125-135`.
- `run_handoff` seeds `<name>_h<n>` and logs the marker.
  `scripts/step.sh:53-97`.
- Budget math: window 262144 minus `max_output_tokens` 32768 gives
  input budget 229376. `bin/assemble/src/main.rs:919-926`.
  `config.toml:11,30,47`.
- Trigger = 229376 minus 16384 = 202992. The trigger sits below the
  trim budget. LLM compaction leads. Correct.
- `length` stop with empty content is terminal today.
  `scripts/step.sh:195-201`.
- Validators skip `enum` and `minimum`. `bin/log/src/main.rs:142-190`,
  `bin/tui/src/port_file.rs:856-910`.
- `step_groups` exists. `bin/assemble/src/main.rs:797`.

### 1.2 pi facts (plan section 1)

Mostly accurate. Four errors to fix in the plan:

- The plan says `settings.jsonl`. pi stores these keys in
  `settings.json` (global and project).
  `pi-coding-agent/src/core/settings-manager.ts:203-204`.
- "Aborted messages skip the check" holds only for the post-run
  call. The pre-prompt call passes `skipAbortedCheck = false`.
  `agent-session.ts:1209`.
- The model-switch guard gates case 1 (overflow) only. The threshold
  case has no model guard. `agent-session.ts:1975-1978, 2024`.
- "The summary is one LLM call" fails for split turns: pi runs a
  second call for the turn prefix at a 0.5x budget, and it clamps
  the cap to the model max output. `compaction.ts:637-640, 833-872`.

None of these breaks the harness design. The harness cuts at step
group boundaries, so it never splits a turn. The errors belong in
the plan's research section.

## 2. Design-correctness findings

New issues the plan misses. Each has a required fix.

### D1. Silent overflow must not re-run a finished turn (high)

Plan 4.4 step 5 folds all four classifications into one path:
"compact, then re-run the model call once". Silent overflow is a
successful `stop` call. pi retries only when `stop_reason` is not
`stop` (`agent-session.ts:1996-1998`). Re-running a finished turn
has two bad effects:

- The valid response is discarded. The tool calls it held never
  route.
- The retry request excludes the last assistant group. If the model
  declared the task done, the retry never sees that. It can redo or
  undo finished work.

Fix: split the path. Silent overflow: compact only, no re-run. The
loop continues by its own state machine. Error overflow, truncation
stop, and recoverable length: compact plus one re-run.

### D2. A failed compaction re-fires every step (high, performance)

After a `compaction_failed` marker, nothing stops the next step's
threshold check from firing again. The level clause and the growth
prediction keep crossing the threshold. Each re-fire runs a summary
call that will fail again.

That call is the loop's longest stall. On a local GPU it costs
minutes per step. The log fills with `compaction_failed` markers.
The plan has no cooldown and no retry policy for the summary call.
pi runs the summary under the session retry policy
(`agent-session.ts:2148-2155`).

Fix: retry the summary call a bounded number of times. After the
last failure, skip the threshold check until the next user message.
Record the skip in the `compact.json` state.

### D3. `compact_reasoning_effort` has no mechanism (medium)

`bin/model` reads `reasoning_effort` from config only. It ignores
any request field. `bin/model/src/main.rs:75-77`. The summary
request shape carries no effort field (the handoff shape holds
`model`, `instructions`, `input`, `tools`, `max_output_tokens`).
Adding the config key in section 4.5 changes nothing. The knob as
written cannot work.

Fix: add an optional `reasoning_effort` field to the summary
request. Make `bin/model` prefer the request value over the config.
Add the `bin/model` change to the phase list.

### D4. The model guard is not implementable as stated (medium)

The plan puts the guard on `parse` recording the model on
`assistant_message` events. `parse` does not run on a failed call.
The failed-call result JSON carries no `model` field
(`bin/model/src/main.rs:683-689, 132-137`). The guard cannot read
the failing call's model from the event log.

The working source is the request JSON: `assemble` writes `model`
into every request (`bin/assemble/src/main.rs:1098-1105`).
`step.sh` holds that file in the work dir. The guard must compare
the request model against the active config model.

Note: the guard has little value in this harness. Classification is
live, inside one step. A config model switch between the failed call
and its classification is not a real race. State that, or keep the
guard with the request JSON as source.

### D5. The projection copy is bigger than admitted (medium)

The plan admits copying "the growth and prediction math" from
`assemble`. The summary input needs more than that. It needs the
full compact-form projection: caps, keep window, drop search,
schema-error pair drops, tool-log pointer markers. That is
`compact_candidate` plus the drop search plus
`schema_error_pair_ids` plus the pointer builder
(`bin/assemble/src/main.rs:771-838, 834`). With no shared crate in
Phase 1, `bin/compact` holds a second copy of that core. Two copies
drift.

The plan also never says which caps and drop count the summary input
uses: the current sticky state, the base caps, or a drop search
bounded by the input budget. That choice is load-bearing.

Fix, either:

- `assemble` gains a summary-input mode. It projects the old region
  and prints the items. `compact` runs it through the glue.
- Or the plan names the full copy and adds a divergence test.

### D6. `compaction_summary` has no `tokens_after` (low)

The plan's TUI line reads "212k to 33k tokens". The schema carries
`tokens_before` only. The renderer has no source for the second
number.

Fix: add a `tokens_after` field (the estimator's value for the
post-compaction request) or drop the second number from the line.

### D7. A crashed compaction leaves an open marker (low)

If the loop dies between `compaction_started` and the summary or
failed event, the log holds an open marker. The TUI renders a
`compacting` line forever. The plan gives no rule for that case.

Fix: treat an open marker older than the summary budget as failed.
Or state the case as accepted and render it as such.

### D8. "The provider window is the backstop" names a backstop that
does not exist (medium)

The trim form caps the request at the input budget (229376). The
window is 262144. A model call can never overflow the provider.

The plan's "no trigger-based readings: no trigger. The provider
window is the backstop" (4.2) and finding #10 ("the overflow
backstop recovers") both cite that missing backstop. The real
backstops are the trim form and the terminal handoff.

In the single-measurement case (state reset, then one jump past the
trim budget), the trigger never fires. The session rides 500-char
previews with no LLM compaction.

Reachability is low: tool outputs cap at 20000 chars, so one step
cannot add 200k tokens. Still, the plan should say so and add the
guard: when the trim form engages with fewer than two trigger-based
readings, fire the trigger.

### D9. "The tool log pointers stay valid" overstates (low)

The pointer stays valid for the kept region only. Events in the old
region are gone from the request after compaction. Their tool-log
bodies lose every pointer. The model can re-read files. It cannot
re-fetch an old tool output. Reword 4.1 to match.

## 3. YAGNI shortcut audit

Section 8 of the plan lists accepted cuts. Three hide real damage.

### Y1. File-op lists re-extracted, not carried (medium)

pi persists the read and modified file lists in the compaction
entry and re-injects them at the next compaction
(`compaction.ts:51-63`). The plan re-extracts the lists from the
old region's tool calls at each compaction. After the first
compaction, the old region no longer holds the old tool calls.

The cumulative list can only survive through the summary text. The
plan specifies the update prompt as pi's preserve/add/move rules for
the progress items. It never says the prompt must merge the previous
file-op list. Without that line, the list erodes one compaction at
a time.

Fix: state that the iterative update prompt merges the previous
file-op list. Or persist the lists as a small field on the
`compaction_summary` event.

### Y2. "A small accounting gap" is not small (low)

The statusline cumulative total sums `assistant_message.usage`.
The summary call's usage joins it only in Phase 4. The summary
input is the drop-search-bounded compact region. That can be tens
of thousands of tokens per compaction.

The total undercounts by that amount until Phase 4. "Small"
mislabels the gap. It is a display and cost matter, not loop
correctness.

Fix: reword the note, or pull the usage sum into an earlier phase.

### Y3. The one-step-late trigger rests on a missing backstop

See D8. The acceptance in finding #10 cites an overflow backstop
that the trim form prevents. The real behavior is the rate clause
firing one step later. The single-measurement edge case has no
recovery at all. Accept the edge case only with the D8 guard in
place.

### Cuts that are genuinely bounded

- No extension hook for a custom summarizer. The `ext-rs` `append`
  cap is the later path. No damage today.
- No branch summary. The harness has no branches.

## 4. Required fixes before implementation

1. Split the overflow recovery path by `stop_reason`. Silent
   overflow compacts without a re-run (D1).
2. Add a bounded summary-call retry and a post-failure cooldown in
   the threshold check (D2).
3. Add the `reasoning_effort` request field and the `bin/model`
   change to the phases (D3).
4. Source the model guard from the request JSON, or drop it (D4).
5. Pick the summary-input caps and drop rule. Move the projection
   into an `assemble` mode, or admit the full copy with a
   divergence test (D5).
6. Add `tokens_after` to the schema (D6).
7. Define the open `compaction_started` case (D7).
8. Add the trim-engaged, single-reading trigger guard. Correct the
   backstop wording (D8).
9. Reword the tool-log pointer claim (D9).
10. State the file-op list carry-forward in the update prompt (Y1).
11. Correct the four pi facts in section 1 (section 1.2 above).
