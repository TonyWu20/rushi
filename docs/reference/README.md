# Rushi Harness Reference

Stable reference for the rushi kernel: architecture, interfaces, configuration,
and extension points. Use this doc when asked how rushi works, how to configure
it, or how to extend it.

---

## 1. Architecture Overview

Rushi is a Rust + Unix agent harness. The design follows Unix philosophy:
each capability is a one-shot CLI stage; the ABI is bytes over pipes (JSON on
stdin/stdout); the harness never imports tool or hook code.

### Layers

| Layer | What | Where |
|-------|------|-------|
| Kernel (Tier 1) | Loop core, base tools, extension host, distribution | this repo |
| Front-end (Tier 2) | TUI + UI extensions (swappable) | `rushi-tui` repo |
| Applications | Project-specific tools, hooks, extensions | per-project `tools/`, `hooks/`, `ui_extensions/` |

### Core principle

- **The session log is the source of truth.** All state is a projection of an
  append-only JSONL event log. Model-visible means logged.
- **Tools stay processes by default.** The harness spawns and supervises; it
  does not import tool code.
- **Hexagonal / ports-and-adapters.** The core defines traits; adapters
  implement them. The composition root is the only place that knows what is
  wired.

---

## 2. The Loop (Stage Pipeline)

One turn of the agent loop:

```
rushi run <session>
  ┌─────────────────────────────────────────────────────────┐
  │  claim → assemble → model → parse → route → log        │
  └─────────────────────────────────────────────────────────┘
```

| Stage | Binary | Responsibility |
|-------|--------|----------------|
| `claim` | `bin/claim` | Acquire session lock, select next user message (or idle claim). |
| `assemble` | `bin/assemble` | Project the session log into a `ModelRequest` JSON (system prompt + tool schemas + input events). |
| `model` | `bin/model` | Call the LLM (Responses API or Chat Completions fallback). Stream the response. |
| `parse` | `bin/parse` | Parse the model response into structured tool-call actions. Detect length-stop, empty turns. |
| `route` | `bin/route` | Supervise and execute tool calls in parallel. Apply timeout, output cap, caps. |
| `log` | `bin/log` | Append events to `events.jsonl` (the append-only session log). |
| `compact` | `bin/compact` | Run a summary call, write handoff document, record a `compaction_summary` boundary event. |
| `user` | `bin/user` | Human I/O: read a user message from stdin or file, emit a `user_message` event. |

The loop binary (`bin/rushi`) orchestrates these stages in sequence, handles
cancellation signals, and manages the session lock (`loop.pid`).

### Loop lifecycle

- **Normal turn:** claim → assemble → model → parse → route → log → repeat.
- **Idle claim:** no pending user messages; the loop stops (unless a
  `run.idle` hook returns `continue`).
- **Overflow:** when the assembled context exceeds the input budget, the
  `overflow.resolve` hook fires. Default: in-session shadow compact.
- **Context exhausted:** when even the compacted context exceeds budget,
  the `exhausted.handle` hook fires. Default: one more shadow compact.
- **Approval:** a `tool.before` hook can return `approve`, which inserts an
  `approval_request` event and waits for a human `approval` event.

---

## 3. Session Log (Event Schema)

The session log is `sessions/<name>/events.jsonl`, append-only JSONL.
Each line is one event object. Schema: `schemas/events/v1/*.json`.

### Event types

| `type` | Payload highlights |
|--------|-------------------|
| `user_message` | `content`, optional `queue` (`"follow"` for injected messages) |
| `assistant_message` | `content`, `tool_calls[]`, `stop_reason`, optional `reasoning[]`, `usage` |
| `tool_call` | `id`, `name`, `arguments` (JSON object) |
| `tool_result` | `tool_call_id`, `content`, `is_error` |
| `compaction_summary` | `version`, `parent_version`, `diverge_seq`, `summary_text`, `boundary` |
| `compaction_started` | `reason` (`threshold` \| `overflow` \| `last_resort`) |
| `compaction_failed` | `reason`, `error` |
| `context_exhausted` | `input_budget`, `context_tokens` |
| `approval_request` | `id`, `tool_call_id`, `prompt` |
| `approval` | `id`, `decision` (`allow`/`deny`), optional `arguments` |
| `rewind` | `target_seq`, `reason` |
| `ext_status` | `id`, `value` (markers: `hook.<window>`, `hook_applied`, etc.) |
| `error` | `stage`, `message` |

