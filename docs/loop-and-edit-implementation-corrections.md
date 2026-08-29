# Loop + Edit Tool — Implementation Corrections

This document records corrections applied to `loop-and-edit-implementation.md` after review against `architecture.md`, `loop-and-edit-tool.md`, and `SPEC_CONTRACT_TESTS.md`.

## Corrections

### 1. Pipeline cannot express spec branching logic

**Reference:** `loop-and-edit-tool.md` §2.3

**Problem:** The original `step.sh` used a straight pipe (`claim | assemble | model | parse | route | log`). This always runs `route` after `parse`. The spec requires skipping tool execution on truncation (`stop_reason == "length"`) or errors (`error`/`aborted`).

**Fix:** Rewrote `step.sh` to:
- Run pipeline up to `parse`, save output to temp file
- Use `parse` exit codes to branch: 1 means tool calls need routing, 2 means no routing
- Route only when needed
- Log all events

### 2. `parse` needs exit codes for branching

**Reference:** `loop-and-edit-tool.md` §2.3

**Problem:** `parse` always exited 0. `step.sh` could not distinguish cases.

**Fix:** `parse` now exits:
- Code 1: tool calls present, route them
- Code 2: no tool calls (error, length, or none)

### 3. `assemble` error handling produces no log event

**Reference:** `loop-and-edit-tool.md` §8.3

**Problem:** On context budget exceeded, `assemble` wrote to stderr and exited 1. No log event was created. Downstream processes could not observe the condition.

**Fix:** `assemble` now emits one `error` event to stdout with the message "Context budget exceeded. Start a new session or reduce scope." and exits 0. The event is logged like any other.

### 4. Tool result clipping logic was missing

**Reference:** `loop-and-edit-tool.md` §8.2

**Problem:** Config listed `tool_result_max_chars = 20000` but no binary applied the cap.

**Fix:** Added clipping step to `assemble`: apply `tool_result_max_chars` cap when projecting `tool_result.value.text` into `FunctionCallOutput`. Append `[tool result clipped: N -> M chars]` if truncated. Full value stays in the log.

### 5. `turn.sh` vs `step.sh` naming mismatch

**Reference:** `loop-and-edit-tool.md` §2.3

**Problem:** Spec says `turn.sh` runs one step. Implementation labeled `step.sh` as the step runner.

**Fix:** Kept names as `step.sh` (one step) and `turn.sh` (loop) for clarity. Updated labels to avoid confusion. The behavior matches the spec.

### 6. Edit tool error messages were not testable

**Reference:** `loop-and-edit-tool.md` §6.2, `SPEC_CONTRACT_TESTS.md` ("A spec line is good only when the tester can turn it into a test")

**Problem:** Edit tool said "exit 1 with message" without specifying the message text.

**Fix:** Added exact error messages:
- Empty `old_string`: "Error: old_string must not be empty."
- Identical strings: "Error: old_string and new_string are identical. No change needed."
- No match: "Error: old_string not found in file. It may have changed since your last read."
- Multiple matches: "Error: old_string appears N times. Use a larger old_string with more context, or set replace_all=true."

### 7. Edit tool omitted BOM preservation

**Reference:** `loop-and-edit-tool.md` §6.2 ("Preserve line endings and BOM")

**Problem:** Implementation mentioned CRLF/LF but not BOM.

**Fix:** Added BOM preservation to edit tool algorithm.

### 8. `assistant_message` schema had optional `tool_calls`

**Reference:** `loop-and-edit-tool.md` §2.2

**Problem:** `tool_calls` was not in the `required` array. The spec implies `assistant_message` always carries tool calls (even if empty).

**Fix:** Added `tool_calls` to the `required` array.

### 9. `model` fallback logic lacked Phase 1 context

**Reference:** `loop-and-edit-tool.md` §1.1

**Problem:** Implementation merged two adapters (`ResponsesModelClient` and `ChatCompletionsModelClient`) into one binary without explaining why.

**Fix:** Added note that this is a Phase 1 consolidation. The adapters split into separate trait implementations in Phase 3.

### 10. `route` validation conflicted with architecture spec

**Reference:** `architecture.md` §3.1 ("non-JSON stdout: wrapped as `{"text": "..."}`")

**Problem:** `route` required valid JSON with a `text` field. The architecture allows non-JSON output to be wrapped.

**Fix:** `route` now wraps non-JSON stdout as `{"text": "<stdout>"}`.

### 11. `read` tool caps were not config-driven

**Reference:** `config.toml` defines caps

**Problem:** Implementation described caps as fixed values.

**Fix:** `read` tool accepts caps via CLI flags (`--read-limit`, etc.). Defaults match `config.toml`.

