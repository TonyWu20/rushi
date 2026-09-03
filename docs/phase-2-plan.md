# Phase 2: the loop into a Rust binary (`harness`)

Status: Spec (2026-09-03).

References: `architecture.md` §5-§7 (the roadmap, the event contract, the
guardrails). `phase-2-readiness.md` (the evidence and findings R1-R10
that triggered this spec). `refinement-policy.md` (P0, P3, P4).
`tui.md` §2.2-§2.3 and §13.3 (the event vocabulary, the opaque loop
command, loop supervision). `auto-compact-plan.md` §4.4 (the step
semantics this spec ports). `notes/itches.md` (the recorded
promotion candidates). `scripts/overflow-classify.sh` (the table this
spec ports). `coding-conventions.md` (the code rules the new Rust
must follow).

## 1. Purpose

`architecture.md` §6 defines Phase 2: the bash orchestration becomes
a small Rust binary (`harness`) that still spawns the same stage
binaries. One authoritative process owns looping, retries,
fan-out, the session lock, and cancellation.

The trigger pressure is recorded, not anticipated (P0):

- `scripts/step.sh` is now a 454-line recovery state machine
  (corrections 51-63). It branches on the claim state, the model
  stop reason, the overflow classifier, the compact outcome, and the
  length-stop case. `architecture.md` §5.3 names this exact pain:
  branching and retry logic promotes the glue to a binary.
- `step.sh` scrapes `[limits]` and `[model]` with four awk snippets
  (finding R6). One misread model section shifts the input budget
  and every compact threshold derived from it.
- No session lock exists. The one-loop-per-session rule holds only
  through the TUI `loop.pid` probe. A direct shell start bypasses it
  (readiness finding R3).
- Cancellation is a process-group kill. The loop has no cancellation
  handles of its own (finding R4).
- `bin/user` hardcodes `scripts/turn.sh`. The TUI uses the opaque
  `[loop]` command. Two entry points, one coupled, one decoupled
  (finding R2).
- The `LogLine` type is a four-way copy, the event validator a
  third, the compact trigger math a second. `notes/itches.md`
  records all three past the rule of three.

The necessary-change checklist (P4) passes: the change unblocks a
named roadmap task, moves the session-lock invariant from human
discipline to code, and removes the fragile awk-scrape failure
mode. The pre-test fails: a script cannot hold a typed retry
state machine, a `flock`-based lock, and cancellation handles in
one process. The recorded evidence allows the change.

## 2. What this phase adds

Two new workspace members:

- `bin/harness` — the loop binary. Subcommands `run` (the full loop,
  replaces `turn.sh`) and `step` (one step, replaces `step.sh`).
  It still spawns the stage binaries: `claim`, `assemble`, `model`,
  `parse`, `route`, and `compact`. `log` stays available to humans
  and e2e. The tools under `tools/` stay untouched.
- `crates/common` (package `harness-common`) — the shared utility
  crate. Modules: `logline` (the one `LogLine`), `event_validation`
  (the one validator), `compact_math` (the pure trigger math and
  cut walk), `stage` (the `StageRunner` trait and its payload
  types). Six existing binaries migrate to it.

Nothing else. No `core` crate. No in-process tools. No daemon.

## 3. The binary contract

### 3.1 CLI

```
harness run SESSION   [--config PATH]
harness step SESSION  [--config PATH]
```

- `SESSION` is a session name under `sessions_root` or a path.
- `--config` defaults to `config.toml`. The `CONFIG` environment
  variable sets it, as `turn.sh` and `step.sh` do today.
- The e2e overrides survive: `MODEL_BIN`, `COMPACT_BIN`,
  `ASSEMBLE_BIN` point the loop at stub binaries, as `step.sh`
  does today. `compact-e2e.sh` sets `MODEL_BIN` to a stub model.
  `cache-e2e.sh` runs the real binaries under its `DEEPSEEK_API_KEY`
  gate.
- Diagnostics go to `stderr`. The loop writes no structured
  `stdout`. Events reach the session log, never the pipe. The
  transcript print `turn.sh` does on stop goes. The session log
  holds the record.
- Exit codes: `0` on a clean stop (idle with no pending follow-ups,
  exhausted, or a logged terminal error event). `1` on a hard
  failure (config unreadable, a log append fails, the lock is
  held). `143` on `SIGTERM`, `130` on `SIGINT` for `run`.
  `step` exits `1` on a signal, matching the `step.sh` trap.

### 3.2 Config surface (read from `config.toml`, parsed as TOML)

| Key | Use |
|---|---|
| `[paths].sessions_root` | resolve `SESSION` |
| `[active].model` | the active model section |
| `[model].max_output_tokens` | the length-stop test (default 32768 when absent) |
| `[model."NAME"].context_tokens` | the model window (default 131072 when absent) |
| `[limits].context_budget_tokens` | the input budget (the clamp math is in `assemble`) |
| `[limits].compact_enabled` | the kill switch (default true when absent) |
| `[limits].compact_strategy` | context-overflow strategy. The only shipped value is `compact` (in-session shadow compact: the compact step asks the model to write a handoff document, shadow the old log region with that document, and continue in the same session; `docs/deepseek-harness-compaction-research.md`). The key is a seam for future strategies. |
| `[limits].approval_timeout_s` | the `awaiting_approval` wait bound. Absent: the loop waits for the answer without limit. Set: on timeout the loop synthesizes a deny `tool_result` and continues (4.8) |
| `[hooks]` | lifecycle hook registration: `timeout_ms` and the ordered `on` list (see `docs/loop-lifecycle-hooks.md`) |

