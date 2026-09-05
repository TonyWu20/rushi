# pi-goal port readiness

Status: Ported (2026-07-04). The pi-goal extension is implemented as a
harness extension per the OS + Applications model.

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

## 3. What was built (application-level)

The infrastructure blockers are resolved. The following application-level
pieces are now implemented:

| Item | Lands where | Size |
|---|---|---|
| `tools/goal/` CLI + `tool.toml` (start or edit goal) | `tools/goal/` | Small |
| `tools/goal_complete/` CLI + `tool.toml` | `tools/goal_complete/` | Small |
| `tools/goal_blocked/` CLI + `tool.toml` | `tools/goal_blocked/` | Small |
| Goal state file (`sessions/<n>/goal.json`) + read/write | `crates/goal-state/` | Medium |
| `run.idle` hook binary (continue loop when goal open) | `bin/hook-goal-idle/` | Medium |
| `compact.before` hook binary (preserve goal across compaction) | `bin/hook-goal-compact/` | Small |
| `tool.before` hook binary (block stale goal tool calls) | `bin/hook-goal-tools/` | Small |
| `model.before` hook binary (inject goal-mode instruction) | `bin/hook-goal-arm/` | Small |
| Token / budget accounting | `crates/goal-state/` (budget_tokens, used_tokens) | Medium |
| User-facing goal commands (`goal`, `goal edit`, `goal resume`) | `ui_extensions/goal/` | Done |
| Conformance test: `run.idle` continue e2e | `scripts/run-idle-continue-e2e.sh` | Done |

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

**G2 — No `/goal` user-facing command. Resolved (2026-07-04).**
The TUI command palette (`docs/tui-command-palette.md`) now supports
extension-registered commands via the `commands` cap.
`ui_extensions/goal/` is a standalone Rust extension that registers
three user-facing commands in the TUI palette: `goal`, `goal edit`,
and `goal resume`. The `goal` and `goal edit` commands arm goal mode
by appending a `goal_armed` ext_status marker to the session log; the
user then types the goal description in the main input box and sends
it. The `harness-hook-goal-arm` hook (registered on `model.before`)
detects the pending marker and injects an instruction into the model
request telling it to call the `goal` tool with the user's message as
the goal description. `goal resume` re-activates a blocked or
completed goal by rewriting `goal.json` directly, then the `run.idle`
hook continues the loop. The agent-facing tools (`goal`,
`goal_complete`, `goal_blocked`) remain under `tools/` and are
invisible in the user-facing palette.

**G3 — No conformance test for `run.idle` continue. Resolved (2026-07-04).**
`scripts/run-idle-continue-e2e.sh` covers three scenarios (no-goal,
budgeted-goal, closed-goal) with 9 assertions, all passing.

**G4 — No `input`-equivalent window.**
pi-goal's `pi.on("input")` intercepts user input to clear recovery
state and handle `/goal` commands. The harness has no user-input
window; user input arrives as `user_message` events in the log.
This is acceptable: goal state is file-based (`goal.json`), so
clearing stale state is a hook-side concern, not a loop concern.
The `/goal` intercept is handled by the `goal` tool (G2
workaround) or the future command palette.

## 5. Verdict

**Ported (2026-07-04).** The three loop seams that blocked the pi-goal
port are all built and tested: `run.idle` (idle-continue),
`compact.before` (compaction veto), and `model.before` (per-turn prompt
transformation). The hook ABI, tool registration, approval round-trip,
and event-log persistence are in place.

The application-level pieces are complete:

- `crates/goal-state/` — `GoalState` over `goal.json` in the session
  dir; supports `new/load/save/is_open/mark_complete/mark_blocked/
  resume/edit_goal/budget_exhausted/remaining_budget/
  continuation_prompt/read_last_assistant_output_tokens`.
- `tools/goal/`, `tools/goal_complete/`, `tools/goal_blocked/` — CLI
  tools that read/write `goal.json` via `HARNESS_SESSION_DIR`. The
  `goal` tool edits an active goal in place when one exists, and
  creates a fresh one otherwise.
- `bin/hook-goal-idle/` — `run.idle` hook: continues the loop with a
  `follow`-queue `user_message` when a goal is open; stops when closed
  or budget exhausted.
- `bin/hook-goal-compact/` — `compact.before` hook: preserves goal info
  into the compaction window.
- `bin/hook-goal-tools/` — `tool.before` hook: blocks stale goal-tool
  calls (e.g. `goal_complete` when no goal is active).
- `bin/hook-goal-arm/` — `model.before` hook: when a `goal_armed`
  ext_status marker is pending (armed by the TUI extension and not yet
  consumed by a `goal` tool_call), injects a goal-mode instruction
  into the model request telling it to call the `goal` tool with the
  user's message as the goal description.
- `scripts/run-idle-continue-e2e.sh` — conformance e2e (9 assertions,
  3 scenarios: no-goal, budgeted-goal, closed-goal).
- `ui_extensions/goal/` — TUI extension that registers `goal`,
  `goal edit`, and `goal resume` in the command palette
  (G2 resolution). `goal` and `goal edit` arm goal mode via a
  `goal_armed` ext_status marker; the user types the goal description
  in the main input box and sends it, and the `model.before` hook
  (`bin/hook-goal-arm/`) injects the goal-mode instruction into the
  model request. Standalone cargo package; build with `cargo build`
  in its directory.
- Plumbing: `session_dir` added to `RouteEnv`, `route` accepts
  `--session-dir` and exports `HARNESS_SESSION_DIR`, four hooks
  registered in `config.toml` / `config-low.toml`, goal tools listed
  in both system prompts.
- Bug fix: `hooks.rs` watchdog now uses `recv_timeout` instead of a
  bare `sleep`, so a fast-exiting hook no longer blocks for the full
  `timeout_ms` before the main thread can continue.

The port is the next natural application build on the harness, in
the spirit of `skill-remapped-to-os-apps.md`: tools on the path,
hooks on the path, the kernel (loop) untouched.