### 12. Conformance tests missing error message checks

**Reference:** `SPEC_CONTRACT_TESTS.md` ("Observable behavior only")

**Problem:** Edit tests only checked exit codes, not error message content.

**Fix:** Added stderr assertions for edit error cases and BOM preservation test.

### 13. DeepSeek model name was wrong for the Responses API

**Reference:** DeepSeek API docs (api-docs.deepseek.com), studied 2026-08-11

**Problem:** Config set `model = "deepseek-chat"` with `api = "responses"`. The Responses endpoint only accepts `deepseek-v4-flash`, `deepseek-v4-pro`, or `deepseek-v4-flash-vision-exp`. `deepseek-chat` was discontinued on 2026-07-24 and does not work. The primary path was broken.

**Fix:** Set `model = "deepseek-v4-flash"`. The spec must reference `deepseek-v4-flash` and `deepseek-v4-pro` (and note the vision model).

### 14. SSE stream termination was unspecified

**Reference:** DeepSeek API docs, studied 2026-08-11

**Problem:** DeepSeek's SSE stream ends with `response.completed`, `response.incomplete`, or `response.failed`. There is no `data: [DONE]` terminator. The implementation doc said "parse SSE events" without specifying the terminator. The parser would hang or misread the stream.

**Fix:** Added the terminal event names to the `model` section. The parser keys on these events. The Responses API streams semantic SSE events with an `event` field and a `sequence_number`; there is no `data: [DONE]` line.

### 15. Cache efficiency layer was missing

**Reference:** `loop-and-edit-tool.md` §1.4 (prefix stability), deepseek-harness cache implementation (studied 2026-08-11)

**Problem:** The design had prefix stability but no cache observability. Usage was not recorded on events. Cache behavior was not observable from the log. The design could not prove it hit the provider cache.

**Fix:** Added a "Cache efficiency" section. Added `usage` to the `model` output and the `assistant_message` schema. Added `cache-e2e.sh` to the build plan. The test verifies `cached_tokens > 0` on at least one request in the second turn, retrying while the provider cache constructs.

### 16. `step.sh` had an unreachable exit code

**Reference:** `SPEC_CONTRACT_TESTS.md` ("A spec line is good only when the tester can turn it into a test")

**Problem:** The script used `set -euo pipefail`. The pipeline `claim | assemble | model | parse` failed if any stage failed. The script exited before `PARSE_EXIT=$?` executed. The branch logic never ran.

**Fix:** Rewrote `step.sh` to run each stage separately. Each stage's output goes to a temp file. Exit codes are captured per stage. `set -e` is disabled only around `parse`.

### 17. `step.sh` ignored `claim`'s state

**Reference:** `refinement-policy.md` G1 (idempotent step replay), G2 (crash consistency)

**Problem:** The script always ran the full pipeline. It called the model even when nothing was owed. Re-running after a complete step appended a new assistant_message. This violated G1. Crash recovery was missing. Pending tool calls never executed. This violated G2.

**Fix:** Rewrote `step.sh` to check `claim`'s state first. It no-ops when idle. It routes pending calls without calling the model when `awaiting_tool_result`.

### 18. Error-event check was in the wrong place

**Reference:** `loop-and-edit-tool.md` §8.3

**Problem:** The `grep` for error events ran after the pipeline. An assemble error flowed through `model` and `parse`. Those stages did not understand error events. The check was too late.

**Fix:** Moved the error check before `model`. The script runs `assemble` separately. It checks for an error event. It skips `model` and `parse` on error.

### 19. `parse` exit-code text was self-contradictory

**Reference:** `loop-and-edit-tool.md` §2.3

**Problem:** The doc said "Exit 0 with code 2" twice. That sentence is meaningless.

**Fix:** Corrected to "Exit with code 2".

### 20. `parse` validation rules were unspecified

**Reference:** `SPEC_CONTRACT_TESTS.md` ("A spec line is good only when the tester can turn it into a test")

**Problem:** The doc said "Validates model output." It did not define what validation means. The tester could not write a test.

**Fix:** Added validation rules. Malformed JSON, unparseable arguments, and unknown tool names each produce an `error` event and exit code 2.

### 21. `route` had undefined behaviors

**Reference:** `architecture.md` §3.1, `SPEC_CONTRACT_TESTS.md`

**Problem:** The doc did not define behavior for unknown tools, non-object JSON, or tool timeouts. The manifest's `timeout_ms` was ignored.

**Fix:** Added rules for unknown tools, schema validation, timeout enforcement, and non-object JSON wrapping.

### 22. `log` omitted sequence numbers and schema validation

