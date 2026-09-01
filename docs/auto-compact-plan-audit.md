# Auto-compact plan: second audit

Status: audit of `docs/auto-compact-plan.md` (revised). Scope: design
correctness, and harmful shortcuts disguised as YAGNI that the first
review (section 8.1 of the plan) did not find. Evidence: the current
repo code, `config.toml`, the pi 0.84.2 source in the nix store, and
the session logs. The fixes land in the plan sections named below.

Method: every load-bearing claim of the plan checks out against the
authoritative source (section 2 below). Then each accepted cut and
each safety argument gets re-derived from the code, not from the plan's
wording. The findings below are what fails that test.

## 1. Findings

### A1. `bin/log` rejects all three marker types (high)

The plan (section 4.1) states that `log` and the TUI `port_file`
validator load the schema file by type name, and that both pick up the
new types with no code change. The TUI side is true: `append_event`
loads `<type>.json` when the file exists
(`bin/tui/src/port_file.rs:518-521`). The `log` side is false:
`bin/log` validates against a hardcoded list of seven schema file
names (`bin/log/src/main.rs:42-50`), and an event type outside the
list is a hard failure: `unknown event type`, exit 1
(`bin/log/src/main.rs:132-134`).

All three markers flow through `bin/log`: `compact` appends
`compaction_started`, `compaction_failed`, and `compaction_summary`
via `log` (plan 4.1, 4.2). Without the list change, every triggered
compaction aborts the step. The TUI and `compact` both load schemas
by type name; only `log` keeps the hardcoded vocabulary.

Fix: add the three schema names to the `bin/log` list. Phase 1.
Note the sync rule: a new event type needs the schema file, the
`bin/log` list entry, and the TUI enum entry.

### A2. The length-stop group is not dropped by correction 60 (high)

The plan (4.4 "Length-stop log shape") describes the group as
"`{}`-argument calls" whose results `route` records as
"schema-validation result (the FT-008 class)". The code does
something else. The `length` branch of `parse` emits the
`assistant_message`, then one `tool_result` per call with the fixed
text "Arguments may be truncated. Re-issue the call with shorter
arguments." (`bin/parse/src/main.rs:217-258`), and exits 2. `route`
never runs: `step.sh` routes only on parse exit 1
(`scripts/step.sh:240`).

Correction 60 keys the drop rule off the route prefix:
`SCHEMA_ERROR_PREFIX = "Tool arguments failed schema validation"`
(`bin/assemble/src/main.rs:600-617`). The parse-fabricated text does
not start with that prefix. The truncated group therefore survives in
every request form, kept region included, until a compaction or a
trim drop removes it. The group is exactly the pattern correction 60
exists to kill: a `{}`-argument call plus a short error result, which
self-primes the next call (FT-008, `docs/failure-tracking.md`).

The plan's safety line is wrong on both counts:

- "Excluding the group excludes no orphan `function_call`" still
  holds: the group is self-contained.
- "Re-inclusion on a later request stays safe: correction 60 drops
  the schema-error pairs from every request form" does not hold: the
  text does not match the drop prefix.
