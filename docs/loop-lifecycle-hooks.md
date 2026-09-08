# Loop lifecycle hooks: granular control and the context-overflow strategy

Status: Spec (2026-09-08). Supersedes the `ContextStrategy` port in
`docs/phase-2-plan-audit.md` section 2.3 and the `ContextStrategy`
port in `docs/handoff-strategy.md` section 2. It amends
`docs/phase-2-plan.md` (sections 3.3, 4.3, 4.6, 9, 10) and
`docs/handoff-strategy.md` (section 2). It follows the OS +
Applications model in `docs/skill-remapped-to-os-apps.md`.

## 1. The problem this resolves

The audit found the context-overflow strategy is not a drop-in
component. It is welded into the loop. Three places weld it in:

- The `StageRunner::compact` payload names only in-session outcomes.
- The `awaiting_model` branch ends every recovery path the same way.
  It re-projects through `assemble`. It continues in this session.
- The `run` skeleton holds one `SESSION`, one lock, one `loop.pid`
  for the process life. Nothing rebinds the session mid-run.

The last session concluded the strategy is not swappable. The fix is
not a bigger strategy port. It is **granularity on the loop
lifecycle**. Break the loop into named windows. Fire a hook at each
window. The strategy stops being a branch in the loop. It becomes a
command on a path that a window invokes. That is the Unix answer to
`hooks`, in the same spirit as `skill-remapped-to-os-apps.md`.

## 2. The reframe: a hook is a command on a path

A hook is not a new runtime. It is not a daemon. It is not an
in-process plugin. A hook is a short-lived command, like a stage
binary or a tool. The harness spawns it at a fixed window. It passes
the window's context as one JSON object on `stdin`. The command reads
it, does its work in any language, and writes one JSON decision to
`stdout`. It exits `0` or `2`. The harness folds the decision into the
loop. The harness never imports the hook code. The ABI is bytes over
pipes, as in `architecture.md` section 3.

This is the OS + Applications split from
`skill-remapped-to-os-apps.md`:

- The **kernel** is the loop plus the fixed set of lifecycle windows
  plus the hook invocation ABI. It ships for every session.
- A **hook is an application**. It registers by being on a path. It
  self-documents via `--help`. It is project-specific and
  composable. It is a tool the *harness* calls, not a tool the *model*
  calls.

The one difference from a tool: the harness invokes a hook on a
schedule (a lifecycle window), not on a model tool call. Everything
else is the same Unix shape: a command, a manifest, `--help`, a
closed output contract.

## 3. The lifecycle windows (the granular control points)

The harness fires these windows. A hook can register on any of them.
Each window names a fixed point in the loop. The list is complete for
the current loop and maps to both reference models. It covers every
swappable blocking point in `docs/phase-2-plan-audit.md` section 2.2.

### 3.1 Session scope (per `harness run`, per session dir)

- `session.start` — after the lock, before the first `claim`.
  Fire-and-forget. No decision. Warm a cache, publish a session-level
  `ext_status`.
- `session.end` — on any exit: clean stop, terminal error, or signal.
  Carries a `reason`. No decision.

### 3.2 Step scope (once per `step`)

- `step.start` — after `claim`, before the branch dispatch.
  Observation. No decision.
- `step.end` — after the step work, before return to `run`.
  Observation. No decision.

### 3.3 Model-call scope (per `model` spawn, inside the retry loop)

- `model.before` — before the `model` spawn. Carries the session
  name, the model id, the projected token count, and the current
  request JSON. One decision: `transform` (default: proceed
  unchanged). A `transform` payload carries `request`: the full
  replacement request object. The harness applies it, logs the
  `hook.model.before` decision marker, and records a `hook_applied`
  marker so the cache-break is visible (4.5). A `transform` without
  an object `request` field is a non-blocking failure: the log
  carries `hook.model.before.error` and the original request
  proceeds. Keep the cache guard on any change.
