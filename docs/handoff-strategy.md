# Handoff document — the compact artifact

Status: Spec (2026-09-08). This document describes the handoff
document produced by the in-session shadow compact step. It amends
`docs/phase-2-plan.md`. It follows the unix-philosophy model in
`docs/skill-remapped-to-os-apps.md`.

## 1. The decision

The terminal context strategy is in-session shadow compact. The
compact step appends a handoff-instruction prompt to the current
context. The model writes a structured handoff document covering
goals, state, open questions, and next steps. The loop saves that
document to `sessions/<n>/handoff.md` and shadows the old log
region. The next request is built as: fixed system + tools + the
handoff doc content + post-boundary events. No new session is
created. The session continues in place.

- **Reactive trigger only.** No proactive threshold cut. The loop
  does not trim on a predicted budget. It keeps the full window.
  Compact fires only when the response shows overflow: an overflow
  `error`, a silent overflow, or a `length` stop. This matches pi.
  The `compact_enabled` switch still gates the reactive compact.
- **No escalation path.** The handoff document is the compact
  output. It shadows the old region. The model can re-read the
  shadowed log via `read` or `bash` at any time. No new session
  is created. No second strategy ships in Phase 2.

The strategy is a config choice, not a hard branch. One knob selects
the terminal strategy:

```toml
[limits]
# context-overflow strategy: only `compact` ships in Phase 2
compact_strategy = "compact"
```

The only shipped value is `compact`: in-session shadow compact.
It shadows the log region with the model-written handoff doc and
stays in the same session. The key is a seam for future strategies.
No second strategy ships in Phase 2.

## 2. The strategy seam (lifecycle windows)

> **Update 2026-09-08.** The `ContextStrategy` port sketched below is
> superseded by the granular lifecycle-window + hook design in
> `docs/loop-lifecycle-hooks.md`. The `ContextStrategy` trait and its
> `TerminalAction` enum are replaced by two named windows
> (`overflow.resolve`, `exhausted.handle`) and a small decision
> envelope. The `SessionStore` port survives as the fs port behind
> the `exhausted.handle` hook. See that document §2 (reframe), §3
> (window list), §4 (hook ABI), and §5 (strategy as plug-in).

### 2.1 Original `ContextStrategy` port (superseded)

The original sketch kept the strategy as a single port with one
method:

```rust
pub trait ContextStrategy: Send {
    fn name(&self) -> &'static str;
    fn on_exhausted(&self, ctx: &ExhaustedCtx) -> Result<TerminalAction>;
}

pub enum TerminalAction {
    Stay,
    Handoff { new_session: SessionName },
    Stop,
}
```

The audit (`docs/phase-2-plan-audit.md` §2.3) showed this monolithic
port is still clumsy: the loop must know the rebind, the lock swap,
and the fs seeding. The window+hook design removes that coupling by
making each blocking point a named window with a closed decision
vocabulary. The loop fires the window and applies the decision; it
does not branch on strategy name.

### 2.2 Current design (lifecycle windows)

The overflow-strategy windows are:

- **`overflow.resolve`** — fires when the model response classifies
  as overflow, silent overflow, or length stop. Decision vocab:
  `stay_compact` | `stop`.
- **`exhausted.handle`** — fires when `assemble` returns the
  `context_exhausted` form (the assembled context exceeds the input
  budget). Decision vocab: `stop`.

The `SessionStore` port remains the fs I/O boundary for session
artifacts. In Phase 2 it handles the single in-session shadow
compact flow: saving `handoff.md` and the `compaction_summary`
event into `sessions/<n>/`.

```rust
pub trait SessionStore: Send {
    fn save_handoff(&self, session: &SessionName, doc: &str)
        -> Result<()>;
}
```