All events share: `v` (schema version, currently `1`), `ts` (ISO-8601
timestamp), `seq` (monotonically increasing sequence number assigned by
the log stage).

### Rewind / Fork

A `rewind` event records a branch point. The active-path computation
(`crates/rushi/src/rewind.rs`) masks events from abandoned branches.
The Lean backstop (`lean/RewindSpec.lean`) and DRT
(`verification/rewind-drt/`) verify the active-path recursion.

---

## 4. Configuration (`config.toml`)

The loop binary reads a TOML config. Resolution order for the config path:

1. `$CONFIG` environment variable
2. `--config` CLI flag
3. `<exe_dir>/../config.toml` (Nix side-by-side layout)
4. `./config.toml` in CWD (dev checkout)

### Keys

```toml
# --- [active] ---
# model = "deepseek"              # which model config to use

# --- [model] ---
# api = "responses"               # "responses" | "chat_completions"
# max_output_tokens = 32768
# reasoning_effort = "xhigh"      # "low" | "medium" | "high" | "xhigh"
# model_timeout_s = 3600

# Per-model overrides:
# [model.my-model]
# model_id = "my-model"
# base_url = "http://127.0.0.1:8080"
# api_key_env = "MODEL_API_KEY"
# context_tokens = 131072
# timeout_s = 3600
# reasoning_effort = "medium"
# vision = true                   # send image tool results to the model

# --- [paths] ---
# sessions_root = "sessions"
# tools_root = "tools"
# extra_tools_roots = []         # additional dirs for extension tool manifests

# --- [limits] ---
# read_limit = 2000
# read_max_line_length = 2000
# read_max_bytes = 51200
# read_stream_min_size = 10485760
# write_max_bytes = 1048576
# tool_result_max_chars = 20000
# bash_max_output_bytes = 16000
# bash_timeout_default = 60
# bash_timeout_max = 300
# compact_enabled = true
# compact_reserve_tokens = 16384
# compact_keep_tokens = 20000
# compact_strategy = "compact"
# context_budget_tokens = 262144   # default: context_tokens - max_output_tokens
# approval_timeout_s = 300
# compact_reasoning_effort = "low"

# --- [hooks] ---
# timeout_ms = 30000

# [[hooks.on]]
# window  = "exhausted.handle"
# command = "harness-hook-compact"

# --- [loop] ---
# command = "rushi"
# args = ["run"]
# arg_style = "append_session"

# --- [system_prompt] ---
# text = "You are an expert coding assistant..."

# --- [tui] ---
# binary = "target/release/tui"
# color = "truecolor"
# color_scheme = "catppuccin macchiato"
# [tui.tool_display]
# preset = "opencode"
# preview_lines = 8
```

---

## 5. Tool Interface

### 5.1 Tool Manifest (`tool.toml`)

Each tool lives in a directory under a tools root (default `tools/`).
The tool name is the directory name. The manifest is `tool.toml`:

```toml
# tools/my_tool/tool.toml
[tool]
description = "One-line description shown to the model"
command = "my-tool-binary"        # resolved on the agent-visible PATH
args = []                         # extra args appended after stdin JSON
timeout_ms = 30000

[tool.schema]
# JSON Schema (draft 2020-12) for the tool's input arguments.
# This is what the model sees in the tool definition.
type = "object"
required = ["some_field"]
properties = {
    some_field = { type = "string", description = "..." }
}
```

### 5.2 Tool Execution Contract

