# Loop + Edit Tool — Implementation Proposal

Phase 1 vertical slice. A one-step-per-turn loop driver over the session log. A model adapter speaking the OpenAI Responses wire format against a DeepSeek provider. Three file tools: read, write, edit.

## Cache efficiency

DeepSeek uses implicit prefix caching. The provider caches byte-identical request prefixes in 64-token blocks. The harness has no explicit cache-control directives. Its job is to make prefixes stable and cache behavior observable.

Mechanisms:

- **Append-only log.** The log grows. Requests derive from it. Each request's prefix is byte-identical to its predecessor. The provider cache then hits.
- **Deterministic projection.** `assemble` renders the log to a request with fixed field order, fixed tool-schema order, and no timestamps or PIDs. Same log, same bytes.
- **Frozen call config.** Model, temperature, and reasoning effort affect cache reuse. The harness holds these values constant across a session. A change invalidates the prefix cache.
- **Usage on the log.** `model` reports token usage. `parse` records it on the `assistant_message` event. Cache behavior is observable from the log.
- **Disjoint token counts.** `input_tokens` excludes cache hits. `cache_read_tokens` reports hits separately. The adapter subtracts hits from the provider total because DeepSeek folds them into `prompt_tokens`.
- **Cache e2e test.** A key-gated test runs a multi-step tool turn against the live API. It verifies `cache_read_tokens > 0` on every request after the first. The system prompt spans one 64-token block.

## Project Layout

Phase 1 uses separate binaries glued by bash. No shared Rust crate. JSON contracts are the spec.

```
rust-unix-harness/
├── Cargo.toml                  # workspace
├── config.toml                 # model config, paths
├── bin/
│   ├── claim                   # Rust binary: derive step state from log
│   ├── assemble                # Rust binary: project log to ModelRequest
│   ├── model                   # Rust binary: call DeepSeek API via Responses format
│   ├── parse                   # Rust binary: validate model output, emit tool_call events
│   ├── route                   # Rust binary: dispatch tool calls, run tools
│   └── log                     # Rust binary: append events atomically
├── scripts/
│   ├── step.sh                 # one-step pipeline with conditional routing
│   ├── turn.sh                 # loop driver (calls step.sh)
│   └── tool-conformance.sh     # G4 conformance harness
├── tools/
│   ├── read/
│   │   ├── tool.toml
│   │   └── read.rs             # compiled to tools/read/bin/read
│   ├── write/
│   │   ├── tool.toml
│   │   └── write.rs
│   └── edit/
│   │   ├── tool.toml
│   │   └── edit.rs
├── schemas/
│   └── events/v1/
│       ├── user_message.json
│       ├── assistant_message.json
│       ├── tool_call.json
│       ├── tool_result.json
│       └── error.json
├── sessions/                   # created at runtime
│   └── <id>/
│       └── events.jsonl
└── notes/
    └── itches.md
```

## Config (`config.toml`)

```toml
[model]
api = "responses"
base_url = "https://api.deepseek.com"
model = "deepseek-v4-flash"
api_key_env = "DEEPSEEK_API_KEY"
max_output_tokens = 4096

[paths]
sessions_root = "sessions"
tools_root = "tools"

[limits]
max_steps = 20
read_limit = 2000
read_max_line_length = 2000
read_max_bytes = 51200
read_stream_min_size = 10485760
write_max_bytes = 1048576
tool_result_max_chars = 20000
context_budget_chars = 180000

[system_prompt]
text = """Use the read tool — not shell commands like cat — to inspect text files.
Results include line numbers. Use offset and limit to continue reading large files.

Use the write tool to create files or completely replace file contents.

Use the edit tool for targeted changes to existing UTF-8 text files.
It replaces literal old_string with new_string; by default old_string must
appear exactly once. If it appears multiple times, provide a more specific
old_string or set replace_all to true. Read the file first unless you just
created or edited it in this session."""
```

## Binary Implementations

### `claim` — Input: session path; Output: step state JSON

Reads `events.jsonl`. Determines what is owed.

Output JSON:

```json
{
  "session": "s1",
  "state": "awaiting_model",
  "last_user_message_seq": 1,
  "pending_tool_calls": []
}
```

States:

