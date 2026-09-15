# Refinement Policy — Rules for Agents Evolving the Harness

This document is the rulebook an agent follows when it wants to change the
harness framework itself (event types, binaries, APIs, shared types). The
framework exists to complete tasks; therefore it is refined **only under
pressure from real episodes**, never from anticipated cleanliness.

## P0. Prime directive: evidence over aesthetics

Every framework change must cite a recorded episode:

- session id / step id, or
- a reproducer command, or
- a correctness bug (lost event, double side effect, deadlock).

"No current task needs it" is a rejection reason. Speculative improvements are
written to `docs/itches.md`, not built.

## P1. Event vocabulary policy

### P1a. Add a new event type only when ALL of these are true

1. **Two or more consumers need the distinction** (loop, claim, TUI, hook, tool).
2. **The fact must survive replay/restart** — if it is transient UI state or an in-memory hint, it is not an event.
3. **It changes a state transition in `claim` or a loop decision** — a TUI-only effect gets a rendering rule instead.
4. **It cannot be carried by an existing type's optional fields** (`meta`, `data`, `details`).

If any condition fails, do not add the type. The TUI renders unknown types via
fallback; unknown fields are ignored by consumers.

### P1b. Versioning

- `v` is bumped **only for breaking changes**: renaming/removing a field, or
  changing a required semantic.
- Additive changes (new event type, new optional field) do **not** bump `v`.
- Consumers must tolerate unknown `type` values; unsupported `v` values render
  raw with a hint (never crash).
- Producers may extend the vocabulary per repo: the TUI adds the event
  types it needs for rendering (for example, `cancel`). The kernel only
  cares about events relevant to the loop with the model, and it skips
  unknown types per the rules above.

### P1c. Definition of done for an event type

- The typed `Event` variant exists in `crates/rushi/src/event.rs`,
  with a round-trip test in `event.rs` covering it.
- One producer test appends it.
- One consumer test reads it.
- One replay test proves an old session containing it still renders/reduces.

## P2. Binary / API granularity policy

One binary = **one responsibility + one input schema + one output schema + one
failure domain**.

- **Split a binary** when it has two unrelated failure modes that need
  independent retry/observability (e.g., `model` splits into API call vs
  response parsing once retries must distinguish them), or when a stage is
  useful standalone to a human or another pipeline.
- **Merge binaries** when their boundary forces a serialized intermediate that
  only those two understand — that intermediate is an implementation detail,
  not a contract, and the boundary is fake.
- **Rule of three for new endpoints/binary flags**: first need → do it inline;
  second need → record an itch; third need → extract the endpoint.
- **No endpoint without a current caller.** No versioning for imagined futures.

## P3. Internal type / core promotion policy

Shared types live in the `rushi-common` crate (`crates/rushi/`): the
typed `Event` vocabulary, `LogLine`, stage payloads, hook-ABI helpers,
rewind active-path math, and model-section resolution.
The old JSON-Schema event files were retired 2026-09-15 (`docs/typed-events.md`).
The typed `Event` enum is the contract. `parse_event` is the single
validation step. The TUI repo (`github.com/TonyWu20/rushi-tui`)
imports the same crate by path.

Promote a new structure into the shared crate only when:

- the same structure appears in **three or more binaries** with identical
  fields and non-trivial validation, or
- two copies have already **diverged on the same field**.

Promote a function into a shared library when:

- it has been copied three times, or
- a bug was fixed in one copy and not another.

Create a `core` crate for the loop state machine only after:

