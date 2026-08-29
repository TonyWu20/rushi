# Handoff: better-ui failure, context compaction, request format

Status: closed 2026-08-30. All three work items are closed: A by
correction 56, B by correction 57, C by correction 58. This note
consolidates the 2026-08-29 pi session
(cwd `rust-unix-harness`, model `sglang/Qwen3.8-27B-NVFP4-RTX5090-DSPARK`).
It hands work to the agent that owns context compaction. It also
carries the owner's demand: the model request must implement the full
OpenAI Responses API spec, not a short-sighted subset.

## Outcomes observed in `sessions/better-ui`

- 55 tool results with text "Tool arguments failed schema
  validation: <field>.". 49 fail on `command`, 6 on `file_path`.
  Every one of them carries `arguments: {}`. All 55 recover within
  one turn. The model re-issues the call with full arguments.
- The session then stopped for good. The last log lines read
  "Context budget exceeded after compaction." The loop process died.
  The log holds 1813 lines.
- The compact agent fixed the dead loop (correction 55, see below).
  The session now assembles instead of erroring.
- Track both defects in `docs/failure-tracking.md`. Entry FT-008
  covers the empty-arguments class.

## The coupled factors

Five factors coupled to produce today's failure. Each one feeds the
next. The outcomes are the 55 schema errors and the dead session.

1. **Context bloat.** Three sources grow the request every turn.
   First, the single event log inlines every tool result, up to
   20k chars each. That is the bloat documented in
   `docs/tool-log-design_from_human.md`. Second, the agent reads
   whole files. `read` supports `offset` and `limit`. The current
   `[system_prompt]` dropped the old line "Use offset and limit to
   continue reading large files." The agent defaults to 2000-line
   reads. Third, the TUI statusline extension is half-done. Its
   token metrics do not match
   `~/programming/pi-config/extensions/starship-statusline.ts`.
   The in-loop agent burned many turns on it.
2. **The model fails at depth.** `Qwen3.8-27B-NVFP4` through
   SGLang stochastically emits a tool call whose arguments are the
   two-char string `"{}"`. The stream still completes with
   `status=completed`. SGLang passes the empty arguments through
   verbatim. `bin/model`, `parse`, and `route` all behave to spec.
   The failure rate rises with context depth. It also rises when
   the history holds many empty-argument calls plus their schema
   errors. Each failure adds another example of the broken call.
   That self-priming explains the bursts of back-to-back failures.
3. **The char-based budget cannot act early.** The budget derives
   from `context_tokens * chars_per_token`, with one global
   `chars_per_token = 4`. Measured in this session: about 8 chars
   per token. The harness believes its budget is 229k tokens. It is
   about 114k. The estimate is off by 2x. The owner flagged the
   char-based idea as naive but left it in place, for lack of
   harness experience. It must go.
4. **The unreachable budget made compaction a dead end.** With no-op
   caps, the compact request equals the full request. The full
   request measures 907,620 chars. A 660k budget is unreachable.
   The terminal error fired. The session could not call the model
   again. Correction 55 fixed this (see "Compact agent state").
5. **The request format is a naive spec subset.** `bin/model`
   drops the `reasoning` output item on every turn. `assemble`
   emits only `message`, `function_call`, and
   `function_call_output` input items. The model's own thinking is
   amputated from the history at every turn. This is the typical
   short-sighted AI implementation: too lazy to build the full
   spec, against "You aren't gonna need it." The owner's demand:
   implement the full OpenAI Responses API spec.

## Evidence

### The empty-arguments class

- Log: 55 failures, all `arguments: {}`, all recovered within one
  turn (3-6 log lines to the next success).
- Context gradient: zero failures below 50k input tokens. First
  failure at 56.8k. Failures cluster at 56k-73k and at 93k-112k.
- SGLang shape: `response.output_item.done` and the terminal
  `response.completed` event both carry the arguments as a JSON
  string. When the model emits `"{}"`, the server returns `"{}"`.
  The terminal output item types are `reasoning`, `message`,
  `function_call`. The `reasoning` item keys are `content`,
  `encrypted_content`, `id`, `status`, `summary`. `content` is a
  list of `{text}` parts holding the full thinking text (3-6k chars
  observed). `encrypted_content` is null. `summary` is an empty
  list.
- `bin/model` substitutes `"{}"` only when a key is missing. The
  key is present in every capture. No server-side drop observed.

### Request-format probes

- Round-trip probe: the 110k assembled request plus the real
  `reasoning` item from a prior run in `input`. SGLang accepted it
  (HTTP 200). The response stream produced a well-formed tool call.
- A/B test at 110k input tokens, 4 runs per arm: no `reasoning`
  field 2-of-4 glitch, `medium` 0-of-4, `xhigh` 0-of-4. The
  field is not the trigger.
- Captures: `/tmp/sse-capture.txt`, `/tmp/sse-r1.txt` through
  `/tmp/sse-r4.txt`, `/tmp/probe-with-reasoning.json`,
  `/tmp/probe-sse.txt` (2026-08-29. Regenerate if gone).