Three new keys: `compact_strategy`, `approval_timeout_s`, and
`[hooks]`. The loop reads these once per step. The awk scrapes
go away. The `assemble` and `compact` binaries read the remaining
`[limits]` compact knobs (`compact_reserve_tokens`,
`compact_keep_tokens`, `compact_reasoning_effort`) through
`--config`. The loop passes the config path and parses none of them.
No `compact_summary_max_tokens` cap: the handoff doc length is
model-determined, bounded only by the model's own `max_output_tokens`.

### 3.3 Stage runner seam (future-proof, subprocess now)

The loop state machine depends on one trait, not on process
handles:

```rust
pub trait StageRunner: Send {
    fn claim(&self, session: &SessionDir) -> Result<Claim>;
    fn assemble(&self, session: &SessionDir, opts: &AssembleOpts) -> Result<RequestFile>;
    fn model(&self, request: &RequestFile) -> Result<ModelOutput>;
    fn parse(&self, output: &ModelOutput) -> Result<ParsedEvents>;
    fn route(&self, calls: &[ToolCallEvent], env: &RouteEnv) -> Result<Vec<ToolResultEvent>>;
    fn compact(&self, session: &SessionDir, opts: &CompactOpts) -> Result<CompactStatus>;
}
```

Phase 2 ships the one implementation: it spawns the stage binaries
with today's exact argv. `run` and `step` differ only in whether
the claim loop wraps the steps.

This trait is the seam that `architecture.md` §4.4 and §6 plan
for. Phase 3 implements it over `crates/core`. Phase 4 swaps in
in-process and wasm runners. The state-machine code written against
the trait moves to Phase 3 without rewrite. The trait is structure,
not a feature. No second runner ships in Phase 2.
The trait and its payload types live in `harness-common` as the
`stage` module. Phase 3 moves them to `crates/core` with the
state machine. Appends are not a stage: the loop appends
in-process through the shared `LogLine` and validator, one
flock-protected write per line (the FT-005 fix). The `log`
binary stays for humans and e2e.

### 3.4 Event boundary

The loop emits only the event types that exist in
`schemas/events/v1` today:

- `ext_status` — the `loop_phase` and `model_thinking` markers
  (the loop publishes them, as `step.sh` does).
- `error` — the terminal events of the recovery paths.
- Everything else (`assistant_message`, `tool_call`,
  `tool_result`, the three `compaction_*` markers,
  `context_exhausted`) lands through the stage binaries, exactly as
  today.

Two sanctioned new event types, both already named in the TUI
`EventKind` (see `docs/tui.md` §13.5): `approval_request` and
`approval`. The TUI renders the request banner and answers it
today. The loop is the missing producer and consumer (4.8). The two
schema files join `schemas/events/v1/`. The validator glob picks
them up with no code change (section 8, the validator-glob row).
No `v` bump.

Everything else stays closed (P1a). The `cancel` event stays
TUI-owned. The loop never appends it (parity with today: `step.sh`
appends no cancel line).

### 3.5 Session artifacts

| File | Writer | Reader |
|---|---|---|
| `sessions/<n>/events.jsonl` | every writer, through `LogLine` | claim, TUI tailer, humans |
| `sessions/<n>/tools.jsonl` | `route` (via `--tool-log`, unchanged) | humans, the TUI later |
| `sessions/<n>/cwd` | `bin/user` (unchanged) | `route --cwd` |
| `sessions/<n>/.loop.lock` | `harness run` holds the flock | `harness run`, the TUI probe |
| `sessions/<n>/loop.pid` | `harness run` (own pid, after the lock) | the TUI probe and stop |
| `sessions/<n>/handoff.md` | `harness` on compact (saved in the same session dir) | the model, `read` tool, humans |

The three new artifacts are files, not events. Old sessions replay
unchanged. The TUI tailer watches `events.jsonl` only, so the new
files disturb no render.

## 4. Behavior

### 4.1 `run`: the turn loop

```
loop:
  step(session)                      # section 4.2
  claim = claim(session)
  if claim.state == idle and claim.pending_follow_ups is empty:
    fire run.idle window (4.9)
    if decision is continue: append the follow message, continue
    else: stop, exit 0
  if claim.state == exhausted: stop, exit 0
```

Port of `turn.sh` including the follow-drain: a queued follow
message runs as a new turn, one drain per boundary
(`docs/tui-pending-user-messages.md` stage 2).

### 4.2 `step`: the one-step pipeline

Given the claim state:

| State | Action | Observable result |
|---|---|---|
| `idle`, no follow-ups | publish `model_thinking` (on-change gate), stop | no new events, except a possible `model_thinking` marker |
| `idle`, follow-ups pending | publish `model_thinking`, set `assemble --inject-follow`, then run the `awaiting_model` branch of 4.3 | the queued follow messages ride this step's new turn, one drain per boundary |
| `exhausted` | stop | no new events, except a possible `model_thinking` marker |
| `awaiting_tool_result` | publish `loop_phase=tools`, route the pending calls with no model call, append the results | one `tool_result` per pending call, crash recovery G2 |
| `awaiting_model` | the branch in 4.3 | per 4.3 |
| `awaiting_approval` | wait for the `approval` event on the matching `approval_request` id, then apply the answer (4.8) | the pending `approval_request` is answered, one `tool_result` or `tool_call` re-dispatch, then the step continues |

Step entry always runs the `model_thinking` on-change publish
before `claim`, as `step.sh` does: the input-border color reflects
the active model even while the step routes tools or the session
idles.