- `idle`: no work owed. The log ends with a terminal event: an `assistant_message` with no tool calls, or an `error` event.
- `awaiting_model`: a `user_message` or `tool_result` has no matching `assistant_message` after it.
- `awaiting_tool_result`: an `assistant_message` has tool calls without matching `tool_result` events. `pending_tool_calls` lists them as `{"id": "...", "name": "...", "arguments": {...}}` in log order.

An `error` event is terminal. The loop stops; a human or a new user message resumes it. `claim` reports `idle` when the last event is an `error`.

Idempotent: same log produces same output. `last_user_message_seq` is the 1-based line number of the last `user_message` in the log.

### `assemble` — Input: claim JSON + log; Output: ModelRequest JSON or error event

Projects the session log into a `ModelRequest`.

Algorithm:

1. Read config for system prompt, tool schemas, limits.
2. Read `events.jsonl`. Build `InputItem` array in log order:
   - `user_message` → `UserText(content)`
   - `assistant_message` → `AssistantText(content)` plus one `FunctionCall` per tool call
   - `tool_result` → `FunctionCallOutput(call_id, value.text or compact JSON)`
   - `error` → skip. Error events are terminal; the loop stops before `assemble` runs again.
3. Apply `tool_result_max_chars` cap to each tool result text. If clipped, append `[tool result clipped: N -> M chars]` to the text. The full value stays in the log. The clip is deterministic: same log, same clipped bytes.
4. Load tool schemas from `tools/*/tool.toml`. Sort by tool name. Serialize to `Vec<ToolSchema>`.
5. Compute char count. If `context_budget_chars` is exceeded, emit one `error` event to stdout with message "Context budget exceeded. Start a new session or reduce scope." Exit 0. `step.sh` checks for this event before running `model`.
6. Otherwise, output `ModelRequest` JSON to stdout. Exit 0.

Prefix stability: system prompt text, tool schema order (sorted by name), and message order (log order) are fixed. No timestamps or PIDs in output. Same log produces byte-identical requests.

### `model` — Input: ModelRequest JSON; Output: assistant_message JSON

Calls the DeepSeek API via OpenAI Responses wire format.

Implementation:

- Use `reqwest` with streaming.
- Build `/v1/responses` request from `ModelRequest`.
- Parse SSE events. Collect text deltas and function_call items.
- DeepSeek's stream ends with `response.completed`, `response.incomplete`, or `response.failed`. There is no `data: [DONE]` terminator. The parser keys on these terminal events.
- On completion, output one JSON object:

```json
{
  "text": "I will use the read tool.",
  "tool_calls": [
    { "id": "call-1", "name": "read", "arguments": "{\"file_path\":\"src/main.rs\"}" }
  ],
  "stop_reason": "stop",
  "usage": {
    "input_tokens": 120,
    "output_tokens": 30,
    "cache_read_tokens": 96,
    "cache_write_tokens": 24
  }
}
```

Usage fields are disjoint. `input_tokens` excludes cache hits. `cache_read_tokens` reports hits. DeepSeek folds cache hits into `prompt_tokens`, so the adapter subtracts them to keep counts disjoint. Fields are present only when the provider reports them.

Stop reasons: `stop`, `length`, `error`, `aborted`.

Fallback: if `/responses` returns 404 or 405, fall back to `/chat/completions` mapping the same input items to chat messages. The fallback is a one-shot capability probe, not a retry.

Note: This fallback merges two adapters (`ResponsesModelClient` and `ChatCompletionsModelClient`) into one binary for Phase 1. They split into separate trait implementations in Phase 3.

### `parse` — Input: model output JSON; Output: events JSONL

Validates model output and emits execution events.

Validation rules:

- Input must be one JSON object with `text`, `tool_calls`, and `stop_reason`. Reject malformed JSON with nonzero exit and a stderr diagnostic.
- Each tool call's `arguments` must parse as a JSON object. If it does not, emit one `error` event with message "Model emitted malformed tool arguments for call <id>." Exit with code 2.
- Each tool call's `name` must match a manifest in `tools/`. If it does not, emit one `error` event with message "Model called unknown tool <name>." Exit with code 2.

Emission rules:

- If `stop_reason` is `error` or `aborted`: emit one `error` event. Exit with code 2 (no tool calls to route).
- If `stop_reason` is `length`: emit one `assistant_message` event. For each tool call, emit a `tool_result` with `is_error: true` and `value.text` equal to "Arguments may be truncated. Re-issue the call with shorter arguments." Exit with code 2 (no tool calls to route).
- If no tool calls: emit `assistant_message`. Exit with code 2 (no tool calls to route).
- Otherwise: emit `assistant_message`. For each tool call, emit `tool_call`. Exit with code 1 (tool calls need routing).

