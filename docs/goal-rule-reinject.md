# Goal-rule reinjection: inject rules only on idle (follow-up)

Status: Proposal (2026-09-07).

## Context

`harness-hook-goal-arm` (registered on `model.before`) injects the full
goal block on **every** model call while a goal is active. The block is
assembled by `GoalState::build_goal_block()`:

```
<goal_objective> … </goal_objective>
Goal-mode rules:
  1. Preserve the full objective; …
  …(10 rules)
<goal_id> … </goal_id>
There is no token budget. …
```

The 10-rule block is roughly 500 tokens. In a long goal session with
many model calls, that text is repeated in every single request.

The user observation: the agent re-acknowledges the rules in its
thinking every round, which is visible noise and wastes context
budget.

## Suggested approach

Split the block into two parts:

| Part | Content | Injected when |
|------|---------|---------------|
| **Core block** (per-call) | objective XML + trust-boundary sentence + `<goal_id>` + "no token budget" line | every `model.before` call while the goal is active |
| **Rules block** (idle-only) | the 10 goal-mode rules | only in the `run.idle` continuation prompt (`build_continue_prompt`), i.e. when the agent is about to stop |

The idle hook (`hook-goal-idle`) already builds a continuation
message via `GoalState::build_continue_prompt()`. That prompt already
contains the goal text and a brief "keep working / call
goal_complete / call goal_blocked" reminder. Appending the 10 rules
there adds the discipline at the moment the agent is most likely to
slack off (about to stop), without paying the cost on every mid-task
model call.

### Concrete changes

1. **`crates/goal-state/src/lib.rs`**
   - Add `fn build_goal_core_block(&self) -> String` — objective +
     trust boundary + `<goal_id>` + no-budget line (everything the
     current `build_goal_block()` has **except** the rules).
   - Change `build_continue_prompt()` to append
     `Self::goal_mode_rules()` after the existing continuation text.
   - `build_goal_block()` either becomes an alias for
     `build_goal_core_block()` (if the full block is no longer needed
     anywhere) or is kept for backward compatibility / testing.

2. **`bin/hook-goal-arm/src/main.rs`**
   - Switch from `goal.build_goal_block()` to
     `goal.build_goal_core_block()`.

3. **`bin/hook-goal-idle/src/main.rs`**
   - `build_continue_prompt()` now includes the rules automatically
     (change is inside the crate, no hook-side change needed).

### Risks / tradeoffs

- **Mid-goal slacking**: without the rules in context, the agent
  might stop early on an intermediate turn without the idle hook
  firing (e.g. it decides it is done and emits no tool calls, but the
  loop's idle check only fires after the model *stops*). The idle
  hook will catch it and inject the rules in the continuation prompt,
  but there is one turn where the agent could under-deliver.
- **Post-compact context**: after a compaction, the conversation
  history shrinks. If the rules were only in the idle prompt and the
  last idle prompt has been compacted away, the agent loses the
  rules. Mitigation: the core block (objective + goal_id) is still
  injected on every call, so the agent always knows *what* to do; it
  just loses the *how* (the 10 rules) until the next idle continuation.
- **Behavioral regression risk**: the 10 rules were ported from
  pi-goal for a reason. Removing them from the per-call block changes
  agent behaviour. Should be A/B tested over several goals before
  committing.

### Open questions

- Should the rules be injected on the **first** model call after goal
  start (so the agent sees them at least once in the "system"
  position), and then only in idle continuations thereafter?
- Is 500 tokens / call actually significant at current context
  budgets (256 K+)? Measure before committing.
- Does the model benefit from seeing the rules on every call, or
  does repetition cause "rule fatigue" (the user's original
  observation)?

## References

- `docs/goal-ux.md` §1.1b (injection is goal-file-driven), §1.2
  (rule list), §1.1c (cache-prefix discipline).
- `crates/goal-state/src/lib.rs` — `build_goal_block`,
  `build_continue_prompt`, `goal_mode_rules`.
- `bin/hook-goal-arm/src/main.rs` — the `model.before` injection point.
- `bin/hook-goal-idle/src/main.rs` — the `run.idle` continuation
  point.