### 4.3 The `awaiting_model` branch, in order

Port of `step.sh` section 4 with identical constants. No knob
gains a config key: the limits are named constants in the binary.

1. Publish `loop_phase=wait`.
2. Resolve the compact config for the active model (section 3.2).
   Call `model --describe` once per step and reuse the result for
   the active model, the guard model id, and the thinking level
   (today: three calls. Parity of source, fewer spawns).
3. Proactive threshold check (pi-style, stateless). Before each
   model call, the loop computes the current context size:
   the last measured `usage.input_tokens` from the log plus a
   chars/4 estimate of trailing messages after that usage.
   This matches pi's `estimateContextTokens`. If the total
   exceeds the trigger level (`budget - reserve`), the loop
   compacts before sending the request. The check is stateless:
   no `compact.json` file, no `trim_engaged` latch, no keep-
   halving or group-drop levers. After a compaction, the
   boundary advances and the next check starts fresh. A post-
   compact sanity check compares the projected post-compact size
   to the trigger level; if the keep window plus summary still
   exceeds the trigger, the loop logs a warning instead of
   compacting again. The `compact_enabled` switch gates both the
   proactive check and the reactive overflow compact in step 6.
4. `assemble` into the work file. An `error` form: append it to the
   log and stop the step (exit 0. The `error` event idles the
   claim).
5. The `context_exhausted` form: fire the `exhausted.handle`
   lifecycle window (`docs/loop-lifecycle-hooks.md` §3.4). The
   only shipped strategy is in-session shadow compact
   (`docs/deepseek-harness-compaction-research.md`):
   - The compact step appends a handoff-instruction prompt to the
     current context. The model writes a structured handoff document
     covering goals, state, open questions, and next steps.
   - The loop saves that document to `sessions/<n>/handoff.md`.
   - It then appends a `compaction_summary` event with
     `first_kept_seq`. The next `assemble` builds the request as:
     fixed system + tools + the handoff doc content + the
     post-boundary events.
   - Shadowed events stay in `events.jsonl`. The model can re-read
     them via `read` or `bash` at any time. The session continues
     in place. No new session is created.
   The `compact_strategy` config key (only value: `compact`) is
   a seam for future strategies. No second strategy ships in
   Phase 2. `harness step` follows the same branch. A step cut
   mid-compact recovers on the next start through `claim` and the
   `context_exhausted` marker (G2).

   The `overflow.resolve` window (same doc §3.4) fires on any
   overflow/silent/length-stop classification inside the retry loop
   (step 6 below). The default decision is `stay_compact`: the loop
   runs one in-session shadow compact and retries. The window is
   the seam for future custom strategies. See
   `docs/loop-lifecycle-hooks.md` for the full window list and
   decision vocabulary.
6. The model call, in the retry loop, with the exact current rules:
   - Binary crash or API failure: 2 retries, 3 s between. Then a
     terminal `error` event and a stop.
   - Overflow: the stop reason is `error` and the detail matches
     the classifier (4.4), and the request model equals the guard
     model. First overflow: one `compact --reason overflow` plus one
     re-run. When the kill switch is off, or this is the second
     overflow: the last-resort forced compaction, then one more
     call. A failure after the last resort logs a terminal `error`
     event and stops.
   - Silent overflow: a successful call whose measured input
     tokens meet the input budget, and the request model equals
     the guard model. Compact only, no re-run. The kill switch off
     sends the path to the last resort.
   - Length stop: a `length` stop with output below the configured
     max output. Log the truncated group, then
     `compact --reason overflow --strip-last-assistant`, re-run.
     Once. A second length stop takes the last-resort path, then one
     more call.
   - Empty turn: no text and no tool calls. Up to 3 total
     attempts (the initial call plus 2 retries). A persisted
     empty turn logs a terminal `error` and stops.
7. `parse`. Exit 1 routes the tool calls. Before the `route`
   call, the loop fires the `tool.before` window on the pending
   batch (`docs/loop-lifecycle-hooks.md` §3.5). The window
   carries the pending `tool_call` events. Three outcomes:
   - `proceed` (default, no hook answer): route the batch as
     today (`loop_phase=tools`, `route` with `--tools`,
     `--cwd`, `--tool-log`).
   - `block`: the hook payload carries `reason` and an optional
     `calls` list (default: all pending calls). The loop
     synthesizes a `tool_result` per blocked call with
     `is_error: true` and the reason as the result text. No
     `route` call is made for blocked calls. The model reads the
     reason on the next step and can correct the command. This is
     the mechanism that makes `no-find-grep` and `no-bare-python`
     work without in-process extensions.
   - `approve`: the hook payload carries `prompt` and a `call_id`.
     The loop appends an `approval_request` event for that call and
     transitions to `awaiting_approval` (4.8). Unblocked calls in
     the same batch route normally; the approved call waits.
   After `route` completes, the loop fires the `tool.after`
   window (observation, no decision) with the results. Then append
   the parsed and routed lines. Any other `parse` exit appends the
   parsed lines only.
8. Every append goes through the shared `LogLine` and validator. A
   failed append aborts the step with exit 1, as today.

### 4.4 The overflow classifier

The table in `scripts/overflow-classify.sh` ports to a Rust
module. Rules port verbatim:

- Case-insensitive match over the lowercased detail.
- The exclusion table (5 patterns: throttling, rate limits, 429,
  `throttl`) checks first. An excluded detail is never overflow.
- The overflow table (25 patterns, pi plus the SGLang and DeepSeek
  shapes) matches next.
