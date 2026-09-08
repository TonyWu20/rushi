# Auto-compact + loop continuation: implementation plan

Status: reviewed, revised, audited. Reviews: `docs/auto-compact-plan-review.md`,
`docs/auto-compact-plan-audit.md`, and `docs/auto-compact-plan-audit-2.md`.
Third audit: B1-B6 in `docs/auto-compact-plan-audit-2.md`, all accepted.
The fixes landed in sections 4-7 (section 8.3).
Research base: pi coding agent 0.84.2 source in the nix store, this
repo's loop and compaction code, and the closed handoff note
(`docs/handoff-compaction-request-format.md`).

## 1. What pi does

All facts below come from the pi 0.84.2 source:
`@earendil-works/pi-coding-agent/src/core/compaction/compaction.ts`,
`src/core/agent-session.ts`, `src/core/session-manager.ts`,
`@earendil-works/pi-ai/src/utils/overflow.ts`.

### 1.1 Trigger points

pi checks after every agent run and before every prompt submission.
The check is one function: `_checkCompaction` (agent-session.ts).
It has two cases.

Case 1: overflow or recoverable length stop.
The model call failed on the context window, or it stopped with
`length` below its desired output limit. Detection has three forms:

- An error message that matches a provider overflow pattern. pi
  keeps a regex table (`OVERFLOW_PATTERNS`) per provider, plus an
  exclusion table (`NON_OVERFLOW_PATTERNS`) so rate limits never
  count.
- Silent overflow: a successful call whose measured
  `usage.input + cacheRead` exceeds the context window.
- A `length` stop with zero output and input filling the window
  (server-side truncation style).
- Recoverable length: a `length` stop with output below the
  configured max output. The response is a truncated generation.

Case 2: threshold.
Estimated context tokens exceed `contextWindow - reserveTokens`.
Defaults: `reserveTokens = 16384`, `keepRecentTokens = 20000`,
compaction enabled by default. The estimate is the last valid
measured usage plus a chars/4 estimate of messages appended after
that usage. No measurement at all: skip.

Guards:

- A `length` stop that filled the model's desired output limit is
  not recoverable. It is a normal stop.
- A message older than the last compaction entry never re-triggers
  compaction (stale usage guard).
- Aborted messages skip the post-run check. The pre-prompt check
  includes them.
- One overflow recovery attempt per turn.
- The model-switch guard gates the overflow case only. The
  threshold case has no model guard.

### 1.2 Compaction mechanics

- `prepareCompaction` finds the boundary of the previous
  compaction entry. The work region is the entries after that
  boundary.
- `findCutPoint` walks backwards from the newest entry,
  accumulating estimated tokens (chars/4), until it reaches
  `keepRecentTokens`. It cuts at the nearest valid boundary: a user
  or assistant entry. It never cuts at a tool result, so a call and
  its result stay together. A cut inside a turn is a split turn:
  the turn prefix gets its own short summary, merged into the main
  summary.
- The summary is one LLM call per region. A split turn adds a
  second call for the turn prefix at a `0.5 * reserveTokens`
  budget, merged into the main summary. Both caps clamp to the
  model max output. Input: the old region serialized as text, a
  structured prompt, and the previous summary if one exists
  (iterative update prompt: preserve, add, move items from In
  Progress to Done). Output cap: `0.8 * reserveTokens`, clamped
  to the model max output. The call runs at the session thinking
  level, with the session retry policy, in an isolated request
  (fresh session id, no cache write).
- Summary format: Goal, Constraints & Preferences, Progress
  (Done / In Progress / Blocked), Key Decisions, Next Steps,
  Critical Context. The prompt requires exact file paths,
  function names, and error messages to survive.
- File operations are extracted from tool calls (read files,
  modified files) and appended to the summary as lists. They also
  carry into the next compaction.
- `sessionManager.appendCompaction` persists one entry: summary,
  `firstKeptEntryId`, `tokensBefore`, usage, file lists.
- `buildSessionContext` rebuilds the message list: the compaction
  summary as one message, then the kept entries from
  `firstKeptEntryId` up to the compaction entry, then the entries
  after it. The agent reloads that state. The session continues in
  place. Same session file. No new session.

### 1.3 Loop continuation

After a post-run check:

- Overflow case: pi removes the failed or truncated assistant
  message from the live state (it stays in the session file),
  compacts, then auto-retries the interrupted turn once with
  `agent.continue()`. The model resumes the tool loop that the
  overflow cut. No user input.
- Threshold case: pi compacts and does not auto-retry. Queued
  steering or follow-up messages trigger one continuation. The
  user's next prompt continues the task.
- The post-run loop is `while (postAgentRun) agent.continue()`.
  Compaction returning true keeps the loop alive.

### 1.4 Settings, events, UI

- Settings: `compaction.enabled`, `compaction.reserveTokens`,
  `compaction.keepRecentTokens` in `settings.json` (global and
  project). A toggle in the settings UI.
- Events: `compaction_start` / `compaction_end` with reason
  (`manual` | `threshold` | `overflow`), the result, and an error
  message on failure. Extensions hook `session_before_compact`
  (cancel or supply a custom result) and `session_compact`
  (notify).
- UI: a status indicator while compaction runs. The summary renders
  as a collapsible message in the transcript.

## 2. What the harness does today

- `turn.sh` loops `step.sh` until `claim` reports `idle` or
  `exhausted`. No step cap (correction 51).
- `step.sh`, `awaiting_model` branch: `assemble` projects the log
  to a request. A budget error logs an `error` event. A
  `context_exhausted` request runs `run_handoff`: one summary call,
  a new session `<name>_h<n>` seeded with the summary as a user
  message, a `context_exhausted` marker on the old session. The
  loop breaks.
- `assemble` compaction is heuristic only: sticky compact form
  (correction 62). Frozen caps, keep window halving, drop jumps,
  all token-driven. No LLM summary. It engages only when the
  predicted tokens outgrow `context_budget_tokens`. Today that
  knob holds 262144. `assemble` clamps it to the input budget of
  229376.
- The model call has bounded retries for transport errors and
  empty-turn glitches. A `length` stop with no content logs a
  terminal error. No overflow detection. No recovery.
- The TUI shows the `context_exhausted` marker. One key press
  (`h`) switches to the seeded session and restarts the loop.

## 3. The gap

pi compacts proactively, in the same session, and continues the
loop. This harness trims reactively, dies at the wall, and hands
the task to a new session behind a key press.

- No threshold trigger. Trimming is the only context management.
- No LLM summary inside the session. The summary call runs only in
  the terminal handoff, and it seeds a new session.
- No overflow recovery. An overflow error logs an error event and
  stops the turn.
- No auto-continue. Every context event ends the loop and waits for
  user action.

## 4. Design

Principles: the log stays append-only. Compaction is a new event.
Projection is deterministic. Each concern stays in one binary.
Bash glues the binaries. JSON on the wire. No shared crate in
Phase 1.

### 4.1 New event types: three markers

Three schemas under `schemas/events/v1/`. `claim` ignores all
three (its `_ => {}` branch). They are no-ops for the state
machine.

`compaction_started.json`: marks the in-flight summary call.
Fields: `reason`, `tokens_before`. The TUI renders it live. The
following `compaction_summary` event closes it.

`compaction_failed.json`: marks a failed summary call. Fields:
`reason`, `detail`, `last_user_seq`. The field holds the seq of the
log's last `user_message` at failure. The threshold check reads it
as the cooldown source (section 4.2). The step proceeds. No
terminal effect.

