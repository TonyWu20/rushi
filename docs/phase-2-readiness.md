# Phase 2 readiness: the loop into a Rust binary

Status: Review (2026-09-03).

This document answers one question. Is the harness ready to write the
loop management into a Rust binary, as `architecture.md` Phase 2
defines it? The grounds are the commits through `e741467` and the
commands run on 2026-09-03.

## 1. What the loop owns today

The architecture doc predates several loop features. A Phase 2 binary
must own every row in the table. Each row names the current location
and the source commit.

| Behavior | Today's location | Source |
|---|---|---|
| Turn loop without a step cap | `scripts/turn.sh` | correction 51 |
| The step pipeline: claim, assemble, model, parse, route, log | `scripts/step.sh` | Phase 1 |
| Crash recovery: re-route the pending tool calls, no model call | `scripts/step.sh` | G2, `refinement-policy.md` |
| Steer and follow queues: the steer injects at the step boundary, the follow drains at idle | `claim`, `step.sh`, `bin/user` | `cef8496` |
| The threshold auto-compact hook before every request | `step.sh`, `bin/compact` | `745dab5`, correction 63 |
| Overflow recovery: one compact, one re-run, then the last-resort forced compact | `step.sh`, `bin/compact` | `745dab5` |
| The length-stop recovery: log the truncated group, strip it, compact, re-run | `step.sh`, `compact --strip-last-assistant` | `745dab5` |
| The silent-overflow check on a successful call | `step.sh` | `745dab5` |
| The kill switch and the cooldown gate the compact paths | `step.sh`, `bin/compact` | `745dab5` |
| The `loop_phase` markers (`wait`, `tools`) | `step.sh` | `32e8e94` |
| The `model_thinking` publish, on change only | `step.sh` | `f8c2725` |
| The per-session tool log: full output plus the slim index | `route --tool-log` | correction 58 |
| The session `cwd` file, read by `route` | `bin/user` writes it | entry-point rule |
| Model error retry (two, 3 s) and the empty-turn guard (three) | `step.sh` | `empty-turn-root-cause.md` |
| Config resolution: the compact knobs, the window, the budget | `step.sh` awk scrape, `model --describe` | `58aa520` |

The sticky compact form, the lever state (`compact.json`), and the
byte-stable prefix stay in `assemble` and `compact`. The loop only
calls them. The Phase 2 scope is the orchestration, not the compact
engine.

In the OS + Applications model (`skill-remapped-to-os-apps.md` §2),
the loop binary is the Tier 1 kernel: the agent loop core. The
Phase 2 binary is that kernel.

## 2. Stability evidence

Commands run in the repo root on 2026-09-03:

- `cargo build --quiet` — exit 0, no warnings.
- `cargo test --quiet` — all crates pass. One TUI timing test fails
  once under full-suite load (finding R10). The re-run passes
  373/373 in `bin/tui`.
- `bash scripts/compact-e2e.sh` — 12 scenarios, 50 assertions,
  0 failures.
- `bash scripts/tool-conformance.sh` — 41/41.
- `bash scripts/cache-e2e.sh` — skipped. The gate is
  `DEEPSEEK_API_KEY`, which is not set in this shell.
- `sessions/` holds 28 session directories. The heaviest real
  session is `better-ui-colors_h1` at 6,909 events. The recorded
  analysis in `notes/harness-vs-pi-model-latency.md` counts 426
  assistant turns in it.

The compact e2e locks the auto-compact state machine. The
conformance suite locks the tool contract. Phase 1 is stable and
its behavior is pinned by tests.

## 3. Findings

**R1 — The replacement point already exists.** The TUI supervises the
opaque `[loop]` command in `config.toml` (`bash scripts/turn.sh`).
The TUI holds no loop internals (`tui.md` §2.3). The swap to a Rust
binary is a one-line config change.

**R2 — A second coupling site.** `bin/user` hardcodes
`scripts/turn.sh` in `run_turn` (`bin/user/src/main.rs`). It ignores
the `[loop]` command. The Phase 2 switch must update it.

**R3 — No session lock.** No lock file and no `flock` exist in the
scripts. The one-loop-per-session rule holds only through the TUI
`loop.pid` probe at start (FT-003). A direct `bash scripts/turn.sh`
start bypasses it. The Phase 2 binary takes a `flock` on the session
dir and writes `loop.pid`. The TUI probe reads the same record.