**Reference:** `architecture.md` §5.2, `refinement-policy.md` G2, G3

**Problem:** Architecture §5.2 says `log` assigns sequence numbers. G2 requires sequence numbers. The implementation's `log` did not assign them. `claim`'s output included `last_user_message_seq`. The claim could not produce sequence numbers if the log did not assign them. G3 requires producers to validate before appending. The `log` did not validate events.

**Fix:** Added sequence number assignment and schema validation to `log`. The log assigns 1-based line positions. It rejects invalid events with nonzero exit.

### 23. `claim` had dead fields and undefined state

**Reference:** `SPEC_CONTRACT_TESTS.md` ("Observable behavior only")

**Problem:** The fields `last_user_message_seq` and `last_assistant_stop_reason` were not consumed. The field `pending_tool_calls` had an unspecified format. The state after an `error` event was undefined. The loop could spin until `max_steps`.

**Fix:** Removed `last_assistant_stop_reason`. Defined `pending_tool_calls` as a list of tool call objects. Defined the state after an `error` event as `idle` (terminal).

### 24. `assemble` omitted ordering guarantees

**Reference:** `loop-and-edit-tool.md` §1.4 (prefix stability)

**Problem:** The doc said "Load tool schemas from tools/*/tool.toml." Glob order is not stable. Prefix stability requires deterministic ordering.

**Fix:** Added "Sort by tool name" to the `assemble` algorithm.

### 25. `read` truncation marker was unspecified

**Reference:** `SPEC_CONTRACT_TESTS.md` ("A spec line is good only when the tester can turn it into a test")

**Problem:** The conformance test said "verify truncation marker." The doc did not define the exact text. The tester could not write the test.

**Fix:** Added the exact marker text: ` ... (line truncated to <N> chars)`. Added input validation for `offset` and `limit`. Added exact error messages.

### 26. `edit` matching semantics were ambiguous

**Reference:** `loop-and-edit-tool.md` §6.2

**Problem:** The doc did not define behavior for a CRLF file with an LF `old_string`. The model emits LF. The file has CRLF. The match would fail. The tool must normalize line endings for comparison.

**Fix:** Added line-ending normalization for matching. The tool compares with both normalized to LF. It writes back with the file's original line endings. Added a conformance test for this case.

### 27. Conformance suite was incomplete

**Reference:** `SPEC_CONTRACT_TESTS.md` ("Enumerate edge cases")

**Problem:** The suite was missing "stderr is ignored on success" (spec §9). Edge-case enumeration was incomplete. The `edit` tool was missing a deletion test (empty `new_string`).

**Fix:** Added "stderr is ignored on success" to the test checks. Added edge cases for missing files, binary files, and empty `new_string` deletion.

### 28. Conformance tests live in the implementation document

**Reference:** `SPEC_CONTRACT_TESTS.md` ("Give the implementer no write access to the test files.")

**Problem:** The conformance tests are in the same document as the implementation. The implementer would have write access to the tests. This violates the two-agent method.

**Fix:** Move the conformance tests to a separate document. The tester reads only the test document and the public interface. The implementer reads only the implementation document. The two documents are separate.

### 29. `step.sh` did not handle model API failures

**Reference:** `refinement-policy.md` G2 (crash consistency), `SPEC_CONTRACT_TESTS.md`

**Problem:** The script used `|| exit 1` on the `model` call. When the model API failed, the script exited with code 1. No error event was logged. The session state was unclear. The loop could not recover.

**Fix:** Capture the model exit code. On failure, log an error event with message "model API call failed". Exit 0. The error event is terminal. `claim` reports `idle` on the next step. The loop stops cleanly.

### 30. Cache e2e test turn 2 had no work

**Reference:** `loop-and-edit-implementation.md` (cache conformance test)

**Problem:** The test ran `turn.sh` to completion (turn 1). It then ran turn 2 with an unchanged log. A completed turn leaves the session idle. `claim` reports `idle`. `step.sh` exits without calling the model. Turn 2 produced zero model requests. The test could never observe a cache hit.

**Fix:** The test appends a new user message before each turn 2 run. The prefix stays byte-identical. Turn 2 now produces requests that extend the turn 1 prefix. If no request hits, the test waits and repeats. Each repeat adds a new user message.

### 31. Responses wire format did not match the DeepSeek API

**Reference:** DeepSeek Responses API docs (api-docs.deepseek.com), verified live 2026-08-24

**Problem:** `assemble` emitted chat-completions shapes. Tools nested `name` under `function`. Messages used `role` and `content`. There was no `instructions`. `model` sent `max_tokens`. The live API returned `400 Bad Request: tools[0]: missing field name`.