The `assistant_message` event carries the model's `usage` object as a `usage` field so cache behavior is observable from the log.

Exit codes enable `step.sh` to branch.

Example output:

```jsonl
{"v":1,"type":"assistant_message","ts":"...","content":"I will read the file.","tool_calls":[{"id":"call-1","name":"read","arguments":{"file_path":"src/main.rs"}}],"stop_reason":"stop","usage":{"input_tokens":120,"output_tokens":30,"cache_read_tokens":96}}
{"v":1,"type":"tool_call","ts":"...","id":"call-1","name":"read","arguments":{"file_path":"src/main.rs"}}
```

### `route` — Input: tool_call events JSONL; Output: tool_result events JSONL

Dispatches each tool call to its subprocess.

Algorithm:

1. For each `tool_call` event:
2. Load `tools/<name>/tool.toml`. If no manifest exists, emit `tool_result` with `is_error: true` and `value.text` equal to "Unknown tool <name>." Do not spawn a process.
3. Validate `arguments` against the manifest's `[tool.schema]`. If validation fails, emit `tool_result` with `is_error: true` and `value.text` equal to "Tool arguments failed schema validation: <field>." Do not spawn a process.
4. Spawn subprocess: `command args`. Write `arguments` JSON to stdin. Enforce the manifest's `timeout_ms` (default 30000). On timeout, kill the process and emit `tool_result` with `is_error: true` and `value.text` equal to "Tool timed out after <timeout_ms> ms."
5. Read stdout. If stdout is a JSON object with a `text` field, use it as `value`. If stdout is non-JSON text, wrap it as `{"text": "<stdout>"}`. If stdout is JSON but not an object, wrap it as `{"text": "<compact JSON>"}`. If stdout is a JSON object without a `text` field, wrap it as `{"text": "<compact JSON>"}`.
6. If exit code is 0: emit `tool_result` with `is_error: false`.
7. If exit code is nonzero: capture stderr (capped at `tool_result_max_chars`). Emit `tool_result` with `is_error: true` and `value.text` from stderr. If stderr is empty, set `value.text` to "Tool exited with code <code>."

### `log` — Input: events JSONL on stdin; Output: nothing

Appends each line to `sessions/<id>/events.jsonl`.

Behavior:

- Validate each input line against its event schema in `schemas/events/v1/`. Reject the whole batch with nonzero exit and a stderr diagnostic naming the offending line if any line is invalid. No partial append on validation failure.
- Assign each event a sequence number: the 1-based line position in `events.jsonl` after append. The first event is seq 1. `claim` reads these positions to report `last_user_message_seq`.
- Append each line with `O_APPEND | O_WRONLY`. Each line is one `write` call.
- If a `write` fails mid-batch, the lines already written stay in the log. Exits nonzero. The log remains a valid prefix; `claim` recovers the partial state on the next step.

## Loop Scripts

### `step.sh` — One step with conditional routing

Each stage runs separately. Each stage's output goes to a temp file. Exit codes are captured per stage. `set -e` is disabled only around `parse`, whose nonzero exit codes are control flow, not failure.

