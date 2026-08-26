# Rust Unix Harness — Architecture & Roadmap

## 1. Vision

A Rust-based agent harness that follows the Unix philosophy:

1. **Each tool is a CLI program** — spawn argv, pipe `stdin`, read `stdout`/`stderr`, check exit code.
2. **Text / structured output is the universal interface** — JSON on stdout as the default contract, plain text as fallback. JSONL/NDJSON streaming is planned, not yet part of the contract.
3. **The harness is extended by writing scripts or programs in any language** — the ABI is bytes over pipes; the harness never imports tool code.

The deeper conclusion from comparing this model with `deepseek-harness`/Cordis:
**the capabilities of a plugin model (composable loop, typed events, lifecycle management, session log as source of truth) are architectural properties, not in-process privileges.** They can be achieved in the Unix model by building a small, deterministic, event-sourced Rust core with process plugins. LLM latency dominates agent cycles, so process boundaries are not the bottleneck. The real cost is protocol design, not syscall overhead.

## 2. Principles

- **The core owns *what*; adapters own *how*.** Core = events, state, contracts, rules. Adapters = process, in-process, wasm, bash, Python.
- **Hexagonal architecture / dependency inversion.** Core depends only on traits. Adapters implement core traits. The composition root is the only place that knows what is wired.
- **The session log is the source of truth.** All state is a projection of an append-only JSONL event log. *Model-visible means logged.*
- **Experiment in bash first, stabilize as a contract, then promote behind a trait.**
- **Tools stay processes by default.** Compile into the binary only what is stable, hot, and safe.

## 3. Tool contract

### 3.1 One-shot tools

- **stdin**: one JSON object per invocation.
- **stdout**: one JSON object — the canonical result.
- **stderr**: human-readable diagnostics, never structurally parsed. On non-zero exit, the harness forwards stderr as the error message.
- **exit code**: `0` success, non-zero failure.
- **non-JSON stdout**: wrapped as `{"text": "..."}`.

### 3.2 Streaming tools (planned)

Use JSONL/NDJSON on stdout: one event per line, with a terminal `{"type":"result", ...}` line. Not yet implemented.

### 3.3 Tool manifest (TOML)

```toml
# tools/fetch_url/tool.toml
[tool]
description = "Fetch a URL and return its text content"
command = "python3"
args = ["main.py"]
timeout_ms = 30_000

[tool.schema]
# JSON Schema exposed to the model
type = "object"
properties = { url = { type = "string" } }
required = ["url"]
```

The tool name is the directory name. The manifest carries no `name` field.

The future core selects the adapter. It only sees the `ToolExecutor` trait; it never knows whether `execute` spawns a process or calls an in-process function.

## 4. Architecture

### 4.1 Hexagonal mapping

```
        ┌───────────────────────────────────────────────┐
        │              CORE (pure Rust, no I/O)          │
        │                                               │
        │  Event log (append-only)                       │
        │  Reducer / state machine (deterministic)       │
        │  Agent loop state machine                      │
        │  Tool registry                                 │
        │  Policy pipeline (pre → execute → post → result)│
        │                                               │
        │  Ports (traits):                               │
        │    ToolExecutor                                 │
        │    ModelClient                                  │
        │    EventLog                                     │
        │    PolicySource                                 │
        │    Clock                                        │
        └────────▲──────────▲──────────▲─────────────────┘
                 │          │          │
        ┌────────┴───┐ ┌────┴────┐ ┌───┴─────────┐
        │ Adapters   │ │ Adapters│ │ Adapters    │
        │            │ │         │ │             │
        │ BashTool   │ │ RustTool│ │ WasmTool    │
        │ PythonTool │ │ (compiled│ │             │
        │ McpTool    │ │  in)    │ │             │
        │ ModelApi   │ │         │ │             │
        └────────────┘ └─────────┘ └─────────────┘
```

- **Core** is a library crate with no `tokio::process`, no `Command`, no HTTP. Dependencies: `serde`, `serde_json`, `thiserror`, maybe `tracing`.
- **Adapters** are separate crates/binaries implementing core traits.
- **Composition root** (the `harness` binary) is the only place that wires adapters.

### 4.2 Cargo workspace layout (target)