**Fix:** `assemble` now emits `{"model", "instructions", "input", "tools"}`. Each tool schema has a top-level `name`. Each input item is `message`, `function_call`, or `function_call_output`. `function_call` arguments serialize to a JSON string. `model` sends `max_output_tokens` and `stream: true`.

### 32. SSE parser used wrong event names

**Reference:** DeepSeek Responses API guide, verified live 2026-08-24

**Problem:** The parser keyed on `input_text.delta`, `input_function_call.added`, and `input_function_call.arguments.delta`. DeepSeek does not emit these events. Text and tool calls never populated.

**Fix:** The parser now handles `response.output_text.delta`, `response.output_item.added` and `response.output_item.done`, and `response.function_call_arguments.delta` and `response.function_call_arguments.done`. The terminal event carries the full response object in the `response` field. The parser reads output items and usage from this object. Streaming deltas are a fallback only.

### 33. Chat completions fallback converted the wrong request shape

**Reference:** `loop-and-edit-implementation.md` (model fallback)

**Problem:** The fallback read a `messages` array. `assemble` no longer emits `messages`. The conversion produced an empty request.

**Fix:** The fallback maps `instructions` to a system message. Each `input` item maps to a chat message. `function_call` items merge into the trailing assistant message as `tool_calls`. `function_call_output` items become `role: "tool"` messages. Tool schemas convert from the top-level `name` form to the nested `function` form. The fallback also extracts tool calls from the chat response.

### 34. Hardcoded timestamps in event emission

**Reference:** `loop-and-edit-implementation.md` (event schemas)

**Problem:** `parse`, `assemble`, and `route` emitted the fixed timestamp `2024-01-01T00:00:00Z` for every event. The `ts` field was fake.

**Fix:** All three binaries now emit real UTC time via `chrono`. `assemble` and `route` gained the `chrono` dependency. The format is RFC 3339 with second precision.

### 35. `parse` emitted string arguments that failed schema validation

**Reference:** `loop-and-edit-implementation.md` (parse), `schemas/events/v1/tool_call.json`

**Problem:** The model returns `arguments` as a JSON string. `parse` copied the string into the emitted events. The schemas require `arguments` to be an object. `log` rejected the whole batch. The loop broke.

**Fix:** `parse` normalizes `arguments` before emission. A string parses into a JSON object. An already-object value passes through. Malformed strings produce an `error` event and exit code 2.

### 36. `claim` reported stale pending tool calls when idle

**Reference:** `loop-and-edit-implementation.md` (claim)

**Problem:** `claim` kept `pending_tool_calls` after the state became `idle`. The output carried tool calls that were already resolved.

**Fix:** `claim` clears `pending_tool_calls` whenever the state becomes `idle`. The field is empty in the idle state.

### 37. `step.sh` model-failure event used pretty-printed JSON

**Reference:** `loop-and-edit-implementation.md` (step.sh)

**Problem:** The error event used `jq -n`. `jq -n` emits multi-line JSON. `log` reads one JSON object per line. It rejected the first line `{`.

**Fix:** The error event uses `jq -cn`. The output is a single compact JSON line. `log` accepts it.

### 38. Config parsing used `sed` and `jq` on TOML

**Reference:** `loop-and-edit-implementation.md` (script dependencies)

**Problem:** `step.sh` and `turn.sh` parsed `config.toml` with `sed`. GNU `sed` and BSD `sed` differ. The original doc called `jq` on a TOML file. `jq` cannot parse TOML. Both approaches break on some systems.

**Fix:** `step.sh` and `turn.sh` parse config with POSIX `awk`. The extraction uses `[[:space:]]`, `-F'"'`, and `print; exit`. These work on macOS BSD awk and GNU awk.

### 39. `route` validation error did not name the missing field

**Reference:** `loop-and-edit-implementation.md` (route)

**Problem:** The error text read "Tool arguments failed schema validation: missing required fields." The spec requires the field name. The model could not correct its call.

**Fix:** `validate_args` returns the first missing required field. `route` emits "Tool arguments failed schema validation: <field>." The live model then retried with `file_path` and succeeded.

### 40. `read` tool printed an empty omission marker at offset 1

**Reference:** `loop-and-edit-implementation.md` (read tool)

**Problem:** At `offset` 1, the output began with `(0 lines omitted)`. The marker is wrong when no lines precede the window.

**Fix:** `read` emits the omission marker only when `offset` is greater than 1.

### 41. Conformance suite was missing three tests

**Reference:** `loop-and-edit-implementation.md` (conformance tests)

**Problem:** The doc listed a 60KB byte-cap read test, an edit binary-file test, and an edit BOM test. The script lacked all three. The suite ran 21 tests.