```bash
#!/usr/bin/env bash
set -uo pipefail

SESSION="$1"
CONFIG="${CONFIG:-config.toml}"
SESSIONS_ROOT=$(jq -r '.paths.sessions_root' "$CONFIG")
SESSION_DIR="$SESSIONS_ROOT/$SESSION"
WORKDIR=$(mktemp -d "${TMPDIR:-/tmp}/step.XXXXXX")
trap 'rm -rf "$WORKDIR"' EXIT

# 1. claim: pure projection. Decide what is owed.
claim --session "$SESSION_DIR" > "$WORKDIR/claim.json" || exit 1
STATE=$(jq -r .state "$WORKDIR/claim.json")

# 2. idle: nothing owed. Append nothing (G1 idempotent replay).
if [ "$STATE" = "idle" ]; then
  exit 0
fi

# 3. awaiting_tool_result: crash recovery (G2).
#    Route the pending calls without calling the model.
if [ "$STATE" = "awaiting_tool_result" ]; then
  jq -c '.pending_tool_calls[]' "$WORKDIR/claim.json" \
    | route --tools tools/ > "$WORKDIR/routed.jsonl" || exit 1
  log --session "$SESSION_DIR" < "$WORKDIR/routed.jsonl" || exit 1
  exit 0
fi

# 4. awaiting_model: assemble, then check for a budget error before model.
assemble --session "$SESSION_DIR" --config "$CONFIG" > "$WORKDIR/model-request.json" || exit 1
if jq -e '.type == "error"' "$WORKDIR/model-request.json" > /dev/null; then
  log --session "$SESSION_DIR" < "$WORKDIR/model-request.json" || exit 1
  exit 0
fi

# 5. model: one API call. Capture exit code.
set +e
model --config "$CONFIG" < "$WORKDIR/model-request.json" > "$WORKDIR/model-output.json"
MODEL_EXIT=$?
set -e

if [ "$MODEL_EXIT" -ne 0 ]; then
  # Model failed. Log an error event and stop.
  ERROR_EVENT=$(jq -n --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    '{v:1, type:"error", ts:$ts, message:"model API call failed"}')
  echo "$ERROR_EVENT" | log --session "$SESSION_DIR" || exit 1
  exit 0
fi

# 6. parse: validate, emit events, choose exit code.
set +e
parse --config "$CONFIG" < "$WORKDIR/model-output.json" > "$WORKDIR/parsed.jsonl"
PARSE_EXIT=$?
set -e

# 7. route only when parse says tool calls need routing (exit 1).
if [ "$PARSE_EXIT" -eq 1 ]; then
  jq -c 'select(.type == "tool_call")' "$WORKDIR/parsed.jsonl" \
    | route --tools tools/ > "$WORKDIR/routed.jsonl" || exit 1
  cat "$WORKDIR/parsed.jsonl" "$WORKDIR/routed.jsonl" | log --session "$SESSION_DIR" || exit 1
else
  log --session "$SESSION_DIR" < "$WORKDIR/parsed.jsonl" || exit 1
fi

exit 0
```

### `turn.sh` — Loop driver

Reads `max_steps` and `sessions_root` from config. Calls `step.sh` by path.

```bash
#!/usr/bin/env bash
set -euo pipefail

SESSION="$1"
CONFIG="${CONFIG:-config.toml}"
SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
MAX_STEPS=$(jq -r '.limits.max_steps // 20' "$CONFIG")
SESSIONS_ROOT=$(jq -r '.paths.sessions_root' "$CONFIG")
STEPS=0

while [ "$STEPS" -lt "$MAX_STEPS" ]; do
  STEPS=$((STEPS + 1))
  "$SCRIPT_DIR/step.sh" "$SESSION" || exit 1
  STATE=$(claim --session "$SESSIONS_ROOT/$SESSION" | jq -r .state)
  if [ "$STATE" = "idle" ]; then
    exit 0
  fi
done

echo "max_steps reached ($MAX_STEPS)" >&2
exit 1
```

### Script dependencies

- `jq` is required for JSON inspection in the scripts.
- The stage binaries (`claim`, `assemble`, `model`, `parse`, `route`, `log`) must be on `PATH` or the scripts must be run from the repo root with the binaries in `./bin`.

## Tool Implementations

### `read` Tool

Compiled Rust binary. Reads one JSON from stdin. Writes one JSON to stdout.

Accepts config via CLI flags: `--read-limit`, `--read-max-line-length`, `--read-max-bytes`, `--read-stream-min-size`. Defaults match `config.toml`.

Input:

```json
{"file_path": "src/main.rs", "offset": 1, "limit": 100}
```

Algorithm:

1. Resolve `file_path` relative to harness cwd.
2. Check file size. If `>= read_stream_min_size`, stream chunks. Otherwise read fully.
3. Scan line by line. Count all lines. Buffer only lines in `[offset, offset + limit)`.
4. Cap each line at `read_max_line_length` chars. Append ` ... (line truncated to <N> chars)` if cut, where `<N>` is `read_max_line_length`.
5. Stop buffering when output bytes exceed `read_max_bytes`.
6. Continue scanning to EOF to get `totalLines`.
7. Render output:

```text
<path>src/main.rs</path>
<type>file</type>
<content>
1: use std::io;
...
100: }
(Showing lines 1-100 of 4123. Use offset=101 to continue.)
</content>
```