- `model.after` — after the `model` call. Carries `stop_reason`,
  `detail`, and `usage`. Observation. No decision.

### 3.4 Compaction and overflow scope (the strategy surface)

These four windows make the overflow strategy a plug-in. They are the
heart of this design.

- `compact.before` — before any `compact` spawn. Carries `reason`
  (`threshold` | `overflow` | `last_resort`) and a `force` flag.
  Decisions: `proceed`, `cancel`, `replace`. `replace` lets a hook
  supply its own summary and boundary. This mirrors the pi
  `session_before_compact` cancel-or-customize room.
- `compact.after` — after a `compact` completes. Carries `status`
  (`noop` | `compacted` | `failed`) and `reason`. Observation.
- `overflow.resolve` — fires when the loop classifies a model call as
  overflow, silent overflow, a truncation stop, or a recoverable
  length stop. It carries `kind`, `stop_reason`, `detail`, `usage`,
  the `input_budget`, the `context_tokens`, and a `can_recover`
  boolean. Decisions: `stay_compact`, `stop`.
  This is the swappable point. The default answer is in-session
  shadow compact (`stay_compact`): the compact step produces a
  handoff document that shadows the old log region, and the session
  continues in place. The `handoff` decision value is reserved for
  future strategies. No second strategy ships in Phase 2.
- `exhausted.handle` — fires when `assemble` returns the
  `context_exhausted` form (the assembled context exceeds the input
  budget). It carries `input_budget`,
  `context_tokens`, and the last `compact` status. Decisions:
  `stay_compact` or `stop`. The default answer is `stay_compact`:
  the loop runs one in-session shadow compact and retries. The
  `handoff` decision value is reserved for future strategies.

### 3.5 Tool scope (per `route` batch)

- `tool.before` — before `route` for a pending tool batch. Carries
  the pending `tool_call`s. Three decisions:
  - `proceed` (the default when no hook answers) — route the batch.
  - `block` — the payload carries a `reason` string and an optional
    `calls` list (default: all pending calls). The loop synthesizes a
    `tool_result` per blocked call with `is_error: true` and the
    reason as the text. It skips `route` for those calls. The model
    reads the reason on the next step and can correct the command.
  - `approve` — the payload carries a `prompt` string and a
    `call_id`. The loop appends an `approval_request` event, holds
    the batch, and waits for an `approval` answer. On `allow` the
    tool runs (with optionally edited arguments). On `deny` the
    loop synthesizes a `tool_result` with `is_error: true` and the
    prompt text. See `phase-2-plan.md` §4.8.
- `tool.after` — after `route`. Carries the results. Observation.

### 3.6 Run-loop scope (per `harness run` iteration)

- `run.idle` — fires when the `run` loop is about to stop on an
  `idle` claim with no pending follow-ups. Carries the session
  name and the last `assistant_message` id. Two decisions:
  - `stop` (default) — the loop exits. This is the current
    behavior with no hooks registered.
  - `continue` — the payload carries a `message` string. The loop
    appends a `user_message` event with `queue = "follow"` and that
    text, then the next `step` drains it as a new turn. This is the
    seam for goal-continuation hooks (the `pi-goal` pattern): a
    hook that knows the goal is not yet complete returns `continue`
    with a continuation prompt.

## 4. The hook ABI (the Unix contract)

### 4.1 Registration

Registration is a config registry in Phase 2, with a documented path
for discovery later. The config surface:

```toml
[hooks]
timeout_ms = 30000

[[hooks.on]]
window  = "exhausted.handle"
command = "harness-hook-compact"

[[hooks.on]]
window  = "overflow.resolve"
command = "harness-hook-compact"
args    = []
```

Rules:

- The `on` list is ordered. The harness runs hooks in that order.
- A hook binds to exactly one `window`. There is no matcher DSL. No
  `if "tool(rm *)"` rules. No tool-name globs. Filtering is the
  hook's own job, from the JSON on `stdin`.
