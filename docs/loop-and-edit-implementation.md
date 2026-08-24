# Loop + Edit Tool — Implementation Proposal

Phase 1 vertical slice. A one-step-per-turn loop driver over the session log. A model adapter speaking the OpenAI Responses wire format against a DeepSeek provider. Three file tools: read, write, edit.

## Cache efficiency

DeepSeek uses implicit prefix caching. The provider caches byte-identical request prefixes in 64-token storage units. The harness has no explicit cache-control directives. Its job is to make prefixes stable and cache behavior observable. Caching is best-effort: cache construction takes seconds, and the provider does not guarantee a hit on the immediately following request.

Mechanisms:

- **Append-only log.** The log grows. Requests derive from it. Each request's prefix is byte-identical to its predecessor. The provider cache then hits.
- **Deterministic projection.** `assemble` renders the log to a request with fixed field order, fixed tool-schema order, and no timestamps or PIDs. Same log, same bytes.
- **Frozen call config.** Model, temperature, and reasoning effort affect cache reuse. The reasoning effort comes from the `reasoning_effort` key in `config.toml`. The harness holds these values constant across a session. A change invalidates the prefix cache.
- **Usage on the log.** `model` reports token usage. `parse` records it on the `assistant_message` event. Cache behavior is observable from the log.
- **Cache observability.** The Responses API reports `input_tokens` (total input tokens, cache hits included) and `input_tokens_details.cached_tokens` (the cache-hit portion). The adapter records both on the log. DeepSeek reports no cache-write metric.
- **Cache e2e test.** A key-gated test runs two consecutive turns against the live API. It verifies that at least one request in the second turn reports `cached_tokens > 0`, retrying while the provider cache constructs.

## Project Layout

Phase 1 uses separate binaries glued by bash. No shared Rust crate. JSON contracts are the spec.

```
rust-unix-harness/
├── Cargo.toml                  # workspace
├── config.toml                 # model config, paths
├── bin/
│   ├── claim                   # Rust binary: derive step state from log
│   ├── assemble                # Rust binary: project log to ModelRequest
│   ├── model                   # Rust binary: call the model API via Responses format
│   ├── parse                   # Rust binary: validate model output, emit tool_call events
│   ├── route                   # Rust binary: dispatch tool calls, run tools
│   ├── log                     # Rust binary: append events atomically
│   └── user                    # Rust binary: append a user_message event to the log
├── scripts/
│   ├── step.sh                 # one-step pipeline with conditional routing
│   ├── turn.sh                 # loop driver (calls step.sh)
│   └── tool-conformance.sh     # G4 conformance harness
├── config.toml                 # DeepSeek provider config
├── config.llama.toml           # llama.cpp provider config
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
max_output_tokens = 32768
reasoning_effort = "medium"

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

`max_output_tokens` bounds the generated output. The value includes reasoning tokens. `32768` gives headroom for long agentic turns. The API accepts a cap up to `384000`.

`reasoning_effort` sets the thinking level. Allowed values are `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, and `max`. `none` disables thinking. `medium` gives high-effort thinking on `deepseek-v4-flash`. The value maps to the `reasoning.effort` field in the request. Both fields are frozen call config.

## Backends

The harness is provider-agnostic. The `[model]` section in the config selects the provider. Both supported backends speak the OpenAI Responses wire format, so the same request shape and SSE parser serve both.

- DeepSeek: `config.toml`. Base URL `https://api.deepseek.com`, model `deepseek-v4-flash`, key from `DEEPSEEK_API_KEY`.
- llama.cpp: `config.llama.toml`. Base URL `http://127.0.0.1:8080`, model name as served by the local server, no API key needed.

Select a config with the `CONFIG` environment variable or the `--config` flag:

```bash
CONFIG=config.llama.toml bash scripts/turn.sh s1
```

llama.cpp requires no code changes. It emits the same SSE event names as DeepSeek. Its terminal event carries the full response object, usage, and tool call items. The empty `Authorization` header is harmless.