- An empty detail is not overflow. The transport path takes it.
- The ten `--self-test` rows of the script become unit tests with
  the same expected outputs. They pin the table through the port.

### 4.5 Markers

- `loop_phase`: the `wait` and `tools` values, appended exactly
  where `step.sh` appends them. One `ext_status` event per
  publish, validated, `id` is `loop_phase`.
- `model_thinking`: the resolved thinking level (0-4) from
  `model --describe`. On-change gate: the last `model_thinking`
  value in the log tail (the current 4096-line window) equal to the
  level skips the publish. The first publish happens on step entry.
  A `describe` failure skips the publish. The TUI falls back to its
  default level, as today.

### 4.6 The session lock

`harness run` acquires an exclusive `flock` on
`sessions/<n>/.loop.lock` before any work. The lock holds for the
process life, not just a step. One session owns the lock for the
whole run. No stale-lock cleanup exists, and none is needed: a dead
holder releases the lock.

- Contested lock: exit 1. The message names the live loop pid read
  from `loop.pid`.
- After the lock: write `loop.pid` with the harness pid (the
  process-group leader when the TUI starts it through `setsid`).
- The harness does not delete `loop.pid` on exit. A stale pid plus
  a free lock reads as not-running to the probe (5.2).
- The TUI probe sees the released lock as free on exit. No
  re-tail is needed: the session continues in place. No TUI loop
  change is required.

### 4.7 Signals and cancellation

- `run` installs handlers for `SIGTERM` and `SIGINT` (tokio
  signal). On either: cancel the in-flight stage child (a
  `CancellationToken` per spawn), remove the work directory,
  exit `143` or `130`.
- A step cut mid-way leaves the log in its crash state. The next
  start recovers through `claim` and the `awaiting_tool_result`
  branch (G2). This is the current behavior of a killed
  `turn.sh`, made explicit.
- No `cancel` event append on either signal (3.4). The TUI
  appends `cancel` when it asks for the stop. A direct terminal
  `Ctrl+C` appends nothing, as a killed `turn.sh` does.
- Stage children share the harness process group when the TUI
  starts the loop (the TUI's `setsid` pre_exec). A group kill from
  the TUI still reaches them. The harness handler covers direct
  signals to the process.

### 4.8 The approval round-trip (human-in-the-loop)

This section closes the gap identified in
`docs/pi-extension-port-investigation.md` (the `rpiv-ask-user-question`
and `pi-automode` permission-ask requirements). It adds a bounded
human-in-the-loop mechanism so that a `tool.before` hook or a tool
itself can pause the loop and ask the user for a decision before a
tool call runs.

**New events.** Two new event types join `schemas/events/v1/`:

- `approval_request` — `{id, tool_call_id, prompt}`. The loop
  appends this when a `tool.before` hook returns an `approve`
  decision. The `prompt` is the human-readable question. The
  `tool_call_id` binds the request to the pending tool call that
  triggered it.
- `approval` — `{id, decision: "allow" | "deny", arguments?: object}`.
  The user's answer. `arguments` is optional: when present (an
  "edit" path in the TUI), it replaces the tool call's arguments.
  The TUI already derives the oldest pending `approval_request` and
  renders the banner (`docs/tui.md` §13.5, G6). It already appends
  `approval` events on `y`/`n`/`e`.

**New claim state.** `claim` gains one state: `awaiting_approval`.
It is derived when the log's last event for the active
`approval_request` id is an unanswered `approval_request`.
The `claim` binary returns it alongside the four existing states.

**Loop behavior (new `step` branch, §4.2 row):**

On `awaiting_approval`, the loop polls the log for an `approval`
event matching the pending `approval_request.id`. It reads the
log tail periodically (the same mechanism the TUI tailer uses;
the loop process can tail the file or use `inotify`). On a match:

- `decision: "allow"` — re-dispatch the pending tool call through
  `route` (with the original or the edited `arguments`). The
  `tool_result` lands as usual. The step continues.
- `decision: "deny"` — the loop appends a `tool_result` with
  `is_error: true` and the prompt text as the result body. The
  model reads the denial on the next step. No `route` call.

If `[limits].approval_timeout_s` is set and the wait exceeds it, the
loop synthesizes a deny `tool_result` with the reason
"approval timed out after N s" and continues the step. When the key
is absent, the loop waits indefinitely. A `SIGTERM`/`SIGINT` during
the wait exits per 4.7. The unanswered `approval_request` remains in
the log. The next `harness run` re-derives `awaiting_approval` via
`claim` and resumes the wait. This is crash-recovery by the existing
G2 principle.

**Interaction with `tool.before`.** The `approve` decision in
§4.3 step 7 is the producer. A hook on `tool.before` returns
`{"decision": "approve", "payload": {"prompt": "...", "call_id":
"<id>"}}`. The loop appends the `approval_request`, appends
synthesized `tool_result` events for any unblocked calls in the
same batch (if the hook did not block them), and transitions to
`awaiting_approval`. This is how a `pi-automode`-style permission
hook would work: the deterministic layers run as `block` decisions;
the LLM-classifier layer that is unsure emits `approve` with a
human-facing prompt.

**What this is not.** It is not a policy pipeline. There is no
per-tool config tree, no matcher DSL, and no policy evaluation
engine. A `tool.before` hook decides which calls need approval.
The loop only implements the wait-and-apply mechanics. The full
policy pipeline remains Phase 3 core work (readiness R8).

