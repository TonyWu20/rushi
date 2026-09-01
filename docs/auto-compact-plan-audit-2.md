# Auto-compact plan: third audit

Status: audit of `docs/auto-compact-plan.md` (revised) and
`docs/auto-compact-plan-audit.md`. Scope: blind spots that neither the
plan nor the second audit properly considered. Evidence: the current
repo code, `config.toml`, and the pi 0.84.2 source in the nix store.

## 1. Method

Re-check the load-bearing claims of both documents against the code.
Then re-derive each failure path from the code, not from the plan's
wording. The findings below are the points that fail that test.

## 2. Findings

### B1. The assemble writer drops the cooldown key (high)

The plan (4.2, the A11 fix) pins the writer contract on one side
only: `compact` read-modify-writes `compact.json` and preserves the
`caps`, `keep`, `drops`, and measurement fields. Both readers
tolerate the other's fields. The `assemble` writer is not pinned.
The code proves it cannot keep the key.

- `CompactState::to_json` serializes only the nine known fields
  (`bin/assemble/src/main.rs:96-108`). No extra key survives.
- `write_compact_state` writes exactly that object
  (`bin/assemble/src/main.rs:156-162`).
- `assemble` writes the state on a fresh engagement and on every
  lever move: `decide_form` returns the state when `moved` is true
  (`bin/assemble/src/main.rs:284,289`). `main` persists it at line
  1174.
- The step order runs `compact` before `assemble` (plan 4.4, steps
  1-2).

Reachable scenario: one step crosses both the trigger level and the
trim budget, or the trim-engaged guard fires. The summary call fails
twice. `compact` writes the cooldown key. `assemble` engages the
trim form or moves a lever in the same step. It writes the state.
The cooldown key is gone.

Damage: the D2 class damage reopens. The threshold check re-fires on
every step while the levers move. Every step adds a
`compaction_failed` marker and two failing summary calls. Each call
is a stall on a local GPU. The session burns minutes per step until
the levers reach the drop max and the handoff ends the session.

The plan's "version bump covers the new key" has no enforcement.
`CompactState::from_json` never reads `v`
(`bin/assemble/src/main.rs:110-125`).

Fix, preferred: derive the cooldown from the log. Add a
`last_user_seq` field to the `compaction_failed` event. The
threshold check skips while the log's last `user_message` seq is at
or below that value. The second writer disappears. The A11 problem
closes.

Fix, within the plan's design: pin both writer sides. `assemble`
must preserve the cooldown key on every state write.

Test: one step where `compact` fails and `assemble` moves a lever.
Assert the cooldown survives. Assert the next step's threshold check
skips.

### B2. The summary failure criteria are unstated. An empty summary
drops the old region silently (medium)

The plan says "summary call failure: two attempts". It never states
what counts as a failure. The code has a precedent.

- `bin/model` exits 0 on an API failure. It prints a JSON object
  with `stop_reason` set to `error`
  (`bin/model/src/main.rs:130-137`).
- `run_handoff` treats `stop_reason = error` as a failure
  (`scripts/step.sh:81`).

The unstated cases:

- A `length` stop with zero output yields an empty summary. A12
  accepts a truncated summary as valid. An empty one is not. The
  projection replaces the old region with an empty item. The old
  region's context is lost without any failure marker.
- A `stop` with empty text has the same effect.

Fix: the failure criteria are three. The model exits non-zero. The
stop reason is `error`. The summary text is empty after trimming.
All three take the `compaction_failed` path. Add a test for the
empty-summary case.

### B3. The kill switch leaves the overflow path open (low)

pi gates both trigger cases on the enabled flag. `_checkCompaction`
returns false when compaction is off
(`agent-session.ts:1964`).

The plan labels `compact_enabled` the "pi parity kill switch"
(4.5). It states only the threshold hook (4.4 step 1). The overflow
recovery (4.4 step 5) runs `compact --reason overflow` with no check
on the switch.

Fix: state which paths the switch gates. pi parity: off means the
overflow path skips compaction and goes straight to the last-resort
handoff.

### B4. A provider overflow can arrive with no detail (low)