**Fix:** Added the three tests to `tool-conformance.sh`. The suite now runs 24 tests and all pass.

### 42. Cache e2e verified against the live API

**Reference:** `loop-and-edit-implementation.md` (cache conformance test)

**Problem:** No live run proved the cache behavior. The test had never run with a real key.

**Fix:** Ran `cache-e2e.sh` with a live key. Turn 2 reported `cached_tokens = 512 > 0`. The test passes. The usage trail climbs across turns.

### 43. Full loop verified against the live API

**Reference:** `loop-and-edit-implementation.md` (build plan)

**Problem:** The loop had only run with a mock model. The real API path was unproven.

**Fix:** Ran a full live turn. The model read `config.toml`, recovered from a schema error, edited `test.txt`, and answered. The loop ended at `idle`. The harness works end to end.

### 44. Thinking level was not a config value

**Reference:** `loop-and-edit-implementation.md` (config, cache efficiency)

**Problem:** The design mentioned reasoning effort as frozen call config. It had no config key and no request field. The model used the provider default thinking behavior. The harness could not set or observe the thinking level.

**Fix:** Added `reasoning_effort = "medium"` to `config.toml`. The `model` binary sends `reasoning.effort` in every request. The value is part of the frozen call config. Documented the allowed values in the config section. The live API accepts the field.

### 45. `max_output_tokens` raised for agentic headroom

**Reference:** `loop-and-edit-implementation.md` (config)

**Problem:** The cap of `4096` truncated long reasoning chains. Reasoning tokens count toward the cap. A truncated response cut the final answer mid-sentence. The loop then treated the partial answer as terminal.

**Fix:** Raised `max_output_tokens` to `32768`. The live API accepts caps up to `384000`. The value stays constant within a session.

### 46. `assemble` sent empty tool parameters

**Reference:** `loop-and-edit-implementation.md` (assemble, backends)

**Problem:** `assemble` read `tool.parameters` from `tool.toml`. The manifest defines the schema under `tool.schema`. The read failed, so every tool in the request had empty `parameters` and `required`. `route` read `tool.schema` correctly, so it still rejected bad calls. The llama.cpp model followed the empty schema and emitted `arguments: "{}"` every turn. The loop hit `max_steps`. DeepSeek masked the bug by guessing argument names from descriptions. It wasted turns before landing on `file_path`.

**Fix:** `assemble` now reads `tool.schema`. The request tools carry full properties and required fields. Both backends now get the argument name on the first call.

### 47. Added the `user` message binary

**Reference:** `loop-and-edit-implementation.md` (user)

**Problem:** The user hand-wrote the `user_message` JSONL line to seed or continue a session. The format is easy to get wrong.

**Fix:** Added `bin/user`. It takes a session name or directory and a message, builds a valid `user_message` event, validates it against the schema, and appends it. Content comes from a positional argument or stdin. The session name resolves against `sessions_root` from config.

### 48. Added the llama.cpp backend

**Reference:** `loop-and-edit-implementation.md` (backends)

**Problem:** The harness was hard-wired to DeepSeek in the docs. No other provider was documented or verified.

**Fix:** Added `config.llama.toml` pointing at a local llama.cpp server. The `model` binary needed no changes. llama.cpp emits the same Responses SSE events and carries the full response in the terminal event. Verified end to end with model `Nail-Qwen3.6-35B-A3B` at `127.0.0.1:8080`. The empty `Authorization` header is harmless.

### 49. Unified multi-model config

**Reference:** `loop-and-edit-implementation.md` (config, models)

**Problem:** Each provider had its own config file. `config.toml` held DeepSeek. `config.llama.toml` held llama.cpp. Adding a model meant copying a whole file. The `CONFIG` variable switched files.

**Fix:** One `config.toml` now holds all models. Each `[model.<name>]` table defines one model. The `[model]` table holds defaults. `[active] model` selects the active model. The `MODEL` environment variable overrides the selection. Removed `config.llama.toml`. The `assemble` and `model` binaries resolve the active model the same way.

### 50. Context length customization

**Reference:** `loop-and-edit-implementation.md` (config)

**Problem:** The request budget was a fixed character count. Local models have a token-based context window. Sending a request larger than the window truncated or failed. The user could not set the window per model.

**Fix:** Each model defines `context_tokens`. `assemble` derives the budget with `(context_tokens - max_output_tokens) * chars_per_token`. `chars_per_token` defaults to 4. For llama.cpp, `context_tokens` must match the server `--ctx-size`. An optional `context_budget_chars` caps the derived value. Per-model `max_output_tokens` reserves output room in the window.

### 51. Removed the step cap from the loop