**TUI side.** No TUI feature work. The banner, the `y`/`n`/`e`
keys, and the `approval` append already exist (the TUI side of
the approval mechanism is shipped). The only new behavior is that
the loop now *consumes* the `approval` event it already sees in
the log.

### 4.9 Goal continuation (the `run.idle` window)

This section closes the gap identified in
`docs/pi-extension-port-investigation.md` (the `pi-goal` requirement
for auto-continuation). It adds a single window so that an external
hook can keep the loop going when it would otherwise stop on idle.

**The window.** `run.idle` fires in the `run` loop at the point
where the loop would stop on an `idle` claim with no pending
follow-ups. It is the last check before the `stop, exit 0`
line. It carries the session name and the id of the last
`assistant_message`.

**Decisions.**

- `stop` (default, no hook registered or no hook answer): the
  loop exits with code 0. This is the current behavior and the
  byte-identical path when no `run.idle` hook is registered.
- `continue` — the hook payload carries a `message` string. The
  loop appends a `user_message` event with `queue = "follow"` and
  that text. The next iteration's `step` drains it as a new turn.
  The loop does not stop. This is the `pi-goal` continuation
  pattern: a hook that holds the goal state (in a session-dir
  file, like `goal.json`) inspects the last assistant message
  for the `goal_complete` tool call. If the goal is not complete,
  it returns `continue` with a continuation prompt that tells the
  model to keep working.

**Where the goal state lives.** The hook owns its own state file
under `sessions/<n>/` (for example `goal.json`). The loop does
not read or write it. It only fires the window and applies the
decision. This keeps the loop free of goal-specific logic, in
keeping with the hooks doc section 2 (the kernel owns the call
point, the application owns the logic).

**Bound.** The `run.idle` window has no iteration cap. A hook that
always returns `continue` produces an infinite loop. The guard is
the context budget: when the session exhausts its context window,
the `exhausted.handle` window fires and the `handoff` or `stop`
decision ends the cycle. The `compact.before` cancel decision (a
`pi-goal`-style budget veto) can also fire here to refuse a
compaction that would lose goal-critical context, forcing a
`stop`.

**What this is not.** It is not a `pi-goal` reimplementation in the
loop. The loop gains one window fire and one branch. The goal
state machine, the token budget, the `goal_complete`/`goal_blocked`
tools, and the `/goal` command are application-level work: tool
binaries under `tools/` and a hook binary on the hook path.

### 4.10 Tool sidecar services

A tool may talk to a long-lived external service (a browser engine,
a search index) exactly as the loop talks to the model server.
The service is a sidecar process on the host. It is not a harness
daemon and is out of scope for this phase. The tool binary spawns
the service on first use and talks to it over a socket or a pipe
subsequent calls. The `tb` (terminal-browser) port is the first
example: the `tb_fetch` and `tb_browser` tools are one-shot CLIs
under `tools/`. The Chromium daemon they talk to is a sidecar,
like the model server. This is a boundary clarification, not a
new design surface. The "No daemon" bullet in section 9 applies
to the harness core, not to the services a tool talks to.

## 5. Entry points and the TUI seam

### 5.1 The `[loop]` command

`config.toml` moves to:

```toml
[loop]
command = "harness"
args = ["run"]
arg_style = "append_session"
```

`.envrc` adds `target/debug` to the PATH (the guarded-line
pattern the root `.envrc` already uses for the extension build
dirs). The TUI resolves `harness`
on the PATH, starts it in its own process group, and streams its
output. No other TUI spawn change.

### 5.2 The `loop.pid` writer moves

- The TUI `spawn_loop` stops writing `loop.pid`. The loop binary is
  the one writer (a file with two writers is the FT-005 defect in
  file form).
- The TUI probe gains one check, first: a non-blocking `flock`
  attempt on `.loop.lock`. Acquired: no live loop, proceed.
  Held: a live loop holds the session. The pid from `loop.pid`
  feeds the message and the stop path (`group_stop`), as today.
- `pid_is_loop` (pid alive plus the session id in its command
  line) stays as the pid-file check. The TUI appends the bare
  session name through `arg_style`, so the check passes
  unchanged. A direct start with a path form `SESSION` clears the
  lock-first check. The pid check stays advisory. The lock is the
  authority.

### 5.3 `bin/user`

`run_turn` reads the `[loop]` table from `config.toml` with the
same semantics as the TUI (command, args, `arg_style`, the config
directory as working dir, the absolute `CONFIG`). The hardcoded
`scripts/turn.sh` path goes. A missing `[loop]` table is a hard
error. `--no-run` is unchanged.

## 6. The `harness-common` crate

Modules, and the copies they replace:

| Module | Replaces | Callers after the move |
|---|---|---|
| `logline::LogLine` | the four copies: `bin/log`, `bin/user`, `bin/tui` (embedded module), `bin/route` | log, user, tui, route, harness |
| `event_validation` | the three copies: `bin/log` (full pass), `bin/user` (one type), `bin/tui` (minimal subset) | log, user, tui, harness |
| `compact_math` | the two copies: `bin/assemble` and `bin/compact` (the one-step reading, the token-count math, the backward cut walk) | assemble, compact |

Validator behavior after the move: the shared validator is the
`bin/log` implementation (the superset: `const`, `enum`,
`required`, `properties`, `items`, the primitive types). The TUI
copy is that subset, so it loses nothing. Schema loading globs
`schemas/events/v1/*.json` and derives the event type from
`properties.type.const`. The hardcoded ten-file list in `bin/log`
goes. A new marker type needs one schema file and zero code
changes in `log` or `harness`. That closes the itch entry
"The marker schemas join the validator list".