### What pi sends (captured live, 2026-08-29)

A local proxy logged pi's real requests against this model. Files:
`/tmp/pi-capture/requests/req-01.json`, `req-02.json`,
`req-03.json`. Findings from request 3 (the follow-up turn that
carries history):

- `reasoning: {"effort": "xhigh", "summary": "auto"}`.
- `include: ["reasoning.encrypted_content"]`.
- `store: false`, `stream: true`, `max_output_tokens: 40000`,
  `prompt_cache_key: <session id>`. No `temperature` field.
- The `input` array carries a `reasoning` item from the prior
  turn, in addition to `message`, `function_call`, and
  `function_call_output` items.
- The `reasoning` item is the server's own item, sent back
  verbatim. Keys: `content`, `encrypted_content`, `id`, `status`,
  `summary`, `type`.

Source confirmation (`@earendil-works/pi-ai`, version 0.84.2 in
the nix store):

- `src/api/openai-responses.ts` lines 319-334: reasoning models
  get `reasoning: {effort, summary: "auto"}` plus
  `include: ["reasoning.encrypted_content"]`. The effort maps
  through `model.thinkingLevelMap`. Off sends `effort: "none"`.
- `src/api/openai-responses.ts` line 689: on a `reasoning` output
  item, pi stores the whole item via `JSON.stringify(item)` in
  the thinking block's `thinkingSignature`. No cap. No trim.
- `src/api/openai-responses-shared.ts` lines 222-224: the next
  turn parses that signature back and pushes the item into
  `input`. The round-trip is verbatim.
- `src/core/compaction/compaction.ts` lines 281-282: thinking
  chars count toward the compaction budget.
- `src/core/compaction/utils.ts` lines 117-133: compacted turns
  fold old thinking into a `[Assistant thinking]:` line in the
  summary. The summarizer runs at the session's thinking level.

Lesson for the harness: pi does not cap thinking text. It stores
the reasoning item whole, counts it in the budget, and lets
compaction fold it away. The harness should copy that design. Do
not trim reasoning items half-way. Count them in the budget.
Drop or summarize them in the compact form.

## Ruled out or withdrawn

- No per-model `quality_context_tokens`. The owner runs pi on this
  model through many rich sessions at the window edge. A hard
  quality wall does not hold. The single-session log correlation
  cannot separate context length from task phase and error
  saturation.
- No sampling temperature change. Qwen3.8 thinking mode recommends
  1.0. The owner owns that knob.
- No claim that SGLang drops arguments. Every capture carries the
  key. The server returns what the model emits.
- No claim that `bin/model` parsing is at fault for the 55
  errors. It handles string arguments per spec.
- The 55k "quality wall" claim from this session is withdrawn. It
  was a single-session ecological correlation.

## Compact agent state

- Correction 55 (`docs/loop-and-edit-implementation-corrections.md`
  entry 55) replaced the fixed compact stages. Caps now halve
  from the base to a floor (`compact_min_result_chars = 128`,
  `compact_min_text_chars = 64`). When the floor still does not
  fit, the search drops the oldest step groups one at a time.
  User messages and the keep window never drop. The terminal error
  now fires only when the keep window and the task alone outgrow
  the budget.
- Current `config.toml` `[limits]` at 2026-08-29: `context_budget_chars = 440000`,
  `compact_result_chars = 8000`, `compact_text_chars = 2000`,
  plus the two min-char keys. Note: the owner's 18:15 values
  (`context_budget_chars = 660000`, `compact_result_chars =
  80000`, `compact_text_chars = 20000`) are gone. The compact
  agent overwrote them with the correction-55 values. The
  correction-57 token knob has since replaced
  `context_budget_chars` with `context_budget_tokens = 55000`
  and the chars-per-token rate with `chars_per_token = 8`.
  See `config.toml` for the current state.
- Measured now: the full better-ui request is 907,620 chars. Under
  the current config the compact request is 394,191 chars. It
  fits the 440k budget. All 16 user messages survive.
- Remaining for the compact agent: closed by correction 57. The
  budget is driven by the measured `usage.input_tokens`. The knob
  is in tokens. The dead-session fix is verified live: one loop
  turn under the token budget, no terminal error, and the
  exhausted case runs the automatic handoff.

## Work items

### A. Urgent: implement the full OpenAI Responses API spec

Owner: closed 2026-08-29 by correction 56
(`docs/loop-and-edit-implementation-corrections.md` entry 56).
All five sub-items are done: the `reasoning` item captures and
round-trips verbatim, `events.jsonl` carries the field, `assemble`
places the item after the turn's user message, compaction counts the
item in the budget and drops it in the compact form, and the model,
assemble, and TUI test suites cover it. Live probes on the SGLang
server confirm the full-spec request and the verbatim round-trip.