- The growth path is path discovery: a `hooks/<window>/<name>/`
  layout, each dir holding a `hook.toml` manifest (`command`, `args`,
  `timeout_ms`, `description`). The harness scans the dir, as `route`
  scans `tools/`. That is the `skill-remapped` register-by-path
  model. It is not a Phase 2 requirement.

### 4.2 Invocation

For each window, the harness spawns each registered hook:

- `stdin` is one JSON object: the window's event (the fields in
  section 3, plus a `window` field and the `session` name).
- `env` carries `SESSION`, `SESSIONS_ROOT`, `CONFIG`,
  `HARNESS_PHASE`, and the window name.
- One `write` of the JSON. Then the harness reads `stdout`.

### 4.3 The decision contract

A hook returns one of three outcomes:

- **Exit 0, empty stdout or `{}`** — no decision. The window default
  applies.
- **Exit 0, a JSON decision** — one object:
  `{"decision": "<name>", "payload": { ... }}`. The `decision` name
  comes from the window's closed vocab (sections 3.4, 3.5, and 3.6).
  The `payload` carries the window's extra fields, for example
  `{"new_session": "<name>"}` on `exhausted.handle`, `reason` on
  `tool.before` block, and `message` on `run.idle` continue.
- **Exit 2** — a blocking decision. The harness aborts the window's
  default action. On `overflow.resolve` and `exhausted.handle`, exit
  2 means `stop`. On `tool.before`, it means `block`. On `run.idle`,
  it means `stop`.
- **Any other non-zero exit** — a non-blocking failure. The harness
  logs an `ext_status` marker `hook.<window>.error` and applies the
  window default. A hook must never wedge the loop. A hook timeout
  is a default decision plus an error marker.

The decision vocab is the whole customize room. Each window exposes a
small closed set of decisions. Anything richer is inside the hook
binary. It stays out of the ABI. This is the restraint that keeps the
design Unix-like: the kernel owns a fixed call point and a closed
vocabulary. The application owns its own logic.

### 4.4 The event channel stays the log

Window firings and decisions log as `ext_status` markers:
`id = "hook.<window>"`, `value = "<decision>"`. No new event type.
No `v` bump. The log stays the source of truth and the TUI reattach
channel. The `context_exhausted` marker is part of the existing
schema. In Phase 2 the in-session shadow compact does not create a
new session, so the TUI stays on the same session dir and re-tails
the same `events.jsonl`.

### 4.5 Self-documentation and cache

Each hook command supports `--help`. It prints the window it serves,
the input JSON shape, and the decision vocab. There is no
`SKILL.md` file. This matches `skill-remapped-to-os-apps.md`
section 4.

A hook that mutates prompt content runs at `model.before`. The
harness records a `hook_applied` marker so the cache-break is
visible. The marker is an `ext_status` event with `id =
"hook_applied"` and the value is the command of the hook that
applied the transform. The prompt prefix stays byte-stable for every
other window.

## 5. The overflow strategy as a plug-in

The loop ships one overflow strategy: in-session shadow compact.
The strategy is registered through the lifecycle window, so a
future user can register a different hook to handle the same
window. The loop fires the window and applies the decision; it does
not hard-code a strategy name.

### 5.1 In-session shadow compact (the only shipped strategy)

No hook registers on `overflow.resolve` or `exhausted.handle`.
The window defaults are:

- `overflow.resolve` default is `stay_compact`.
- `exhausted.handle` default is `stay_compact`.

This is the out-of-the-box path. The loop runs an in-session
shadow compact: it appends the handoff-instruction prompt to the
current context, the model writes a structured handoff document,
the loop saves it to `sessions/<n>/handoff.md`, shadows the old log
region, and the next request is built as `system + tools +
handoff doc + new content`. The log stays append-only; shadowed
events remain in `events.jsonl` and the model can read them via
`read` or `bash`. No new session is created.

The `compact_strategy` config key (value `compact`) and the window
decision vocabulary (`stay_compact`, `handoff`, `stop`) are seams
for future strategies. No second strategy ships in Phase 2.