`compaction_summary.json`:

```json
{
  "type": "object",
  "required": ["v", "type", "ts", "summary", "first_kept_seq",
               "reason", "tokens_before"],
  "properties": {
    "v": { "type": "integer", "const": 1 },
    "type": { "type": "string", "const": "compaction_summary" },
    "ts": { "type": "string", "format": "date-time" },
    "summary": { "type": "string" },
    "first_kept_seq": { "type": "integer", "minimum": 1 },
    "reason": { "type": "string", "enum": ["threshold", "overflow"] },
    "tokens_before": { "type": "integer" },
    "tokens_after": { "type": "integer" },
    "read_files": { "type": "array" },
    "modified_files": { "type": "array" },
    "usage": { "type": "object" }
  }
}
```

- `first_kept_seq`: the 1-based log sequence of the first event
  the summary does not cover. Projection replaces every earlier
  event with the summary.
- `reason`: which trigger fired.
- `tokens_before`: the measured input tokens at trigger time.
- `tokens_after`: the estimator's value for the post-compaction
  request. It sources the TUI's second number.
- `read_files`, `modified_files`: the file-ops lists. They
  accumulate across compactions. The next compaction carries them
  forward. Without the carry, the lists erode one compaction at a
  time: after the first compaction, the old region no longer
  holds the old tool calls.
- `usage`: the token usage of the summary call itself. The TUI accounts for it in Phase 1 (section 4.6).
- `enum` and `minimum` are outside the validators' supported
  subset (`const`, `required`, `properties`, `items`, primitive
  `type`). Both validators skip the unsupported keys silently.
  Enforcement moves to the producers: `compact` checks `reason`
  against the two values and `first_kept_seq >= 1` before
  calling `log`. `assemble` rejects a boundary value it cannot
  parse, and re-renders without the summary.
- Add a test that proves all three types leave `awaiting_model`
  and `idle` tails unchanged.
- The TUI `port_file` validator loads the schema file by type
  name at append. It picks the new types up with no code change.
  `log` validates against a hardcoded list of schema names
  (`bin/log`). The three new names join that list in Phase 1.
  Without the entry, `log` rejects every marker as an unknown
  event type. The TUI event parser and renderer gain the types in
  section 4.6.
- The tool log pointers stay valid for the kept region only. The
  old region leaves the request after a compaction. Its tool-log
  bodies lose every pointer. The model can re-read files. It
  cannot re-fetch an old tool output. The file-ops lists mitigate
  by naming the files and commands.

### 4.2 New binary: `bin/compact`

One concern per binary, per the layout in
`docs/loop-and-edit-implementation.md`. The trigger check and the
summary call need the model. `assemble` stays a pure projection.
The `--summary-input` mode (section 4.3) keeps the projection core
in one place.

Interface:

```
compact --session <dir> --config <config>
        [--reason threshold|overflow]
        [--strip-last-assistant]
        [--force]
```

- `--reason threshold` (default): run the trigger check. Not
  triggered: print a `noop` JSON line, exit 0. Triggered: append
  the `compaction_started` marker, run the summary call, append
  the `compaction_summary` event via `log`, exit 0.
- `--reason overflow`: skip the trigger check. Run the summary
  call. `--strip-last-assistant` excludes the logged truncated
  group from the request. The failed response of an error overflow
  is not in the log. The strip targets the length-stop group only
  (section 4.4).
- `--force`: skip the trigger check and the cooldown. Run the
  summary call now. The last-resort compaction (section 4.4) uses
  this flag.
- Summary call failure: two attempts. A failed attempt is a
  non-zero model exit, an `error` stop reason, or an empty
  summary after trimming. After the second failure, append the
  `compaction_failed` marker with `last_user_seq`, print the
  detail to stderr, exit 1. No terminal
  event. A terminal `error` event kills the loop: `claim` maps
  `error` to `idle`, and `turn.sh` breaks. Cooldown: the field
  holds the seq of the log's last `user_message` at failure. The
  threshold check skips while the log's last `user_message` seq is
  at or below the recorded value. The overflow path ignores the
  cooldown. It is the last-resort recovery. A `length` stop on the
  summary call is a truncated summary. It is not a failure. The
  truncated text becomes the summary, pi parity. A zero-output
  `length` stop leaves an empty summary. That is a failure. The
  state file keeps a single writer: `assemble` only. No second
  writer joins it.

Trigger check (threshold mode), in token space:

- Threshold: `context_budget_tokens - compact_reserve_tokens`,
  with the budget clamped to the input budget exactly as
  `assemble` does today (window minus `max_output_tokens`). The
  trigger sits one reserve below the trim budget. LLM compaction
  leads. The trim form is the backstop.