- The section 5 test ("Re-inclusion without the flag drops the
  schema-error pairs (correction 60)") asserts a drop that does not
  happen.

Two sub-points. First, ordering: the strip and the retry must target
the truncated group, so the group lands in the log before the retry
request builds. The plan's e2e pins that ("the `{}`-argument group
lands in the log"), but the step list in 4.4 does not say the group
is logged first. Second, the plan describes a `route` step that does
not exist on this path.

Fix, chosen: add the truncation-notice prefix to the drop rule
(`bin/assemble`, `schema_error_pair_ids` matches two prefixes). The
group stays in the log, the pair goes out of every request form, and
correction 60's property extends to the truncated group. The state-
reset, boundary, and keep-window behavior all keep working: the drop
set is recomputed per projection. State the ordering (log the group,
then strip, then retry) and update the test to assert the extended
drop. The alternative (route the `{}` calls through the tool
manifests so `route` emits the FT-008 text) is heavier: it adds a
`tool_call` event to the group and changes the parse exit contract.
The two-prefix rule is the smaller surface.

### A3. The strip flag strips the last good group on error overflow (high)

The plan passes `--strip-last-assistant` / `--drop-last-assistant` on
the whole overflow path (4.2 interface, 4.4 step 5). The flag means
"exclude the last assistant group" as it exists in the log (4.3).
On an error overflow, the failed response is not in the log: the
error path skips `parse` and logs nothing
(`scripts/step.sh:188-205`). The last assistant group in the log is
the previous, valid step. The flag strips that step from the summary
input and the retry request. The retry loses the last step's context
and re-derives it. The strip is needed only for the length-stop case,
where the truncated group is logged before the retry.

Fix: the strip applies to the logged length-stop group only. The
error-overflow retry carries no flag. State the rule in 4.2 and 4.4.
Add a test: error-overflow retry keeps the last good group in the
request.

### A4. "The trim caps every request at the input budget" is false (medium)

The plan (4.2, finding 10, and the D8 wording in 8.1) states that
after trim engages, the trim form caps every request at the input
budget, so no model call can overflow the provider. The code does
not hold that cap. `decide_form` moves one lever per run. While the
keep window halves and while the blind crawl drops one group per run
(no compact measurement yet, `bin/assemble/src/main.rs:253-262`),
the request sent is the compact candidate at the current lever
position. Its size can exceed the input budget (229376): the keep
tail stays in the full form with unbounded reasoning items, and the
drop jump closes the deficit only over several runs. The code's own
comment names the real backstop for that window: "The provider
window is the backstop until the first measurement recalibrates the
drop jump" (`bin/assemble/src/main.rs:255-258`).

The correct backstop chain after trim engages:

- Provider window (262144). Requests between the input budget and
  the window succeed. That is the silent-overflow case.
- Silent-overflow classification: compact only, no re-run.
- Overflow error or recoverable length: compact plus one retry.
- Terminal handoff when the estimate is over budget at max drops.

The design stays safe. The written acceptance for the one-step-late
trigger (finding 10) rests on a guarantee the code does not give.
The silent-overflow path is load-bearing in the post-engagement
phase, not an edge for one provider family.

Fix: reword 4.2 and finding 10 to the chain above. Keep the
provider window in the backstop set. Add the post-engagement
silent-overflow e2e (section 5): a session past the trigger with the
trim form engaged, one successful call with measured input above
the input budget, assert compact-only and no re-run.

### A5. The summary prompt is contradicted inside the plan (medium)

Section 4.3 says the `--summary-input` mode "appends the summary-ask
user item and the handoff instructions". Section 4.2 says the prompt
is the structured compaction format (Goal, Constraints, Progress,
Key Decisions, Next Steps, Critical Context), with the iterative
update prompt when a previous summary exists. The handoff
instructions are a different text
(`HANDOFF_INSTRUCTIONS`, `bin/assemble/src/main.rs`), written for a
session that dies, not for an in-session checkpoint.

If the mode ships with the handoff instructions, the structured
format, the preserve/add/move update rules, and the file-op list
merge (the Y1 fix) all silently stop applying. The mode reuses the
handoff request envelope (`model`, `instructions`, `input`, `tools`,
`max_output_tokens`). It must not reuse the handoff instruction
text.

Fix: state in 4.3 that the mode carries the compaction prompt: the
first-time format, or the update format when a previous
`compaction_summary` exists, including the previous file-op lists.

### A6. The last-resort handoff after a failed retry is under-specified (medium)

Plan 4.4: "A second overflow, or a failed compaction, falls through
to the existing terminal handoff". The existing handoff fires only
when `assemble` emits the `Exhausted` form: the estimate outgrows
the budget at the max drop count (`bin/assemble/src/main.rs:281-284`
via `decide_form`). On a failed retry the estimate is under the
budget. `assemble` emits a normal request. Nothing builds the
handoff summary request, and no `context_exhausted` marker lands.
The "fall through" has no mechanism.

Fix: `assemble` gains a force-handoff flag. The flag emits the
`context_exhausted` event with the handoff summary request built
from the current form, reusing the `Exhausted` builder
(`context_exhausted_event`). `step.sh` logs the marker and runs
`run_handoff` on the retry-failure path. The TUI `h` key path is
unchanged. State it in 4.3 and 4.4. Add the e2e: a failed overflow
retry lands the `context_exhausted` marker.

### A7. The statusline usage sum is three changes, not one line (low)

The plan (4.6) moves the `compaction_summary` usage sum into Phase 1
as "one-line type check". The statusline extension receives only
`assistant_message` events: its manifest declares
`kinds = ["assistant_message"]` (`ui_extensions/statusline/ext.toml:24`),
and the host forwards events by that kinds list
(`bin/tui/src/ext.rs:1036-1057`). At restart, the host re-sends
only usage-bearing `assistant_message` events
(`bin/tui/src/ext.rs:1074-1083`). The script's extractor filters on
`assistant_message` (`ui_extensions/statusline/statusline.sh:173`).
The sum needs all three: the manifest kinds, the host re-send set,
and the script filter.

Fix: name the three components in 4.6. Keep it in Phase 1.

### A8. The custom-summarizer cut cites a path that does not exist (low)

The plan accepts "no extension hook for a custom summarizer" and
points at "the `ext-rs` system" as the later path. `ext-rs` is the
UI extension layer: status, render, and notify caps
(`ext-rs/README.md`). No entry in that layer can run a model call or
inject a `compaction_summary` event. The cut itself is harmless
today: the built-in prompt is the only path. The stated future path
is not one.

Fix: reword the acceptance. The later path is a config-level prompt
override, or a producer hook when the need lands. No damage today
stays true.

### A9. The overflow table has no SGLang or DeepSeek log evidence (low)

The plan's risk note says the pattern table extends "from
`sessions/` error events" with "the SGLang and DeepSeek phrasings
observed in this repo's logs". The session logs hold no provider
overflow error. The only model-API error event in `sessions/` reads
"model API call failed after retries: no detail"
(`sessions/better-ui/events.jsonl`). The rest of the context-
related errors are the harness's own budget messages.

The table starts from pi's regexes (`pi-ai/src/utils/overflow.ts:37-64`).
The SGLang overflow message ("This model's maximum context length is
N tokens. However, your messages consumed M tokens.") matches pi's
OpenRouter pattern `/maximum context length is \d+ tokens/i`. The
DeepSeek phrasing is unverified. The table needs a live probe per
provider before Phase 3, not a log extension.

Fix: reword the risk note. State the SGLang match. State the probe
as a Phase 3 gate.

### A10. The truncation-stop threshold is unstated (low)

Plan 4.4 classifies the "truncation stop" as "a `length` stop, zero
output, input filling the window". Pi pins the check: zero output
and input at or above `0.99 * contextWindow`
(`pi-ai/src/utils/overflow.ts:152-159`). The plan leaves the
fraction open. Pin it: zero output, measured input at or above
`0.99 * context_tokens` (the model's window, not the input budget).

### A11. `compact.json` gains a second writer (low)

`bin/assemble` documents the state file as single-writer
("One writer: assemble runs in the step loop",
`bin/assemble/src/main.rs:153`). The cooldown field (4.2) makes
`compact` a second writer. Two writers are safe in the serial step
loop, but the contract is now: `compact` read-modify-writes,
preserving `caps`, `keep`, `drops`, and the measurement fields.
Both readers tolerate the other's fields. The version bump covers
the new key. The plan states none of this.

Fix: one paragraph in 4.2 on the writer contract. Both binaries
keep their readers backward compatible.

### A12. Small spec gaps

- Summary-call `length` stop. A truncated summary lands in the log
  as a valid summary. Pi accepts the same outcome
  (`compaction.ts`: only an `error` stop throws). State it in 4.2:
  a `length` stop on the summary call is a truncated summary, not a
  failure.
- E2E wording. "The loop continues in the trim form to `idle`"
  (section 5, compact-failure case) assumes the trim form engaged.
  At the trigger level, below the trim budget, the form is still
  full. Say "in the current form".
- `reason` enum. The schema lists `threshold` and `overflow`. Pi
  adds `manual`. The harness has no manual trigger. The enum is
  correct as stated. No change.

## 2. What checks out

Verified against the current code and the pi 0.84.2 source. No
change needed.

- Section 1 (pi facts): the two trigger cases, the abort asymmetry,
  the model-switch guard on the overflow case only, the stale-usage
  guard, the one-recovery-per-turn limit, the `stop_reason` split
  (silent overflow compacts without a re-run), the defaults
  (`reserveTokens` 16384, `keepRecentTokens` 20000, enabled by
  default), `settings.json` global and project, the summary caps
  (`0.8 * reserveTokens`, clamped to the model max output), the
  isolated summary request, the cut rules (never at a tool result,
  split turns), the iterative update prompt, the file-op carry in
  the compaction entry, the `buildSessionContext` reload in place,
  the post-run loop (`while (postAgentRun) agent.continue()`), the
  events and extension hooks, the chars/4 estimator
  (`pi-ai/src/utils/estimate.ts`).
- Section 2 (repo facts): the `turn.sh` loop with no step cap, the
  `step.sh` `awaiting_model` order, the `run_handoff` seed and
  marker, the sticky compact form with frozen caps and the
  token-driven levers, the 262144 knob clamped to the 229376 input
  budget, the bounded model retries, the terminal empty-`length`
  error, the TUI `context_exhausted` render and the `h` key
  (`bin/tui/src/app.rs:1028-1038`).
- The first-review fixes: the D1-D9 and Y1-Y3 dispositions all
  landed where the plan records them. The path split by
  `stop_reason`, the bounded retry plus cooldown, the
  `reasoning_effort` request field and the `bin/model` change in the
  phase list, the request-JSON model guard, the `--summary-input`
  mode holding the projection core, `tokens_after`, the
  interrupted-marker render, the tool-log pointer reword, the
  file-op carry, and the statusline sum in Phase 1.
- `claim` ignores unknown types via its `_ => {}` branch
  (`bin/claim/src/main.rs`), including all three markers.
- The TUI loop liveness: `loop_running` flips on `LoopLine::Exited`
  (`bin/tui/src/app.rs:720-769`). The interrupted-marker render is
  implementable as stated.
- `bin/model` carries no `model` field on a failed-call output
  (`bin/model/src/main.rs`), and `assemble` writes `model` into
  every request. The D4 guard source holds.
- The e2e stub style exists: corrections 51-54 used stub model
  runs (`docs/loop-and-edit-implementation-corrections.md:423,439`).
- The trigger arithmetic: 229376 input budget minus 16384 reserve is
  202992, one reserve below the trim budget. LLM compaction leads.
  The clamp warning for an inverted config is stated in 4.5.

## 3. Required fixes, in order

1. A1: the `bin/log` schema list gains three names. Phase 1.
2. A2: the drop rule matches the truncation-notice prefix. The
   truncated group logs before the retry. The test asserts the
   extended drop.
3. A3: the strip flag applies to the logged length-stop group only.
4. A4: the backstop chain wording, the post-engagement
   silent-overflow e2e.
5. A5: the `--summary-input` mode carries the compaction prompt, not
   the handoff instructions.
6. A6: the force-handoff flag on `assemble`. The retry-failure
   e2e.
7. A7: the three statusline components named.
8. A8-A12: the rewords and the small spec pins.