## Binary Implementations

### `claim` — Input: session path. Output: step state JSON

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

- `idle`: no work owed. The log ends with a terminal event. The terminal event is an `assistant_message` with no tool calls, or an `error` event.
- `awaiting_model`: a `user_message` or `tool_result` has no matching `assistant_message` after it.
- `awaiting_tool_result`: an `assistant_message` has tool calls without matching `tool_result` events. `pending_tool_calls` lists them as `{"id": "...", "name": "...", "arguments": {...}}` in log order.

An `error` event is terminal. The loop stops. A human or a new user message resumes it. `claim` reports `idle` when the last event is an `error`.

When the state is `idle`, `pending_tool_calls` is empty.

Idempotent: same log produces same output. `last_user_message_seq` is the 1-based line number of the last `user_message` in the log.

### `assemble` — Input: claim JSON + log; Output: ModelRequest JSON or error event

Projects the session log into a `ModelRequest`.

Algorithm:

1. Read config for system prompt, tool schemas, limits.
2. Read `events.jsonl`. Build the `input` array in log order:
   - `user_message` → `{"type":"message","role":"user","content": <content>}`
   - `assistant_message` → `{"type":"message","role":"assistant","content": <content>}` plus one `{"type":"function_call","call_id": <id>, "name": <name>, "arguments": <JSON string>}` per tool call
   - `tool_result` → `{"type":"function_call_output","call_id": <id>, "output": <value.text>}`
   - `error` → skip. Error events are terminal. The loop stops before `assemble` runs again.
3. Apply `tool_result_max_chars` cap to each tool result text. If clipped, append `[tool result clipped: N -> M chars]` to the text. The full value stays in the log. The clip is deterministic. Same log produces the same clipped bytes.
4. Load tool schemas from `tools/*/tool.toml`. Sort by tool name. Serialize each schema to `{"type":"function","name": <name>, "description": <desc>, "parameters": <params>}`. The `name` field is top level. This is the Responses API shape.
5. Compute char count. If `context_budget_chars` is exceeded, emit one `error` event to stdout with message "Context budget exceeded. Start a new session or reduce scope." Exit 0. `step.sh` checks for this event before running `model`.
6. Otherwise, output `ModelRequest` JSON to stdout. Exit 0.

The `ModelRequest`:

```json
{
  "model": "deepseek-v4-flash",
  "instructions": "<system prompt>",
  "input": [ ...input items... ],
  "tools": [ ...tool schemas... ]
}
```

The system prompt goes in the `instructions` field. `arguments` in a `function_call` item is a JSON string. The log stores tool arguments as JSON objects. `assemble` serializes each object back to a string.

Prefix stability: `instructions` text, tool schema order (sorted by name), and input item order (log order) are fixed. No timestamps or PIDs in output. Same log produces byte-identical requests.

### `model` — Input: ModelRequest JSON. Output: assistant_message JSON

Calls the DeepSeek API via the OpenAI Responses wire format.

Implementation:

- Use `reqwest` with streaming.
- Build the `/v1/responses` request from `ModelRequest`. Add `max_output_tokens` from config and set `stream` to `true`.
- Add the `reasoning` object with an `effort` field. The value comes from `reasoning_effort` in `config.toml`. The field controls the thinking mode and its effort.
- Parse SSE events. Each event carries a `type` field.
- `response.output_text.delta` carries visible text deltas.
- `response.output_item.added` and `response.output_item.done` carry `function_call` items with `name` and `call_id`.
- `response.function_call_arguments.delta` and `response.function_call_arguments.done` carry the arguments JSON string.
- DeepSeek's stream ends with `response.completed`, `response.incomplete`, or `response.failed`. There is no `data: [DONE]` terminator. The terminal event carries the full response object in the `response` field. The parser reads the output items and usage from this object. The streaming deltas are a fallback only.
- `response.incomplete` maps to stop reason `length`. `response.failed` maps to `error`.
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
    "cached_tokens": 96
  }
}
```

Usage fields follow the Responses API shape. `input_tokens` is the total input tokens, cache hits included. `cached_tokens` is the cache-hit portion from `input_tokens_details.cached_tokens`. DeepSeek reports no cache-write metric. Fields are present only when the provider reports them.

Stop reasons: `stop`, `length`, `error`, `aborted`.

Fallback: if `/responses` returns 404 or 405, fall back to `/chat/completions`. The fallback maps `instructions` to a system message. Each `input` item maps to a chat message. `message` items become role/content messages. `function_call` items merge into the trailing assistant message as `tool_calls`. `function_call_output` items become `role: "tool"` messages. Tool schemas convert from the top-level `name` form to the nested `function` form. The fallback is a one-shot capability probe, not a retry.

Note: This fallback merges two adapters (`ResponsesModelClient` and `ChatCompletionsModelClient`) into one binary for Phase 1. They split into separate trait implementations in Phase 3.

### `parse` — Input: model output JSON. Output: events JSONL

Validates model output and emits execution events.

Validation rules:

- Input must be one JSON object with `text`, `tool_calls`, and `stop_reason`. Reject malformed JSON with nonzero exit and a stderr diagnostic.
- Each tool call's `arguments` must parse as a JSON object. If it does not, emit one `error` event with message "Model emitted malformed tool arguments for call <id>." Exit with code 2.
- Each tool call's `name` must match a manifest in `tools/`. If it does not, emit one `error` event with message "Model called unknown tool <name>." Exit with code 2.

The model returns `arguments` as a JSON string. `parse` parses it into a JSON object before emission. All emitted events carry `arguments` as an object.

Emission rules:

- If `stop_reason` is `error` or `aborted`: emit one `error` event. Exit with code 2 (no tool calls to route).
- If `stop_reason` is `length`: emit one `assistant_message` event. For each tool call, emit a `tool_result` with `is_error: true` and `value.text` equal to "Arguments may be truncated. Re-issue the call with shorter arguments." Exit with code 2 (no tool calls to route).
- If no tool calls: emit `assistant_message`. Exit with code 2 (no tool calls to route).
- Otherwise: emit `assistant_message`. For each tool call, emit `tool_call`. Exit with code 1 (tool calls need routing).

The `assistant_message` event carries the model's `usage` object as a `usage` field so cache behavior is observable from the log.

Exit codes enable `step.sh` to branch.

Example output:

```jsonl
{"v":1,"type":"assistant_message","ts":"...","content":"I will read the file.","tool_calls":[{"id":"call-1","name":"read","arguments":{"file_path":"src/main.rs"}}],"stop_reason":"stop","usage":{"input_tokens":120,"output_tokens":30,"cached_tokens":96}}
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
- If a `write` fails mid-batch, the lines already written stay in the log. Exits nonzero. The log remains a valid prefix. `claim` recovers the partial state on the next step.

### `user` — Input: message content. Output: appended event

Composes a `user_message` event and appends it to a session log. The user does not hand-write the JSONL format.

Usage:

```bash
user --session s1 "Read config.toml"
printf 'multi\nline' | user --session s1
user --session s1 --config config.llama.toml "Read config.toml"
```

Behavior:

- Accept `--session` as a session name or a session directory. A name resolves against `sessions_root` from config. A value with a path separator or an existing directory is used as-is.
- Take content as a positional argument or read it from stdin. Reject empty content with exit 1.
- Build the event with `v: 1`, `type: "user_message"`, a real UTC `ts`, and the content.
- Validate the produced event against `user_message.json` before append. Exit 1 on mismatch.
- Create the session directory if missing. Append with `O_APPEND`. Print the appended event on success.

## Loop Scripts

### `step.sh` — One step with conditional routing

Each stage runs separately. Each stage's output goes to a temp file. Exit codes are captured per stage. `set -e` is disabled only around `parse`, whose nonzero exit codes are control flow, not failure.