1. `bin/model`:
   - Capture the `reasoning` output item verbatim. Keep
     `content`, `encrypted_content`, `id`, `status`, `summary`.
     Fall back to the `reasoning_text` delta stream.
   - Send `reasoning: {"effort": <configured>, "summary": "auto"}`
     for reasoning models. Send `effort: "none"` for off.
   - Send `include: ["reasoning.encrypted_content"]`.
   - Keep `store: false` semantics.
2. `events.jsonl`: carry the reasoning item on `assistant_message`
   as a new field. The TUI event parser must tolerate the field.
   Add a TUI regression test.
3. `assemble`: emit the `reasoning` input item for each old turn.
   Position it after that turn's user message and before its
   `function_call` items. Send it verbatim, pi-style.
4. Compaction: count reasoning item chars in the budget. In the
   compact form, fold old thinking into the summary like pi does.
   Do not trim a reasoning item to a middle slice.
5. Tests: model binary, assemble, TUI.

### B. Compaction and the unrecoverable session

Owner: closed 2026-08-30 by correction 57
(`docs/loop-and-edit-implementation-corrections.md` entry 57). All
three items are done: the budget is driven by the measured
`usage.input_tokens` of the last turn, with the char heuristic kept
only as the pre-measurement fallback. The user knob is
`context_budget_tokens`. The terminal case runs the automatic
handoff: one cheap summarization call on the compacted log, a seeded
new session, and a one-key resume in the TUI. `claim` exposes the
`exhausted` state. See FT-010 for the 2x budget error.

- [x] Replace the char estimate with measured tokens. Every
  `assistant_message` carries `usage.input_tokens`. The budget
  drives from the last measured value plus expected growth,
  against the model window. The char heuristic stays as the
  pre-measurement fallback only.
- [x] Expose the user knob in tokens, not chars.
  `context_budget_tokens` replaces `context_budget_chars`, which
  survives as a converted legacy fallback.
- [x] Turn the remaining terminal case into an automatic handoff.
  One cheap summarization call on the compacted log. A new session
  seeded with the summary. The task continues there. `claim`
  exposes the `exhausted` state. The TUI offers the one-key
  handoff on the `h` key.

### C. Reduce the bloat at the source

Owner: closed 2026-08-30 by correction 58
(`docs/loop-and-edit-implementation-corrections.md` entry 58). The
full tool result body moves to the per-session tool log
(`tools.jsonl`). The event log keeps the slim index with `bytes`,
the `tool_log` pointer, and a head/tail preview. `assemble`
resolves the body from the log, with the legacy fallback to the
index text. The compact pass drops the old schema-error pairs
outside the keep window, which kills the self-priming amplifier.
The TUI writes its own trace log (`tui-trace.jsonl`). The
range-read line is back in the system prompt. The statusline
matches the reference metrics.

- [x] Implement `docs/tool-log-design_from_human.md`. Move tool
  result bodies out of `events.jsonl` into a per-session tool
  log. This also removes the old schema-error pairs from the
  history. That kills the self-priming amplifier.
- [x] Restore the range-read line in `[system_prompt]`: "Use
  offset and limit to continue reading large files."
- [x] Finish or descope the TUI statusline extension. Its token
  metrics do not match the reference
  `starship-statusline.ts`. The half-done task kept burning
  context turns.

## Open questions

- The owner's `models.json` sets `requiresReasoningContentOn
  AssistantMessages: true` and `thinkingFormat: "deepseek"` on the
  sglang model. Those flags live in the completions compat
  schema. Under `openai-responses` they may be inert. The live
  capture shows pi sending `reasoning` input items, so the
  responses path is covered. The completions fallback in
  `bin/model` needs the same treatment for the deepseek thinking
  format.
- SGLang returns `encrypted_content: null`. Continuity rides on
  the plain `content` text. OpenAI returns opaque
  `encrypted_content` and may redact content. The full-spec
  implementation must handle both carriers, pi-style.

## Suggested reading order for the compact agent

1. `docs/failure-tracking.md` FT-008 (current state, corrected).
2. `docs/better-ui-root-cause.md` (the auto-compact design).
3. `docs/tool-log-design_from_human.md` (the log split).
4. `docs/loop-and-edit-implementation-corrections.md` entry 55.
5. `bin/assemble/src/main.rs` (budget, caps, keep-window search).
6. `bin/model/src/main.rs` (SSE parsing, the dropped reasoning
   item, the responses-vs-completions split).

## Files this handoff depends on

- `sessions/better-ui/events.jsonl` (1813 lines, frozen).
- `docs/failure-tracking.md` (FT-008, current).
- `/tmp/sse-capture.txt`, `/tmp/sse-r1.txt` through
  `/tmp/sse-r4.txt`, `/tmp/probe-with-reasoning.json`,
  `/tmp/probe-sse.txt` (2026-08-29 probes. Regenerate if gone).
- `/tmp/pi-capture/requests/req-01.json` through `req-03.json`
  (pi request captures through the 30099 proxy. The proxy is
  stopped. `/tmp/pi-capture` also holds a copy of the owner's
  agent config dir. The credentials file was deleted from the
  copy. The `models.json` there points at port 30099.)