**Reference:** `loop-and-edit-tool.md` (the loop), `scripts/turn.sh`

**Problem:** `turn.sh` capped each user turn at 20 model steps. At the cap it printed `max_steps reached` and exited 1. The session died mid-task with pending tool calls. The model still wanted to work.

**Fix:** The loop runs until the model finishes the task. It breaks only when `claim` reports `idle`. An error still stops it. A 30-step stub model run finished at step 30 with `claim` idle and exit 0. No cap message.

### 52. Auto-compact when the request outgrows context

**Reference:** `loop-and-edit-implementation.md` (assemble), entry 50

**Problem:** When a request outgrew the context budget, `assemble` emitted a terminal `error` event. The session could not continue. No compaction path existed.

**Fix:** `assemble` now keeps the most recent events full. It compacts older tool results and assistant text into short markers. It halves the caps and the keep window down to a floor of two. It emits the terminal error only when nothing fits. New config keys: `compact_keep_events`, `compact_result_chars`, `compact_text_chars`. A five-step run with 16 KB results fit a 40 KB budget with compacted markers.

### 53. Recovered tool calls embedded in assistant text

**Reference:** `bin/parse`

**Problem:** Some backends emit tool calls as text in the assistant `content` (`invoke` marker blocks with `parameter` blocks). `parse` saw an empty `tool_calls` array and treated the message as final text. The loop idled silently and the call never ran.

**Fix:** `parse` now scans for the marker blocks when structured tool calls are absent. It extracts JSON arrays and the XML `parameter` format into calls with deterministic ids. Bad inner JSON still emits an error event and exits 2. A stub model run proved the embedded call executed: the log gained a `tool_call` and a `tool_result` event.

### 54. Replaced the system prompt with the pi-derived prompt

**Reference:** `config.toml` `[system_prompt]`

**Problem:** The old prompt taught tool usage. It lacked planning, build-and-test, and completion discipline. The model spent its steps on read-only exploration.

**Fix:** `config.toml` now carries a condensed prompt from the `pi` coding agent. It states: plan before code, write the code, build and test, never stop a task half-done, and always end with a status line that names what remains.

### 55. Compact floor and step dropping end the dead-session loop

**Reference:** `bin/assemble`, `config.toml` `[limits]`, FT-009

**Problem:** Entry 52 halved the compact caps in three fixed stages: `8000, 4000, 2000` for results, `2000, 1000, 500` for text. The `better-ui` session grew to 604 tool results and 586 assistant messages. The full request reached about 1.47M chars against a 440k budget. Even the tightest stage stayed near 740k chars. No stage fit, so `assemble` emitted the terminal error on every turn. The session was unrecoverable: a user `Continue` only reproduced the error. Per-event caps cannot express a total-size constraint on a long session.

**Fix:** `compact_search` replaces the fixed stages. It halves the caps from the base to a configurable floor (`compact_min_result_chars`, `compact_min_text_chars`; defaults 128 and 64). When the floor still does not fit, it drops the oldest step groups, one step at a time, until the request fits. A step group is one assistant message, its tool calls, and their results. User messages are never dropped: the task statement must survive. The keep window is never dropped: it is the recent context. The terminal error now fires only when the keep window and the task alone outgrow the budget. The `better-ui` session now assembles to a 380k-char request under the 440k budget with all 16 user messages kept.

**Verification:** Four new `assemble` tests cover the floor halving, step grouping, drop-oldest fitting, and the still-unsatisfiable case. All 13 `assemble` tests pass. Running the fixed `assemble` on `sessions/better-ui` prints a model request, not an error event.

### 56. The model request carries the full OpenAI Responses spec surface

**Reference:** `bin/model`, `bin/parse`, `bin/assemble`, `bin/tui`, `schemas/events/v1/assistant_message.json`, handoff work item A

**Problem:** The model request is a short subset of the OpenAI Responses spec. `bin/model` drops the `reasoning` output item on every turn. `assemble` emits only `message`, `function_call`, and `function_call_output` input items. The request lacks the `reasoning`, `include`, and `store` fields. The request omits the model's own thinking from the history on every turn.

**Fix:**