```
harness/
├── Cargo.toml                # workspace
├── crates/
│   ├── core/                 # domain + ports only
│   │   └── src/
│   │       ├── event.rs      # SessionEvent, ToolCall, ToolResult, ...
│   │       ├── reducer.rs    # pure fn reduce(state, event) -> state
│   │       ├── loop.rs       # the loop state machine
│   │       └── ports.rs      # traits: ToolExecutor, ModelClient, EventLog, ...
│   ├── process-adapter/      # SubprocessTool: spawn CLI, pipe JSON
│   ├── inproc-adapter/       # RustTool / WasmTool: compiled-in implementations
│   └── harness/              # binary: config, wiring, composition root
├── tools/
│   ├── fetch_url/
│   │   ├── tool.toml
│   │   └── main.py
│   └── grep_project/
│       ├── tool.toml
│       └── main.sh
└── experiments/              # throwaway scripts; promotion happens here
```

### 4.3 Core ports (sketch)

```rust
use async_trait::async_trait;
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,        // validated against schema before dispatch
    pub parent: Option<String>,  // for nested calls
}

#[derive(Debug, Clone)]
pub struct ToolOutcome {
    pub call_id: String,
    pub value: Option<Value>,   // canonical JSON value
    pub is_error: bool,
    pub rendered: String,       // model-facing text
}

#[async_trait]
pub trait ToolExecutor: Send + Sync {
    fn schema(&self) -> ToolSchema;
    async fn execute(&self, call: ToolCall) -> Result<ToolOutcome, ToolError>;
}

#[async_trait]
pub trait ModelClient: Send + Sync {
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError>;
}

pub trait EventLog: Send + Sync {
    fn append(&self, event: Event) -> Result<u64, LogError>;
    fn replay(&self, from: u64) -> Box<dyn Iterator<Item = Event> + '_>;
}
```

### 4.4 The port that makes promotion possible

Both adapters implement the same trait; the manifest decides which is live:

```rust
// Subprocess adapter — the default Unix implementation
#[async_trait]
impl ToolExecutor for SubprocessTool {
    fn schema(&self) -> ToolSchema { self.manifest.schema.clone() }
    async fn execute(&self, call: ToolCall) -> Result<ToolOutcome, ToolError> {
        // spawn command, write JSON to stdin, close it,
        // wait with timeout, map stdout/stderr/exit code to ToolOutcome
    }
}

// In-process adapter — promoted tools later
#[async_trait]
impl ToolExecutor for RustTool {
    fn schema(&self) -> ToolSchema { self.schema.clone() }
    async fn execute(&self, call: ToolCall) -> Result<ToolOutcome, ToolError> {
        (self.f)(call).await
    }
}
```

The tool registry depends on `ToolExecutor`; `SubprocessTool` and `RustTool` depend on the trait. The loop knows only the trait.

## 5. Phase 0/1 — separate binaries glued by bash

> **Illustrative, not normative.** The binary names and shell snippets in this section are the Phase 1 implementation. The TUI does not depend on them; it depends on the `SessionPort`, the versioned event vocabulary, and an opaque loop command (see `docs/tui.md`).

Before building the full workspace: break the core into **separate one-shot binaries**, each `stdin → stdout`, JSON in → JSON out, glued by bash pipes. The pipeline *is* the architecture; the JSON shapes discovered here become the future core types.

```
┌────────┐  ┌─────────┐  ┌────────┐  ┌────────┐  ┌────────┐  ┌────────┐
│ claim  │→│ assemble│→│ model  │→│ parse  │→│ route  │→│  log   │
│ input  │  │ prompt  │  │ call   │  │ output │  │ tool   │  │ event  │
└────────┘  └─────────┘  └────────┘  └────────┘  └────────┘  └────────┘
 stdin/stdout JSON, stderr for diagnostics, exit code for pass/fail
```

| Binary | Responsibility |
|---|---|
| `claim` | Read session state, decide whose turn / what is owed |
| `assemble` | Prompt sections + tool schemas → one model request JSON |
| `model` | Call the LLM API, emit `assistant/message` and tool-call events |
| `parse` | Extract tool calls from the model output, validate JSON |
| `route` | Look up tool manifest, build tool call input |
| `tool-*` | The actual tools (Rust, bash, Python — irrelevant to the harness) |
| `log` | Append events to the session log, assign sequence numbers |

A one-step turn:

