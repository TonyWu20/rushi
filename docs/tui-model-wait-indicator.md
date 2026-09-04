# TUI model-wait indicator

Status: shipped (2026-09-01). The feature request lives in
`docs/tui_feature_requests_from_human.md` (2026-09-01 item),
closed with a Shipped note. This doc is the plan and spec;
sections 2 and 4 are the contract.

Revision (2026-09-01, same day): the value `model` renamed to
`wait`. The timer moved from the built-in statusline row to a
working row above the input box, after the `pi` working
indicator. The first ship kept the timer in the statusline
row. The reference statusline extension owns that slot and hid
the timer. The working row shows under any statusline.

Revision (2026-09-02): the reference statusline stopped listing
`ext_status` values (the bash reference and the Rust port).
The generic `key=value` pill dump is gone: it duplicated host
presentation in the footer, ran two values together with no
separator, and showed the thinking level as a bare number.
The marker's presentation is the title bit and the working
row. The `statuses` map stays on the tick payload.

## 1. Purpose and justification

While the loop runs, the TUI shows two loop signals only. The
`[running]` title bit and the last loop output line. Neither names
the current phase. The model call is log-silent. No event lands
until the response completes. The user sees a frozen screen for
the longest phase of the loop.

Justification, by criterion 1 of `docs/spec-review-criteria.md`:

- Triggering episode: the 2026-09-01 feature request, recorded in
  `docs/tui_feature_requests_from_human.md`.
- Data: `notes/harness-vs-pi-model-latency.md` puts the model
  round-trip at a median of 9 s, a p90 of 50 s, and a max of
  242 s. The wait is visible and long.
- Necessary-change check: config or a plain script cannot surface
  the loop phase. The loop emits nothing during the model call.
  A code change in the loop and the TUI is required.
- The change moves an invariant from human discipline to code.
  The user no longer has to guess that the loop is alive.

## 2. Contract

The contract is additive. It reuses the `ext_status` event. No
new event type. No schema change. The schema is
`schemas/events/v1/ext_status.json`. Its `value` field allows
any JSON. This contract fixes one id and two values on that
field:

- id: `loop_phase`
- values: the strings `wait` and `tools`

The value names the phase the loop enters, not the model. The
word `model` stayed out of the value on purpose. The title already
shows the model name in a pill. A `model` bit next to it read as
the name.

The event envelope follows the event contract (architecture.md
section 5.2). One event per line. Version 1. One `ts` in RFC3339
UTC. No embedded newlines.

### Publish rules (loop side, `scripts/step.sh`)

- Given the step state is `awaiting_model`, the step appends one
  `ext_status` event with value `wait` before the `assemble`
  call. The marker covers assemble, the model call, the parse,
  and the retry loop.
- Given the step routes tool calls, it appends one `ext_status`
  event with value `tools` before the routing. Two routing
  sites: the crash-recovery path and the post-parse path.
- The loop appends no `done` or `idle` marker.
- The marker appends through `bin/log` with schema validation.
  A failed append aborts the step. This matches every other
  append in the script.

### Render rules (TUI side, `bin/tui`)

The TUI reads the log. It keeps the last `ext_status` value per
id, per active session. It derives one of four display states
from two inputs. The input is the last `loop_phase` value. The
other input is the FT-003 loop-running bit.

| State | Condition | Title bit | Working row |
|---|---|---|---|
| `idle` | loop not running | `[idle]` | no row |
| `running-unknown` | loop running, no marker or a value outside `wait`/`tools` | `[running]` | `Working...` |
| `wait` | loop running, last value `wait` | `[wait]` | `waiting for model · Ns` |
| `tools` | loop running, last value `tools` | `[tools]` | `tools running · Ns` |

The state is observable. It is a pure function of the log and
the running bit. It survives a TUI restart.

Working row rules (after the `pi` working indicator):

- The row is a layout cell above the input box, below the
  transcript and the optional steering and approval rows.
- The row shows while the loop runs. When the loop stops, no row
  draws and the transcript absorbs the place. The input box does
  not move: the transcript is the flexible cell. This matches
  the `pi` working indicator.