The hook calls `SessionStore::save_handoff` for the fs work and
returns `{"decision": "stop"}` on `exhausted.handle` (the loop then
runs the in-session shadow compact and continues) or
`{"decision": "stay_compact"}` on `overflow.resolve`. The
composition root wires the registered hook in `[hooks].on`. The
loop never names a strategy. It fires the window and applies the
decision.

The `handoff` decision value (new-session seeding) is reserved for
future strategies. No second strategy ships in Phase 2.

## 3. The handoff document

The handoff doc is a data artifact, not a skill. It is a file the loop
writes into the current session dir. It is self-describing. It is
produced from the summary call (`assemble --summary-input` plus
`model`, the same path `bin/compact` uses). This is the
unix-philosophy read of the mattpocock handoff skill: the procedure
is a tool, and the output is data on the path.

### 3.1 Format

The doc is markdown. The sections mirror the mattpocock handoff, with
two changes for this repo.

```markdown
# Handoff: <short task title>

Date: <ISO-8601>
Session: sessions/<n>

## Goal
<what the task is, what the session continues>

## Progress
### Done
- ...
### In progress
- ...
### Blocked
- ...

## Key decisions
- <the calls that are hard to reverse>

## Next steps
1. <the immediate next action>

## Critical context
<invariants, gotchas, caps that must hold>

## Suggested next actions
<the tools and commands the model should run first>

## Log index (source of truth)
- events: sessions/<n>/events.jsonl
- tools:  sessions/<n>/tools.jsonl

This document is a summary. The logs are the truth. When the summary
and a log disagree, read the log. Do not trust this doc over the log.
```

The format keeps one session dir. The `From` / `To` fields do not
apply: the shadow stays in the current session.

### 3.2 Adaptations to this repo

- **No SKILL.md.** The mattpocock "suggested skills" section becomes
  "Suggested next actions". It names the commands and tools the
  model runs next. Those tools self-document through `--help` (§4 of
  `skill-remapped-to-os-apps.md`). There is no skill file.
- **Reference, do not duplicate.** The doc points to existing artifacts
  by path. It does not copy their content. It names the log files, not
  their lines.
- **Logs are the truth.** The doc ends with the log index and the rule:
  the logs win over the doc. The model may read
  `sessions/<n>/events.jsonl` and `sessions/<n>/tools.jsonl` at
  will. It reads them to see what ran, what failed, and what worked.
  The summary is an aid to orientation, not a record.
- **Redact.** Drop API keys, tokens, and personal data. The doc is a
  session artifact that ships in the repo.

### 3.3 Storage (both directories, not the OS temp dir)

The mattpocock skill saves the doc to the user's temp dir, outside the
workspace. This repo keeps it in the session dir alongside
`events.jsonl`.

- `sessions/<n>/handoff.md` — the shadow summary. It is the entry
  point for the next `assemble` cycle. It sits next to the log it
  shadows.

The doc is a file, not an event. No new event type. The
`compaction_summary` event carries `first_kept_seq`. The file is
separate from the log. It adds no schema. It adds no `v` bump (P1a).

## 4. The terminal flow, in order

This is the in-session shadow compact flow. It runs when the context
is exhausted and the reactive compact path (overflow, silent, length
stop) does not recover enough headroom.

1. Run the summary call on the compacted log. One call. No tools.
   The handoff document's length is model-determined; it is not
   capped below the model's own `max_output_tokens`. The
   `assemble --summary-input` path already builds this.
2. Write `handoff.md` into `sessions/<n>/` (the current session
dir). No new session is created.
3. Append a `compaction_summary` event with `first_kept_seq` to
   `sessions/<n>/events.jsonl`. The next `assemble` skips events
   before `first_kept_seq` and prepends the handoff doc content to
   the request.
4. Shadowed events remain in `events.jsonl`. The model can re-read
   them via `read` or `bash` at any time.
5. Continue the loop in the same session. No rebind, no lock swap,
   no new session.

### 4.1 TUI observation