- **stdin:** one JSON object (the model's `arguments` for that tool call).
- **stdout:** one JSON object (the canonical result). Non-JSON output is
  wrapped as `{"text": "..."}`.
- **stderr:** human-readable diagnostics. On non-zero exit, stderr is
  forwarded to the model as the error message.
- **Exit code:** `0` success, non-zero failure.
- **Streaming (planned):** JSONL on stdout with a terminal
  `{"type":"result", ...}` line.

### 5.3 Base Tools (shipped with the kernel)

| Tool | Directory | Notes |
|------|-----------|-------|
| `read` | `tools/read/` | Read a file. Supports `offset`/`limit`. Image files returned as image content when model has `vision=true`. |
| `write` | `tools/write/` | Write content to a file. |
| `edit` | `tools/edit/` | String replacement in a file. `replace_all` flag. |
| `list` | `tools/list/` | List directory entries. |
| `bash` | `tools/bash/` | Run a shell command. Timeout default 60 s, max 300 s. Output capped. |

### 5.4 Adding a New Tool

1. Create a directory `tools/my_tool/`.
2. Place an executable binary or script named `my_tool` (or set
   `command` in the manifest to point elsewhere).
3. Write `tools/my_tool/tool.toml` with the manifest above.
4. The binary reads one JSON object from stdin, writes one JSON object to
   stdout.
5. Add the tool name to `rushi.toml` → `[tools] enabled = [...]` and
   re-run `rushi setup` (or just have the directory present in
   `tools_root`; discovery is by scan).
6. `assemble` picks up the manifest on the next build. The tool appears
   in the generated tool list in the system prompt and in the model's
   tool schemas.

No registry. No install step. Being on the path makes it runnable.

---

## 6. Hook ABI (Lifecycle Windows)

A hook is a short-lived command on a path. The harness spawns it at a
fixed lifecycle window. The ABI is bytes over pipes.

### 6.1 Registration

```toml
[hooks]
timeout_ms = 30000

[[hooks.on]]
window  = "overflow.resolve"
command = "my-overflow-hook"
args    = []

[[hooks.on]]
window  = "tool.before"
command = "my-approval-hook"
```

- The `on` list is ordered. Hooks run in order.
- One hook binds to exactly one window.
- No matcher DSL; filtering is the hook's own job from the JSON on stdin.
- Growth path: `hooks/<window>/<name>/hook.toml` directory layout (not yet
  built; config registry is the current mechanism).

### 6.2 Invocation

- **stdin:** one JSON object with the window's event fields plus a
  `window` field and `session` name.
- **env:** `SESSION`, `SESSIONS_ROOT`, `CONFIG`, `HARNESS_PHASE`, window name.
- The harness writes one JSON object to stdin, reads one JSON object from
  stdout, then checks the exit code.

### 6.3 Decision Contract

| Exit | stdout | Meaning |
|------|--------|---------|
| `0` | empty or `{}` | No decision. Window default applies. |
| `0` | `{"decision":"<name>","payload":{...}}` | Apply the named decision. |
| `2` | (ignored) | Blocking default (see per-window table). |
| other non-zero | (ignored) | Non-blocking failure. Log `ext_status` marker. Window default applies. |
| timeout | (killed) | Same as non-blocking failure. |

### 6.4 Windows and Decision Vocab

| Window | Scope | Decisions | Default |
|--------|-------|-----------|---------|
| `session.start` | per `rushi run` | *(observation only)* | — |
| `session.end` | per `rushi run` | *(observation only)* | — |
| `step.start` | per step | *(observation only)* | — |
| `step.end` | per step | *(observation only)* | — |
| `model.before` | per model call | `transform` | proceed unchanged |
| `model.after` | per model call | *(observation only)* | — |
| `compact.before` | before compact | `proceed`, `cancel`, `replace` | proceed |
| `compact.after` | after compact | *(observation only)* | — |
| `overflow.resolve` | on overflow | `stay_compact`, `stop` | `stay_compact` |
| `exhausted.handle` | on context exhaustion | `stay_compact`, `stop` | `stay_compact` |
| `tool.before` | per tool batch | `proceed`, `block`, `approve` | `proceed` |
| `tool.after` | per tool batch | *(observation only)* | — |
| `run.idle` | on idle claim | `stop`, `continue` | `stop` |

### 6.5 Writing a Hook

1. Write a binary/script that reads one JSON object from stdin.
2. Write one JSON object to stdout (or nothing for no-decision).
3. Exit `0` (normal), `2` (blocking), or `1` (error).
4. Register it in `config.toml` under `[[hooks.on]]`.
5. Support `--help` to self-document the window, input shape, and
   decision vocab.

The built-in `bin/hook-compact` is a reference implementation.

---

## 7. Prompt Fragments & Extensions

The system prompt is built by `assemble` in this order:

```
instructions = base_prompt + generated_tool_list + cwd_line + join(fragments)
```

- `base_prompt`: from `[system_prompt] text` in config.
- `generated_tool_list`: generated from discovered tool manifests.
  Byte-stable per session.
- `cwd_line`: the session working directory.
- `fragments`: one named entry per extension. The kernel joins them;
  it never inspects the text.

Fragments are passed to `assemble` as a JSON array of `[id, text]` pairs
via `--fragments`. An extension (e.g., goal mode) owns one key
(`"goal"`). Adding or removing a fragment invalidates the prompt prefix
cache once.

### Extension Model

- **Tools:** add a directory to `tools/` or an `extra_tools_roots` path.
- **Hooks:** register in `config.toml` or (future) `hooks/` directory.
- **Prompt fragments:** an extension hook at `model.before` can insert or
  replace a fragment. The kernel only joins; it does not author content.
- **UI extensions:** `ui_extensions/` + `ext-rs/` in the TUI repo.
  The kernel does not depend on them.

---

## 8. Distribution

### 8.1 `rushi.toml` (per-project manifest)

```toml
[rushi]
version = "0.1"

[tools]
enabled = ["read", "write", "edit", "bash"]

[ui_extensions]
enabled = ["statusline-rs", "mermaid"]

[loop]
compact_reserve_tokens = 16384
```

### 8.2 `rushi setup`

- Reads `rushi.toml` from CWD.
- Materializes selected kernel tools into `./tools/`.
- Writes `rushi.lock` (pins kernel commit + external source commits).
- Writes `.envrc` (PATH wiring via direnv).
- Generates `config.toml` from kernel defaults merged with project
  overrides. Does not overwrite an existing `config.toml`.
- `--locked` verifies against an existing `rushi.lock` instead of
  regenerating.

### 8.3 Install Paths

| Method | How | Docs location |
|--------|-----|---------------|
| Nix (primary) | `flake.nix` → store path | Docs ship in the store; binary resolves side-by-side |
| Plain | `cargo build --release` + `install.sh` → `$PREFIX/bin` | Docs copy to `$PREFIX/share/rushi/docs/` |

Both produce the same binary. The `rushi.lock` pins source revisions,
not build artifacts.

---

## 9. How to Extend Rushi (Quick Recipes)

### Add a tool

```bash
mkdir -p tools/my_tool
# write your binary, then:
cat > tools/my_tool/tool.toml <<'EOF'
[tool]
description = "Do X"
command = "my_tool"
args = []
timeout_ms = 30000

[tool.schema]
type = "object"
required = ["input"]
properties = { input = { type = "string" } }
EOF
chmod +x tools/my_tool/my_tool
```

Add `"my_tool"` to `[tools] enabled` in `rushi.toml`. Done.

### Add a hook

```bash
# 1. Write your hook binary/script, ensure --help works.
# 2. Register in config.toml:
[[hooks.on]]
window  = "tool.before"
command = "my-guard-hook"
```

### Add a prompt fragment

- In an extension hook at `model.before`, read the current fragment map
  from the `ext_status` / request JSON, insert or replace your key,
  write the updated map back. The kernel joins it in.

### Add a UI extension

- Lives in `rushi-tui` repo. Register in `rushi.toml`
  `[ui_extensions] enabled`. The TUI discovers it on the extension host.
  The kernel does not know about it.

---

## 10. Self-Documentation & This Doc

- Every tool and hook binary supports `--help`.
- This reference doc is embedded in the `rushi` binary via `include_str!`.
  Run `rushi docs` to print it, or `rushi docs <section>` to print one
  section (matched by `## N. Title` heading).
- The system prompt carries a one-line pointer:
  *"Harness reference: run `rushi docs` for the full rushi reference."*
- The agent reads this doc on demand via the `bash` tool when the user
  asks how rushi works, how to configure it, or how to extend it.

---

## 11. Sibling Repos

| Repo | Contents |
|------|----------|
| `rushi-tui` | TUI binary, UI extensions, TUI docs |
| `rushi-exts` | Goal tools, goal hooks, `lean-verify` |
| this repo | Kernel: loop, base tools, extension host, hooks, distribution |

The kernel has no build-time dependency on the siblings.
`config-exts.example.toml` shows how to wire exts for development.