- While the loop runs, the row shows one braille spinner frame
  in the thinking-level border color, then the state text in
  dim. The frame is a pure function of the wall clock, one frame
  per redraw, about 100 ms.
- The row is its own cell. A statusline extension does not own
  it. The row shows under any statusline.
- `N` is the whole seconds between the marker timestamp and now.
- `N` under 60 s shows as `Ns`. At 60 s and above it shows as
  `Mm SSs`. A negative span clamps to `0s`.
- An unparseable timestamp drops the span and keeps the label.
- The statusline slot shows no phase content. The slot keeps its
  precedence: flash, naming bar, extension row, handoff hint,
  last loop line, help line.

Extension rule: no protocol change. The marker reaches the
extension tick through the existing `statuses` map. The
reference statusline no longer lists ext_status values (a
2026-09-02 revision dropped the generic `key=value` pill dump:
it shared a slot with no separator between two values and
duplicated host presentation in the footer). The marker's
presentation is the title bit and the working row.

## 3. Behavior

State transitions, all observable in the log:

1. A user message or a tool-result batch lands. The next step
   claims `awaiting_model`. It appends `loop_phase=wait`. The
   TUI shows the `wait` state from that event on.
2. The model response lands. The step routes tool calls. It
   appends `loop_phase=tools`. The TUI shows the `tools` state.
3. The tool results land. The next step claims `awaiting_model`.
   It appends `loop_phase=wait` again. The state returns.
4. The step logs an error, records exhaustion, or claims idle.
   The loop exits. The running bit falls. The state is `idle`.
   No marker carries the transition.
5. The TUI restarts. The start read rebuilds the per-id values
   from the log. The probe re-marks the loop running. The state
   restores without a marker change.

The `wait` marker appends before `assemble`. The assemble time
counts as model wait. After the batch lands, the last marker
stays `tools` until the next step's marker. That gap is the step
loop plus the claim plus the append. It runs in milliseconds.

Retries keep the current marker. An empty-turn retry or an
API-error sleep stays inside the `wait` state.

## 4. Failure modes

| Condition | Behavior |
|---|---|
| Marker append fails | The step aborts with exit 1. Same as every other log append. |
| Marker timestamp does not parse | The bit shows the state. The working row keeps the label and drops the span. |
| Value outside `wait` and `tools` | The state is `running-unknown`. The bit shows `[running]`. The row shows `Working...`. |
| Loop dies on error or exhaustion | The running bit falls. The state is `idle`. |
| `cancel` event while waiting | The loop stops. The state is `idle`. The cancel event does not touch the marker. |
| Loop host clock runs behind the TUI | `N` clamps to `0s`. |
| Two markers in the same second | Log order wins. The later event is the last value. |
| More than 128 distinct ext_status ids | The oldest-updated id drops from the per-id map. `loop_phase` drops only under that pressure. |

## 5. Conformance tests

Every row is a check. The mutation gate is the last row.

| Test | Given | Expected |
|---|---|---|
| Marker in log | The log holds a `loop_phase=wait` event. The loop runs. | The title bit reads `[wait]`. The working row shows `waiting for model · Ns`. |
| No marker | The log holds no `loop_phase` event. The loop runs. | The title bit reads `[running]`. The working row shows `Working...`. |
| Tools marker | The last value is `tools`. The loop runs. | The bit reads `[tools]`. The row shows `tools running · Ns`. |
| Stopped loop | Any marker. The loop is stopped. | The bit reads `[idle]`. No working row. |
| Unknown value | The last value is a string outside the two. The loop runs. | The bit reads `[running]`. The row shows `Working...`. |
| Unparseable ts | The last marker value is `wait`. Its `ts` fails to parse. | The bit reads `[wait]`. The row keeps the label, drops the span. |
| Long wait | The marker timestamp is 90 s old. The loop runs. | The row shows `waiting for model · 1m 30s`. |
| Clock skew | The marker timestamp is in the future. | The row shows `waiting for model · 0s`. |
| Restart | A TUI restarts onto a running session. The log holds the marker. | The state restores on the first draw. |
| Session switch | Two sessions hold different markers. The user tabs between them. | Each session shows its own last value. |
| Cap drop | 129 distinct ids precede a fresh `loop_phase` marker. | The marker survives. An older id drops. |
| e2e marker | `scripts/cache-e2e.sh` runs one step for a session. | The session log holds a `loop_phase` event. |
| Mutation gate | The emit helper is removed from `step.sh`. | The e2e marker test fails. |

