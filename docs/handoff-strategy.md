# Handoff strategy — session continuation across context exhaustion

Status: Spec (2026-09-08). This document records the owner's decision to
steer the context strategy back to a session handoff. It amends
`docs/phase-2-plan.md`. It follows the unix-philosophy model in
`docs/skill-remapped-to-os-apps.md`.

## 1. The decision

The terminal context strategy is a session handoff. The owner picks it
over the in-session auto-compact of correction 63. The two strategies
sit behind one port, so the swap is a recompile, not a rewrite.

- **Reactive trigger only.** No proactive threshold cut. The loop
  does not trim on a predicted budget. It keeps the full window.
  Compact fires only when the response shows overflow: an overflow
  `error`, a silent overflow, or a `length` stop. This matches pi.
  The `compact_enabled` switch still gates the reactive compact.
- **Terminal path.** When nothing fits after the reactive compact,
  the loop runs a handoff. It summarizes the session, seeds a new
  session, and starts that session's loop. The loop does not stop.
  It rebinds to the new session and continues.

The strategy is a config choice, not a hard branch. One knob picks the
terminal strategy:

```toml
[limits]
# terminal context strategy: "handoff" or "compact"
compact_strategy = "handoff"
```

The default is `handoff` (the owner's decision). `compact` keeps the
correction-63 in-session behavior. Both share the same reactive
overflow handling. They differ only on the terminal step.

## 2. The strategy port (the seam)

This is the seam that makes the swap a drop-in. It lands in
`harness-common` next to the `stage` module (plan §3.3). It stays out
of the loop. The loop calls it and acts on the result.

```rust
/// What to do when the context no longer fits. The loop owns the
/// decision; the strategy owns the outcome. One implementation per
/// strategy.
pub trait ContextStrategy: Send {
    fn name(&self) -> &'static str;
    /// Decide the terminal action for one exhausted context.
    fn on_exhausted(&self, ctx: &ExhaustedCtx) -> Result<TerminalAction>;
}

pub enum TerminalAction {
    /// Continue in this session (the in-place compact result).
    Stay,
    /// Seed `new_session`, then rebind the loop to it and continue.
    Handoff { new_session: SessionName },
    /// Nothing fits anywhere. Stop in this session.
    Stop,
}
```

The loop's `awaiting_model` terminal step becomes:

```
match strategy.on_exhausted(ctx)? {
    Stay      => { reproject; continue; }
    Handoff{new_session} => {
        let dir = store.seed(new_session, &summary, &old_dir)?;
        append_exhausted_marker(old_dir, &new_session)?;
        release_lock(old_dir);
        reacquire_lock(dir);
        session = dir;          // rebind
        continue;
    }
    Stop => break,
}
```

The second port owns session lifecycle. It is the piece correction
57 lacked. That correction seeded the session by hand.

```rust
/// Create and seed a session directory. The loop owns no fs layout.
pub trait SessionStore: Send {
    fn next_handoff_name(&self, base: &str) -> SessionName;
    fn seed(&self, name: &SessionName, seed: &Seed) -> Result<SessionDir>;
}
```

Two strategies ship as two `ContextStrategy` impls: `HandoffStrategy`
and `CompactStrategy`. The composition root (the `harness` binary) wires
the one named in `[limits].compact_strategy`. The loop never names a
strategy. It calls the trait.

## 3. The handoff document

The handoff doc is a data artifact, not a skill. It is a file the loop
writes. It is self-describing. It is produced from the summary call
(`assemble --summary-input` plus `model`, the same path `bin/compact`
uses). This is the unix-philosophy read of the mattpocock handoff
skill: the procedure is a tool, and the output is data on the path.

### 3.1 Format

The doc is markdown. The sections mirror the mattpocock handoff, with
two changes for this repo.

```markdown
# Handoff: <short task title>

Date: <ISO-8601>
From: sessions/<old>
To:   sessions/<new>

## Goal
<what the task is, what the next session continues>

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
<the tools and commands the next agent should run first>

## Log index (source of truth)
- events: sessions/<old>/events.jsonl
- tools:  sessions/<old>/tools.jsonl

This document is a summary. The logs are the truth. When the summary
and a log disagree, read the log. Do not trust this doc over the log.
```

### 3.2 Adaptations to this repo

- **No SKILL.md.** The mattpocock "suggested skills" section becomes
  "Suggested next actions". It names the commands and tools the next
  agent runs. Those tools self-document through `--help` (§4 of
  `skill-remapped-to-os-apps.md`). There is no skill file.
- **Reference, do not duplicate.** The doc points to existing artifacts
  by path. It does not copy their content. It names the log files, not
  their lines.
- **Logs are the truth.** The doc ends with the log index and the rule:
  the logs win over the doc. The next agent may read
  `sessions/<old>/events.jsonl` and `sessions/<old>/tools.jsonl` at
  will. It reads them to see what ran, what failed, and what worked.
  The summary is an aid to orientation, not a record.
- **Redact.** Drop API keys, tokens, and personal data. The doc is a
  session artifact that ships in the repo.

### 3.3 Storage (both directories, not the OS temp dir)

The mattpocock skill saves the doc to the user's temp dir, outside the
workspace. This repo keeps it in the session, in both session
directories. Both copies are session artifacts.

- `sessions/<old>/handoff.md` — the record of what happened and where
  the task went. It stays with the session it closed.
- `sessions/<new>/handoff.md` — the onboarding doc the new agent reads
  first. It is the seed of the new session.

The old copy is the audit trail. The new copy is the entry point. They
are the same file, written to both dirs. This is the owner's override of
the skill's temp-dir rule.

The doc is a file, not an event. No new event type. The
`context_exhausted` event already carries `new_session`. The file is
separate from the log. It adds no schema. It adds no `v` bump (P1a).

## 4. The terminal flow, in order

This is the `Handoff` arm of §2, fully spelled out. It runs when the
last-resort compact fails, or when `compact_strategy = "handoff"` and
the context is exhausted.

1. Run the summary call on the compacted log. One cheap call. A capped
   output. No tools. The `assemble --summary-input` path already builds
   this.
2. Write `handoff.md` into `sessions/<old>/`.
3. Create `sessions/<new>` via `SessionStore::next_handoff_name`. The
   name is `<base>_h<N>`. `N` is the next free index.
4. Copy `handoff.md` into `sessions/<new>/`.
5. Seed `sessions/<new>/events.jsonl` with one `user_message` event.
   Its content is the handoff summary plus the instruction to continue
   the task plus the log index from §3.1. The log index names both log
   files. It tells the new agent to read them when in doubt.
6. Copy the `cwd` file from the old session to the new one.
7. Append the `context_exhausted` marker to `sessions/<old>/events.jsonl`.
   Its `new_session` field is the new name. This marker already exists
   and `bin/log` already validates it.
8. Release the old session's `.loop.lock`.
9. Acquire the new session's `.loop.lock`.
10. Rebind the loop's `session` to the new dir. Continue the loop. No
    stop. No restart from the TUI. The loop process stays one process.

### 4.1 Auto-start

Correction 57 seeded the session but left the start to the human. The
TUI `h` key started the new loop on demand. This design removes the key
press. Step 10 rebinds and continues in the same process. The TUI
follows automatically.

The TUI side needs one change. Today the TUI tails one session and
waits for the `h` key to switch (§5.2 of the plan, the `Action::Handoff`
path in `bin/tui/src/main.rs`). Under auto-start the TUI re-attaches on
the `context_exhausted` marker alone. It reads `new_session` from the
marker and tails that session. No key press. The `h` key stays as the
manual override for the failed-seed case (`new_session` empty, the
summary call died).

### 4.2 Failure of the seed

If the summary call fails, no session seeds. The marker still lands on
the old session with an empty `new_session`, as correction 57 did. The
loop logs the terminal `error` and stops in the old session. The `h`
key path still covers a later manual retry.

## 5. What changes in the Phase 2 plan

This section lists the edits the plan takes. No new event type. No new
config key beyond `compact_strategy`. No `v` bump.

- **§3.3** — add `ContextStrategy` and `SessionStore` to the
  `harness-common` `stage` module list. They move to `crates/core` in
  Phase 3 with the state machine, like `StageRunner`.
- **§4.3 step 5** — the `context_exhausted` form no longer unwraps the
  embedded request and stays in the same session. It calls
  `strategy.on_exhausted`. The `Stay` result keeps the current
  in-session behavior. The `Handoff` result runs §4.
- **§4.6** — the lock invariant changes. It held for the process life.
  It now holds per session. The handoff releases one lock and takes
  another. The TUI probe (a non-blocking `flock` attempt) sees the
  released old lock as free. It sees the new lock as held.
- **§7** — add two rows. "Handoff seeded: the old session is
  `exhausted`, the new session runs." and "Seed summary failed: the
  marker carries an empty `new_session`, the loop stops in the old
  session."
- **§8** — add one conformance row. "handoff: a fixture session that
  exhausts, with the strategy set to handoff. Assert: one new session
  dir, `handoff.md` in both dirs, the seed event, the marker, the
  rebind, and the old log untouched."
- **§9** — "no daemon" and "no TUI feature work" still hold. The new
  non-goal is "no in-process strategy". The two strategies stay
  subprocess-wired behind the port.

## 6. Design debt this removes

The owner's question: would switching be clumsy? It is clumsy today
because the strategy is not a port. This document makes it one.

- The swap is one trait, one enum, one config key, and a recompile.
- The loop never names a strategy. It names the port.
- `claim`, `assemble`, `model`, `parse`, `route`, `log` change nothing
  on the handoff path. They stay adapters over the same contracts.
- The only new module work is `ContextStrategy` plus `SessionStore`.
  Both are small and I/O-light in `harness-common`. The fs writes in
  `SessionStore::seed` are the one I/O call. It stays out of the pure
  core.

The cost of deferring the port to Phase 3 is higher. It means the loop
ships welded to one strategy and the swap becomes a loop rewrite.
Adding the port now is cheap. Deferring it is not.

A second debt: the running `step.sh` and `auto-compact-plan.md` still
specify a proactive threshold hook. It fires before the request when
the predicted reading crosses the trigger level. That early cut
wastes the context window. The owner's decision retires the proactive
threshold. The loop triggers only from the response. The threshold
hook stays in `bin/compact` for manual use. The loop does not call
it.

## 7. Open items

- The `n` index rule for `<base>_h<N>`: next free index, not a
  timestamp. It must scan the sessions root. It must skip a dir that
  exists but is empty. Confirm against `sessions/` before wiring
  `next_handoff_name`.
- The seed event content: how much of the handoff doc goes into the
  `user_message` body. The doc stays in the dir. The seed event
  carries the summary and the log index. It does not paste the whole
  doc into the log.
- The TUI auto-reattach: it re-uses `pending_handoff`. It drops the
  key press on the success path. The `h` key stays for the
  failed-seed retry. This is a TUI touch, so it lands in the same
  stage as the loop, not a TUI feature pass.