- Trigger-based readings: measured input tokens of full-form
  requests only. The range: between the last compaction boundary
  and the trim engagement (the state's `engaged_at`), or all
  measurements when no state file exists. Shrunken compact-form
  readings never feed the trigger.
- Base: the last trigger-based reading, at event index `i`, value
  `t`. Growth: the token delta over event delta of the last two
  trigger-based readings. Reuse the `decide_form` algorithm. A
  small copy is acceptable under the Phase 1 no-shared-crate
  policy. Note it as a promotion itch.
- Fire when `t + rate * (n_events - i) > threshold`, or when
  `t >= threshold`. The level clause keeps the trigger alive
  under an engaged trim form. Without it, shrunken compact-form
  readings read as zero growth. The LLM compaction starves while
  the trim form holds the session on 500-char previews.
- Fire when the trim form has engaged at or after the last
  compaction boundary. The guard covers the single-reading case
  the level clause misses: growth crosses the budget while the
  last reading still sits under the threshold.
- No trigger-based reading: no trigger. A fresh session sends the
  full log. The provider window is the backstop for that first
  request only. After trim engages, the backstops are the provider
  window, the silent-overflow path, and the terminal handoff. The
  trim form caps the estimate, not the request. Each lever move is
  one step. The sent request can outgrow the input budget while
  the levers catch up. A successful call with measured input
  above the input budget is the silent-overflow case. It compacts
  without a re-run.

Cut point:

- Walk backwards from the last event, accumulating estimated
  tokens of the projected items (chars/4, the same estimate pi
  uses), until `compact_keep_tokens` is reached.
- Snap the cut to a step group boundary (the `step_groups`
  rule: a group is one assistant message plus its tool results. A
  user message is a group. A cut never lands between a call and
  its result).
- An empty old region is a no-op. A trigger with nothing to
  summarize exits 0 without a model call. pi returns an empty
  preparation for the same case.
- Never orphan a user message: if the cut leaves a user message
  as the last event of the old region, pull that event into the
  kept region. pi handles the same case with a second
  prefix-summary call. The pull is the cheaper equivalent here.

Summary request:

- Old region: events after the previous boundary and before the
  cut point, projected in the compact form by the `assemble`
  `--summary-input` mode (section 4.3). The mode appends one
  summary ask, no tools, a capped output. It reuses the handoff
  `summary_request` envelope: `model`, `instructions`, `input`,
  `tools`, `max_output_tokens`. The instructions are the
  compaction prompt, not the handoff instructions. The first-time
  format, or the update format with the previous summary and its
  file-op lists.
- Caps and drops: the caps are the sticky state's frozen caps, or
  the base caps when no state. The drop count is the search max
  bounded by the input budget (the handoff invariant).
- Iterative merge: when a previous `compaction_summary` exists,
  include its summary in the request and switch to the update
  prompt (pi's preserve/add/move rules). The prompt carries the
  previous event's `read_files` and `modified_files` lists. The
  lists merge, not re-extract. The new event supersedes the old
  one.
- Output cap: `compact_summary_max_tokens`, default
  `0.8 * compact_reserve_tokens` (pi's ratio), clamped to the
  model max output.
- Prompt: the structured format of section 1.2, adapted: Goal,
  Constraints, Progress (done / in progress / blocked), Key
  Decisions, Next Steps, Critical Context. Require exact file
  paths, commands, and error messages. Extract the read and
  modified file lists from the old region's tool calls and append
  them (pi's file-ops block).
- Send through `model` exactly like `run_handoff` does today:
  stdin request JSON, stdout result JSON.
- Invariant: the summary input fits the input budget at the drop
  cap. The drop search bounds the old region. The previous
  summary carries at most one output cap. A test pins it
  (section 5).
- The no-op path reads only `events.jsonl`. The tool log is read
  only on a triggered run. The per-step cost stays the event-log
  read that `assemble` already pays.

### 4.3 `bin/assemble`

- Read the last `compaction_summary` event. When present:
  items = one user-role framing item carrying the summary, then
  the projected events from `first_kept_seq` onward.
- The sticky compact form (caps, keep window, drops) applies to
  the kept region only. The drop search and `max_drops` compute
  over that region. The mechanism itself is unchanged.
- The terminal `context_exhausted` handoff retires. The
  `Exhausted` form now signals the last-resort in-session
  compaction (section 4.4). The summary replaces the old region in
  the original session. No new session. No `context_exhausted`
  marker. Legacy markers keep their render and the `h` key.
- Prefix stability holds: the summary item is frozen in the log.
  Between compaction moves the request prefix is byte-stable
  (correction 62 property, preserved).
- New flag `--drop-last-assistant`: request-time exclusion of the
  last assistant group. Nothing is persisted. `step.sh` uses it
  for the overflow retry request (section 4.4).
- New mode `--summary-input --up-to <seq>`: project the events
  from the last boundary to `up-to` in the compact form, append
  the summary-ask user item with the compaction instructions, and
  print the bare model request JSON. The instructions are the
  first-time prompt, or the update prompt with the previous
  summary and its file-op lists. Not the handoff instructions. No
  budget decision. No state write. `bin/compact` runs the mode
  and pipes the output to `model`. This keeps the projection core
  in one binary. The summary input copies nothing from `assemble`.
- No `--force-handoff` flag. The retry-failure path runs the
  last-resort in-session compaction (section 4.4). The `Exhausted`
  arm of `assemble` keeps printing its signal. `step.sh` swaps
  `run_handoff` for the last-resort compaction. `run_handoff` and
  the new-session seed retire with the handoff.
- Boundary-aware state: when the last `compaction_summary` event
  is newer than the `compact.json` state, `assemble` re-engages
  the state fresh at the boundary index: zero measurements, zero
  drops, the full form. Pre-compaction measurements poison the
  growth rate and the drop search.

### 4.4 `step.sh` and `turn.sh`: the loop continues

`awaiting_model` branch, new order:

1. `compact --reason threshold`. No-op when not triggered, or
   when the cooldown from a failed summary call is active. When
   it compacts, the next step below projects through the
   boundary. On failure, the marker event, and the step
   proceeds.
2. `assemble` (with `--drop-last-assistant` on the length-stop
   retry request only. The error-overflow retry carries no strip,
   the failed response is not in the log). When `assemble`
   prints the `Exhausted` form, skip the model call. Run the
   last-resort compaction: `compact --reason threshold --force`.
   On success, the next step projects through the boundary. On
   failure, the marker, and the step proceeds in the current
   form.
3. The model call, in the existing retry loop.
4. Failure classification, before the transport retry:
   - Overflow: `stop_reason = error`, and the detail matches the
     overflow pattern table (port pi's regexes, extend from live
     probes of the SGLang and DeepSeek error shapes. The session
     logs hold no provider overflow error, section 6). Exclude
     the non-overflow patterns (rate limits). An `error` stop
     with no detail, or a detail that matches no pattern, is not
     recoverable. The call takes the transport path: the bounded
     retries, then the terminal `error` event. A provider can
     report overflow through an SSE `response.failed` event that
     holds no detail. The table cannot see that case.
   - Silent overflow: a successful call whose measured input
     tokens meet or exceed the input budget.
   - Truncation stop: `length` stop, zero output, measured input
     at or above `0.99 * context_tokens` of the model window
     (pi's ratio).
   - Recoverable length: `length` stop with output below the
     configured max output. That includes the empty-content case
     today's script logs as terminal. New: recoverable, once.
   - Model guard: classify only when the request model matches
     the active config model. The source is the request JSON in
     the work dir: `assemble` writes `model` into every request.
     `parse` does not run on a failed call. The failed-call
     output carries no model field. The guard has little value
     in this harness: classification is live inside one step. A
     config model switch mid-step is not a race. The plan keeps
     the guard as a cheap check.
5. On overflow or recoverable length, split by `stop_reason`:
   - Silent overflow: a successful `stop` call with usage over
     the input budget. Compact only. No re-run. The valid
     response is on the log. The loop continues by its own state
     machine: routed tool calls, or `idle` at a final answer. pi
     makes the same split: it retries only when the stop reason
     is not `stop`.
   - Error overflow, truncation stop, recoverable length:
     `compact --reason overflow` (with `--strip-last-assistant`
     when a truncated group is logged), then re-run the model
     call once. The strip applies to the logged length-stop group
     only. An error-overflow retry carries no strip: the failed
     response is not in the log, and the last group is a valid
     step. Stripping it would lose that step's context. This is
     the auto-continue: the interrupted turn resumes in the same
     session. When `compact_enabled` is off, the path skips the
     overflow compaction and runs the last-resort compaction.
   - One attempt. A second overflow, or a failed compaction,
     runs the last-resort compaction: `compact --reason overflow
     --force` (the strip rule still applies), then one more model
     call. A second failure appends a terminal `error` event. The
     loop stops in the original session. No new session. No
     `context_exhausted` marker. The user reopens the same
     session.
6. On success: `parse`, route, log. The loop continues as today.
   `claim` still reports `awaiting_model` after a `tool_result`.
   The marker events are no-ops. No key press.

Length-stop log shape:

- A `length` stop cuts the stream mid-arguments. `parse` cannot
  keep the partial JSON. It substitutes `{}` for the arguments
  and emits the call. `parse` then emits one `tool_result` per
  call with the fixed truncation notice. It names the truncation
  and asks for a shorter call. `route` does not run on this
  path.
- The group is an assistant message, `{}`-argument calls, and the
  truncation-notice results. It is self-contained. Excluding the
  group excludes no orphan `function_call`.
- The group lands in the log before the retry. The strip and the
  retry request build from the log, and the truncated group is
  the last one.
- Re-inclusion on a later request stays safe: correction 60's
  drop rule matches two result prefixes, the schema-validation
  prefix and the truncation-notice prefix. Both pair shapes go
  out of every request form.

`turn.sh` is unchanged: it stops only on `idle` and `exhausted`.

The threshold hook also runs before a request, not only after a
step. That covers the first request after a new user message in an
idle session.

### 4.5 Config: new knobs in `[limits]`

- `compact_enabled = true` (pi parity kill switch). It gates both
  paths. Off sends the overflow path to the last-resort
  compaction without the overflow compaction.
- `compact_reserve_tokens = 16384` (pi default)
- `compact_keep_tokens = 20000` (pi default)
- `compact_summary_max_tokens = 13107` (0.8 * reserve)
- `compact_reasoning_effort`: default is the session effort
  (`reasoning_effort`). A cheaper value cuts the summary latency
  on a local GPU. Mechanism: the summary request carries an
  optional `reasoning_effort` field. `bin/model` prefers the
  request value over the config. The Phase 1 list carries the
  `bin/model` change.
- `compact_trigger_base = "input_budget"` (default) or
  `"context_budget"` (pi parity). The `input_budget` base sits the
  trigger at the clamped input budget minus
  `compact_reserve_tokens`: one reserve below the trim budget, so
  the LLM compaction leads. The `context_budget` base sits the
  trigger at the full `context_budget_tokens` minus the reserve
  (pi's `contextWindow - reserveTokens`; 245760 for the 262144
  window). Under that base the trigger estimate adds the full-form
  estimate of the kept region (chars/4 since the last compaction
  boundary), because the measured readings of the clamped
  (trim-form) request read shrunken and starve the LLM
  compaction. The silent-overflow backstop follows the base: it
  compares the measured input to the input budget (default) or to
  the full context budget (pi parity, the provider wall).
  The `assemble` wire budget follows the base: the default base
  clamps it to the input-only window (`context_tokens -
  max_output_tokens`), which reserves space for output. The
  `context_budget` base clamps it only to the model window, so the
  `context_exhausted` gate and the summary call run on the full
  window. That matches pi, which does not reserve `maxTokens` from
  the window.

`context_budget_tokens` stays the cap of the reactive trim form.
The trigger sits at `context_budget_tokens - compact_reserve_tokens`.
Ordering: LLM compaction leads, the trim form is the backstop
inside the budget, the provider window is the wall, the handoff
is the last resort. A config that puts the trigger at or over the
budget inverts the order and starves the LLM compaction.
`compact` prints a warning and clamps the trigger to one reserve
below the budget.

### 4.6 TUI

- Event parser: add the three new types to the typed event enum.
  Tolerance tests, same shape as the existing `context_exhausted`
  regression test in `event.rs`.
- Renderer: the `compaction_started` line shows
  `compacting (threshold): 212k tokens` while the summary call
  runs. The `compaction_summary` line shows
  `context compacted (threshold): 212k to 33k tokens, keeping
  events from seq 312` (`tokens_after` sources the second
  number), with the summary body available expanded. A
  `compaction_failed` line shows the detail. An open
  `compaction_started` marker with the loop process not running
  renders as `compacting (interrupted)`. The TUI supervises the
  loop command. It knows when the loop dies. A restarted loop may
  add more open markers. The renderer closes the first one.
- Statusline extension: sum the `compaction_summary` usage into
  the cumulative totals alongside the `assistant_message` usage.
  Three components: the statusline manifest gains
  `compaction_summary` in its `kinds` (the host forwards events
  by the kinds list), the host re-send set gains the type at
  restart, and the script's usage filter accepts it. Phase 1.
- The `h` key serves legacy `context_exhausted` markers only. The
  new flow appends no marker and seeds no session.

## 5. Verification

Unit tests, per binary:

- `assemble`: the boundary projection. The summary replaces the
  old region. The kept region is byte-identical to the pre-feature
  output. The prefix is byte-stable between two compaction moves.
  A step group never splits across the cut.
- `assemble` reset: a `compaction_summary` event newer than the
  `compact.json` state re-engages the state fresh. Pre-compaction
  measurements do not leak into the growth rate or the drop
  search.
- `claim`: all three marker types leave `awaiting_model` and
  `idle` tails unchanged.
- `compact`: trigger arithmetic (no-op below threshold, trigger
  above, stale guard after a boundary), cut point snapping,
  iterative merge of the previous summary.
- `compact` trigger starvation: the trim form engaged, shrunken
  readings follow, one trigger-based reading sits above the
  threshold. The trigger fires on the level clause. A
  prediction-only check does not fire. One trigger-based reading:
  the trigger runs one step late. An empty old region: a no-op
  without a model call. A config that inverts trigger and budget:
  the warning fires, the trigger clamps.
- `compact` failure: the `compaction_failed` marker, no terminal
  event, exit 1.
- `compact` invariant: at the drop cap, the summary request input
  fits the input budget.
- `compact` cooldown: two attempts per run. The final failure
  lands a `compaction_failed` event with `last_user_seq`. The
  threshold check skips for the rest of the turn. The next user
  message re-arms it. The state file holds no cooldown field. A
  lever move in the same step cannot drop the source.
- `compact` empty summary: a zero-output `length` stop takes the
  `compaction_failed` path. No `compaction_summary` event lands.
- `compact` trim-engaged guard: the state file records trim
  engagement with one trigger-based reading. The trigger fires on
  the guard clause.
- `compact` cut point: a cut that would leave a user message as
  the old region's tail pulls it into the kept region.
- `assemble` summary-input mode: the byte-stable projection. The
  caps are the frozen state caps, or the base when no state. The
  drop search bounds the input at the input budget.
- Producer-side checks: `compact` rejects a `reason` outside the
  enum and a `first_kept_seq` below 1 before the append. The
  schema keywords stay for documentation. They are not
  enforced by the validators.
- `assemble` flag: `--drop-last-assistant` leaves no orphan
  `function_call` in the request. Re-inclusion without the flag
  drops the schema-error pairs and the truncation-notice pairs
  (correction 60, two prefixes). The request stays valid.
- `bin/log`: the three marker types validate through the
  hardcoded schema list. A missing list entry rejects the type
  as unknown.
- Strip rule: the error-overflow retry keeps the last good group
  in the request. The strip flag is the length-stop path only.
- Last-resort compaction: `compact --reason threshold --force`
  runs without a trigger and without a cooldown. The
  `compaction_summary` event lands in the original session. No
  `context_exhausted` marker. No new session.
- `step.sh` classification: table tests over stub model outputs.
  Every overflow pattern, the exclusions, silent overflow (compact
  only, no re-run), the two `length` cases, the model guard (the
  request JSON is the source), the no-match case, and the
  no-detail case (an `error` stop with no detail takes the
  transport path to the terminal `error` event).
- `step.sh` kill switch: `compact_enabled` off sends the overflow
  path to the last-resort compaction without the overflow
  compaction.

Script e2e, with the stub model binary (the existing stub-run
style used for corrections 51-54):

- Threshold case: a session that crosses the trigger level
  mid-turn. Assert: one `compaction_summary` event, the next
  request carries the summary plus the kept region, the loop runs
  to `idle` with no handoff and no `context_exhausted`.
- Overflow case: the stub returns an overflow error once, then a
  normal answer. Assert: one `compaction_summary` with
  `reason = overflow`, one retry, no handoff.
- Silent-overflow case: the stub returns a successful `stop` with
  usage over the input budget. Assert: one `compaction_summary`,
  no model re-run, the loop continues by its state machine.
  Post-engage variant: the session is past the trigger and the
  trim form is engaged. The stub returns a successful `stop`
  with usage over the input budget. Assert: one
  `compaction_summary`, no model re-run.
- Length-stop case: the stub stops with `length` and partial
  tool-call arguments. Assert: the `{}`-argument group lands in
  the log, one `compaction_summary` with `reason = overflow`, one
  retry, no terminal error event. The re-included request drops
  the truncated pair (the two-prefix rule).
- Compact-failure case: the stub summary call fails. Assert: the
  `compaction_failed` marker, no terminal event, the loop
  continues in the current form to `idle`.
- Empty-summary case: the stub summary call stops with `length`
  and zero output. Assert: the `compaction_failed` marker, no
  `compaction_summary` event, the loop continues in the current
  form to `idle`.
- Iterative case: two trigger levels in one session. Assert: the
  second summary request carries the first summary.
- Last-resort case: force the kept region over budget. Assert:
  one `compaction_summary` event, no `context_exhausted` marker,
  no new session, the loop runs to `idle` in the original
  session.
- Failed-retry case: the overflow retry fails. Assert: one more
  `compaction_summary` (the last-resort attempt). When it also
  fails, the terminal `error` event stops the loop in the
  original session. No `context_exhausted` marker. No new
  session.

Live: a long session against the SGLang model. Watch the first
compaction at the trigger level, the loop continuing without
input, and the second compaction merging the first summary.

## 6. Risks and open questions

- Summary quality on the local Qwen3.8-27B-NVFP4. The summary is
  the only context of the old region. Mitigations: keep
  `compact_keep_tokens` generous. Tool log pointers survive
  compaction, so the model can re-read any full body.
- Each compaction changes the request prefix once. That is
  unavoidable and matches pi. Between compactions the prefix is
  frozen.
- Estimator duplication: `bin/compact` copies the trigger math
  and the cut walk from `assemble`. The projection core stays in
  `assemble` through the `--summary-input` mode. No second copy
  of the projection. Track the estimator copy in
  `docs/itches.md` as a shared-crate candidate.
- The overflow pattern table starts from pi's regexes. The
  `sessions/` logs hold no provider overflow error. The repo has
  no observed SGLang or DeepSeek phrasing. The SGLang overflow
  message matches pi's OpenRouter pattern (`maximum context
  length is N tokens`). The DeepSeek phrasing is unverified. A
  live probe per provider gates Phase 3. The probe records the
  delivery shape per provider: an HTTP error body or an SSE
  `response.failed` event. A delivery without a detail is not
  recoverable by the table.
- `compact_keep_tokens` in tokens vs `compact_keep_events` in
  events: the token cut replaces the event keep window as the
  primary keep control. Keep the events knob for the trim form
  only.
- Schema keywords `enum` and `minimum` are outside the minimal
  validator subset (the TUI copy and the `log` copy both skip
  unknown keys). The producers self-check (section 4.1). Track
  the validator subset in `docs/itches.md`.

## 7. Phases

Each phase ships and tests standalone.

1. Events and projection. The three marker schemas (with
   `tokens_after`, the file-ops fields, and `last_user_seq` on
   `compaction_failed`), the `claim` no-op,
   the `assemble` boundary, the state reset, the
   `--summary-input` mode, the `--drop-last-assistant` flag, the
   `bin/log` schema-list entry, the two-prefix drop rule, the
   `bin/model` request-level
   `reasoning_effort` field, the TUI parser and renderer, the
   statusline usage sum, the config knobs. Testable without a
   model call: hand-write a `compaction_summary` event into a
   fixture log and assert the projection and the state reset.
2. Trigger and summary call. `bin/compact` with the trigger check
   (the level clause, the trim-engaged guard), the cut point, the
   iterative merge, the failure marker and the log-derived
   cooldown, the `--force` flag, the `step.sh` threshold hook.
   Stub-model e2e for the threshold, starvation,
   compact-failure, empty-summary, and iterative cases.
3. Overflow recovery. The classifier in `step.sh` (the patterns,
   the request-JSON model guard), the `stop_reason` path split
   (silent overflow compacts without a re-run), the `--reason
   overflow` path, the one-shot retry with the last assistant
   group excluded. The strip flag is limited to the length-stop
   path. The terminal empty-content-`length` error moves behind
   the recovery. The `Exhausted` form runs the last-resort
   in-session compaction through `compact --force`. The
   retry-failure path runs the last-resort compaction, one more
   model call, then the terminal `error` event. The
   `compact_enabled` off case skips the overflow compaction and
   runs the last-resort compaction. The no-detail case takes the
   transport path. Stub-model e2e for the overflow,
   silent-overflow (including the post-engage variant),
   length-stop, compact-failure, kill-switch, no-detail, and
   last-resort cases. The handoff retires. The session continues
   in place.
4. Live and polish. The live session run of section 5, the
   status line check (including the `compaction_summary` usage
   sum), the docs update, the itch entry.

## 8. Design review: YAGNI shortcuts and their damage

This section answers the review question: any shortcut under the
YAGNI principle that underestimates its damage to correctness or
performance? Findings, with disposition. The fixes land in
sections 4-5. Section 8.1 records the external review.

**Correctness damage found, fixed in the plan.**

1. Inverted trigger level. The first draft placed the trigger at
   `context_tokens - compact_reserve_tokens` (245760 tokens). The
   repo input budget is `context_tokens - max_output_tokens`
   (229376). The trim form engages at that budget. The trim
   engages first. Shrunken compact-form readings read as zero
   growth. The LLM compaction starves and never fires. The
   session rides 500-char previews to the handoff. The
   auto-compact dies silently. Fixed: the trigger sits at budget
   minus reserve, the trigger consumes trigger-based readings
   only, and the level clause fires under an engaged trim form.
   The `compact_trigger_base = "context_budget"` knob opts back
   into the pi-parity ordering on purpose: the trigger sits
   above the trim budget, and the full-form estimate keeps the
   LLM compaction from starving. The trim form is the degraded
   ride between the two; the provider wall is the final
   backstop.
2. The failure path kills the loop. The first draft logs a
   terminal `error` event on summary-call failure. `claim` maps
   `error` to `idle`. `turn.sh` breaks. The promised trim-form
   fallback is unreachable. Fixed: the `compaction_failed`
   marker. The step proceeds in the trim form.
3. Contaminated sticky state. `compact.json` holds absolute
   event indices. Pre-compaction measurements poison the growth
   rate and the drop search after a compaction. Fixed: the state
   re-engages fresh at the boundary.
4. The length-stop group shape. The first draft assumes the
   excluded group is a lone assistant message. `parse`
   substitutes `{}` for cut-off arguments and emits the call.
   `route` records the schema-error result. The group carries
   both. Excluding the whole group stays safe: correction 60
   drops the pairs from every request form. Now stated, with a
   test. The second audit (8.2, A2) corrects the log shape:
   `parse` fabricates the truncation-notice result. `route` does
   not run. The drop rule matches two prefixes.
5. Summary-input overflow. The old region, the previous summary,
   and the prompt must fit the input budget. The first draft
   leaves that unstated. Now an invariant with a test: the drop
   search bounds the input at the cap.
6. No model-switch guard. A stale overflow error from a smaller
   model triggers a compaction after a switch to a bigger one. pi
   has the guard. Now: `parse` records the model on the
   `assistant_message` event. The classifier compares.

**Performance damage found, fixed in the plan.**

7. Invisible stall. The first draft drops pi's `compaction_start`
   event as noise. The summary call on a local GPU can run for
   minutes. The TUI shows nothing. The user sees a hung loop.
   Now: the `compaction_started` marker event, rendered live.
8. Summary latency at the session reasoning effort. The summary
   call is the loop's longest stall. A new
   `compact_reasoning_effort` knob lets a local GPU run the
   summary cheap. The session effort stays the default.
9. Double log read per step. `compact` and `assemble` both read
   `events.jsonl`. Accepted: `assemble` already pays that read.
   The no-op path does not read the tool log.
10. One-step-late trigger on a single measurement. After a state
    reset, the first trigger check holds no rate. The level
    clause fires one step late. The backstops are the provider
    window, the silent-overflow path, and the terminal handoff.
    The trim form caps the estimate, not the request: the
    halving and blind-crawl phases can send a request over the
    input budget. The silent-overflow classification catches that
    window. The trim-engaged guard (section 4.2) closes the
    single-reading case. Accepted and documented. The second
    audit re-derives the chain (8.2, A4).

### 8.1 External review: `docs/auto-compact-plan-review.md`

The external review accepted all of its findings. The fixes
landed in sections 1 and 4-5.

Design-correctness findings, all accepted:

1. D1: silent overflow re-ran a finished turn. The path split by
   `stop_reason` (section 4.4 step 5).
2. D2: a failed compaction re-fired every step. Bounded retry
   plus the cooldown in `compact.json` (section 4.2).
3. D3: `compact_reasoning_effort` had no mechanism. The request
   field and the `bin/model` change now carry it (sections 4.5,
   7).
4. D4: the model guard could not read the model from the event
   log. The request JSON is the source (section 4.4).
5. D5: the projection copy spanned the whole compact form. The
   `assemble --summary-input` mode holds the core. The caps and
   drop rule are stated (sections 4.2, 4.3).
6. D6: no `tokens_after` for the TUI line. Field added
   (section 4.1).
7. D7: a crashed compaction left an open marker. The TUI renders
   it as interrupted when the loop is not running (section 4.6).
8. D8: the provider window is not the backstop after trim
   engages. The guard and the corrected wording landed in
   section 4.2.
9. D9: the tool-log pointer claim overstated. Reworded to the
   kept region only (section 4.1).

YAGNI audit findings, all accepted:

- Y1: the file-op lists eroded across compactions. The event
  carries the lists. The update prompt merges them (sections
  4.1, 4.2).
- Y2: the usage gap was a display matter, not small. The
  statusline change moved to Phase 1 (section 7).
- Y3: the one-step-late trigger rested on a missing backstop.
  Restated with the D8 guard (section 8 finding 10).

Research-section corrections from the review landed in section 1:
`settings.json` (not `settings.jsonl`), the aborted-message
asymmetry, the model guard on the overflow case only, and the
split-turn second call with the clamped cap.
11. Unenforced schema keywords. The new schemas use `enum` and
    `minimum`. Both repo validators support only `const`,
    `required`, `properties`, `items`, and primitive `type`.
    The keywords pass through unchecked. A bad producer value
    reaches the log. Fixed: producer-side checks in `compact`,
    a parse-time reject in `assemble`, and a note in the risks
    section.

**YAGNI cuts accepted, damage bounded.**

- No extension hook for a custom summarizer (pi's
  `session_before_compact`). `ext-rs` is the UI extension layer:
  status, render, and notify caps. It has no model-call surface.
  The later path is a config-level prompt override, or a
  producer hook when the need lands. No damage today: the
  built-in prompt is the only path.
- No branch summary. The harness has no branches.
- File-operation lists now carry on the `compaction_summary`
  event. The update prompt merges the previous lists. No
  erosion.
- The `compaction_summary` event carries the summary call's
  usage. The statusline sums it from Phase 1. No display gap
  after the three statusline components.

### 8.2 Second audit: `docs/auto-compact-plan-audit.md`

The second audit re-derived the backstop chain and the length-stop
flow from the code. Twelve findings. All are accepted. The fixes
landed in sections 4-7.

Design-correctness findings, all accepted:

1. A1: `bin/log`'s hardcoded schema list rejects all three marker
   types. The plan claimed no code change. Fixed: the list entry
   is Phase 1 (sections 4.1, 5, 7).
2. A2: the length-stop group carries the parse-fabricated
   truncation notice, not a route schema-error result. The
   single-prefix drop rule misses it. The group re-enters every
   request form, the FT-008 priming shape. Fixed: two-prefix drop
   rule, the group logs before the retry, the test asserts the
   extended drop (sections 4.4, 5).
3. A3: the strip flag on the error-overflow path strips the last
   valid group. The failed response is not in the log. Fixed: the
   strip applies to the logged length-stop group only (sections
   4.2, 4.4).
4. A4: "the trim caps every request at the input budget" is
   false. The halving and blind-crawl phases send over-budget
   requests. The real backstops: the provider window, the
   silent-overflow path, the overflow retry, the handoff. Fixed:
   the rewording and the post-engage silent-overflow e2e
   (sections 4.2, 5, 8 finding 10).
5. A5: the `--summary-input` mode was stated to carry the
   handoff instructions. The structured prompt and the Y1 file-op
   merge need the compaction prompt. Fixed: the mode carries the
   compaction prompt (sections 4.2, 4.3).
6. A6: the retry-failure handoff had no mechanism. The existing
   handoff fires only on the estimate. Fixed: `assemble
   --force-handoff`, the e2e (sections 4.3, 4.4, 5, 7).

YAGNI audit findings, all accepted:

- A7: the statusline usage sum is three components, not one line.
  The manifest `kinds`, the host re-send set, the script filter.
  Fixed: named in section 4.6.
- A8: the custom-summarizer acceptance cited `ext-rs` as the later
  path. The layer has no model-call surface. Fixed: reworded
  (section 8).
- A9: the "observed SGLang phrasings" claim has no log evidence.
  The table starts from pi's regexes and needs a live probe.
  Fixed: reworded in the risks section and 4.4.
- A10: the truncation-stop threshold pins pi's 0.99 ratio
  (section 4.4).
- A11: the cooldown makes `compact` a second writer of
  `compact.json`. The writer contract is stated (section 4.2).
- A12: the summary-call `length` stop and the e2e wording are
  pinned (sections 4.2, 5).

### 8.3 Third audit: `docs/auto-compact-plan-audit-2.md`

The third audit re-derived the failure paths from the code. Six
findings. All accepted. The fixes landed in sections 4-7.

Design-correctness findings, all accepted:

1. B1: the `assemble` state writer dropped the cooldown key. The
   writer contract pinned one side only. The state `to_json`
   serializes known fields. A lever move rewrote the file without
   the key. The D2 failure loop reopened. Fixed: the cooldown
   source is the event log. The `compaction_failed` event holds
   `last_user_seq`. The state file keeps a single writer
   (sections 4.1, 4.2, 5, 7).
2. B2: the summary-call failure criteria were unstated. An empty
   summary dropped the old region silently. Fixed: a failed
   attempt is a non-zero exit, an `error` stop, or an empty
   summary after trimming. A zero-output `length` stop is a
   failure (sections 4.2, 5).
3. B3: the kill switch did not gate the overflow path. pi gates
   both cases on the enabled flag. Fixed: off sends the overflow
   path to the last-resort compaction (sections 4.4, 4.5, 7).
4. B4: a provider overflow can arrive without a detail. The SSE
   `response.failed` event holds none. Fixed: the no-detail rule
   and the probe gate extension (sections 4.4, 6, 7).

Spec-gap findings, all accepted:

- B5: open markers accumulated across restarts. Fixed: the
  renderer closes the first open marker (section 4.6).
- B6: an unseeded handoff left the `h` key without a target.
  Superseded by the owner's intent: the last-resort path seeds no
  session. It compacts in-session and continues the original
  session. The handoff retires (sections 4.3, 4.4, 4.6, 5, 7).
  The `--force-handoff` mechanism of A6 retires with it.

## 9. Decision record: the pi parity trigger base (2026-09-15)

### 9.1 The decision

Decision: rushi's auto compact reaches pi's compact threshold.
The mechanism stays opt-in. The default behavior does not move.

- The knob: `compact_trigger_base` under `[limits]` (section 4.5).
- `input_budget` (default): the trigger is `input_budget -
  compact_reserve_tokens`. That is 212992 for the 262144 window
  model.
- `context_budget` (pi parity): the trigger is `context_budget_tokens
  - compact_reserve_tokens`. That is 245760 for the same model. It
  equals pi's `contextWindow - reserveTokens`.
- The backstop follows the base. Under pi parity, the provider
  wall is the last backstop.
- Under `context_budget`, the trigger estimate adds the full-form
  estimate of the kept region. The trim-form readings read shrunken.
  The full form keeps the LLM compaction from starving.

The live `config.toml` keeps the default base. The knob line and its
comment block were removed from it. `config-low.toml` keeps the
commented-out documentation.

### 9.2 The change

- `crates/rushi/src/compact_math.rs`: the `TriggerBase` enum, the
  `trigger_level_for` clamp (reserve 0 lands at base minus 1), and
  the `full_form_estimate` (chars/4 over the kept region, with the
  handoff framing).
- `bin/rushi/src/config.rs`: the unclamped `context_budget`, the
  `compact_trigger_base` load, and the base-aware `trigger_level()`
  and `compact_overflow_budget()`. An unknown value falls back to
  `input_budget` with a warning.
- `bin/rushi/src/step.rs`: `estimate_context` takes the max of the
  measured plus trailing estimate and the full-form estimate under
  the `context_budget` base. The silent overflow backstop uses
  `compact_overflow_budget()`.
- `bin/compact/src/main.rs`: the trigger decision uses the same
  base. Under `context_budget`, the full-form reading of the kept
  region joins the trigger readings.
- `bin/assemble/src/main.rs`: `estimate_request_tokens` skips the
  measured anchor when a compaction boundary exists (`framing.is_some()`)
  and estimates the full kept region from scratch instead (see 9.5).
  `resolve_budget_tokens` clamps the wire budget to the input-only
  window on the default base and to the full model window on the
  `context_budget` base. The `context_exhausted` gate and the summary
  call budget follow the base (see 9.6).
- `bin/rushi/src/step.rs`: `estimate_context` applies the same rule.
  A last reading that predates the boundary is stale and is replaced
  by the full-form estimate of the kept region.
- `scripts/compact-e2e.sh`: the `last-resort` fixture was re-calibrated
  so the heavy result lands in the old region, not the kept tail
  (see 9.5).
- `config-low.toml`: the knob documentation, commented out.
  `config.toml` carries no trace of the knob.
- `scripts/compact-e2e.sh`: the `pi-parity` and `pi-parity-cold`
  scenarios.
- This document: section 4.5 states the knob semantics.

### 9.3 The evidence

- Unit: `context_budget_base_reaches_the_pi_threshold` in
  `bin/rushi/src/config.rs` asserts `trigger_level() == 245760`.
  The matching test in `bin/compact` asserts the same level from
  the binary's own resolver.
- E2E: `pi-parity` fires the threshold compact at the context
  budget level. `pi-parity-cold` proves the default base stays
  cold at the same session width. `pi-parity-no-exhaust` proves
  the unclamped wire budget: a kept region above the clamped input
  budget but below the trigger proceeds to idle without
  `context_exhausted`.
- Regression: the default base e2e scenarios pass. The workspace
  test suite is green. `scripts/compact-e2e.sh` passes 62 of 62
  assertions, including `last-resort`.

### 9.4 Enable

Add one line under `[limits]` in the session config. Copy the
commented-out documentation from `config-low.toml` if wanted:

```toml
compact_trigger_base = "context_budget"
```

Set `compact_reserve_tokens` to at least the expected next-response
size so the next response fits inside the window after the trigger.
The pi default is 16384, which gives the 245760 trigger. A smaller
reserve (e.g. 4096) pushes the trigger toward the window wall and
leaves too little headroom for this model's 4-8k responses.

Restart the loop after changing the config. The config is read once
at loop start; a running loop does not pick up the change.

### 9.5 Post-mortem: the tui-stream-impl unrecoverable failure

The session ended with `context still exhausted after in-session
compact; stopping` followed by `the last-resort compaction did not
recover the session`.

Root cause: `estimate_request_tokens` anchored on the last measured
`usage.input_tokens` in the kept region. After the overflow compact
(`first_kept_seq = 1206`), that reading (228821, taken before the
compact) sat inside the kept region. The estimate was 228821 plus
825 trailing tokens = 229646, which exceeds the 229376 input budget
by 270 tokens. The true post-compact kept region was 32023 tokens
and fit the budget with room to spare. The stale estimate produced a
false `context_exhausted` form, the last-resort compact found no old
region to summarize, and the loop stopped.

Fix: when a compaction boundary exists, the last reading predates the
boundary and is stale. `estimate_request_tokens` now skips the anchor
and estimates the full kept region from scratch, plus the framing
item. `estimate_context` in `step.rs` applies the same rule so the
threshold trigger and the silent-overflow backstop see the same
number.

Verification: the pre-fix release `assemble` emits the
`context_exhausted` form on a copy of the dead session. The fixed
release `assemble` emits a normal request for the same session. All
58 e2e scenarios pass.

The `last-resort` e2e fixture had the same shape of bug. The old
fixture put a 24000-char result in the kept tail, which exceeded the
whole 3904 input budget. No compact could make that fit, so the
scenario could never pass. The fixture now keeps the heavy result
in the old region: a 4000-char result (1000 tokens) in the old
region, a 14000-char result (3500 tokens) in the kept tail group.
The cut puts the 1000-token result in the old region, the kept tail
(3510) plus the framing summary fits under the budget, and the loop
recovers to idle.

### 9.6 Unclamp: the pi-parity wire budget (2026-09-07)

The `context_budget` base still left one clamp: `assemble` clamped
its wire budget to `context_tokens - max_output_tokens` (229376 for
the 262144 window). The `context_exhausted` gate and the summary
call ran on that clamped budget, so a pi-parity session declared
exhausted at 229376, below the 245760 trigger. The pi reference
reserves no `maxTokens` from the window: `shouldCompact` compares
against `contextWindow - reserveTokens`, and its session logs show
requests measured up to 266837 input.

Fix: `resolve_budget_tokens` in `bin/assemble`. The default
`input_budget` base keeps the clamp (output reservation). The
`context_budget` base clamps only to the model window (262144).
Under pi parity the `context_exhausted` gate now sits at the model
window and the proactive trigger at 245760 remains the first
compaction point.

- `scripts/compact-e2e.sh`: the `pi-parity-no-exhaust` scenario.
- Unit: the `budget_*` tests in `bin/assemble/src/main.rs`.

### 9.7 The tui-separation-repo degradation (2026-09-07)

The session ran with `compact_trigger_base = "context_budget"` and
`compact_reserve_tokens = 4096`, so the trigger sat at
`262144 - 4096 = 258048`. Measured input peaked at 254808 tokens
(plus about 700 of trailing estimate), which stayed under the
trigger. No proactive compact fired. The session then died on model
quality errors (empty turns, malformed tool arguments) near the
window wall, not on a provider context-overflow error, so the
reactive `is_overflow` compact-and-retry path never ran.

pi's own logs show it compacting in this same band because its
reserve is 16384 (trigger 245760), leaving real headroom for the
next response. The fix is the reserve value, not a missing
capability: set `compact_reserve_tokens` to 16384 (the pi
default) so the trigger leaves enough room for the next model
response. The reactive overflow recovery path in `step.rs`
(`is_overflow` + `CompactReason::Overflow` + last-resort fallback)
already mirrors pi's `isContextOverflow` compact-and-retry.

### 9.8 Hard-trim backstop: mechanical cut-down-to-fit (2026-09-16)

The LLM compaction leads at the trigger level
(`context_budget_tokens - compact_reserve_tokens`). When a request's
full-form estimate still exceeds that level, `bin/assemble` now
drops whole step groups from the oldest until the request fits the
trigger level. The append-only event log keeps every dropped event
(source of truth); the request is the projection that fits. The
trim marker rides the request JSON as a top-level `hard_trim`
field; `bin/rushi` logs it as a `hard_trim` `ext_status` event and
strips it before the model call. `bin/model` strips it defensively.

- Target: `trigger_level_for(budget_tokens, compact_reserve_tokens)`.
- Cut: whole step groups only. A tool call and its result never split.
- No model call, so a stalled provider cannot hang the backstop.
- When even the framing alone exceeds the target, the trim returns
  `None`; the `context_exhausted` form fires and the last-resort
  in-session compaction takes over (`bin/compact`).
- The summary-input request clamps its target to
  `min(budget_tokens, window_input)` so the summary call itself
  fits the window even under the unclamped pi-parity base.

This backstop makes the `log-tree-design` input-overflow case
recoverable: instead of dying on a server-side context-length
error, the loop mechanically trims the oldest groups and
continues.

- `bin/assemble/src/main.rs`: `hard_trim_groups`, the
  `hard_trim` marker, and the `context_exhausted` fallback.
- `bin/rushi/src/step.rs`: `log_and_strip_hard_trim`.
- `bin/model/src/main.rs`: the defensive strip.
- `scripts/compact-e2e.sh`: the `last-resort` scenario now
  exercises the hard-trim success path; the `context-exhausted`
  scenario exercises the framing-too-large fallback.

### 9.9 Truncation-aware parse: the length-stop recovery (2026-09-16)

`bin/parse` previously hard-failed (exit 2) on any tool call whose
`arguments` were not a JSON object, regardless of stop reason. When
a `length` stop cut the stream mid-call, the last call's JSON was
truncated, so parse exited 2 and the loop's existing
`stop_reason == "length"` recovery in `bin/rushi/src/step.rs`
(`log_truncated_group` + compact + retry) never ran.

pi does not hard-fail here. It treats the truncated response as a
length stop and recovers. The fix matches pi: when
`stop_reason == "length"`, parse skips strict argument validation,
normalizes each call's arguments to an object (empty object when
truncated), records the assistant message with `stop_reason`
preserved, and attaches a `Re-issue the call` hint to each tool
result. Strict validation still hard-fails for a non-length
malformed call. This unblocks the `log-tree-fork-recursion-prove`
output-exhaustion stall, where thinking consumed the full output
budget and cut the last call mid-stream.

- `bin/parse/src/main.rs`: the `stop_reason != "length"` gate and
  the `length` branch.
- `bin/rushi/src/step.rs`: the unchanged length-stop recovery
  now runs.
- Unit: the parse length-stop tests in `bin/parse/src/main.rs`.

## Properties

Lean-style invariants for this spec (see `lean-driven-development.md`).

P1. threshold-trigger: given trigger-based readings above `context_budget_tokens - compact_reserve_tokens`, observe the compact fire.
P2. cut-snap: given a cut point, observe it snap to a step-group boundary so a tool call and its result never split.
P3. orphan-pull: given a cut that leaves a `user_message` as the last old-region event, observe it pull into the kept region.
P4. empty-region-noop: given no old region to summarize, observe the compact exit 0 with no model call.
P5. summary-fits-budget: given the drop cap, observe the summary request input stay within the input budget.
P6. failure-marker: given a failed summary call, observe a `compaction_failed` marker with `last_user_seq`, no terminal event, and the loop continue in the current form.
P7. silent-overflow: given a successful call whose input usage meets the input budget, observe compact only, with no model re-run.
P8. overflow-retry: given a recoverable overflow or length stop, observe one `compact --reason overflow` then one model re-run. A second overflow runs the last-resort compact then a terminal `error`.
P9. iterative-merge: given a prior `compaction_summary`, observe the next summary request carry the previous summary and its file-op lists.

## Verification

| P# | Property | Proof | Status |
|----|----------|-------|--------|
| P1 | threshold-trigger | `trigger_fires_above_level` in `crates/rushi/src/compact_math.rs`; `scenario_threshold` in `scripts/compact-e2e.sh` | proven |
| P2 | cut-snap | `cut_snaps_to_the_group_start` in `crates/rushi/src/compact_math.rs` | proven |
| P3 | orphan-pull | `cut_pulls_in_the_orphan_user` in `crates/rushi/src/compact_math.rs` | proven |
| P4 | empty-region-noop | `cut_at_zero_is_the_empty_region` in `crates/rushi/src/compact_math.rs`; `summary_input_empty_region_is_the_ask_alone` in `bin/assemble/src/main.rs` | proven |
| P5 | summary-fits-budget | `summary_input_drop_search_bounds_at_the_budget` in `bin/assemble/src/main.rs` | proven |
| P6 | failure-marker | `scenario_compact_failure`, `scenario_empty_summary` in `scripts/compact-e2e.sh` | proven |
| P7 | silent-overflow | `scenario_silent_overflow` in `scripts/compact-e2e.sh` | proven |
| P8 | overflow-retry | `scenario_overflow`, `scenario_failed_retry` in `scripts/compact-e2e.sh` | proven |
| P9 | iterative-merge | `scenario_iterative` in `scripts/compact-e2e.sh`; `summary_input_update_prompt_carries_the_previous_summary` in `bin/assemble/src/main.rs` | proven |
| P10 | hard-trim-backstop: given a request whose full-form estimate exceeds the trigger level, observe the oldest step groups drop so the request fits; the log keeps every event | `hard_trim_drops_groups_from_the_oldest`, `scenario_last_resort` in `scripts/compact-e2e.sh` | proven |
| P11 | hard-trim-fallback: given a framing alone that exceeds the target, observe the `context_exhausted` form fire the last-resort compaction | `hard_trim_fails_when_the_framing_alone_exceeds_the_target`, `scenario_context_exhausted` in `scripts/compact-e2e.sh` | proven |
| P12 | length-stop-recovery: given a `length` stop that truncates a tool call, observe parse record the turn without a hard-fail and the loop re-issue via compact | the parse length-stop tests in `bin/parse/src/main.rs` | proven |

## Gate

The acceptance commands. All must exit 0 for this spec to be proven.

```
cargo build
cargo test
scripts/compact-e2e.sh
```
