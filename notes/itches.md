# Itches

Problems to solve.

## LogLine is a four-way copy (2026-08-30)

`LogLine`, the only type that may write a session log (FT-005),
lives in four places:

1. `bin/log/src/logline.rs`
2. `bin/user/src/logline.rs`
3. `bin/tui/src/port_file.rs` (embedded `mod logline`)
4. `bin/route/src/logline.rs` (the per-session tool log,
   correction 58)

The duplication follows the phase-1 policy (no shared crate). It
joins the validator as a promotion candidate for a shared
`core`/`bin/common` crate. Keep the four copies in sync. Do not
grow them in parallel.

## Event schema validator is now a third copy (2026-08-27)

Producer-side G3 validation (check the event against
`schemas/events/v1/<type>.json` before append) exists in three
places:

1. `bin/user/src/main.rs` — `validate_event` (user_message producer).
2. `bin/log/src/main.rs` — loads all schemas, validates every line.
3. `bin/tui/src/port_file.rs` — minimal local validator used by
   `append_event`.

The duplication is intentional for Phase 1 (no shared `core` crate,
per `architecture.md`), but the third copy crosses the promotion
threshold in `refinement-policy.md`. Candidates for one shared
validator:

- a small `schemars`-free JSON-Schema subset crate (`core` or
  `bin/common`), or
- one binary that owns validation and the producers call it.

Blocker to watch: the validator subset must stay small enough to be
portable. The TUI copy (`port_file.rs`) supports only `const`,
`required`, `properties`, `items`, and primitive `type` checks
(string/integer/number/boolean/array/object). Do not grow the three
copies in parallel.

## The compact trigger math is a second copy (2026-09-03)

`bin/compact` duplicates the trigger math and the cut walk of
`bin/assemble` (phase-1 policy: no shared crate). The one-step
predicted reading (`last + rate`), the `est_tokens` estimator, and
the backward cut walk each live in both binaries. The estimator
copy is noted in the `find_cut` doc comment. Keep the two copies in
sync. Do not grow them in parallel.

The trigger math joins the `LogLine` and validator copies as a
promotion candidate for a shared `core`/`bin/common` crate.

## The compact strategy is not a port (2026-09-07)

The Phase 2 plan (`docs/phase-2-plan.md`) bakes the in-place compact
strategy into the `harness` loop. The `StageRunner::compact` payload
names no handoff outcome. The `awaiting_model` branch ends every
recovery path with a re-projection in the same session. The `run`
skeleton holds one session, one flock, one `loop.pid` for the
process life. A handoff strategy (summary, seeded new session, pointer
to the old `events.jsonl`, rebind the loop) touches four modules:
`harness-common` (the outcome type), `bin/harness` (the strategy
branch, the rebind, the lock swap), the session-creation path, and
`bin/tui` (auto-follow). It is not a drop-in as the plan stands.

The fix is one seam in the Phase 2 spec: a `ContextStrategy` port
(action enum: in-place, handoff, stop) and a `SessionStore` port
(create and seed the session dir). The default implementation is the
in-place behavior. The handoff is a second implementation and one
config key. No loop rewrite. The vocabulary side already exists:
the `context_exhausted` schema with `new_session`, the `claim`
`exhausted` state, the TUI `pending_handoff` and the `h` key.
Recorded in `docs/phase-2-plan-audit.md` section 2. Trigger: the
owner's 2026-09-07 question on strategy swappability. Episode 1 of
3 (P9). No live episode demands the handoff yet. Add the seam to
plan section 3.3 before the loop is written. Skip it if the
in-place strategy is the final design.

## The marker schemas join the validator list (2026-09-03)

The three auto-compact marker schemas (`compaction_started`,
`compaction_failed`, `compaction_summary`) join the `bin/log`
hardcoded schema list and the `bin/claim` no-op list. The
`bin/compact` binary does not validate: it pipes every marker
through `bin/log`, the owner of the validator list. The
`bin/tui` semantic parser gains the three `EventKind`s.