- `bin/model` sends the full spec surface: `store: false`, `include: ["reasoning.encrypted_content"]`, `reasoning: {effort: <configured>, summary: "auto"}`. A configured effort of `off` sends `effort: "none"`. The SSE parser captures the `reasoning` output item verbatim: `content`, `encrypted_content`, `id`, `status`, `summary`. A cut or failed stream uses the `reasoning_text` delta stream. The model output JSON gains a `reasoning` array.
- `bin/parse` forwards the `reasoning` array onto the `assistant_message` event. The event schema gains the `reasoning` property. The server's item stays in the log unchanged.
- `bin/assemble` emits one `reasoning` input item per old turn. The item comes after that turn's user message and before that turn's `function_call` items. It is sent verbatim, pi-style. The full form counts the item's chars in the budget. The compact form drops the item: a half-trimmed thinking item is worse than none. A summary call restores the old thinking later (handoff work item B).
- The chat-completions fallback uses the deepseek thinking format: it captures `reasoning_content` in the response and attaches it to the next assistant message in the request.
- The TUI keeps parsing `assistant_message` lines with the new field as semantic. A regression test covers the field and a malformed entry.

**Verification:** All 11 `model` tests pass: verbatim capture from a terminal event, delta-stream rebuild of a cut stream, the failed-stream case, the completions capture, and the chat conversion. All 17 `assemble` tests pass, including item placement, compact-form dropping, and a budget test that fits only after the compact form drops the items. The TUI suite passes 175 tests. The live SGLang server at 127.0.0.1:30000 accepts the full-spec request with HTTP 200. A second request sends the captured `reasoning` item back in `input` verbatim. The server accepts it and returns a well-formed response. `assemble` on the frozen `sessions/better-ui` log still prints a model request, not an error event.

### 57. Token-driven context budget and the automatic handoff

**Reference:** `bin/assemble`, `bin/model`, `bin/claim`, `bin/log`,
`bin/tui`, `scripts/step.sh`, `scripts/turn.sh`, `config.toml`
`[limits]`, `schemas/events/v1/context_exhausted.json`, handoff work
item B, FT-009, FT-010

**Problem:** Three coupled gaps left a session that outgrew its
budget dead and unrecoverable. First, the context budget was a char
estimate (`context_budget_chars` over `chars_per_token = 4`). The
measured ratio on this model is about 8 chars per token, so the
harness believed a budget 2x off and never compacted early
(FT-010). Second, the budget knob was in chars, not tokens. The
model window is a token fact. Third, the terminal case (the keep
window plus the task outgrow the budget) emitted a dead-end error.
No path resumed the task (FT-009 residual risk).

**Fix:**

- `bin/assemble` drives the budget from measured
  `usage.input_tokens`. The full log goes out when the last measured
  value plus the projected growth of the appended events fits the
  token budget. The user knob is `context_budget_tokens`. The legacy
  `context_budget_chars` converts through the chars-per-token rate
  when the token knob is absent. The default is the model window
  minus the output reservation. The char heuristic survives only as
  the pre-measurement fallback: a fresh session, a server that
  reports no usage, and the compact candidates (which have no
  measurement by construction). The compact candidates check
  against the token budget times the chars-per-token fallback.
- `bin/model` honors a request-provided `max_output_tokens` over the
  config value. The handoff summary request carries a cheap cap.
  The completions fallback normalizes its usage to
  `usage.input_tokens`, so both API paths feed the same field.
- `bin/assemble` turns the terminal case into the automatic handoff.
  When nothing fits, it emits a `context_exhausted` event instead of
  the dead-end error. The event carries the summary request: the
  largest compact candidate the search tried, plus the summary
  instructions, a cheap output cap
  (`handoff_summary_max_tokens`, default 4096), and no tools.
- `scripts/step.sh` runs the handoff on a `context_exhausted`
  assemble output. One summarization call on the compacted log. It
  seeds a new session `<base>_h<N>` with the summary plus a
  continue-the-task instruction, copies the session `cwd`, and
  records the `context_exhausted` marker on the old session with the
  seeded name. A failed summary call still records the marker, with
  an empty seed name and an error event before it.
- `bin/claim` reports the new `exhausted` state from the marker.
  `scripts/turn.sh` breaks on it. A user message after the marker
  reopens normal work.
- `bin/tui` offers the one-key resume. A `context_exhausted` event
  is semantic (new `EventKind::ContextExhausted`). The transcript
  names the seeded session. The status row carries the hint. The
  `h` key switches to the seeded session and starts its loop. It
  preempts the editor only when a seeded marker exists and no loop
  runs. The old session's local loop stops on the switch.

**Verification:** All 23 `assemble` tests pass, including the
measured-vs-char decision, the skipped full candidate, and the
exhausted event shape. All 14 `model` tests pass, including the
request cap and the usage normalization. All 5 `claim` tests pass,
including the exhausted state and the reopen-after-marker rule. All
184 `tui` tests pass, including the marker parse, the `h` key gates,
and the transcript line. Live run against the SGLang server at
127.0.0.1:30000: a scratch session with a 50-token budget hit the
terminal case. The loop summarized it, seeded `h1test_h1`, and
recorded the marker. The claim state read `exhausted`. A turn on
the seeded session continued the task to completion under the real
55k-token budget. `assemble` on the frozen `sessions/better-ui`
log now prints a 366k-char compacted request under the 440k-char
candidate check, not the terminal error.

