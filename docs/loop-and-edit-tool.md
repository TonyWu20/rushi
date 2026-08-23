# Loop + Edit Tool — Deep Spec

Phase 1 vertical slice: a one-step/turn loop driver over the session log, a
model adapter speaking the **OpenAI Responses** wire format against a
**DeepSeek** provider, and the first real capability: file **read/write/edit**
tools with mature token-efficiency behavior borrowed from
[pi-coding-agent](https://github.com/earendil-works/pi) and
[deepseek-harness](https://github.com/deepseek-ai/deepseek-harness).

## 1. Model communication

### 1.1 Port first: `ModelClient`

The loop depends on a `ModelClient` trait, not on a wire format. Two adapters
implement it:

- `ResponsesModelClient` — primary; speaks the OpenAI Responses API wire format.
- `ChatCompletionsModelClient` — fallback; same internal request/response
  types, adapts to `/chat/completions` if a deployed DeepSeek endpoint rejects
  `/responses`.

```rust
#[async_trait]
pub trait ModelClient: Send + Sync {
    async fn stream(&self, req: ModelRequest) -> Result<ModelResponse, ModelError>;
}

pub struct ModelRequest {
    pub system: String,        // system/developer prompt
    pub input: Vec<InputItem>, // projected session log
    pub tools: Vec<ToolSchema>,
    pub max_output_tokens: Option<u32>,
    pub temperature: Option<f32>,
}

pub enum InputItem {
    UserText(String),
    AssistantText(String),
    FunctionCall { call_id: String, name: String, arguments: String },
    FunctionCallOutput { call_id: String, output: String },
}

pub struct ModelResponse {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub stop_reason: StopReason, // stop | length | error | aborted
    pub usage: Usage,            // input_tokens (incl. cache hits), output_tokens, cached_tokens
}
```

The loop only sees `ModelRequest`/`ModelResponse`. DeepSeek quirks live in the
adapter.

### 1.2 Provider config

```toml
[model]
api = "responses"                  # or "chat_completions"
base_url = "https://api.deepseek.com"
model = "deepseek-v4-flash"        # or "deepseek-v4-pro"
api_key_env = "DEEPSEEK_API_KEY"
max_output_tokens = 4096
```

The Responses API accepts `deepseek-v4-flash`, `deepseek-v4-pro`, and
`deepseek-v4-flash-vision-exp`. The legacy names `deepseek-chat` and
`deepseek-reasoner` were discontinued on 2026-07-24; they never work with the
Responses endpoint.

The `model` binary calls the API over HTTP with `reqwest` (the Rust `openai`
crate does not cover the Responses API). `DEEPSEEK_API_KEY` is read from the
environment; the harness never writes it to the session log.

### 1.3 Responses wire subset

Request:

```json
{
  "model": "deepseek-v4-flash",
  "stream": true,
  "store": false,
  "input": [
    { "role": "developer", "content": [ { "type": "input_text", "text": "<system prompt>" } ] },
    { "type": "message", "role": "user", "content": [ { "type": "input_text", "text": "rename the file" } ] },
    { "type": "message", "role": "assistant", "content": [ { "type": "output_text", "text": "I'll use the edit tool." } ] },
    { "type": "function_call", "call_id": "call-1", "name": "edit", "arguments": "{\"file_path\":\"a.txt\",\"old_string\":\"x\",\"new_string\":\"y\"}" },
    { "type": "function_call_output", "call_id": "call-1", "output": "{\"text\":\"The file a.txt has been updated.\"}" }
  ],
  "tools": [
    {
      "type": "function",
      "name": "edit",
      "description": "Edit an existing UTF-8 text file by replacing literal text.",
      "parameters": { "type": "object", "properties": { "file_path": { "type": "string" }, "old_string": { "type": "string" }, "new_string": { "type": "string" }, "replace_all": { "type": "boolean" } }, "required": ["file_path", "old_string", "new_string"] }
    }
  ],
  "tool_choice": "auto"
}
```

Notes:

- `store: false` — the session log is our store; do not rely on server-side
  response storage.
- `stream: true` — parse SSE events; support both `response.output_text.delta`
  and `response.output_item.done` (for `function_call` items). The stream ends
  with a `response.completed`, `response.incomplete`, or `response.failed`
  event. There is no `data: [DONE]` terminator.
- If the provider rejects the `developer` role, fall back to `system`.
- If the provider rejects `/responses` entirely (405/404), the
  `ChatCompletionsModelClient` adapter maps the same `InputItem` stream to
  OpenAI chat messages with `tool_calls`/`tool` roles.
- Usage: the Responses API reports `input_tokens` (total input tokens, cache
  hits included) and `input_tokens_details.cached_tokens` (the cache-hit
  portion). DeepSeek reports no cache-write metric. The Chat Completions
  adapter maps `prompt_tokens`/`prompt_cache_hit_tokens` to the same fields.

### 1.4 Token efficiency: prefix stability

DeepSeek's caching is prefix-based and best-effort. Cache construction takes
seconds, and the provider does not guarantee a hit on the immediately following
request. Therefore `assemble` must produce **byte-identical prefixes for
unchanged history** within a session:

- system prompt: fixed string, no timestamps, no random ids
- tool schemas: same order, same JSON serialization
- past messages: append-only; never reorder, never re-render old tool results

Corollary: no wall-clock time, no temp dir paths, no PIDs may appear in the
system prompt or tool descriptions. The first time such data is needed, append
it as a user message instead.

## 2. Loop design

### 2.1 Refined stage pipeline

The earlier pipeline is kept, with two stages now fully specified:

```
claim | assemble | model | parse | route | tool-* | log
```

- `claim` — pure projection of `events.jsonl`; determines whether work is owed
  and which step/turn is active. Idempotent: same log → same claim.
- `assemble` — pure projection of the log into a `ModelRequest`. Owns all
  token-efficiency rules (this document).
- `model` — the `ModelClient` adapter; emits `assistant/message` (text and/or
  tool calls) to stdout.
- `parse` — validates model output shape; emits `tool/call` events; if the
  model response was truncated (`stop_reason == "length"`), it **does not**
  execute tool calls; it emits tool results with `is_error: true` telling the
  model to re-issue the call (borrowed from pi).
- `route` — resolves tool manifests and invokes `tool-*` subprocesses; emits
  `tool/result` events.
- `log` — validates and appends events atomically.

### 2.2 Event types used by the loop

Reuse the versioned vocabulary from `docs/tui.md`, with these additions:

```jsonl
{"v":1,"type":"user_message","ts":"...","content":"rename the file"}
{"v":1,"type":"assistant_message","ts":"...","content":"I'll use the edit tool.","tool_calls":[{"id":"call-1","name":"edit","arguments":{"file_path":"a.txt","old_string":"x","new_string":"y"}}],"stop_reason":"stop"}
{"v":1,"type":"tool_call","ts":"...","id":"call-1","name":"edit","arguments":{"file_path":"a.txt","old_string":"x","new_string":"y"}}
{"v":1,"type":"tool_result","ts":"...","id":"call-1","value":{"text":"The file a.txt has been updated.","path":"a.txt"},"is_error":false}
{"v":1,"type":"error","ts":"...","message":"model call failed"}
```

Rules:

- `assistant_message` carries both the text and the tool calls the model made.
- `tool_call` is the execution order, derived by `parse` from the
  `assistant_message`; `tool_result` pairs by `id`.
- `assemble` projects `tool_result.value.text` into
  `function_call_output.output`; if `text` is absent, it uses a compact JSON
  rendering of `value`.

### 2.3 Step/turn algorithm

```
step:
  claim:   read log, decide whether a step is owed
           (a user_message or tool_result exists that has not been answered)
  assemble: project log -> ModelRequest
  model:   stream -> assistant_message event (text + tool_calls + stop_reason)
  if stop_reason == "error"|"aborted":
           append error event; stop
  if stop_reason == "length":
           append assistant_message; for each truncated tool call append
           tool_result is_error=true "arguments may be truncated, re-issue"
           stop (human/agent decides to continue)
  if no tool_calls:
           append assistant_message; turn ends
  else:
           append assistant_message
           for each tool_call: append tool_call; run tool; append tool_result
           loop to claim (another step)
```

`turn.sh` runs one **step**; the loop repeats until no tool calls or
`max_steps` (default 20) is reached. `turn.sh` is idempotent in the sense of
G1: rerunning it after a complete step appends nothing.

## 3. Tool CLI contract (refined)

A tool reads one JSON object on stdin and writes one JSON object on stdout.
The stdout object MUST contain a `text` field for the model-facing message;
additional fields are canonical data kept in the log.

```json
{"text":"The file a.txt has been updated successfully.","path":"a.txt","before":"x","after":"y"}
```

On failure: nonzero exit; stderr is diagnostic; the harness creates
`tool_result` with `is_error: true` and `value.text` from stderr (capped).

## 4. Read tool (`read`)

Borrowed from pi (offset/limit pagination, 2000 lines/50KB caps) and
deepseek-harness (streaming window scan, per-line cap, exact line count).

### 4.1 Schema

```json
{
  "name": "read",
  "description": "Read a UTF-8 text file and return line-numbered content. Paginate with offset/limit.",
  "parameters": {
    "type": "object",
    "properties": {
      "file_path": { "type": "string", "description": "Path to read (relative or absolute)." },
      "offset":     { "type": "integer", "description": "1-based first line to return. Defaults to 1." },
      "limit":      { "type": "integer", "description": "Maximum lines to return. Default 2000, capped at 2000." }
    },
    "required": ["file_path"]
  }
}
```

### 4.2 Caps

| Cap | Default | Meaning |
|---|---|---|
| `read_limit` | 2000 | default and max lines returned per call |
| `read_max_line_length` | 2000 | chars kept per line; overflow gets `... (line truncated to 2000 chars)` |
| `read_max_bytes` | 51200 | byte cap on the selected window; overflow stops the window |
| `read_stream_min_size` | 10485760 | files at or above this size are streamed chunk-wise |

### 4.3 Algorithm (from deepseek-harness `buildWindow`)

```
open file; if size >= stream_min_size: stream chunks else read whole
scan chunks line by line:
  count every line (exact totalLines)
  keep a line buffer capped at max_line_length + 1 chars
  collect only lines in [offset, offset+limit)
  stop collecting once collected bytes > max_bytes
    (but keep counting lines until EOF for the footer)
on EOF:
  if offset > totalLines: error (unless empty file with offset=1)
  render numbered lines + footer
```

Never hold a huge file in memory, and never return partial lines. The line
buffer cap means a single 1GB no-newline line still costs only ~2001 chars of
memory.

### 4.4 Model-facing output

Borrow the dsh format (line numbers, explicit continuation footer):

```text
<path>a.txt</path>
<type>file</type>
<content>
1: use std::io;
2:
3: fn main() {
...
2000: }
(Showing lines 1-2000 of 4123. Use offset=2001 to continue.)
</content>
```

If the byte cap hits before the line cap, the footer is:

```text
(Output capped. Showing lines 1-1480. Use offset=1481 to continue.)
```

If a file is empty: `(End of file - total 0 lines)`. If `offset` is past EOF:
error to stderr, exit nonzero, `value.text` explains the file has N lines.

Why this format: line numbers make edit targets unambiguous; the footer makes
pagination an explicit next action for the model; the path/type envelope makes
results greppable for the TUI later.

## 5. Write tool (`write`)

Borrowed from pi (full overwrite, creates parents) with dsh-style snake_case
args and a before/after canonical value.

### 5.1 Schema

```json
{
  "name": "write",
  "description": "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Creates parent directories.",
  "parameters": {
    "type": "object",
    "properties": {
      "file_path": { "type": "string" },
      "content": { "type": "string" }
    },
    "required": ["file_path", "content"]
  }
}
```

### 5.2 Behavior

- create parent directories if missing
- full overwrite
- reject content larger than `max_write_bytes` (default 1 MiB) to avoid
  accidental huge writes
- stdout:

```json
{"text":"Successfully wrote 4123 bytes to src/main.rs.","path":"src/main.rs","operation":"create","bytes":4123}
```

`operation` is `"create"` or `"update"` based on prior existence.

Phase 1 policy: unconditional write. The dsh read-before-write gate is deferred
until a real policy need appears (per the refinement policy).

## 6. Edit tool (`edit`)

Borrowed from deepseek-harness (`old_string`/`new_string`/`replace_all`,
unique literal match) with pi's multi-edit noted as a future extension.

### 6.1 Schema

```json
{
  "name": "edit",
  "description": "Edit an existing UTF-8 text file by replacing literal text. old_string must match exactly once unless replace_all is true.",
  "parameters": {
    "type": "object",
    "properties": {
      "file_path":   { "type": "string" },
      "old_string":  { "type": "string", "description": "Literal text to replace. Must match exactly." },
      "new_string":  { "type": "string", "description": "Literal replacement text. Empty string deletes the match." },
      "replace_all": { "type": "boolean", "description": "Replace all matches. Default false." }
    },
    "required": ["file_path", "old_string", "new_string"]
  }
}
```

### 6.2 Behavior

- `old_string` must be non-empty; `old_string == new_string` is rejected as a
  guaranteed no-op (dsh rule).
- `replace_all` defaults to false. When false, `old_string` must occur exactly
  once; zero or multiple matches are errors with precise messages:

```
Error: old_string not found in file. It may have changed since your last read.
Error: old_string appears 3 times. Use a larger old_string with more context, or set replace_all=true.
```

- Match against the **original file content** (read once), then apply. Preserve
  line endings and BOM (pi rule). CRLF stays CRLF; LF stays LF.
- stdout on success:

```json
{"text":"The file src/main.rs has been updated successfully.","path":"src/main.rs","before":"old","after":"new","replace_all":false}
```

### 6.3 Why literal unique-match first

It is the Claude-Code/ACP convention, it is stateless, and it makes "the file
changed under you" a local, explainable error. Pi's multi-edit array is more
round-trip-efficient, and is the first upgrade once a real episode shows a task
needing multiple edits per call.

## 7. System prompt guidance

Borrowed from dsh; fixed text so the prompt prefix stays cache-stable:

```markdown
Use the read tool — not shell commands like cat — to inspect text files.
Results include line numbers. Use offset and limit to continue reading large files.

Use the write tool to create files or completely replace file contents.

Use the edit tool for targeted changes to existing UTF-8 text files.
It replaces literal old_string with new_string; by default old_string must
appear exactly once. If it appears multiple times, provide a more specific
old_string or set replace_all to true. Read the file first unless you just
created or edited it in this session.
```

## 8. Token-efficiency checklist

1. **Read never uploads a whole file.** Caps: 2000 lines / 50KB / 2000
   chars-per-line; explicit `offset=` continuation footer.
2. **Tool results are bounded at the source.** Every tool caps its own output
   (read, bash later). `assemble` adds a safety cap of
   `tool_result_max_chars` (default 20000) on any tool result text, with
   `[tool result clipped: N -> M chars]`; the full value stays in the log.
3. **Context budget fails loud.** `assemble` computes an estimated char count;
   if it exceeds `context_budget_chars` (default 180000) and no compaction is
   implemented yet, it emits an `error` event telling the user to start a new
   session or reduce scope. It never silently drops history. (Compaction is the
   next milestone after real sessions exist.)
4. **Prefix stability.** System prompt, tool schemas, and past messages are
   byte-identical across requests within a session. No timestamps/PIDs/temp
   paths in the prefix.
5. **Model tool schemas are minimal.** No giant descriptions. Every schema
   field description earns its token cost by teaching pagination or match
   semantics.
6. **Truncated model output is never executed.** `stop_reason == "length"`
   fails all tool calls with `is_error` results instead of running borked
   arguments (pi rule).
7. **Snake_case arguments.** Matches Claude Code/ACP conventions; saves tokens
   over long names and reduces model confusion.

## 9. Conformance tests (extend G4)

`tool-conformance` runs each tool with fixed inputs:

- `read`: small file, offset/limit pagination, empty file, offset past EOF,
  long-line file (line longer than `read_max_line_length`), 60KB file (byte cap
  footer), binary file (clear error).
- `write`: create new file, overwrite existing file, create parent dirs,
  content too large.
- `edit`: unique match success, no match error, multiple matches error,
  `replace_all` success, empty `old_string` error, CRLF preservation.

Each test checks: stdout is one JSON object with a `text` field, stderr is
ignored on success, exit code is 0/1.

## 10. Deferred (do not build until triggered)

- images in `read`
- multi-edit array in `edit`
- read-before-write policy gates
- compaction / summarization
- parallel tool execution
- sandboxing beyond process boundary + cwd
- `list`/`glob`/`grep` tools — the first real episode that needs them is the
  trigger