- the typed `Event` vocabulary has been stable through **20 real sessions** (default), and
- the loop state machine (`claim`'s owed-state derivation) has replay tests over those sessions.

Before that, duplicate deliberately. The typed-enum contract is the spec.
Duplication is cheaper than a premature shared crate.

## P4. Necessary-change checklist

Run every proposed change through all four questions:

1. Does it unblock a **current task**?
2. Does it fix a **correctness bug** (data loss, double execution, deadlock, unrecoverable state)?
3. Does it remove a **recurring manual step**?
4. Does it move an invariant from **human discipline to code** (e.g., "remember to append after the tool returns")?

And one pre-test:

- **Can this be done in config or a script without changing the protocol?**
  If yes, do that first.

If all answers are no, write an itch, do not build.

## P5. Initial concrete goals (in priority order)

Each goal has acceptance criteria so an agent can self-evaluate.

### G1 — Idempotent step replay

> If the same step is run twice on the same log, no event is duplicated and no
> tool side effect happens twice.

- Acceptance: run `rushi step <session>` twice. The second run emits no new
  `tool_call` events and appends no duplicate `assistant_message`.
- Forces: a correct reducer in `claim`; "what is owed" derived only from the log.

### G2 — Crash consistency

> Kill -9 any stage at any point; the log is either complete or absent for that
> event, and the next start recovers without double execution or dangling state.

- **G2a — log completeness and no dangling state: Done.**
  `scripts/crash-e2e.sh` kills `log` mid-append, `route` mid-tool,
  and `claim` in flight, then restarts. Every committed line is a
  whole event, and a truncated tail from a mid-write kill is the one
  tolerated worst case. After the restart, each unresolved `tool_call`
  still owes exactly one `tool_result` or a terminal `error`. The
  atomic half is FT-005 `LogLine` (exclusive `flock` plus one
  `write(2)`). The owed-state half is `claim`'s log-only derivation.

- **G2b — no repeated tool side effects: Parked** (docs/itches.md).
  A `tool_call` is logged before the tool runs. A kill mid-tool means
  the recovery re-runs the tool, so a non-idempotent tool repeats
  its side effect. Closing G2b needs a `tool_started` marker event
  plus a recovery rule, or a documented idempotent-tools-only scope.
- Forces: one locked `write(2)` per event line (FT-005), and owed
  state derived only from the log.

### G3 — Event vocabulary

> Every event type is a typed `Event` variant in `rushi-common`.
> Producers serialize it and readers parse it with `parse_event` (serde).

- Acceptance: `crates/rushi/src/event.rs` covers all current event types with
  a round-trip test. `log` rejects invalid events with a nonzero exit. The
  `tui` binary now lives in `github.com/TonyWu20/rushi-tui`. It reads a
  deliberately malformed line and shows a fallback, not a crash.
- Forces: the versioned envelope (`v`, `type`, `ts`) and the typed
  validation step.

### G4 — Tool conformance test

> A script/program in any language is a valid tool if and only if it passes the
> conformance harness.

- Acceptance: `scripts/tool-conformance.sh` runs the tools with sample inputs.
  It checks: stdout is one JSON object, empty stderr on success, exit 0 =
  success, nonzero = failure. The runner is language-neutral: it drives the
  tool through stdin, stdout, and the exit code. A Python tool and a Bash
  tool pass the same way. Today it hard-codes the four native tools, with
  no manifest argument.
- Forces: the stdin/stdout/stderr/exit-code contract as a test, not a comment.

### G5 — TUI resilience

> The TUI never crashes on log content. It renders what it knows and falls back
> for what it does not.

- Acceptance: feed the TUI (a) unknown event type, (b) `v: 99`, (c) malformed
  JSON line, (d) missing fields in a known type; TUI stays responsive and shows
  a fallback/hint in each case.
- Forces: the fallback rules and `SessionPort` in the TUI repo
  (`github.com/TonyWu20/rushi-tui`: `bin/tui/src/event.rs`
  `UnknownType` / `UnsupportedVersion` / `BadLine`, `bin/tui/src/port.rs`).

### G6 — Approval recovery

> An approval request survives TUI restart and is answered from the log alone.

- Acceptance: run a tool that requests approval; kill the TUI before answering;
  restart the TUI; it shows the pending `approval_request`; answer; the loop
  resumes without timeout and without re-running the tool call.
- Forces: approval as events, pending state derived from the log, idempotent resume.

### G7 — Producer-side event validation

> Every event line is validated against the typed `Event` vocabulary at the
> producer, before append. Readers never fail on a well-formed line.

- Acceptance: `echo '{}' | log --session <s>` exits nonzero with a validation
  error on stderr. The `rushi` loop append paths, `user`, and `log` all
  reject lines that do not parse into a known `Event`. Reader-side
  consumers (`claim`, TUI) skip or fall back on malformed lines instead
  of failing.
- Forces: the typed `Event` enum (`rushi-common::event::parse_event`) is
  the single validation step (`docs/typed-events.md`). Garbage is loud at
  the producer and silent-safe at the reader.
- Known gap: the TUI's own producer paths (`user_message`, `cancel`,
  `approval`) append straight through `LogLine` without the typed check.

