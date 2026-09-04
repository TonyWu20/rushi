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
written to `notes/itches.md`, not built.

## P1. Event vocabulary policy

### P1a. Add a new event type only when ALL of these are true

1. **Two or more consumers need the distinction** (loop, reducer, TUI, policy, tool).
2. **The fact must survive replay/restart** — if it is transient UI state or an in-memory hint, it is not an event.
3. **It changes a state transition in the reducer or a decision in the loop** — if it only affects how the TUI renders, add a rendering rule instead.
4. **It cannot be carried by an existing type's optional fields** (`meta`, `data`, `details`).

If any condition fails, do not add the type. The TUI renders unknown types via
fallback; unknown fields are ignored by consumers.

### P1b. Versioning

- `v` is bumped **only for breaking changes**: renaming/removing a field, or
  changing a required semantic.
- Additive changes (new event type, new optional field) do **not** bump `v`.
- Consumers must tolerate unknown `type` values; unsupported `v` values render
  raw with a hint (never crash).

### P1c. Definition of done for an event type

- JSON Schema exists under `schemas/events/v1/`.
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

Phase 1 has no shared Rust crate. The shared types are JSON Schema files.

Promote into a shared Rust type only when:

- the same structure appears in **three or more binaries** with identical
  fields and non-trivial validation, or
- two copies have already **diverged on the same field**.

Promote a function into a shared library when:

- it has been copied three times, or
- a bug was fixed in one copy and not another.

Create the `core` crate only after:

- the JSON event/tool schemas have been stable through **20 real sessions** (default), and
- the reducer/loop state machine has replay tests over those sessions.

Before that, duplicate deliberately. The JSON contract is the spec; duplication
is cheaper than a premature shared crate.

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

- Acceptance: run `step.sh <session>` twice; second run emits no new
  `tool_call` events and appends no duplicate `assistant_message`.
- Forces: a correct reducer in `claim`; "what is owed" derived only from the log.

### G2 — Crash consistency

> Kill -9 any stage at any point; the log is either complete or absent for that
> event, and the next start recovers without double execution or dangling state.

- Acceptance: crash-injection script kills each stage at 3 points; after
  restart, every `tool_call` has exactly one `tool_result` or a terminal
  `error`, and rerunning a crashed tool call does not repeat side effects.
- Forces: O_APPEND atomic appends, sequence numbers, and a recovery rule.

### G3 — Event schemas

> Every event type has a JSON Schema; producers validate before append; readers
> validate on read.

- Acceptance: `schemas/events/v1/*.json` exist for all current event types;
  `log` rejects invalid events with nonzero exit; `tui` reads a deliberately
  malformed line and shows a fallback, not a crash.
- Forces: the versioned envelope (`v`, `type`, `ts`) and validation tooling.

### G4 — Tool conformance test

> A script/program in any language is a valid tool if and only if it passes the
> conformance harness.

- Acceptance: `tool-conformance <manifest>` runs the tool with sample inputs and
  checks: stdout is one JSON object, stderr is ignored, exit 0 = success,
  nonzero = failure; a Python tool and a Bash tool both pass without any
  language-specific handling.
- Forces: the stdin/stdout/stderr/exit-code contract as a test, not a comment.

### G5 — TUI resilience

> The TUI never crashes on log content. It renders what it knows and falls back
> for what it does not.

- Acceptance: feed the TUI (a) unknown event type, (b) `v: 99`, (c) malformed
  JSON line, (d) missing fields in a known type; TUI stays responsive and shows
  a fallback/hint in each case.
- Forces: `render_event` fallback rules and `SessionPort` validation.

### G6 — Approval recovery

> An approval request survives TUI restart and is answered from the log alone.

- Acceptance: run a tool that requests approval; kill the TUI before answering;
  restart the TUI; it shows the pending `approval_request`; answer; the loop
  resumes without timeout and without re-running the tool call.
- Forces: approval as events, pending state derived from the log, idempotent resume.

### G7 — Stage contract conformance

> Every pipeline binary validates its input and output against its schema and
> exits nonzero on mismatch.

- Acceptance: `echo '{}' | claim` exits nonzero with a schema error on stderr;
  `claim --help` prints its input/output schemas.
- Forces: each binary owns its contract; pipelines fail loudly instead of
  passing garbage downstream.

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
| `core` crate | Schemas stable 20 sessions + replay-tested reducer |
| Retry/timeout on a stage | A real transient failure observed (e.g., model API 5xx) |
| Daemon + attachable TUI | TUI restart killing the loop becomes unacceptable |
| In-process compiled tool | A specific tool's process overhead measured and exceeds G8 budget |
| NDJSON streaming tools | A tool must emit progress that changes control flow |
| Plugin/dynamic-loading system | Hot reload beyond editing scripts is a real requirement |

## P8. Not-yet list (do not build until triggered)

- Shared Rust `core` crate (see P3)
- Compiled-in tools (see P7)
- Daemon/TUI split (see P7)
- HTTP/WebSocket API (until a non-terminal/remote client is a current requirement)
- Plugin system / dynamic loading (until script editing is insufficient)
- NDJSON streaming for tools (until progress affects control flow)
- Multi-agent scheduling (until a second concurrent session is actually in use)
- Approval policy engine beyond allow/deny/edit (until a real policy need appears)

## P9. Itches (parking lot)

When a proposal is rejected as speculative, it is parked as an itch in
`notes/itches.md` with the date and the triggering episode (if any). Three
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
