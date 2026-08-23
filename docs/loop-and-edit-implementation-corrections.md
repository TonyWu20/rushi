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

**Problem:** Config set `model = "deepseek-chat"` with `api = "responses"`. The Responses endpoint only accepts `deepseek-v4-flash` or `deepseek-v4-pro`. `deepseek-chat` is deprecated and will not work. The primary path was broken.

**Fix:** Set `model = "deepseek-v4-flash"`. This is the default Responses API model. The spec must reference `deepseek-v4-flash` and `deepseek-v4-pro`.

### 14. SSE stream termination was unspecified

**Reference:** DeepSeek API docs, studied 2026-08-11

**Problem:** DeepSeek's SSE stream ends with `response.completed`, `response.incomplete`, or `response.failed`. There is no `data: [DONE]` terminator. The implementation doc said "parse SSE events" without specifying the terminator. The parser would hang or misread the stream.

**Fix:** Added the terminal event names to the `model` section. The parser keys on these events.

### 15. Cache efficiency layer was missing

**Reference:** `loop-and-edit-tool.md` §1.4 (prefix stability), deepseek-harness cache implementation (studied 2026-08-11)

**Problem:** The design had prefix stability but no cache observability. Usage was not recorded on events. Cache behavior was not observable from the log. The design could not prove it hit the provider cache.

**Fix:** Added a "Cache efficiency" section. Added `usage` to the `model` output and the `assistant_message` schema. Added `cache-e2e.sh` to the build plan. The test verifies `cache_read_tokens > 0` on every request after the first.

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