```bash
#!/usr/bin/env bash
set -euo pipefail

claim --session s1 \
| assemble --profile default \
| model --provider anthropic \
| parse \
| route --tools tools/ \
| log --session s1
```

The cyclic loop lives in bash:

```bash
#!/usr/bin/env bash
set -euo pipefail

session="$1"
while true; do
  if step.sh "$session" | grep -q '"stop"'; then
    break
  fi
done
```

### 5.1 State lives in files, not bash variables

```
sessions/
  s1/
    events.jsonl              # append-only source of truth
    state.json                # derived, rebuilt from events.jsonl by `claim`
    tools.lock                # registry lock if needed
    pending/
      approval.json           # transient: the loop's outbox to the human
    .turn.lock                # only one loop running per session
```

`claim` and `assemble` are pure projections of the log. The log is the database; the binaries are views over it.

### 5.2 Event contract (minimal)

```jsonl
{"event":"user/message","ts":"...","content":[{"type":"text","text":"rename the file"}]}
{"event":"assistant/chunk","ts":"...","text":"I'll"}
{"event":"assistant/message","ts":"...","content":"I'll use the bash tool."}
{"event":"tool/call","ts":"...","id":"call-1","name":"bash","arguments":{"command":"mv a b"}}
{"event":"tool/result","ts":"...","id":"call-1","value":{"exit":0},"isError":false}
{"event":"user/approval","ts":"...","callId":"call-1","decision":"allow"}
{"event":"user/cancel","ts":"...","target":"turn"}
```

Rules:

- one event per line, no embedded newlines
- append with `O_APPEND` (single `write`)
- for phase 1, two writers (TUI + current step pipeline) is fine; add `flock` or a tiny log daemon when strict multi-writer sequence numbers are needed

### 5.3 Where bash glue hurts

- **Branching and retry logic** gets ugly fast — promote the glue to a Rust binary when retries/backoff/traps dominate.
- **Parallel tool calls** — serial loop first, then `xargs -P` or a small Rust fan-out.
- **Cancellation** — bash can only kill the process group; a later Rust scheduler with cancellation handles is needed.
- **Error semantics** — use `set -o pipefail`; pick one convention early: JSON error on stdout *or* exit code + stderr, not both.

## 6. Roadmap

### Phase 1 — Bash + separate binaries
- Harness is `step.sh` / `turn.sh` + `claim`, `assemble`, `model`, `parse`, `route`, `log`.
- No shared Rust crate. Share only JSON contracts.
- Tool manifests as TOML; tools are arbitrary scripts/programs.
- TUI is the first stateful Rust binary (see `docs/tui.md`).

### Phase 2 — Promote the glue
- The bash orchestration becomes a small Rust binary (`harness`) that still shells out to the same stage binaries.
- One authoritative process owns looping, retries, fan-out, session lock, cancellation.

### Phase 3 — Extract the core
- The types and state transitions repeated across binaries become `crates/core` with ports/traits.
- The binaries remain as adapters (subprocess) or become in-process trait impls.

### Phase 4 — Compile in what is stable
- Hot/stable stages become in-process trait impls.
- Isolation-sensitive stages stay subprocesses.
- Optionally add `wasmtime` as the middle point between subprocess and compiled-in Rust (hot loading + isolation).

## 7. Guardrails

- **Core must stay I/O-free.** If `cargo tree -p core` shows `reqwest` or process spawning, a boundary leaked.
- **No shared crate in Phase 1.** If two binaries need the same type, that is evidence for a future core type — write it down, resist importing it until Phase 3.
- **Compile into the binary only stable, hot, safe things.** Keep process-based by default: anything model-authored, anything network-facing, anything parsing untrusted input, anything changed frequently.
- **TUI contains no decision logic.** It renders and appends; it never validates tools, assembles prompts, or decides policy.

## 8. Practical crate list

- `serde` / `serde_json` — wire format
- `toml` or `figment` — manifests/config
- `tokio` — async process management, timeouts, concurrency
- `async_trait` — trait objects for ports
- `clap` — CLI binaries
- `tracing` / `tracing-subscriber` — structured diagnostics
- `ratatui` + `crossterm` — TUI (see `docs/tui.md`)
- `jsonrpsee` — if/when the core API becomes a socket interface for loop/policy plugins
- `wasmtime` — later, for in-process isolated plugins
- `duct` — alternative for simple blocking subprocess handling