### 5.2 The handoff document, not a strategy

The handoff document is the artifact the compact step produces. It
is not a separate strategy. There is no escalation path and no new
session. The `handoff` decision value remains in the window
vocabulary as a reserved seam for future user strategies. No
second strategy ships in Phase 2.

### 5.3 What each blocking point in the audit maps to

- "The `CompactStatus` names only in-session outcomes" is fixed by
  the richer `compact.after` envelope and by moving the choice to
  `overflow.resolve`.
- "The `awaiting_model` branch order" is fixed by firing the windows
  instead of hard-coding the terminal action.

## 6. What pi does, and where we are the same

Pi 0.84.2 ships the same idea in-process. Its extension API is one
call, `pi.on(event, handler)`, over a typed `ExtensionEvent` union.
The events split into the same scopes as this design:

- Session: `session_start`, `session_shutdown`,
  `session_before_compact`, `session_compact`.
- Agent and turn: `agent_start`, `agent_end`, `turn_start`,
  `turn_end`, `message_start/update/end`.
- Tool: `tool_execution_start/update/end`, `tool_call`,
  `tool_result`.
- Model: `model_select`, `before_provider_request`.

The customize room is a typed return. `SessionBeforeCompactResult`
carries `cancel` and `compaction`. `InputEventResult` carries an
`action` of `continue`, `transform`, or `handled`. `ToolCallEventResult`
carries `block` and `terminate`. The loop owns the *when*. The
handler owns the *how*. The typed result is the room.

Pi's compaction is the direct model for our `compact.before` and
`overflow.resolve` windows. Its `session_before_compact` fires with a
`reason` of `manual`, `threshold`, or `overflow` and a `willRetry`
flag. A handler can cancel it or return its own `compaction` result.
Its auto-compact `_runAutoCompaction("overflow", willRetry)` is the
reactive, response-driven compact that this repo already ports. That
is why our `overflow.resolve` window carries the same `reason` and
`willRetry` shape.

Where we diverge is the handler shape. Pi uses an in-process typed
handler. We use a command on a path that returns JSON over the pipe.
We keep the event taxonomy and the closed decision vocab. We drop the
in-process runtime, the matcher DSL, and the multi-handler config
tree. That is the streamlining this design asks for. The kernel keeps
the call points and the vocab. The application is any short-lived
command.

## 7. Where this is deliberately smaller than Claude Code

Claude Code hooks are the upper bound. They add a large config tree,
per-event matchers, and five handler kinds: shell, HTTP, MCP tool,
prompt, and subagent. They also add exit-code-2 blocking and async
background hooks.

This design adopts two of those ideas and caps the rest:

- **Adopted:** the event taxonomy (session, turn, model,
  compaction, tool) and the idea that a handler returns a closed
  decision.
- **Capped:** the handler is a command on a path with a JSON
  decision. There is no HTTP hook, no MCP hook, no LLM-as-hook, no
  in-process plugin ABI, and no async/background hook in Phase 2.
- **Capped:** registration is a flat ordered config list. There is
  no matcher DSL. A hook binds to one named window. Filtering is the
  hook's own job.

These are growth paths, not omissions. A hook ABI that can spawn a
command today also spawns a command that wraps an HTTP call or an MCP
call tomorrow. The window and the pipe are stable. The handler is
free. That is the Unix answer to "feature rich without a bloated
core".

## 8. What is not in scope for Phase 2 (named so it is not cut)

- No in-process plugin ABI. No wasm hook. No daemon. No HTTP hook.
  No MCP hook. No LLM prompt hook. No async hook.
- No matcher DSL. No per-event config tree. Registration is the flat
  ordered `hooks.on` list in section 4.1.
- The approval round-trip adds two new event types (`approval_request`
  and `approval`). They ride the existing `schemas/events/v1/` dir
  and the validator glob. No `v` bump. This is the only sanctioned
  new event type in Phase 2.
