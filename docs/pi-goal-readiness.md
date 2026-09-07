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
| `run.idle` hook binary (continue loop when goal open) | `goal-hooks/hook-goal-idle/` (rushi-exts) | Medium |
| `compact.before` hook binary (preserve goal across compaction) | `goal-hooks/hook-goal-compact/` (rushi-exts) | Small |
| `tool.before` hook binary (block stale goal tool calls) | `goal-hooks/hook-goal-tools/` (rushi-exts) | Small |
| `model.before` hook binary (inject goal-mode instruction) | `goal-hooks/hook-goal-arm/` (rushi-exts) | Small |
| Token / budget accounting | `crates/goal-state/` (budget_tokens, used_tokens) | Medium |
| User-facing goal commands (`goal`, `goal edit`, `goal resume`) | `ui_extensions/goal/` | Done |
| Conformance test: `run.idle` continue e2e | `run-idle-continue-e2e.sh` (rushi-exts root) | Done |

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
five user-facing commands in the TUI palette: `goal`, `goal edit`,
`goal pause`, `goal clear`, and `goal resume`. The `goal` and
`goal edit` commands arm an in-memory flag in the TUI extension; the
next `user_message` event (forwarded via `kinds = ["user_message"]`)
triggers a direct write of `goal.json` in the session dir — no agent
round-trip needed. `goal pause` sets `active = false`; `goal clear`
deletes `goal.json`; `goal resume` re-activates a blocked or
completed goal by rewriting `goal.json` directly, then the `run.idle`
hook continues the loop. The `model.before` hook
(`harness-hook-goal-arm`) reads `goal.json` and appends a cache-stable
goal block (objective + goal-mode rules + trust-boundary framing,
ported from pi-goal's prompt template) to every model request while a
goal is active. The agent-facing tools (`goal`, `goal_complete`,
`goal_blocked`) remain under `tools/` and are invisible in the
user-facing palette.

**G3 — No conformance test for `run.idle` continue. Resolved (2026-07-04).**
`run-idle-continue-e2e.sh` (rushi-exts root) covers nine scenarios (no-goal,
active-goal, closed-goal, wrong-id, contradictory, paused,
blocked-stops, cleared, block-stable) with 26 assertions, all passing.

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
  dir; supports `new/load/save/is_open/pause/resume/
  mark_complete/mark_blocked/edit_goal/build_continue_prompt/
  build_goal_block/format_duration/format_token_count`.
  No budget cap (user decision, docs/goal-ux.md §1.6).
- `tools/goal/`, `tools/goal_complete/`, `tools/goal_blocked/` — CLI
  tools that read/write `goal.json` via `HARNESS_SESSION_DIR`. The
  `goal` tool edits an active goal in place when one exists, and
  creates a fresh one otherwise. `goal_complete` rejects
  contradictory summaries (P9). `goal_blocked` records the block
  reason.
- `goal-hooks/hook-goal-idle/` (rushi-exts) — `run.idle` hook: continues the loop with a
  `follow`-queue `user_message` when a goal is open; stops when
  closed. No budget check (§1.6).
- `goal-hooks/hook-goal-compact/` (rushi-exts) — `compact.before` hook: always-allow
  (no budget veto; §1.6).
- `goal-hooks/hook-goal-tools/` (rushi-exts) — `tool.before` hook: blocks stale goal-tool
  calls (e.g. `goal_complete` when no goal is active).
- `goal-hooks/hook-goal-arm/` (rushi-exts) — `model.before` hook: reads `goal.json`
  (goal.json-driven, §1.1b) and appends the cache-stable goal block
  (objective + goal-mode rules + trust-boundary framing) as the last
  item in `request.input` while a goal is active. No log-derived
  state (§1.1b); byte-stable across turns (P16/P17).
- `bin/tui/` — zero goal-state coupling (docs/goal-ux.md §1.7):
  the host exposes a generic `row` capability
  (docs/ui-extension.md section 4) that the goal extension owns;
  no goal fields, no goal-specific rendering, no `goal` /
  `goal_edit` special-casing.
- `run-idle-continue-e2e.sh` (rushi-exts root) — conformance e2e (26 assertions,
  9 scenarios: no-goal, active-goal, closed-goal, wrong-id,
  contradictory, paused, blocked-stops, cleared, block-stable).
- `ui_extensions/goal/` — TUI extension that registers `goal`,
  `goal edit`, `goal pause`, `goal clear`, and `goal resume` in the
  command palette (G2 resolution). `goal` and `goal edit` set an
  armed flag; the next `user_message` event triggers a direct write of
  `goal.json` (no agent round-trip, §1.1/§1.8). It also owns the
  host-reserved row slot (docs/ui-extension.md section 4, `row`
  capability): the goal status line (goal text, elapsed time, token
  count — no budget ratio, §1.7) and the armed hint (§1.8); the bare
  TUI shows no goal row. Standalone cargo
  package; build with `cargo build` in its directory.
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