### 58. The tool log, the slim index, and the dropped FT-008 pairs

**Reference:** `bin/route`, `bin/assemble`, `bin/tui`,
`schemas/events/v1/tool_result.json`, `scripts/step.sh`,
`ui_extensions/statusline/`, `config.toml`,
`docs/tool-log-design_from_human.md`, handoff work item C, FT-008

**Problem:** Every tool result body sat inline in `events.jsonl`.
A long tool output inflated every later model request, and the
schema-error pairs that fail the tool-call schema (FT-008) replayed
themselves in every compacted request, teaching the model its own
mistakes. Second, the TUI kept no log of its own faults: a render
error or a dropped key was invisible after the fact. Third, the
range-read guideline in the system prompt was missing, and the TUI
statusline token metrics did not match the reference
`starship-statusline.ts`.

**Fix:**

- `bin/route` with a `--tool-log <path>` arg writes the full result
  to the per-session tool log (`tools.jsonl` in the session dir, one
  JSON record per run: raw stdout, stderr, exit, and the display
  text). The event log gets the slim index instead: the old fields
  plus `bytes` (the full body byte count) and `tool_log` (the log
  file name). The index `value` carries a head-and-tail preview with
  an elision marker naming the log. Without the arg, route keeps the
  old inline behavior for the legacy callers.
- `scripts/step.sh` passes the tool log path on both the normal
  and crash-recovery route calls. A re-run of a pending call
  overwrites the record for the same call id; the log keeps the
  last record per id.
- `bin/assemble` reads the tool log and resolves each tool result
  from the full body there, falling back to the index text when the
  log is absent (legacy logs). The full pass and the measurement
  pass use the full bodies. The compact pass drops the old
  schema-error pairs: a failed call and its result stay out of the
  compacted request once they leave the keep window. Pairs inside
  the keep window stay, so the model still sees the failure it is
  recovering from. The event log is untouched.
- `bin/tui` writes a TUI trace log (`tui-trace.jsonl` in the
  session dir). One JSON record per fault: the port read and write
  errors, the render failures, the dropped input events, the
  malformed lines, and the loop spawn and stop. The trace write
  takes no event-log lock and a failed trace write drops the
  record: the trace must not take the UI down.
- `ui_extensions/statusline` carries the reference metrics: the
  context fullness (last measured input tokens over the model
  window, `ctx:% (n/window)`), the cumulative in/out/sum tokens, and
  the run total. The width parser cut at the next comma; it used to
  strip the digits of the later keys.
- `config.toml` back to the range-read guideline: `Use offset and
  limit to continue reading large files.`

**Verification:** All 13 `route` tests pass, including the slim
index shape, the legacy fallback, and the per-id overwrite rule.
All 32 `assemble` tests pass, including the tool-log preference,
the legacy fallback, the rebuild of the display text from a legacy
record, the keep-window rule, and the compact search that drops the
old pairs. All 184 `tui` tests pass, including the two trace log
tests: the record shape and the escape rejection. Live run against
the SGLang server at 127.0.0.1:30000: a scratch session ran a real
turn. The event log held the slim index (`bytes` 1975, `tool_log`
pointing at `tools.jsonl`). The tool log held the full 1975-char
body. `assemble` on that session sent the full body, not the
preview.

### 59. The schema rejection teaches the model the resend

**Reference:** `bin/route`, `docs/bash-tool.md`, FT-008

**Problem:** The rejection text was a bare "Tool arguments failed
schema validation: <field>." The FT-008 glitch (the local NVFP4
model emits `arguments: {}` at long context) recovered on its own
in 55 of 55 historical runs. The revived better-ui turn of 2026-08-30
broke the pattern: seven consecutive empty-argument rejections with
no success before the user stop. The model's own reasoning blamed
the tool layer ("the tool layer is still flaky"). It read its own
empty calls as a harness fault and re-sent the identical call.

**Fix:** `route` appends the resend instruction to the rejection.
The first sentence keeps the stable prefix that the `assemble`
compact pass keys the FT-008 pairs off. The missing-field case
names the fields and ends with "Resend the call with all required
fields filled in.". The non-object and bad-JSON cases end with
"Resend the call with a JSON object.".

**Verification:** The `route` suite passes, including the new
`validate_args` tests for the multi-field list, the single-field
case, the valid call, and the non-object value. The prefix match in
`assemble` is unchanged: it keys off the first sentence.