- No TUI feature work beyond reading the existing
  `context_exhausted` marker for reattach and the existing approval
  banner. The `ask_user_question` rich dialog is application work,
  not a Phase 2 loop change.
- No change to the tool contract, the tool registry, or the stage
  binaries. Hooks are a new, parallel invocation path for the
  harness only. A tool that talks to an external daemon (for example
  a browser engine) is a long-lived sidecar service, like the model
  server. It is not a harness daemon.

## 9. The seams to add to the Phase 2 plan

These are the concrete edits to `docs/phase-2-plan.md` this document
authorizes.

- **Section 3.2 (config surface):** add `[hooks]` with `timeout_ms`
  and the ordered `hooks.on` list. The loop reads it once per step.
- **Section 3.3 (the seam):** add a `hooks` module to
  `rushi-common`, next to `stage`. It holds the window registry,
  the spawn-and-fold dispatcher, and the decision types. It is
  I/O-light: it spawns a command and reads one line. The fs and lock
  work stays in the loop through the `SessionStore` and `SessionLock`
  ports.
- **Section 4.3 (`awaiting_model`):** replace the hard-coded terminal
  action with a window fire. The branch computes the overflow
  classification, fires `overflow.resolve`, and applies the decision.
  The `Exhausted` form fires `exhausted.handle` and applies its
  decision. The compact path fires `compact.before` and
  `compact.after` around each `compact` spawn.
- **Section 4.6 (the lock):** the lock holds for the process life.
  The in-session shadow compact does not release or reacquire the
  lock. No rebind. The TUI probe sees the same lock held for the
  full run.
- **Section 4.3 step 7 (route):** fire `tool.before` before the
  route call. On `block`, synthesize `tool_result` events with the
  reason. On `approve`, append `approval_request` and wait in
  `awaiting_approval`. See `phase-2-plan.md` §4.8.
- **Section 4.1 / 4.2 (run loop and step table):** add the
  `run.idle` window (section 3.6). The `run` loop fires it before
  the idle-stop decision. A `continue` decision appends a follow
  `user_message` and the loop continues. The `step` table gains an
  `awaiting_approval` row.
- **Section 8 (conformance):** add two rows. One registers the
  in-place hooks and asserts byte-identical `events.jsonl` against
  the no-hooks default. One asserts the shadow-compact flow: a
  fixture that exhausts with the default `compact_strategy` produces
  one `compaction_summary` event, one `handoff.md` in the session
  dir, the shadowed range logged, and the next `assemble` skips
  shadowed events.
- **Section 9 (non-goals):** add "no in-process hook ABI, no
  matcher DSL, no daemon" to the named non-goals.
- **Section 10 (stages):** the hook dispatcher lands in stage 2 with
  the `awaiting_model` branch. The shadow-compact hook lands in
  stage 3 with the entry points. It keeps its own gate.

## 10. Reconciliation with the two prior docs

`docs/handoff-strategy.md` section 2 proposed one `ContextStrategy`
port with a `TerminalAction` enum. That port is the right shape for
the *terminal* action only. It is too coarse. The window design
replaces it with two windows (`overflow.resolve`,
`exhausted.handle`) and the `SessionStore` port behind them. The
handoff doc's section 3 (the doc format) still holds as the format
for the in-session `handoff.md` artifact. Section 9 of this document
is authoritative for the Phase 2 plan edits.

`docs/phase-2-plan-audit.md` section 2.3 answered "the swap touches
four modules, that is clumsy." That is true for a monolithic
`ContextStrategy` port. It is not true for the window design. The
swap now touches the generic dispatcher (written once), the window
definitions, and the strategy hook. It does not touch the loop's
core branch structure beyond firing the windows. The audit's
recommendation to add `ContextStrategy` and `SessionStore` ports is
superseded by the window + dispatcher + `SessionStore` design.
`SessionStore` survives as the fs port behind `exhausted.handle`.