Unit tests live in `bin/tui/src/app.rs` (state derivation, cap,
restart rebuild, session switch) and `bin/tui/src/render.rs`
(bit per state, working-row text per state, spinner cycle, `N`
formatting).
The e2e row runs in `scripts/cache-e2e.sh`. It needs a model
backend. The manual pass covers a live session.

## 6. What this does not do

- No first-class `model_call` event. Wait-time stats stay out
  of the transcript. `jq` pipelines cannot report them. A
  future spec can add the event type.
- No phase for other running loops. They keep the plain
  `+N loop` bit.
- No `done` marker. The running bit carries the exit.
- No cumulative timer. `N` counts the current wait only.
- No sub-second resolution. The marker timestamp has one-second
  resolution.
- No styled statusline pill. The reference statusline does not
  list ext_status values at all (a 2026-09-02 revision dropped
  the generic `key=value` pill dump). A styled pill that picks
  its own ids is a separate, optional piece of work. It spans
  `ext-rs/statusline-rs`, `ui_extensions/statusline`, and the
  pi-config starship port.

## 7. Impact

Affected: `scripts/step.sh`, `bin/tui` (`app.rs`, `render.rs`),
the optional statusline extensions, `scripts/cache-e2e.sh`, and
the doc updates named in section 11.

Unaffected: `bin/claim`, `bin/assemble`, `bin/model`,
`bin/parse`, `bin/route`, `bin/log`, every schema, and the
extension protocol. The TUI adds no loop decision logic. It
reads one published value. The guardrail tests stay green. The
TUI source adds no `claim`, `assemble`, or storage-path
literals.

## 8. Migration

The change is additive. No `v` bump. Old logs hold no marker.
They replay into the `running-unknown` state. Every consumer
falls back:

- The TUI shows `[running]`. The working row shows `Working...`.
- A statusline shows no `loop_phase` pill.
- A `jq` reader finds no event. Nothing breaks.

The marker is a new id on an existing event type. It does not
duplicate any existing id. The id registry entry lands in
`docs/ui-extension.md` section 5.

## 9. Trade-offs, recorded

- The marker append is fatal. A failed append aborts the turn,
  not just the indicator. This matches every other append in
  `step.sh`.
- The channel allows any JSON value. A garbage value falls back
  to `running-unknown`. No enum validation.
- The indicator covers the active session only.
- `ext_status` rows stay suppressed from the transcript. The
  log keeps the markers. External tooling cannot read wait
  times without a first-class event. That is the cost of this
  channel choice.
- The marker host and the TUI host share one clock. A remote
  loop host is out of scope.
- The first ship kept the timer in the built-in statusline row.
  The reference statusline extension owns that slot and hid the
  timer. The indicator moved to its own working row above the
  input box. The statusline extension keeps its slot.

## 10. Rejected alternative

A TUI-only derivation reads the log tail. It infers the owed
model call from the last event kind. The loop keeps no new
publish. The loop-side publish wins. It keeps decision logic
out of the TUI. That split is a rule in `docs/tui.md` section
4. The TUI-only form also cannot show the tool phase and it
approximates the wait time.

## 11. Implementation notes

This section is non-normative. The contract is sections 2 and 4.

Loop side:

- One helper in `scripts/step.sh`. It builds the event with
  `jq -cn` and appends it through `bin/log` with the schema dir.
  The `ts` comes from `date -u +%Y-%m-%dT%H:%M:%SZ`, the same
  form every other event in the script uses.
- Three call sites: the `awaiting_model` branch before
  `assemble`, the crash-recovery routing, and the post-parse
  routing.

TUI side:

- `bin/tui/src/app.rs` adds the constant `LOOP_PHASE_STATUS_ID`.
  It adds a timestamp side map next to the existing per-id value
  map. Each id maps to the timestamp of the event that last set
  its value. The side map drops ids with the value map at the
  128-id cap. The rebuild on session switch builds both maps.
