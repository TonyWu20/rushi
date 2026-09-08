# Itches

Problems to solve.

## LogLine is a four-way copy (2026-08-30) → resolved 2026-09-08

At the Phase 2 split, `LogLine` moved to the shared
`rushi-common` crate (`crates/rushi/src/logline.rs`). All kernel
producers (`bin/log`, `bin/user`, `bin/route`, `bin/rushi`) import
it. The TUI copy moved to the `rushi-tui` repo. This itch is closed.

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

## Event schema validator is now a third copy (2026-08-27) → resolved 2026-09-08

At the Phase 2 split the validator moved to the shared `rushi-common`
crate (`crates/rushi/src/event_validation.rs`). All kernel producers
(`bin/user`, `bin/log`, `bin/rushi`) call it. The old TUI copy moved
with the TUI to the `rushi-tui` repo. This itch is closed.

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

## The compact trigger math is a second copy (2026-09-03) → resolved 2026-09-09

`bin/compact` duplicated the trigger math and the cut walk of
`bin/assemble` (phase-1 policy: no shared crate). The one-step
predicted reading (`last + rate`), the `est_tokens` estimator, and
the backward cut walk each lived in both binaries. The estimator
copy is noted in the `find_cut` doc comment. Keep the two copies in
sync. Do not grow them in parallel.

Resolved: the math now lives once in `crates/rushi/src/compact_math.rs`
(the shared `rushi-common` crate, docs/phase-2-plan.md section 6).
`bin/compact` imports `rushi_common::compact_math`; the local
copies are removed.

## The compact strategy is not a port (2026-09-07) → resolved 2026-09-08

The Phase 2 plan (`docs/phase-2-plan.md`) baked the in-place compact
strategy into the `harness` loop. The `StageRunner::compact` payload
named no handoff outcome. The `awaiting_model` branch ended every
recovery path with a re-projection in the same session. The `run`
skeleton held one session, one flock, one `loop.pid` for the
process life. A handoff strategy touched four modules.

**Resolution (2026-09-08).** The design in
`docs/loop-lifecycle-hooks.md` resolves this by decomposing the
monolithic strategy into fine-grained lifecycle windows. Each window
is an independent, swappable extension point. The
`exhausted.handle` and `overflow.resolve` windows carry the decision
vocabulary (`stay_compact`, `handoff`, `stop`). The `SessionStore`
port survives as the fs boundary behind the `exhausted.handle` hook.
The loop no longer branches on a strategy name; it fires the window
and applies the decision. The swap is now a hook registration plus
config, not a loop rewrite.

## The marker schemas join the validator list (2026-09-03)

The three auto-compact marker schemas (`compaction_started`,
`compaction_failed`, `compaction_summary`) join the `bin/log`
hardcoded schema list and the `bin/claim` no-op list. The
`bin/compact` binary does not validate: it pipes every marker
through `bin/log`, the owner of the validator list. The
`bin/tui` semantic parser gains the three `EventKind`s.

## `lean-verify` `drt` op has no progress, checkpoint, or smoke mode (2026-09-12, episode 1) → resolved 2026-09-12

**Observed.** Running `lean-verify op=drt` with `n=100000`
takes ~3 h with zero output until completion. No progress
counter, no checkpoint/resume, no `--smoke` alias. When the
goal was re-posted (context overflow) mid-run, the only way to
know whether the gate was still alive was `ps aux` — the tool
gave no feedback.

**Reproduce.** Pipe `{"op":"drt","n":100000,...}` to
`target/release/lean-verify`; wait 3 h with no stdout until
the final JSON blob.

**P4 checklist.**
1. Unblock a current task? Marginally — a `--smoke` (n=2000,
   ~15 s) would make the "quick check" a one-liner instead of a
   judgment call.
2. Correctness bug? No.
3. Recurring manual step? Yes — every DRT run requires manually
   picking `n` and deciding whether to background the process.
4. Invariant → code? Partially — a `--smoke` flag moves "how
   many inputs for a quick check" from human discipline to a
   named preset.

**Pre-test.** A `--smoke` alias or auto-tier (`<10k` = fast,
`≥10k` = full) is a one-line config, not a protocol change.
But per P2 rule-of-three, one episode is not enough to add a
new flag to the tool. **Parked as an itch.**

**Resolution (2026-09-12).** The itch was confirmed real and solved
in the exts-owned `lean-verify` tool (`rushi-exts/goal-tools/lean-verify/`):
- `"smoke":true` is the named quick tier (n=2000, ~15 s): the
  quick check is a one-liner, no judgment call on `n` (an
  explicit `n` still wins; the full tier gates the release).
- A progress/heartbeat file `<dir>/.drt-progress.json` is written
  every ~10 s or 100 inputs (pid, next index, rate, eta,
  `updated_at`): a re-posted goal polls it instead of `ps aux`;
  a clean run deletes the file.
- A stop on mismatch/timeout keeps the file as a checkpoint, and
  `"resume":true` continues the same call from the first failed
  index (parameters must match the checkpoint; `input_gen` must
  be deterministic). The result JSON reports `progress_file`,
  `checkpoint`, and `resumed_from`; a live run is protected by a
  pid liveness guard.
Covered by `scripts/lean-verify-drt-e2e.sh` (kill+resume,
fix+resume, live-run guard) and the drt steps of
`scripts/lean-verify-e2e.sh`.

## `lean-verify` `drt` op has no input-validation mode (2026-09-12, episode 1) → resolved 2026-09-12

**Observed.** The `input_gen` parameter is a shell command that
must emit well-formed scenario lines. There is no way to verify
that the generator's output is parseable without running the
full DRT comparison. When a test list contained a line that was
supposed to be malformed but was actually well-formed, the only
detection path was a full DRT run (~3 h for 100 K inputs).

**Reproduce.** Write a generator that emits one unparseable line;
run `lean-verify op=drt` with `n=100000`. The mismatch is only
visible after the full comparison completes.

**P4 checklist.**
1. Unblock a current task? Yes — a `--check-inputs` mode (parse
   each generated line, report the first unparseable one, exit)
   would save a 3 h run on a broken generator.
2. Correctness bug? No, but a *wasted-compute* bug: 3 h × 100 K
   inputs to discover the generator was off by one.
3. Recurring manual step? Yes — every time the generator or the
   protocol changes, re-validation requires a full gate run.
4. Invariant → code? Yes — "the generator emits well-formed
   lines" is currently human discipline; a `--check-inputs`
   mode makes it a 10-second automated check.

**Pre-test.** The parser logic lives inside the Lean model
executable and the Rust production binary. There is no standalone
"parse one line" subcommand, so a pure script cannot do the job
without duplicating the parser. A tool-level `--check-inputs`
flag (or a separate `op=check-inputs`) is the right shape.
**Parked as an itch** (one episode).

**Resolution (2026-09-12).** Solved by a new `op=check-inputs` in the
exts-owned `lean-verify` tool (`rushi-exts/goal-tools/lean-verify/`): it
runs the generator's lines through the
model executable (and the production executable when given) and
reports the first line either side rejects — a non-zero exit or a
timeout; accepted inputs exit 0. Default n=2000 makes the
preflight ~10 s instead of a 3 h full drt; `"stop_on_reject"`
stops at the first rejection, `"max_rejections"` bounds
collection. It shares the progress/checkpoint/resume machinery
with drt. The invariant "the generator emits well-formed lines"
moved from human discipline to an automated check. Covered by the
check-inputs steps of `scripts/lean-verify-drt-e2e.sh` and
`scripts/lean-verify-e2e.sh`.