`docs/itches.md`, the entry "The compact strategy is not a port
(2026-09-07)", is resolved by this document. The trigger and the
episode notes stay as history. The fix it proposed (one seam in
section 3.3) is implemented here as the window + dispatcher seam,
which is strictly more granular and covers all the audit blocking
points, not just the terminal action.

## 11. Acceptance

- The loop fires every window in section 3 at its named point.
- With no hooks registered, the loop is byte-identical to today's
  `step.sh` on the `compact-e2e.sh` fixtures. The in-place strategy
  is the default and the conformance suite stays green.
- With the shadow-compact hook registered on a fixture that
  exhausts, the conformance row in section 8 passes: one
  `compaction_summary` event, one `handoff.md` in the session dir,
  the shadowed range logged, the next `assemble` skips shadowed
  events, and no new session dir.
- A hook that exits with an unexpected code logs a
  `hook.<window>.error` marker and the loop applies the window
  default. No wedge. No hang. No new event type.
- The `hooks` module is I/O-light: it spawns a command and reads one
  line. The fs and lock work stays in the loop through ports.
- `cargo tree -p rushi-common` shows no HTTP and no new process
  spawn beyond the stage runners and the hook spawn. The guardrails
  in `architecture.md` section 7 hold.

## Properties

Lean-style invariants for this spec (see `lean-driven-development.md`).
One property per non-trivial invariant. Each property is observable:
given an input, an output guarantee.

P1. no-hooks-identical: given a session with no hooks registered,
    observe the run byte-identical to the no-hooks default on the
    `compact-e2e.sh` fixtures.
P2. decision-fold: given a hook on a window that exits 0 with a JSON
    decision, observe the harness fold that decision and log a
    `hook.<window>` marker.
P3. tool-block: given a `tool.before` hook returning `block` with a
    reason, observe one `tool_result` per blocked call with
    `is_error` true and no `route` spawn for those calls.
P4. nonblocking-fail: given a hook that exits with an unexpected
    non-zero code, observe a `hook.<window>.error` marker and the
    window default applied.
P5. hook-timeout: given a hook that passes the window timeout, observe
    the window default applied and no hang.
P6. shadow-compact: given a `context_exhausted` form under the `compact`
    strategy, observe one `compaction_summary` event, one `handoff.md`
    in the session dir, the shadowed range logged, and the next
    `assemble` skip shadowed events.

## Verification

Each property maps to its proof. `proven` means the cited test exists
and passes. `open` names the blocker and what unblocks it.

| P# | Property | Proof | Status |
|----|----------|-------|--------|
| P1 | no-hooks-identical | the `default` scenario in `scripts/model-before-transform-e2e.sh` and the no-hooks runs in `scripts/compact-e2e.sh` assert a marker-free byte-identical default | proven |
| P2 | decision-fold | `fold_prefers_the_first_explicit_decision`, `window_roundtrip` in `crates/rushi/src/hooks.rs`; the `transform` scenario in `scripts/model-before-transform-e2e.sh` | proven |
| P3 | tool-block | Blocked: no e2e drives a `tool.before` block decision to synthesized `tool_result`s. Unblocked by a `tool.before`-block e2e row | open |
| P4 | nonblocking-fail | `fold_failed_hooks_yield_no_decision` in `crates/rushi/src/hooks.rs` | proven |
| P5 | hook-timeout | `a_slow_hook_times_out` in `crates/rushi/src/hooks.rs` | proven |
| P6 | shadow-compact | Blocked: the shadow-compact conformance row (one `compaction_summary`, one `handoff.md`, shadowed range, next `assemble` skips it) is not yet an e2e. Unblocked by adding that row to `scripts/compact-e2e.sh` | open |

## Gate

The acceptance commands. All must exit 0 for this spec to be proven.

```
cargo build
cargo test
scripts/compact-e2e.sh
scripts/model-before-transform-e2e.sh
run-idle-continue-e2e.sh   # rushi-exts root (docs/tui-ext-repo-split.md section 4)
```