Input validation:

- `offset` must be >= 1. If missing, default to 1. If < 1, exit 1 with stderr "Error: offset must be >= 1."
- `limit` must be >= 1. If missing, default to `read_limit`. If < 1, exit 1 with stderr "Error: limit must be >= 1." If > `read_limit`, cap to `read_limit`.

Error cases (all exit 1 with a stderr message):

- `offset > totalLines`: "Error: offset <N> is past end of file. File has <M> lines."
- File is binary (null bytes in first 8KB): "Error: <path> is not a UTF-8 text file."
- File not found: "Error: file not found: <path>."

### `write` Tool

Compiled Rust binary.

Input:

```json
{"file_path": "src/main.rs", "content": "fn main() {}"}
```

Algorithm:

1. Resolve `file_path`.
2. If `content` length exceeds `write_max_bytes`, exit 1 with stderr "Error: content is <N> bytes, exceeds max <M> bytes."
3. Create parent directories if missing. If a parent path component is a file, exit 1 with stderr "Error: cannot create directories: <component> is a file."
4. Detect if file existed before write. If `file_path` is a directory, exit 1 with stderr "Error: <path> is a directory."
5. Write content. Use `O_WRONLY | O_CREAT | O_TRUNC`.
6. Output:

```json
{"text":"Successfully wrote 4123 bytes to src/main.rs.","path":"src/main.rs","operation":"create","bytes":4123}
```

Empty `content` is valid: it writes an empty file.

### `edit` Tool

Compiled Rust binary.

Input:

```json
{"file_path": "src/main.rs", "old_string": "fn main() {}", "new_string": "fn main() { println!(); }"}
```

Algorithm:

1. Resolve `file_path`. Read full content once. Preserve BOM if present.
2. Detect line ending style (CRLF vs LF). Preserve it.
3. If `file_path` is missing: exit 1 with stderr "Error: file not found: <path>."
4. If the file is binary (null bytes in first 8KB): exit 1 with stderr "Error: <path> is not a UTF-8 text file."
5. If `old_string` is empty: exit 1 with stderr "Error: old_string must not be empty."
6. If `old_string == new_string`: exit 1 with stderr "Error: old_string and new_string are identical. No change needed."
7. Normalize line endings for matching only: compare `old_string` against the file content with both normalized to LF. This lets an LF `old_string` match a CRLF file. The replacement writes back with the file's original line endings.
8. Count occurrences of `old_string` in the normalized content.
9. If count == 0: exit 1 with stderr "Error: old_string not found in file. It may have changed since your last read."
10. If `replace_all` is false and count > 1: exit 1 with stderr "Error: old_string appears N times. Use a larger old_string with more context, or set replace_all=true."
11. Apply replacement(s).
12. Write back with original line endings and BOM.
13. Output:

```json
{"text":"The file src/main.rs has been updated successfully.","path":"src/main.rs","before":"old","after":"new","replace_all":false}
```

Empty `new_string` is valid: it deletes the match.

## Event Schemas

### `user_message`

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "required": ["v", "type", "ts", "content"],
  "properties": {
    "v": { "type": "integer", "const": 1 },
    "type": { "type": "string", "const": "user_message" },
    "ts": { "type": "string", "format": "date-time" },
    "content": { "type": "string" }
  }
}
```

### `assistant_message`

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "required": ["v", "type", "ts", "content", "tool_calls", "stop_reason"],
  "properties": {
    "v": { "type": "integer", "const": 1 },
    "type": { "type": "string", "const": "assistant_message" },
    "ts": { "type": "string", "format": "date-time" },
    "content": { "type": "string" },
    "tool_calls": {
      "type": "array",
      "items": {
        "type": "object",
        "required": ["id", "name", "arguments"],
        "properties": {
          "id": { "type": "string" },
          "name": { "type": "string" },
          "arguments": { "type": "object" }
        }
      }
    },
    "stop_reason": { "type": "string", "enum": ["stop", "length", "error", "aborted"] },
    "usage": {
      "type": "object",
      "properties": {
        "input_tokens": { "type": "integer" },
        "output_tokens": { "type": "integer" },
        "cache_read_tokens": { "type": "integer" },
        "cache_write_tokens": { "type": "integer" }
      }
    }
  }
}
```