`bin/model` attaches a `detail` field to HTTP-level failures. It
emits `stop_reason = error` with no `detail` on an SSE
`response.failed` event (`bin/model/src/main.rs:549-553`).

The plan's classifier (4.4 step 4) matches overflow patterns
against the detail. No detail means no match. The call enters
transport handling: two retries, then a terminal `error` event. The
loop stops.

If a provider reports a context overflow through `response.failed`
without a matching body, the recovery path skips silently. The
pattern table cannot catch it.

Fix: state the rule. An error stop with no matchable detail is not
recoverable. The backstops are the trim form and the next step's
threshold trigger. Extend the live probe gate (plan 4.4, section 6)
to record the delivery shape per provider: an HTTP 400 body or an
SSE `response.failed` event.

### B5. Open markers accumulate across restarts (low)

D7 pins the render of one open marker: `compacting (interrupted)`
when the loop is not running. It says nothing about a restart.

The next trigger after a crash appends a fresh
`compaction_started` marker. Open markers accumulate in the log.
The plan's rule "the following `compaction_summary` event closes it"
does not say which marker it closes.

Fix: state the render rule. Close the first open marker. Or reap
orphaned markers on the next trigger.

### B6. A failed handoff seeds no session. The `h` key has no
target (informational)

This edge belongs to correction 57. When the handoff summary call
fails, the marker records an empty `new_session`. The TUI
`pending_handoff` returns none for an empty seed
(`bin/tui/src/app.rs:614-620`). The `h` key offers no resume.

The new overflow recovery routes more sessions to the handoff. The
condition that breaks the summary calls (a down model) also breaks
the handoff summary call. The user reaches a dead session with no
one-key resume.

Fix: state the behavior. The user reopens the old session by hand.
Or add one retry to the single-attempt handoff summary call in
`run_handoff`.

Superseded: the owner retired the handoff. The last-resort path
compacts in-session and continues the original session. No session
seeds. The plan carries the change (plan section 8.3).

## 3. Verified claims

The re-check confirmed the fact base of both documents.

- A1: `bin/log` validates against a hardcoded list of seven schema
  names. An unknown type is a hard failure (`bin/log/src/main.rs:
  42-50, 132-134`). The TUI validator loads the schema by type name
  (`bin/tui/src/port_file.rs:518-521`).
- A2: the `length` branch of `parse` emits the
  truncation-notice results and exits 2. `route` runs only on exit
  1 (`bin/parse/src/main.rs:217-258`, `scripts/step.sh:240`).
- A3: the error path of `step.sh` logs nothing
  (`scripts/step.sh:188-205`). The strip flag targets the logged
  length-stop group only.
- A4: the levers move one step per run. A blind-crawl request can
  outgrow the input budget (`bin/assemble/src/main.rs:253-262`).
- A5: the handoff instructions are a separate text
  (`HANDOFF_INSTRUCTIONS` in `bin/assemble/src/main.rs`).
- A6: the existing handoff fires only on the `Exhausted` form. The
  force-handoff flag is the missing mechanism.
- A7: the statusline usage sum needs three components: the manifest
  `kinds`, the host re-send set, the script filter
  (`ui_extensions/statusline/ext.toml:24`,
  `bin/tui/src/ext.rs:1074-1083`,
  `ui_extensions/statusline/statusline.sh:173`).
- A9: the session logs hold no provider overflow error. The error
  messages in `sessions/*/events.jsonl` are the harness's own.
- pi facts: the 0.99 truncation ratio (`overflow.ts:152-159`), the
  defaults 16384 and 20000 (`compaction.ts:134-135`), the 0.8
  summary cap, the `stop_reason` split
  (`agent-session.ts:1996-1998`).
- Repo facts: `turn.sh` runs without a step cap. `claim` ignores
  unknown types. The budget of 262144 clamps to 229376. The trigger
  is 202992.

## 4. Required fixes, in order

1. B1: the cooldown source. Move it to the log, or pin the
   `assemble` writer side.
2. B2: the failure criteria, including the empty-summary check.
3. B3: the kill switch gate on the overflow path.
4. B4: the no-detail rule, the probe gate extension.
5. B5: the open-marker render rule.
6. B6: superseded. The last-resort path compacts in-session. No
   session seeds.
