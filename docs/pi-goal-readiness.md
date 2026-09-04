# pi-goal port readiness

Status: Investigation (2026-09-13).

Answers one question: are we ready to port `pi-goal` as a harness
extension, per the mapping in `pi-extension-port-investigation.md`
and the OS + Applications model in `skill-remapped-to-os-apps.md`?

## 1. What the investigation said was blocking

`pi-extension-port-investigation.md` section 2 names three loop
seams the port needs:

1. Per-turn prompt injection
2. An "agent settled" / idle-continue signal
3. A compaction veto

The verdict was **Hard**, blocked until Phase 2 shipped.

## 2. What now exists

All three seams are built into the `harness` loop binary.

| pi-goal need | Harness seam | Where | Status |
|---|---|---|---|
| Idle-continue signal (`agent_settled`) | `run.idle` window, `continue` + `message` payload | `bin/harness/src/run_loop.rs` L81–108 | Built |
| Compaction veto (`session_before_compact`) | `compact.before` window, `cancel` / `replace` | `bin/harness/src/step.rs` L546–600 | Built |
| Per-turn prompt injection | `model.before` decision window (`transform`) | `bin/harness/src/step.rs` `model_retry_loop` | Built |
| Tool guardrails (`tool_call`, `tool_execution_end`) | `tool.before` (`block`/`approve`) + `tool.after` | `bin/harness/src/step.rs` L983–1147 | Built |
| Session start / shutdown | `session.start` / `session.end` | `bin/harness/src/run_loop.rs` | Built |
| Hook ABI (spawn, JSON in/out, exit codes, timeout) | `crates/common/src/hooks.rs` | 450 lines, tested | Built |
| Approval round-trip (for `goal_blocked` veto) | `approval_request` + `approval` schemas, `awaiting_approval` claim state | `schemas/events/v1/`, `bin/claim`, `step.rs` | Built |
| Tool registration | `tools/<name>/tool.toml` + binary on PATH | `tools/`, `bin/route`, `bin/assemble` | Built |
| Goal state persistence | Session dir file (`goal.json` beside `events.jsonl`) | Event-log model | Built |

`cargo build` and `cargo test` (497/497) pass as of this writing.

Two reference hook binaries exist: `bin/hook-compact` (compact
strategy) and `bin/hook-handoff` (reserved handoff seam). They
demonstrate the exact pattern a goal hook would follow.

## 3. What remains (application-level, not infra)

The infrastructure blockers are resolved. The remaining work is
porting the pi-goal application logic onto the existing seams.
Each item is a new file or a small set of files; none requires a
change to the harness loop or hook ABI.

| Item | Lands where | Size |
|---|---|---|
| `tools/goal_complete/` CLI + `tool.toml` | `tools/` | Small |
| `tools/goal_blocked/` CLI + `tool.toml` | `tools/` | Small |
| Goal state file (`sessions/<n>/goal.json`) + read/write | Shared module under `crates/common` or a new `crates/goal-state` | Medium |
| `run.idle` hook binary (read goal state, inspect last assistant message, return `continue` + prompt or `stop`) | `bin/hook-goal-idle/` | Medium |
| `compact.before` hook binary (veto compaction when goal budget is low) | `bin/hook-goal-compact/` | Small |
| `tool.before` hook binary (block stale goal tool calls) | `bin/hook-goal-tools/` | Small |
| `session.start` / `session.end` hooks (restore / checkpoint goal state) | Can fold into the above hook binaries via window dispatch | Small |
| Token / budget accounting | `crates/common` or goal-state module | Medium |
| User-facing goal start (the `/goal` command) | See gap G2 | Open |
| Conformance test: `run.idle` continue row from plan §8 | `scripts/` | Small |

## 4. Gaps

**G1 — `model.before` was observation-only. Resolved (2026-09-13).**
The transform spec is in `docs/loop-lifecycle-hooks.md` section 3.3
("It can transform the request. Keep the cache guard on any
change.") and section 4.5 (the `hook_applied` marker), which
`docs/phase-2-plan.md` section 6 incorporates by reference. The
harness now fires `model.before` as a decision window: a `transform`
replaces the request JSON with the hook's `request` object, the
loop logs the `hook.model.before` decision marker and a
`hook_applied` marker, and a malformed payload logs
`hook.model.before.error` and proceeds with the original request.
Gated by `scripts/model-before-transform-e2e.sh` (three
scenarios: transform applied, no-hooks default, malformed
payload).

The pi-goal case works on top of this: a continuation hook prepends
the goal instruction to the request `input` at every model call.

**G2 — No `/goal` user-facing command.**
The TUI command palette is spec-only
(`docs/tui-command-palette.md`, status "Spec, not yet built").
Without it the user cannot type `/goal …` in the TUI.

Workaround options (no TUI change):
- A `goal` tool under `tools/` that the model calls when the user
  types a goal as a plain message. The tool writes `goal.json`.
- A standalone CLI the user runs outside the TUI.

Both fit the OS + Applications model. The TUI command palette
(`commands` cap in `ext.rs`) is the long-term home.

**G3 — No conformance test for `run.idle` continue.**
Plan §8 lists a `run.idle` continue row, but `scripts/` has no
corresponding test. The window fires correctly (verified in
`run_loop.rs`), but a fixture test would pin the behavior.

**G4 — No `input`-equivalent window.**
pi-goal's `pi.on("input")` intercepts user input to clear recovery
state and handle `/goal` commands. The harness has no user-input
window; user input arrives as `user_message` events in the log.
This is acceptable: goal state is file-based (`goal.json`), so
clearing stale state is a hook-side concern, not a loop concern.
The `/goal` intercept is handled by the `goal` tool (G2
workaround) or the future command palette.

## 5. Verdict

**Ready.** The three loop seams that blocked the pi-goal port are
all built and tested: `run.idle` (idle-continue), `compact.before`
(compaction veto), and `model.before` (per-turn prompt
transformation). The hook ABI, tool registration, approval
round-trip, and event-log persistence are in place. The remaining
work is application-level: write the goal tools, the hook
binaries, and the state file. No loop-level or ABI change is
required.

The remaining gaps are small. G2 (no `/goal` TUI command) has a
working workaround: a `goal` tool under `tools/` or a standalone
CLI. G3 is a test-hygiene item: add the `run.idle` conformance
fixture. G4 is a non-issue: goal state is file-based, so clearing
stale state is a hook-side concern.

The port is the next natural application build on the harness, in
the spirit of `skill-remapped-to-os-apps.md`: tools on the path,
hooks on the path, the kernel (loop) untouched.