`usage` is optional. It is present when the provider reports token counts. The counts are disjoint: `input_tokens` excludes cache hits.

### `tool_call`

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "required": ["v", "type", "ts", "id", "name", "arguments"],
  "properties": {
    "v": { "type": "integer", "const": 1 },
    "type": { "type": "string", "const": "tool_call" },
    "ts": { "type": "string", "format": "date-time" },
    "id": { "type": "string" },
    "name": { "type": "string" },
    "arguments": { "type": "object" }
  }
}
```

### `tool_result`

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "required": ["v", "type", "ts", "id", "value", "is_error"],
  "properties": {
    "v": { "type": "integer", "const": 1 },
    "type": { "type": "string", "const": "tool_result" },
    "ts": { "type": "string", "format": "date-time" },
    "id": { "type": "string" },
    "value": { "type": "object" },
    "is_error": { "type": "boolean" }
  }
}
```

### `error`

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "required": ["v", "type", "ts", "message"],
  "properties": {
    "v": { "type": "integer", "const": 1 },
    "type": { "type": "string", "const": "error" },
    "ts": { "type": "string", "format": "date-time" },
    "message": { "type": "string" }
  }
}
```

## Conformance Tests

`tool-conformance.sh` runs each tool with fixed inputs.

Read tests:

- Small file: verify line numbers and content.
- Offset/limit pagination: verify correct window.
- Empty file: verify footer `(End of file - total 0 lines)`.
- Offset past EOF: verify exit 1.
- Long line: verify truncation marker.
- 60KB file: verify byte cap footer.
- Binary file: verify exit 1.

Write tests:

- Create new file: verify content and `operation: create`.
- Overwrite existing file: verify `operation: update`.
- Create parent dirs: verify directory creation.
- Content too large: verify exit 1.

Edit tests:

- Unique match success: verify replacement.
- No match error: verify exit 1 and stderr contains "old_string not found".
- Multiple matches error: verify exit 1 and stderr contains "appears N times".
- `replace_all` success: verify all replaced.
- Empty `old_string` error: verify exit 1 and stderr contains "must not be empty".
- Identical strings error: verify exit 1 and stderr contains "identical".
- Empty `new_string` deletion: verify the match is removed.
- CRLF preservation: verify line endings unchanged.
- CRLF match with LF `old_string`: verify the match succeeds and the file stays CRLF.
- BOM preservation: verify BOM unchanged after edit.
- Missing file error: verify exit 1 and stderr contains "file not found".
- Binary file error: verify exit 1 and stderr contains "not a UTF-8 text file".

Each test checks:

- Stdout is one JSON object with a `text` field on success.
- Stderr is ignored on success.
- Exit code is 0 on success and nonzero on failure.
- Stderr contains expected error text on failure.

## Cache conformance test

`cache-e2e.sh` is key-gated. It runs only when `DEEPSEEK_API_KEY` is set.

Steps:

1. Create a session with a user message that forces a tool call.
2. Run `turn.sh` to completion. This produces at least two model requests.
3. Read the `usage` field from each `assistant_message` event in the log.
4. Verify every request after the first has `cache_read_tokens > 0`.

This test proves the append-only log and deterministic projection produce byte-identical prefixes that hit the DeepSeek provider cache. It is the production observable for cache behavior.

## Build Plan

1. Create workspace `Cargo.toml` with binaries: `claim`, `assemble`, `model`, `parse`, `route`, `log`, `read`, `write`, `edit`.
2. Implement `log` first. It is the simplest and needed for integration tests.
3. Implement `claim` and `assemble`. They are pure projections.
4. Implement `model`. Use `reqwest` for streaming.
5. Implement `parse` with exit code branching.
6. Implement `route`.
7. Implement tools: `read`, `write`, `edit`.
8. Write `step.sh` with conditional routing and `turn.sh`.
9. Write `tool-conformance.sh`.
10. Run end-to-end test: `turn.sh s1` with a user message asking to read and edit a file.
11. Write `cache-e2e.sh` and run it with a real `DEEPSEEK_API_KEY` to verify provider cache hits.

This plan satisfies the spec. Each binary has one responsibility. The session log is the source of truth. Tools are subprocesses. The system prompt and tool schemas are fixed strings for prefix stability. The cache e2e test is the external-fact gate for the DeepSeek provider.