**R4 — Cancellation is kill-only.** The TUI stops the loop group with
`SIGTERM` and escalates to `SIGKILL` after 3 s. Nothing in the loop
reads a `cancel` event. The Rust binary owns the cancellation
handles. Decide who appends the `cancel` event (today: the TUI).

**R5 — Tool calls run in series.** `route` consumes the stdin
`tool_call` lines one at a time. Phase 2 may add a bounded parallel
run. Start serial for behavior parity, add parallelism as a later
stage.

**R6 — Fragile config scraping.** `step.sh` reads `[limits]` and
`[model]` through four awk snippets. The binary parses TOML natively.
The knob set is `context_budget_tokens`, the `compact_*` knobs,
`max_output_tokens`, and the per-model `context_tokens`.

**R7 — The shared-crate tension is the main decision.** Phase 1
forbids a shared crate. The itch list (`notes/itches.md`) has passed
the rule of three: `LogLine` in four copies, the validator in three,
the compact trigger math in two, the marker schema list in two. The
new binary becomes the fifth `LogLine` copy and the fourth validator
copy without a shared crate. `refinement-policy.md` P3 sets
promotion at three or more copies of the same structure, and at the
JSON schemas stable through 20 real sessions. Eight sessions carry
heavy content, and the replay coverage is unit-level today. The
Phase 2 spec must name the choice: a small shared crate (say
`bin/common`) holding `LogLine`, the validator, and the compact math,
or one more duplication until Phase 3. This review names the shared
crate: a fifth copy is a maintenance trap, and the new binary is
the natural host.

**R8 — No approval gate.** `route` runs a tool at once, under caps
and a timeout. No producer emits `approval_request`. The schema is
still missing (the G3 known limit in `tui.md` §13.5). The TUI
renders the banner, but the loop never waits on an answer. Phase 2
must name this a non-goal or add the policy hook now. The
architecture doc names a policy pipeline as core. Today it does not
exist. The fenced-host caps and the `not_run` reports exist in
`route` (`skill-remapped-to-os-apps.md` §6). The audit of the
failure reports is still pending.

**R9 — Doc drift.** `architecture.md` §5.2 shows a stale event
contract: no `v` field, no `ext_status`, no compaction markers, no
`queue` field, no tool log. `INDEX.md` still marks
`tui-pending-user-messages.md` stage 2 as open. It shipped in
`cef8496`. The `scratch/` triage is still pending
(`skill-remapped-to-os-apps.md` §11).

**R10 — One flaky TUI test.** `ext::tests::append_whitelist_rejects_and_accepts`
timed out once under full-suite load. It passes 3/3 in isolation.
Record it. Fix the timing gate when the TUI next moves.

## 4. Verdict

**Ready to proceed, under six conditions.**

1. Write the Phase 2 spec first. Base it on the §1 list and the real
   event vocabulary (`schemas/events/v1`, `tui.md` §2.2). Not on
   `architecture.md` §5.
2. The binary owns the loop, the retries, the compact hooks, the
   follow drain, the marker publishes, the session lock, and the
   cancellation handles. It still spawns the stage binaries, as
   `architecture.md` §6 requires.
3. Settle R7 (shared crate) in the spec, before the first commit.
4. Move `bin/user` onto the `[loop]` command (R2).
5. Keep an e2e parity gate. Re-point `scripts/compact-e2e.sh` at the
   new step command. All 12 scenarios must pass before `step.sh`
   retires.
6. Tool-call parallelism (R5) is a later stage inside Phase 2. The
   serial path is the baseline.

Nothing in the recent commit history blocks Phase 2. The loop is
specified, tested, and locked by e2e. The open items are decisions,
not build failures. The binary is the Tier 1 kernel named in
`skill-remapped-to-os-apps.md` §2.

## 5. Evidence

Recent commits evaluated (2026-08-29 to 2026-09-03):

- `e741467` bon builder pattern across the wide functions.
- `3e59e11` the 31 dead-code warnings cleared.
- `7b327f0` the tool_result demo off the default layer, the pty
  smoke hardened.
- `cef8496` the steer/follow split at the loop side.
- `f373dcd` the TUI pass: tool boxes, thinking control, markdown
  grids, color schemes.
- `745dab5` the in-session auto-compaction (correction 63).
- `f8c2725` the `model_thinking` publish.
- `32e8e94` the `loop_phase` markers.
- `58aa520` the context budget matched to the model window.
- `6d305ec` the reattach across a TUI restart (FT-003).
- `60b88e2` the locked single-write `LogLine` (FT-005).
- `e6aac64` the per-session tool log (correction 58).

Commands and results: see section 2.