The script below is the exact `scripts/step.sh`. It reads config with `awk` because `config.toml` is TOML, not JSON. `jq` cannot parse it. The binaries come from `target/debug` relative to the script location.

```bash
#!/usr/bin/env bash
set -uo pipefail

SESSION="$1"
CONFIG="${CONFIG:-config.toml}"
SESSIONS_ROOT=$(awk -F'"' '/^sessions_root[[:space:]]*=/{print $2; exit}' "$CONFIG")
SESSION_DIR="$SESSIONS_ROOT/$SESSION"
WORKDIR=$(mktemp -d "${TMPDIR:-/tmp}/step.XXXXXX")
trap 'rm -rf "$WORKDIR"' EXIT

BIN_DIR="$(cd "$(dirname "$0")/../target/debug" && pwd)"
TOOL_DIR="$(cd "$(dirname "$0")/../tools" && pwd)"
SCHEMA_DIR="$(cd "$(dirname "$0")/../schemas/events/v1" && pwd)"

# 1. claim: pure projection. Decide what is owed.
"$BIN_DIR/claim" --session "$SESSION_DIR" > "$WORKDIR/claim.json" || exit 1
STATE=$(jq -r .state "$WORKDIR/claim.json")

# 2. idle: nothing owed. Append nothing (G1 idempotent replay).
if [ "$STATE" = "idle" ]; then
  exit 0
fi

# 3. awaiting_tool_result: crash recovery (G2).
#    Route the pending calls without calling the model.
if [ "$STATE" = "awaiting_tool_result" ]; then
  TS=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  jq -c '.pending_tool_calls[] | . + {type: "tool_call", ts: $ts}' --arg ts "$TS" "$WORKDIR/claim.json" \
    | "$BIN_DIR/route" --tools "$TOOL_DIR" > "$WORKDIR/routed.jsonl" || exit 1
  "$BIN_DIR/log" --session "$SESSION_DIR" --schemas "$SCHEMA_DIR" < "$WORKDIR/routed.jsonl" || exit 1
  exit 0
fi

# 4. awaiting_model: assemble, then check for a budget error before model.
"$BIN_DIR/assemble" --session "$SESSION_DIR" --config "$CONFIG" > "$WORKDIR/model-request.json" || exit 1
if jq -e '.type == "error"' "$WORKDIR/model-request.json" > /dev/null; then
  "$BIN_DIR/log" --session "$SESSION_DIR" --schemas "$SCHEMA_DIR" < "$WORKDIR/model-request.json" || exit 1
  exit 0
fi

# 5. model: one API call. Capture exit code.
set +e
"$BIN_DIR/model" --config "$CONFIG" < "$WORKDIR/model-request.json" > "$WORKDIR/model-output.json"
MODEL_EXIT=$?
set -e

if [ "$MODEL_EXIT" -ne 0 ]; then
  # Model failed. Log an error event and stop.
  ERROR_EVENT=$(jq -cn --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    '{v:1, type:"error", ts:$ts, message:"model API call failed"}')
  echo "$ERROR_EVENT" | "$BIN_DIR/log" --session "$SESSION_DIR" --schemas "$SCHEMA_DIR" || exit 1
  exit 0
fi

# 6. parse: validate, emit events, choose exit code.
set +e
"$BIN_DIR/parse" --config "$CONFIG" < "$WORKDIR/model-output.json" > "$WORKDIR/parsed.jsonl"
PARSE_EXIT=$?
set -e

# 7. route only when parse says tool calls need routing (exit 1).
if [ "$PARSE_EXIT" -eq 1 ]; then
  jq -c 'select(.type == "tool_call")' "$WORKDIR/parsed.jsonl" \
    | "$BIN_DIR/route" --tools "$TOOL_DIR" > "$WORKDIR/routed.jsonl" || exit 1
  cat "$WORKDIR/parsed.jsonl" "$WORKDIR/routed.jsonl" | "$BIN_DIR/log" --session "$SESSION_DIR" --schemas "$SCHEMA_DIR" || exit 1
else
  "$BIN_DIR/log" --session "$SESSION_DIR" --schemas "$SCHEMA_DIR" < "$WORKDIR/parsed.jsonl" || exit 1
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
MAX_STEPS=$(awk -F'[[:space:]]*=[[:space:]]*' '/^max_steps[[:space:]]*=/{print $2; exit}' "$CONFIG")
MAX_STEPS=${MAX_STEPS:-20}
SESSIONS_ROOT=$(awk -F'"' '/^sessions_root[[:space:]]*=/{print $2; exit}' "$CONFIG")
STEPS=0

while [ "$STEPS" -lt "$MAX_STEPS" ]; do
  STEPS=$((STEPS + 1))
  "$SCRIPT_DIR/step.sh" "$SESSION" || exit 1
  BIN_DIR="$(cd "$SCRIPT_DIR/../target/debug" && pwd)"
  STATE=$("$BIN_DIR/claim" --session "$SESSIONS_ROOT/$SESSION" | jq -r .state)
  if [ "$STATE" = "idle" ]; then
    exit 0
  fi
done

echo "max_steps reached ($MAX_STEPS)" >&2
exit 1
```

