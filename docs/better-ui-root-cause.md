# Root cause: the better-ui session stopped mid-work

Status: analysis complete and corrected. Evidence:
`sessions/better-ui/events.jsonl`, `scripts/turn.sh`, `config.toml`,
`bin/assemble/src/main.rs`, and a controlled re-run under `/tmp/sim`.

## 1. Observed facts

- The log has 83 events: 1 user message, 20 assistant messages,
  31 tool calls, 31 tool results.
- Every tool call has a result. No error events exist.
- All 20 assistant messages end with tool calls. The model wanted more steps.
- The last event is a tool result. No final answer follows.
- `claim` derives state `awaiting_model`. The next owed work is a model call.
- All 20 steps are read-only exploration: 16 reads, 8 lists, 7 bash.
  Zero writes, edits, builds.
- Context grew from 1300 to 54502 input tokens across the 20 steps.
- `config.toml` sets `max_steps = 20`. `scripts/turn.sh` stops after
  20 model calls.
- The user message asked for a large feature: a new TUI input area.
  It must add rounded corners, thinking-linked colors, vim modes,
  multi-line support, and extensibility per `docs/ui-extensions.md`.
- The session ran from 09:20:42 to 09:21:44 UTC. One user message,
  20 model steps.

## 2. Direct cause

The hard 20-step cap in `turn.sh` ended the turn.

- `turn.sh` counts one step per model call. After 20 steps it prints
  `max_steps reached (20)` to stderr and exits 1.
- Step 20 ended with two tool calls: a search for `pi-vim` files and a
  read of `starship-statusline.ts`. The model was mid-exploration.
- The cap fired while work remained. No code was written.
- The cap is fixed. It does not scale to task size.
- A step cap is not a feature. Mature harnesses do not cap by
  steps. They loop until the model ends the task. They manage the
  context window with compaction, not with a step budget.
- A 5-part feature task needs far more than 20 steps of
  exploration plus implementation plus build and test.

## 3. Controlled re-run

The re-run used a scratch copy. The repo, session log, and config
live under `/tmp/sim`. The real tree stayed unchanged.

The model made one new assistant message, then the turn stopped.

- The message carries no structured tool calls. Its `tool_calls`
  array is empty.
- The message content holds a tool call as raw text in an XML block:
  `ls /tmp/pi-vim/ ...`.
- `parse` treats an empty tool-call list as terminal text.
  The state becomes `idle`.
- `turn.sh` exited 0. The turn ended. No cap message appeared.

Two defects surface here.

1. The model emitted the tool call as text instead of a structured
   call. This is the first such event in the 13 session logs.
   Input was 55466 tokens, about 21% of the 262144-token window.
   This is a format quirk, not a context limit.
2. The loop has no recovery for text-embedded tool calls. It idles
   silently. It does not warn. It does not retry.

In the real session the cap fired at step 20, so this second defect
never appeared in the real log. It is a latent defect. It would have
ended a continued turn at step 21. The model shows no signs of
context strain at this size. The gap is on the harness side.

The harness also ends sessions when context grows. `assemble`
emits a terminal error event: "Context budget exceeded. Start a
new session or reduce scope." Mature harnesses compact the log and
keep working. This harness stops.

## 4. Verdict: prompt or loop

The stop is a loop defect. The prompt is not the cause.

- The model did not choose to stop. Its 20th step still issued
  tool calls.
- The cap is the only event that ended the real turn. No error,
  no idle, no length stop.
- The re-run shows the next step would end the turn through a
  second loop defect: silent idle on text-embedded calls.

The system prompt is also not strong enough. It acts only as a
contributing cause.

- The prompt is about 20 lines of tool-usage guidance in
  `[system_prompt]` of `config.toml`.
- It has no planning rule. It has no budget rule. It has no
  finish-and-verify rule.
- So the model spent all 20 steps reading. It wrote zero code.
- A stronger prompt would start writing code by step 5. It would
  cut the damage from the cap.
- No prompt change can override a hard cap.

## 5. What the loop lacks to drive the agent to a goal

The core requirement is simple. The loop keeps working until the
model ends the task. Mature harnesses do not cap by steps. Goal
mode is not part of the core loop. It is an extension in pi and
an optional built-in in claude code.

This loop is missing:

- An unbounded tool-call loop. The 20-step cap ends work in
  flight. The user must re-prompt to continue.
- Auto-compact. `assemble` ends the session when the request
  exceeds the context budget. Mature harnesses summarize old
  events and keep working.
- Recovery for text-embedded tool calls. The turn idles silently.
- Prompt discipline: plan, implement, verify.

The model is not the limit. It has a 262144-token window. The
re-run failure sat at 55466 input tokens, about 21% of the window.
That is a harness robustness gap, not a model limit.

## 6. Reproduction

- Evidence: `sessions/better-ui/events.jsonl`, 83 events, unchanged.
- Scratch re-run: `/tmp/sim` (repo, log, and config copies).
  Command used:
  `cd /tmp/sim && CONFIG=/tmp/sim/config.toml bash \
    /home/tony/programming/rust-unix-harness/scripts/turn.sh better-ui`
  The harness scripts run against the scratch config and scratch log.
  Result: one assistant message, no structured calls, exit 0.
- State check: `target/debug/claim --session sessions/better-ui`
  reports `awaiting_model` on the real session.

## 7. Recommendations, in priority order

1. [x] Remove the step cap. Done. The loop runs until the model ends
   the task. See correction 51. The cap stops work early and irritates
   the user.
2. [x] Replace the terminal context-budget error with auto-compact.
   Done. `assemble` compacts old events and continues. It errors only
   when nothing fits. See correction 52. The model runs for long tasks.
   The harness must follow.
3. [x] Detect tool calls embedded in text content. Done. `parse`
   recovers them to structured calls or reports a loud error. See
   correction 53. Never idle silently.
4. [x] Strengthen the system prompt. Done. `config.toml` now carries
   the condensed `pi` prompt: plan first, then implement, then build
   and test. State the remainder if work stays unfinished. See
   correction 54.
5. Treat goal mode as an optional extension. It is not a core-loop
   requirement. pi ships it as an extension. claude code ships it
   as an optional built-in.

Each item maps to a gap in section 5. Items 1 to 4 are implemented
and verified with stub model runs. See corrections 51 to 54.