The `stage` module holds the `StageRunner` trait and its payload
types. It holds no copy and no event vocabulary. It joins
`crates/core` in Phase 3 with the state machine.

The `hooks` module (new) holds the lifecycle-window dispatcher and
the decision types. It spawns registered hook commands and folds
their stdout/exit-code into a typed decision. It is I/O-light: it
spawns and reads one line. The fs and lock work stays in the loop
through the `SessionStore` and `SessionLock` ports. See
docs/loop-lifecycle-hooks.md for the full window set and ABI.

Crate boundary rules (guardrail §7 stays intact):

- `harness-common` is a utility crate, not the Phase 3 `core`
  crate. It holds no event vocabulary type, no reducer, no loop
  state machine. Those move to `crates/core` in Phase 3, where
  `compact_math` and the event types join them.
- P3's gate on `core` (the schemas stable through 20 real
  sessions, replay tests over them) is untouched. The utility
  crate does not count against it.
- `logline` and `event_validation` do file and JSON work.
  `compact_math` is pure. The trigger decision consumes only
  measured `usage.input_tokens` from the log, plus a chars/4
  estimate of trailing messages after the last usage (matching
  pi's `estimateContextTokens`). The backward cut walk keeps a
  local chars/4 per-event sizing heuristic (pi's
  `estimateTokens`); the trigger itself never uses char-based
  math. The shadow projection uses the cut walk to determine
  `first_kept_seq`; the compact step then asks the model to write
  a handoff document for the shadowed region. The loop saves the
  document to `sessions/<n>/handoff.md` and the next `assemble`
  builds the request as: fixed system + tools + handoff doc
  content + post-boundary events. Shadowed events remain in
  `events.jsonl`; the model can re-read them via `read` or `bash`.
  The module owns no persistent state file. It is the Phase 3
  extraction candidate that lands in an I/O-free `core`.

Migration gate (stage 0 acceptance): every caller's behavior is
byte-identical. The full `cargo test`, `compact-e2e.sh` (12/12),
the TUI suite, and `tool-conformance.sh` (41/41) pass. One extra
check: `assemble` output on a real session is byte-identical
before and after the move. The provider prefix cache depends on
that byte stability.

## 7. Failure modes

| Condition | `run` | `step` |
|---|---|---|
| `config.toml` unreadable or a required key invalid | exit 1, stderr names the key | same |
| Session directory absent | `run` creates it on first append | create, then per state |
| Lock held by a live loop | exit 1, stderr names the pid | `step` takes no lock (a step is a tool, not a loop owner) |
| `loop.pid` unreadable or stale | proceed. The lock is the authority | n/a |
| A stage binary fails its contract (bad JSON, bad exit) | the stage's failure rule from 4.3 | same |
| A log append fails | exit 1, stderr | exit 1, stderr |
| `SIGTERM` / `SIGINT` mid-step | 143 / 130 after cleanup | exit 1 after cleanup |
| `model --describe` fails | the thinking publish skips, the step proceeds on the defaults | same |
| The model call fails after all retries | a terminal `error` event, exit 0 (the loop stops without error, the session reopens) | same |
| `awaiting_approval` and no answer arrives within `approval_timeout_s` | the loop synthesizes a deny `tool_result` (reason: "approval timed out") and continues | same |
| `awaiting_approval` and the loop is killed | the log ends in an unanswered `approval_request`; the next `harness step` re-derives `awaiting_approval` via `claim` and re-waits (G2) | same |

## 8. Conformance tests

Runnable per criterion 6: one binary, fixed input, observed
output. The tables pin the observable behavior.

| Test | Input | Expected |
|---|---|---|
| `run` idle stop | session log ends in a turn boundary | exit 0, no new events |
| `run` follow drain | idle tail plus one `queue=follow` message | one new turn carries the message, then exit 0 |
| `run` exhausted stop | a `context_exhausted` tail | exit 0, no new events |
| lock rejected | a second `run` while the first holds the lock | exit 1, stderr carries the live pid |
| `SIGTERM` mid-model-call | signal after the model spawn | exit 143, the workdir is gone, the model child is dead |
| crash recovery | a log ending in an unresolved `tool_call` | `step` routes it, appends one `tool_result`, no model call |
| classifier table | the ten `--self-test` details | the ten recorded verdicts, as unit tests |
| classifier exclusion | `"ThrottlingException: Too many tokens"` | not overflow (the exclusion wins over a pattern match) |
| empty detail | `stop_reason=error`, no detail | the transport path, no compact |
| reactive overflow compact | an overflow `error` stop with a matching classifier detail | one `compact --reason overflow` marker pair, one re-run |
| shadow compact | a `context_exhausted` form with `compact_strategy = compact` | one `compaction_summary` event, one `handoff.md` in the session dir, the shadowed range logged, the next `assemble` skips shadowed events, no new session dir |
| last-resort failure | a failed summary call twice | a terminal `error` event, the loop stops in the session |
| length-stop strip | a `length` stop below max output | the truncated group in the log, `--strip-last-assistant` on the compact, one re-run |
| `model_thinking` gate | two consecutive steps, same level | one `model_thinking` event total |
| `loop_phase` markers | one step with a tool call | a `wait` then a `tools` marker, in that order |
| validator glob | a new `*.json` schema in the dir | `log` accepts its events with no code change |
| `user` entry | `user --session X` with a `[loop]` table | the loop starts through the table, not a hardcoded script |
| `tool.before` block | a hook on `tool.before` returns `block` with a `reason` | one `tool_result` per blocked call with `is_error: true` and the reason; no `route` spawn for those calls |
| `tool.before` approve | a hook on `tool.before` returns `approve` with a `prompt` | one `approval_request` appended, the step halts in `awaiting_approval` |
| `awaiting_approval` allow | an `approval` event with `decision: "allow"` | the tool runs once through `route`, one `tool_result` |
| `awaiting_approval` deny | an `approval` event with `decision: "deny"` | one `tool_result` with `is_error: true` and the prompt text; no `route` call |
| `awaiting_approval` crash | a log ending in an unanswered `approval_request` | `step` re-waits; no `route` until an answer arrives |
| `run.idle` continue | a `run.idle` hook returns `continue` with a `message` | one `user_message` (`queue=follow`) appended, the loop continues |
| `run.idle` default | no `run.idle` hook registered | exit 0, byte-identical to the no-hooks path |
| parity | one fixture session through old `step.sh` and `harness step` | byte-identical `events.jsonl` |

The parity row is the mutation gate: a dropped rule in the port
diverges the fixture logs and fails the diff. The parity script
(`scripts/loop-parity.sh`) is temporary. It deletes with the
scripts it diffs against (stage 4).

## 9. What this does not do

Named here so the scope holds (criterion 4, and the user's
YAGNI guard):

- No `core` crate. P3's gate decides `core`, not this phase.
- No in-process or wasm stage runners. The trait is the seam.
  Phase 4 fills it.
- No in-process hook ABI. Hooks are subprocess commands on a path.
  The dispatcher spawns and reads stdout. No shared memory, no
  plugin loader, no daemon (`docs/loop-lifecycle-hooks.md` §8).
- No matcher DSL or per-event config tree. Each hook binds to one
  named window. Filtering is the hook's own job from the stdin
  JSON (`docs/loop-lifecycle-hooks.md` §4.1).
- No parallel tool calls. `route` takes the batch, runs it in
  series, as today. The later parallel stage swaps the batch body
  for a bounded fan-out. Invariant it must hold: `claim` resolves
  tool results by id, so completion order is safe. The stage that
  adds fan-out proves that against the log.
- No broad policy pipeline. The bounded approval round-trip
  (4.8) is in scope: one `approval_request`, one `approval`, the
  `awaiting_approval` claim state. The general policy engine
  (per-tool config, matcher DSL, multiple evaluators) is Phase 3
  core work (readiness R8, left open).
- No new event type, no `v` bump. Two new config keys: `[limits].compact_strategy` (default `compact`: in-session shadow compact. The key is a seam for future strategies.) and `[hooks]` (window registration, `docs/loop-lifecycle-hooks.md` §4.1).
- No harness daemon, no socket API. The single authoritative
  process shape is the one `architecture.md` §8 names for a future
  `jsonrpsee` layer. It wraps this state machine. Nothing here
  precludes it. A tool that talks to a long-lived external
  service (a browser engine, a search index) is a sidecar, not a
  harness daemon (4.10).
- No TUI feature work beyond the two named seams (5.1, 5.2).
- No CI. The script gates stand.

## 10. Stages

Each stage ships green before the next starts. "Green" is the
stage gate, not a promise.

### Stage 0 — `harness-common` and the migration

Build `crates/common` with the four modules. Migrate `log`,
`user`, `route`, `tui`, `assemble`, `compact`. The local copies
and the hardcoded schema list go.

Replace the Phase 1 token-math and sticky state. The `est_tokens`
char/4 estimator in `bin/compact` and its mirror in `bin/assemble`,
and the `compact.json` state file (`trim_engaged` latch, `last_tokens`)
are removed. The Phase 1 sticky state re-fired the compact on every
step once engaged, causing the repeated-compaction cascade in
`tui-picker-follow-up`.

- `compact_math` in `harness-common` stays pure. The trigger
  decision uses only measured `usage.input_tokens` from the log
  plus a chars/4 estimate of trailing messages, matching pi's
  `estimateContextTokens`. The cut-point walk keeps a local
  chars/4 per-event sizing heuristic (pi's `estimateTokens`);
  the trigger never uses char-based math.
- No `compact.json` state file. No `trim_engaged` latch. No
  keep-halving or group-drop levers.
- The compact step appends a handoff-instruction prompt to the
  current context. The model writes a structured handoff document
  covering goals, state, open questions, and next steps. The loop
  saves it to `sessions/<n>/handoff.md` and appends a
  `compaction_summary` event with `first_kept_seq`. The next
  `assemble` builds the request as: fixed system + tools + handoff
  doc content + post-boundary events. Shadowed events remain in
  `events.jsonl`; the model can re-read them via `read` or `bash`.
  No new session is created.
- Post-compact sanity check: after writing the summary, compare
  the projected post-compact size to the trigger level. If it
  still meets or exceeds the trigger, log a warning and skip the
  next compaction instead of looping.
- This part is not byte-identical (the trigger timing changes).
  Gate it with the compact trigger-timing e2e tests. The
  section 6 byte-identity gate covers the module moves.

Gate: section 6 migration gate, plus the TUI probe tests
(`port_file` suite) against the shared validator.

### Stage 1 — the loop skeleton

`bin/harness` with `run`/`step`, the TOML config read, the claim
branches (`idle`, `exhausted`, `awaiting_tool_result`,
`awaiting_approval`), the session lock, the `loop.pid` write, the
signal handlers, the markers (4.5), the `StageRunner` trait with its
subprocess implementation.

Gate: a manual TUI session (start, stream, stop, restart, the
probe shows the running loop, the stop works). The lock-rejected
and `SIGTERM` rows of section 8 pass. `harness step` recovers a
crashed session (the crash-recovery row).

### Stage 2 — the full step pipeline

The `awaiting_model` branch (4.3), the classifier module (4.4)
with its unit tests.

Gate: `compact-e2e.sh` re-pointed at `harness step` — all 12
scenarios, 50 assertions. The parity script passes against the old
`step.sh` on three fixture sessions (one idle, one mid-turn
crash, one near the compact trigger). The lifecycle-window
firing order is pinned by the hook-marker assertion (each
`ext_status` id `hook.<window>` appears in log order). The
`tool.before` window fires before `route`; the `tool.after`
window fires after. The block and approve decision rows of §8
pass.

Add the shadow-compact row of section 8: a fixture that exhausts
with the default `compact_strategy` produces one `compaction_summary`
event, one `handoff.md` in the session dir, the shadowed range
logged, and the next `assemble` skips the shadowed events.
Gate: one live compact cycle against the SGLang server (the trigger
fires, the shadowed region is recorded, the next assemble skips it,
the old log is untouched).

### Stage 3 — the entry points and hooks

`[loop]` in `config.toml` points at `harness`. `.envrc` gains
`target/debug`. `bin/user` reads the `[loop]` table (5.3). The
TUI stops writing `loop.pid` and the probe takes the lock first
(5.2).

Ship the `hooks` module and the shadow-compact hook:
`harness-hook-compact`. It runs the in-session shadow compact
(the compact step produces the handoff document; the loop saves it
and shadows the old region). Register it in `[hooks]` of
`config.toml`. The hook calls the `SessionStore` port for the fs
work; it returns the decision envelope to the dispatcher. Gate:
the conformance row in §8 passes with the hook registered.

Ship the approval round-trip (4.8): the two schema files
(`approval_request.json`, `approval.json`) in
`schemas/events/v1/`, the `awaiting_approval` branch in `step`, the
`tool.before` `approve` decision, and the `[limits].approval_timeout_s`
key. The TUI already renders the banner and appends `approval`
events; the loop now consumes them. Gate: the
`awaiting_approval` rows of §8 pass.

Ship the `run.idle` window (4.9). The window fires at the idle
stop point in the `run` loop. The default is `stop` (byte-identical
to the no-hooks path). A registered hook that returns `continue`
with a `message` payload appends a follow `user_message` and the
loop continues. Gate: the `run.idle` rows of §8 pass.

Gate: the TUI end-to-end matrix — start a loop, stream, stop,
kill the TUI, restart, the probe reattaches (`verify-reattach.py`
passes: the running bit, the blocked restart, the loop outlives
the TUI quit, the trace record). `Ctrl+C` stops the external loop
(a manual check). The reattach script skips `Ctrl+C` by design:
the checked loop must stay alive. `user` starts a session loop
through the table. The `cache-e2e.sh` gate when
`DEEPSEEK_API_KEY` is set.

### Stage 4 — retire the scripts

Delete `scripts/turn.sh`, `scripts/step.sh`,
`scripts/overflow-classify.sh` (the self-test rows live in the
harness unit tests), and `scripts/loop-parity.sh`. Repoint
`cache-e2e.sh` at `harness step`.

Gate: the full matrix — `cargo test` (workspace),
`compact-e2e.sh` 12/12, `tool-conformance.sh` 41/41,
`tui-pty-smoke.py`, `verify-reattach.py` on a live loop, one real
model session to completion through the TUI.

## 11. Impact and migration

Affected:

- New: `bin/harness`, `crates/common`.
- New schema files: `schemas/events/v1/approval_request.json`,
  `schemas/events/v1/approval.json`. The validator glob picks
  them up; no code change in `log` or the validator.
- Changed: `log`, `user`, `route`, `tui`, `assemble`, `compact`,
  `claim` (the `awaiting_approval` state). The copy-to-crate
  migration. No CLI or wire change in the stage binaries.
  `bin/user` loop start. The TUI `loop.pid` writer and probe.
  `config.toml` `[loop]`, `[limits].approval_timeout_s`. `.envrc`.
- Unchanged: the tools under `tools/`, the existing schema files,
  the session log format, the `loop.pid` format,
  `verify-reattach.py`'s expectations.

Migration: additive. No `v` bump (P1b). Old sessions replay
unchanged: the two new artifacts are files beside
`events.jsonl`, and no reader other than the loop and the probe
knows them. A session directory written before this phase lacks
`.loop.lock` and works: the lock creates on first `harness run`.

Reversibility: the scripts stay until stage 4. Any stage reverts
by restoring `[loop]` to `bash scripts/turn.sh` and the `bin/user`
hardcode.

## 12. Risks

| Risk | Mitigation |
|---|---|
| The shared validator behaves differently from the TUI's minimal one | the shared validator is the `bin/log` superset. The TUI tests plus one live TUI session gate stage 0 |
| A subtle port miss in the retry/compact branches | the 12-scenario e2e and the parity diff. Both run the real branch shapes |
| `flock` on a new file confuses a TUI tailer | the tailer watches `events.jsonl` only (verified in `port_file`). No render path reads the lock file |
| A recycled pid in `loop.pid` blocks a start | the lock is the authority (4.6). The pid file feeds the message and the stop only. The existing cmdline check stays as a second gate |
| The `bon` rule on wide functions | `coding-conventions.md` applies to all new Rust. The step-pipeline entry point gets a builder at 8+ parameters |