- The lookup reads the two maps in O(1). It parses the marker
  timestamp with `chrono`. The crate is already a dependency.
- `bin/tui/src/render.rs` computes the state at draw time. The
  title bit follows the table in section 2. The working
  row above the input box shows the spinner and the state text.
  The spinner frame is a pure function of the wall clock. The
  row is a `Length(1)` layout cell drawn while the loop runs.
- No change in `bin/tui/src/main.rs`. The tick payload already
  carries the status map and the running bit. The main loop
  redraws about every 100 ms, which drives the spinner.

Docs on ship: `docs/tui.md` section 6 gains the phase-aware bit
and the working row. `docs/ui-extension.md` section 5 gains the
id registry entry.
The request item closes with a `Shipped` note.

## 12. Acceptance

- `cargo test -p tui` passes. The guardrail tests pass.
- `scripts/cache-e2e.sh` finds the marker in the session log.
- A live session shows `[wait]` and the counter in the working
  row above the input box while waiting. The row shows under a
  statusline extension. The indicator clears on the response.
- A TUI restart mid-call restores the indicator.
- The request item in `docs/tui_feature_requests_from_human.md`
  shows the `Shipped` note.

## Properties

Lean-style invariants for this spec (see `lean-driven-development.md`).
One property per non-trivial invariant. Each property is observable:
given an input, an output guarantee.

P1. wait-state: given the last `loop_phase` value is `wait` and the
    running bit is set, observe the title bit reads `[wait]` and the
    working row shows `waiting for model · Ns`.
P2. tools-state: given the last `loop_phase` value is `tools` and the
    running bit is set, observe the title bit reads `[tools]` and the
    working row shows `tools running · Ns`.
P3. idle-state: given the running bit is clear, observe the title bit
    reads `[idle]` and no working row draws.
P4. unknown-fallback: given the running bit is set with no marker or a
    value outside `wait` and `tools`, observe the title bit reads
    `[running]` and the working row shows `Working...`.
P5. wait-span-format: given a marker timestamp, observe `N` under
    60 s shows as `Ns`, at 60 s and above shows as `Mm SSs`, a
    negative span clamps to `0s`, and an unparseable timestamp drops
    the span and keeps the label.
P6. restart-rebuild: given a TUI restart onto a running session whose
    log holds the marker, observe the state restores from the log on
    the first draw without a new marker.
P7. session-isolation: given two sessions holding different last
    marker values, observe each session shows its own last value on
    switch.
P8. cap-drop: given more than 128 distinct ext_status ids preceding a
    fresh `loop_phase` marker, observe the `loop_phase` marker still
    renders and an older id's value is the one dropped.

## Verification

Each property maps to its proof. `proven` means the cited test or
script exists and passes. `open` names the blocker and what unblocks
it.

| P# | Property | Proof | Status |
|----|----------|-------|--------|
| P1 | wait-state | `working_row_shows_the_wait` in `bin/tui/src/render.rs`, `loop_phase_value_and_ts_track_the_last_event` in `bin/tui/src/app.rs` | proven |
| P2 | tools-state | `working_row_shows_the_tools_run` in `bin/tui/src/render.rs` | proven |
| P3 | idle-state | `working_row_is_blank_when_idle` in `bin/tui/src/render.rs` | proven |
| P4 | unknown-fallback | `working_row_shows_working_for_an_unknown_value`, `phase_state_table` in `bin/tui/src/render.rs` | proven |
| P5 | wait-span-format | `wait_span_text_formats_and_clamps`, `working_row_drops_the_span_on_an_unparseable_ts` in `bin/tui/src/render.rs` | proven |
| P6 | restart-rebuild | `loop_phase_restart_rebuilds_from_the_log` in `bin/tui/src/app.rs` | proven |
| P7 | session-isolation | `loop_phase_session_switch_keeps_own_marker` in `bin/tui/src/app.rs` | proven |
| P8 | cap-drop | `loop_phase_marker_survives_the_id_cap` in `bin/tui/src/app.rs` | proven |

## Gate

The acceptance commands. All must exit 0 for this spec to be proven.

```
cargo build
cargo test
scripts/cache-e2e.sh
```