The TUI tails one session and does not need to switch session dirs.
The shadow compact stays in the same session. The TUI re-reads the
same `events.jsonl` and the `handoff.md` file in place. No re-tail
of a different session dir is needed.

### 4.2 Failure of the summary call

If the summary call fails, no `handoff.md` is written. The loop logs
a terminal `error` and stops in the session. A later manual retry can
re-invoke the compact step.

## 5. What changes in the Phase 2 plan

This section lists the edits the plan takes. No new event type. Two new
config keys: `compact_strategy` and `[hooks]`. No `v` bump.

- **§3.3** — add the `hooks` module to `harness-common`, next to
  `stage`. It holds the lifecycle-window dispatcher and decision
  types. It is I/O-light: spawn a command, read one stdout line.
  The `SessionStore` port remains the fs boundary. Both move to
  `crates/core` in Phase 3 with the state machine.
- **§4.3 step 5** — the `context_exhausted` form fires the
  `exhausted.handle` lifecycle window. The harness dispatches
  registered hooks for that window. The shipped hook runs the
  in-session shadow compact: it saves `handoff.md` in the session
dir and returns `stop`. The loop continues in the same session.
- **§4.3 step 6** — the overflow/silent/length classification fires
  the `overflow.resolve` window. Default decision is `stay_compact`.
  The hook runs one in-session shadow compact and the loop retries.
- **§4.6** — the lock invariant: it holds for the process life in
  the current session. No lock swap. No rebind.
- **§7** — add one row. "Shadow compact: a `context_exhausted` form
  with `compact_strategy = compact`. One `compaction_summary` event,
  one `handoff.md` in the session dir, the shadowed range logged,
  the next `assemble` skips shadowed events, no new session dir."
- **§8** — add one conformance row. "Shadow compact: a fixture
  session that exhausts with the default `compact_strategy` produces
  one `compaction_summary` event, one `handoff.md` in the session
  dir, the shadowed range logged, the next `assemble` skips shadowed
  events, and no new session dir."
- **§9** — "no daemon" and "no TUI feature work" still hold. The new
  non-goal is "no in-process hook ABI". The strategy hook stays
  subprocess-wired behind the hook ABI.
- **§10** — stage 2 adds the window dispatcher. Stage 3 adds the
  shadow-compact hook and its conformance rows.

## 6. Design debt this removes

The owner's question: would switching be clumsy? It is clumsy today
because the strategy is not a port. This document makes it one.

- The swap is one hook registration, one config key, and a recompile.
- The loop never names a strategy. It fires the window. It applies
  the decision.
- `claim`, `assemble`, `model`, `parse`, `route`, `log` change
  nothing on the compact path. They stay adapters over the same
  contracts.
- The only new module work is the `hooks` dispatcher plus the
  `SessionStore` port. Both are small and I/O-light in
  `harness-common`. The fs writes in `SessionStore::save_handoff`
  are the one I/O call. It stays out of the pure core.

The cost of deferring the seam to Phase 3 is higher. It means the
loop ships welded to one strategy and the swap becomes a loop
rewrite. Adding the window seam now is cheap. Deferring it is not.

A second debt: the running `step.sh` and `auto-compact-plan.md` still
specify a proactive threshold hook. It fires before the request when
the predicted reading crosses the trigger level. That early cut
wastes the context window. The owner's decision retires the proactive
threshold. The loop triggers only from the response. The threshold
hook stays in `bin/compact` for manual use. The loop does not call
it.

## 7. Open items

- The `n` index rule for `<base>_h<N>` is a future concern. No new
  session dir is created in Phase 2. The `compact_strategy` key and
  window vocabulary are seams for a future strategy that may seed a
  new session. Confirm against `sessions/` before wiring that future
  path.
- The TUI auto-reattach is a future concern. In Phase 2 the TUI
  tails one session dir for the process life. No re-tail of a
  different session dir is needed.
  stage as the loop, not a TUI feature pass.