### Script dependencies

- `awk` parses `config.toml`. The extraction uses POSIX `awk` only, so it works on macOS BSD awk and GNU awk.
- `jq` inspects JSON in the scripts.
- `cargo` builds the binaries into `target/debug`.
- The scripts resolve binaries from `target/debug` relative to the script location. They do not need `PATH` entries.

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
7. Render output as one JSON object:

```json
{
  "path": "src/main.rs",
  "type": "file",
  "content": "1: use std::io;\n...\n100: }\n(15 lines omitted)",
  "total_lines": 4123
}
```

The `content` field holds the visible lines with line numbers. It uses `(N lines omitted)` markers for skipped or capped lines. An empty file produces `(End of file - total 0 lines)`. A truncated line appends ` ... (line truncated to <N> chars)`. The output is JSON, so `route` wraps it as compact JSON for the model.

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
        "cached_tokens": { "type": "integer" }
      }
    }
  }
}
```

`usage` is optional. It is present when the provider reports token counts. `input_tokens` includes cache hits; `cached_tokens` is the cache-hit portion of the input.

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

The suite runs 24 tests. All 24 pass against the current binaries.

## Cache conformance test

`cache-e2e.sh` is key-gated. It runs only when `DEEPSEEK_API_KEY` is set.

Steps:

1. Create a session with a user message that forces a tool call.
2. Run `turn.sh` to completion. This produces at least two model requests (turn 1).
3. Append a new user message to the log. The turn 1 prefix stays byte-identical.
4. Run `turn.sh` again (turn 2). Requests in turn 2 share the turn 1 prefix.
5. Read the `usage` field from each `assistant_message` event in the log.
6. Verify at least one request in turn 2 has `cached_tokens > 0`. If none does, wait a few seconds and repeat from step 3. The provider cache constructs asynchronously and is best-effort.

This test proves the append-only log and deterministic projection produce byte-identical prefixes that hit the DeepSeek provider cache. It is the production observable for cache behavior.

The test passed against the live API. Turn 2 reported `cached_tokens = 512 > 0`.

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
10. Run end-to-end test: `turn.sh s1` with a user message asking to read and edit a file. Done and verified against the live API.
11. Write `cache-e2e.sh` and run it with a real `DEEPSEEK_API_KEY` to verify provider cache hits. Done and verified.

All steps are complete. The harness runs end to end against the live DeepSeek API.

This plan satisfies the spec. Each binary has one responsibility. The session log is the source of truth. Tools are subprocesses. The system prompt and tool schemas are fixed strings for prefix stability. The cache e2e test is the external-fact gate for the DeepSeek provider.