### G8 — Overhead budget

> Harness overhead (everything except model and tool time) stays under budget;
> performance work is deferred until it exceeds budget.

- Default budget: max(100 ms, 1% of model time) per step.
- Acceptance: a `--profile` mode records stage timings; no optimization work is
  accepted unless the budget is exceeded.
- Forces: measurement before optimization; LLM latency dominates.

## P6. Change proposal format (for agents)

Every proposed framework change is a short note with:

```
Observed:   session/step id + log excerpt + what failed
Reproduce:  command that shows the failure
Pre-test:   why this cannot be done in config/script
Proposal:   minimal diff to event schema / binary boundary / type layout
Impact:     which consumers are affected; does v bump; does TUI fallback cover it
Migration:  additive vs breaking; how old sessions still replay
Tests:      producer/consumer/replay tests added
```

If any section is missing, the proposal is incomplete.

## P7. Trigger thresholds (summary table)

| Change | Trigger |
|---|---|
| New event type | All 4 conditions in P1a true |
| `v` bump | Breaking change to an existing event type |
| New binary | Two unrelated failure modes, or independent standalone use |
| New endpoint/flag | Third real caller (rule of three) |
| Shared Rust type | 3+ duplicated copies, or a divergence bug |
| `core` crate | Typed event vocabulary stable 20 sessions + replay-tested loop state machine |
| Retry/timeout on a stage | A real transient failure observed (e.g., model API 5xx) | **Done** for the model call: `step.rs` model retry loop (2 × 3 s) plus the `model_timeout_s` config knob |
| Daemon + attachable TUI | TUI restart killing the loop becomes unacceptable | **Done** (loop.pid reattach) |
| In-process tool execution (remove the per-call fork/exec) | A tool's per-call process overhead is measured and exceeds the G8 budget (measurement not yet built. All base tools are already compiled Rust binaries, so the remaining cost is fork/exec plus shell spawn for `bash`) |
| NDJSON streaming tools | A tool must emit progress that changes control flow |
| Plugin/dynamic-loading system | Hot reload beyond editing scripts is a real requirement |

## P8. Not-yet list (do not build until triggered)

- Shared Rust `core` crate for the loop state machine (see P3). The shared
  *utility* crate (`rushi-common`) already exists and is imported by every
  kernel stage and by the TUI. Only the state-machine core crate is not-yet.
  Re-evaluation triggers are recorded in `architecture.md` Phase 3
  (decision 2026-09-16) and parked in `docs/itches.md`.
  Re-evaluation triggers are recorded in `architecture.md` Phase 3
  (decision 2026-09-16) and parked in `docs/itches.md`.
- Compiled-in tools (see P7)
- ~~Daemon/TUI split~~ — **done**: `rushi run` runs the loop as a standalone process; the TUI binary attaches via `loop.pid` and can reattach after restart to send SIGINT/SIGTERM
- HTTP/WebSocket API (until a non-terminal/remote client is a current requirement)
- Plugin system / dynamic loading (until script editing is insufficient)
- NDJSON streaming for tools (until progress affects control flow)
- Multi-agent scheduling (until a second concurrent session is actually in
  use). The `docs/subagent-design.md` spec is approved but not built.
- Approval policy engine beyond allow/deny/edit (until a real policy need appears)

## P9. Itches (parking lot)

When a proposal is rejected as speculative, it is parked as an itch in
`docs/itches.md` with the date and the triggering episode (if any). Three
recorded episodes for the same itch convert it into a proposal.

## P10. Spec-driven (Lean) gate

Every framework change lands through the spec-driven workflow in
`lean-driven-development.md`:

1. The change cites its episode and states its properties (P1...Pn
   invariants in input-to-output form).
2. Each property gets a proof: a named test or e2e scenario.
3. The gate commands in the spec's `## Gate` section pass before the
   work is accepted. A clean gate with zero open properties is the
   guarantee. An unproven property is an open row with a named
   blocker, never an untracked gap.

The property list is frozen before implementation. A property that
changes after implementation started requires a new review pass.
