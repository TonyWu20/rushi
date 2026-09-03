# Phase 2 Plan Audit — readiness and the compact-strategy seam

Status: Audit (2026-09-07).

This audit answers two questions. Is `docs/phase-2-plan.md` ready to
implement? Does the Phase 2 core keep the compact strategy as a
drop-in component, so a handoff strategy can replace it? It
cross-checks the plan against the current tree. It maps the debt a
strategy swap would hit.

## 1. Readiness: the plan checks out against the code

| Plan claim | Evidence in the tree | Verdict |
|---|---|---|
| `step.sh` is a 454-line recovery state machine | `wc -l scripts/step.sh` = 454 | holds |
| The awk scrapes shift the input budget | `step.sh` lines 6, 99-133 | holds |
| Four `LogLine` copies | `bin/log`, `bin/user`, `bin/route`, `bin/tui/src/port_file.rs` | holds |
| Three validator copies | `bin/user`, `bin/log`, `bin/tui` | holds |
| Compact trigger math in two places | `bin/assemble` + `bin/compact` | holds |
| Ten `--self-test` classifier rows | `scripts/overflow-classify.sh` line 82 ff. | holds |
| Three `model --describe` calls per step | `step.sh`: `active`, `model_id`, `thinking_level` | holds |
| `bin/user` hardcodes `scripts/turn.sh` | `bin/user/src/main.rs` line 136 | holds |
| `turn.sh` stops on `idle` and `exhausted` | `scripts/turn.sh` | holds |
| `compact-e2e.sh` 12 scenarios, 50 assertions | `docs/phase-2-readiness.md` section 2 | holds |

The plan ports `step.sh` faithfully. Exit codes match. Marker
semantics match. Lock semantics match. The two internal tensions
are already handled. The byte-identical module move lands before
the token-count swap. The swap gets its own trigger-timing gate.
See plan section 10, the stage 0 note.

**Verdict: the plan is ready to implement as a port.** The risk is
port fidelity. The parity gate (plan section 8) covers it.

## 2. The hexagonal question: what stays swappable

The plan keeps the hexagonal property for **stage runners** and
**wiring**:

- `StageRunner` is the one seam over the stage binaries. See plan
  section 3.3. Phase 2 ships the subprocess implementation.
  Phase 4 swaps in in-process and wasm runners. No loop rewrite.
- The composition root is the `harness` binary. The `[loop]`
  config table is the switch. See plan section 5.
- `harness-common` stays pure and wasm-safe. See
  `docs/phase-2-crate-research.md` section 6.

The property does **not** hold for the **compact strategy**.
"In-place compact, then continue" is baked into the loop. It
sits behind no port.

### 2.1 Where the in-place strategy is baked in

1. The `StageRunner::compact` payload. `CompactStatus` names only
   the in-session outcome: `noop`, `compacted`, `failed`. It
   names no handoff result.
2. The `awaiting_model` branch order. See plan section 4.3. The
   threshold hook, last-resort compact, overflow re-run, and
   length-stop strip all end the same way. They re-project
   through `assemble`. They continue in this session.
3. The `run` skeleton. See plan sections 4.1 and 4.6. One
   `SESSION` argument. One flock for the process life. One
   `loop.pid`. Nothing rebinds the session mid-run.

No session-creation port exists. `bin/user` creates session
dirs. The TUI append path creates its own dir. The loop holds
none.

### 2.2 The handoff strategy, mapped to the tree

The candidate swap: summarize the session effort. Seed a new
session with the summary. Point it at the old `events.jsonl`.
Continue the loop in the new session.

| Handoff need | State in the tree | Cost |
|---|---|---|
| A summary of the session effort | `assemble --summary-input` plus `model`. The iterative merge and file-op lists live in `bin/compact` | **exists. Reuse as-is.** |
| A `context_exhausted` marker with `new_session` | Schema exists. `bin/log` validates it. `claim` maps it to `exhausted` | **exists. One append.** |
| TUI reattach to the seeded session | `pending_handoff` plus `Action::Handoff` in `bin/tui/src/app.rs` | **exists. Manual `h` key today.** |
| Create and seed the new session dir | Only `bin/user` and the TUI append path create dirs | **Gap. New port or fs work in the loop.** |
| Rebind the loop to the new session mid-run | Absent. The lock invariant says "process life" (plan 4.6) | **Gap. Loop skeleton and lock change.** |
| TUI follows the new session automatically | The `h` key is manual. The tailer binds one session | **Gap. TUI change.** |

The vocabulary side of the handoff survived the correction-57
retirement intact. The schema still requires `new_session`.
`claim` still reports `exhausted`. The TUI still reads
`pending_handoff`. The retired part is create, seed, rebind.

### 2.3 Is the swap clumsy?

As the plan stands, the swap touches four modules:
- `harness-common`: the outcome type.
- `bin/harness`: the strategy branch, the rebind, the lock swap.
- the session-creation path.
- `bin/tui`: auto-follow.

That is clumsy. It is not a drop-in.

The fix is one small seam added to the Phase 2 spec before the
loop is written: the lifecycle-window dispatcher and the
`SessionStore` port (docs/loop-lifecycle-hooks.md). The
`ContextStrategy` port sketched here is superseded by the window
design. The `SessionStore` port survives as the fs boundary behind
the `exhausted.handle` hook. The loop skeleton executes the decision
from the hook. It does not branch on a strategy name.

That seam costs a dispatcher module, a set of window names, and one
config key. If you add it after the loop is written, it also costs
re-pointing the 12-scenario compact e2e. Add it in Phase 2 or skip it.

## 3. Other debts found

- **Stage 0 mixes two gates.** The stage-0 gate names the
  byte-identical module move and the char/4 to measured-token
  swap. The second breaks `assemble` byte-identity by design.
  Keep them as separate commits. Give each its own gate, as the
  plan note intends.
- **Per-step probe cost.** The one-token probe is a model call on
  the hot path. Gate it on the measured last-count screen. That
  is the mitigation in `docs/phase-2-crate-research.md` section
  8. On the local GPU model the latency shows. Measure it in
  stage 2.
- **TUI auto-follow.** If the handoff strategy is chosen, the TUI
  must follow the `new_session` pointer itself. The `h` key
  covers the manual case. Note the seam now. It is not cheap
to discover late.

## 4. Recommendation

1. Implement the plan as a port. It is ready.
2. Add the `hooks` module and the `SessionStore` port to plan
   section 3.3. The `hooks` module owns the lifecycle-window
   dispatcher. The `SessionStore` port owns fs seeding. Default to
   the in-place compact hook. Register the handoff hook as the
   alternate. See `docs/loop-lifecycle-hooks.md`.
3. The handoff strategy is now a hook registration, not a parked
   itch. It is ready when the `exhausted.handle` window ships.
   See `docs/loop-lifecycle-hooks.md` section 5.2.
