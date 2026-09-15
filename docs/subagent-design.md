# Subagent mode: a `spawn_agent` extension tool

Status: **draft, not built.** This is a design spec for an extension tool
and one kernel config change. It changes no kernel loop, no schema, and no
tool contract.

A **subagent** is a second `rushi run` session the main agent starts from
inside a step. The parent loop blocks for the child's lifetime, reads the
child's terminal `ext_status` on exit, and injects that result back into
the parent conversation. The child is a plain `rushi` session with its own
config, its own session dir, and a restricted tool set. The mechanism is
the same one the human operator uses (blocking `rushi run` with a
`$CONFIG` pointing at a curated config); the tool just automates it with a
generated config and a nested session dir.

The TUI and extension UIs are separate repos
(`github.com/TonyWu20/rushi-tui`, `rushi-exts`). The kernel does not depend on them at
build time. This doc is the kernel's contract for what a subagent must be
able to do and what it must not be able to do.

## 1. What "subagent" means here

A subagent is:

- a **child session** created under the parent's session dir
  (`sessions/<n>/sub-<uuid8>`), with its own `events.jsonl`,
  `tools.jsonl`, `meta.json`, and `.loop.lock`.
- a **restricted agent** at **depth 1**: the main agent calls the
  subagent, and that is the whole chain. The `spawn_agent` tool
  projects the parent config into a child config that keeps only
  the tool paths the parent chose for this child. The tool lives
  in the exts repo (decision 2026-09-16). The child cannot call
  `spawn_agent` again because it is an extension tool (P1) and
  the child's tool paths omit it. Depth is 1 by construction,
  not by a counter (D6).
- a **blocking call in v1**: the parent step does not advance until the
  child loop exits. No parallel fan-out. No shared memory. The only
  channel is the child's terminal `ext_status` value, read from the child
  log after the child process exits.

The tool is **not** a second agent in the same process. There is no
in-process "agent runtime." Everything is a subprocess the tool manages.
This keeps the kernel loop unchanged: the `spawn_agent` tool is just
another tool behind `route`, and the kernel knows nothing about it.

## 2. Why an extension and not the kernel

The kernel already exposes exactly the two things a subagent needs:

1. **Tool selection.** `config.toml` picks which tools the loop is
   exposed to. `[paths] native_tool_paths` is a list of native tool
   dirs, each a directory that directly contains a `tool.toml`
   manifest (e.g. `tools/bash`). `[paths] extension_tool_paths` is a
   parallel list for extension tool dirs, scanned after the native
   list so a native tool shadows an extension tool of the same name.
   `route` discovers tools from the two lists in that order and
   rejects any tool call whose name is not registered
   (`route: unknown tool 'X'`). This is the natural place to scope a
   child's capabilities: point its config at a path set that omits
   `spawn_agent`. The two lists share one path resolution rule (D6).
2. **The hook ABI.** A tool can run `on_start` / `on_end` hooks
   (exit-0 continuation / non-zero error, exit-code-2 blocking approval),
   can read the session dir from `$HARNESS_SESSION_DIR`, and can write
   structured results to stdout. This is how `spawn_agent` reports the
   child's outcome back to the parent.

Everything specific to a particular subagent (e.g. a bash-only child, a
read-only child, a child that may not write outside its session) is a
**matter of which tool roots the parent selected** and **which hooks the
parent registered**. The kernel stays general; the shaping is extension
work. The kernel's contribution is (a) a config generator that projects a
subset of the parent config into a child config, and (b) allowing the
child session dir to nest under the parent's.

## 3. Design decisions

The numbered items are the load-bearing decisions. Each names the
property it supports, so the Properties section below can be checked
against the code without re-deriving intent.

### D1. The tool is an extension, not kernel code

`spawn_agent` lives in a dedicated extension tool dir (for example
`tools-ext/spawn_agent/`), outside the native tool set. It is
discovered through `[paths] extension_tool_paths` (D6), registered in
the tool manifest, and conformed to by `scripts/tool-conformance.sh`.
No kernel crate gains a `spawn_agent` symbol, and the default
`native_tool_paths` never contains it. P1.

### D2. Session nesting: `sessions/<parent>/sub-<uuid8>`

The child session dir lives inside the parent's session dir. The parent
loop's `resolve_session` already passes through any path containing a
`/`, so `sessions/<n>/sub-<uuid8>` resolves without a kernel change. The
nesting gives the operator one `rm -rf sessions/<n>/` to clean up
parent plus all of its children, and it makes the parent-child
relationship visible on disk. P2.

The child gets its own `.loop.lock` via the existing `Acquire`
mechanism. No new locking is added.

### D3. The child config is generated, not hand-written

A new kernel module (`bin/rushi/src/config_gen.rs` or
`crates/rushi/src/config_gen.rs`, decided at implementation time)
projects a subset of the **parent** `HarnessConfig` into a child
`config.toml`:

- `[model]`: always inherited, unless the tool argument overrides it.
- `[paths]`: `native_tool_paths` (D6) is set to the subset of tool
  dirs the caller chose. They are install-relative paths so the
  generated config never carries a full Nix store path.
  `extension_tool_paths` is the caller's extension subset, or omitted
  when the child gets none. `sessions_root` is the child's session
  dir (`<parent_session_dir>/sub-<uuid8>`). `schemas_dir` is not
  needed (D8). `log` and `handoff_root` point inside the child dir.
- `[hooks]`: inherited from the parent, **plus** any tool-specific hooks
  the caller adds (e.g. a write-blocking hook).
- `[tui]`: **omitted** (D4). The child runs headless.

The generated file is written to
`<parent_session_dir>/sub-<uuid8>/config.toml`. The tool then invokes
`$HARNESS_BIN run --session-dir <child_dir> --config <child_config>
<task prompt>` and blocks. The parent controls the child's behavior
explicitly; no config is hand-written, and the tool's `inputSchema`
makes every choice the parent can make visible in one place.

### D4. The child is headless

The generated child config omits the `[tui]` section. A `rushi run`
process with no `[tui]` runs in the existing headless mode: it writes
events, calls tools, and exits when the agent stops. No TUI server, no
`$HARNESS_WS`. The parent reads the result from the child log, not from
a socket. This keeps the kernel free of any TUI dependency and means
the child works in a CI or bare-terminal context. P4.

### D5. Config generator stays in the kernel

The projection logic (pick a model, pick tool roots, inherit hooks,
drop `[tui]`, stamp the session dir) is a pure function over
`HarnessConfig` plus a small argument struct. It lives in the kernel
because:

- it is the single source of truth for "what a child config looks like,"
  which keeps subagent configs uniform and reviewable;
- a subagent is a kernel concept (a second session), not a TUI concept;
- the TUI never sees the child config, so putting the generator in the
  TUI would be an unnecessary build-time dependency.

The TUI's only job is to show the operator that a subagent is running
and to render the terminal `ext_status` when it finishes. It does not
generate or edit child configs. P3.

### D6. Tool paths: `native_tool_paths` and `extension_tool_paths`

Today `[paths] tools_root` is one directory and
`extra_tools_roots` is a list. Both are scanned one level deep.
Each sub-directory that carries a `tool.toml` is registered under
its directory name. That walk is blind to the tool dirs themselves.

`tools/bash` holds a `tool.toml` directly. Scanning `tools/bash`
as a root instead looks for `tools/bash/*/tool.toml`.
The `tools-bash-only/` symlink dir in this repo is a workaround
for that blindness.

The new design has two keys. `native_tool_paths` is a list of
native tool dirs. Each entry names a dir that directly contains a
`tool.toml`, e.g. `tools/bash`, `tools/read`, `tools/edit`.
`extension_tool_paths` is a parallel list for extension tool dirs.

```toml
[paths]
native_tool_paths = ["tools/bash", "tools/read", "tools/edit"]
extension_tool_paths = ["tools-ext/goal", "tools-ext/spawn_agent"]
```

Both lists share one resolution function. A user must never face two
different path rules. Precedence: native entries scan before
extension entries. An earlier entry wins a tool-name collision,
matching the existing `route` precedence.

Path resolution tries the config dir first. It uses the entry when
`<config_dir>/<entry>/tool.toml` exists. This covers the dev
checkout and the Nix package. There the config and tool dirs sit
next to the binary, so no full Nix store path is needed.

It falls back to `<exe_dir>/../<entry>`. A child config written
into a session dir would otherwise miss the install dir. The
config-declared paths always take priority, so a subagent child
config can fully override where its tools come from. P3.

Hard break: `tools_root` and `extra_tools_roots` are removed, as is
the `RUSHI_EXTRA_TOOLS_ROOT` env fallback. The config file is the
only channel for tool paths. The repo is pre-1.0, so no compatibility
shim is needed.

`spawn_agent` is an extension tool (D1). It is never part of
`native_tool_paths`. The parent passes the child a `native_tool_paths`
list that omits it, so the child cannot spawn further. P1, P5.

### D7. Blocking, one child at a time (v1)

The tool runs the child `rushi run` synchronously and waits for it to
exit. The parent's step does not advance until the child exits with a
terminal state (`ext_status: idle`, `error`, or timeout). No `kill`, no
`restart`, no `input` in v1 — the child is fire-and-block. The tool's
stdout (the tool result) is the child's terminal `ext_status` value plus
a short summary. Parallel fan-out (multiple children per step, merged
results) is explicitly out of scope for v1. P7.

### D8. Typed event vocabulary (serde)

The event schemas in `schemas/events/v1/` are 14 JSON files loaded at
runtime by `event_validation::load_schemas()`, which is called on every
event append (`step.rs:201`, `step.rs:222`), doing 14 file reads and 14
JSON parses per event. The Nix build does not ship the schema files, so
a Nix-installed binary silently runs with no validation (an empty schema
list is a pass-through).

Replace the JSON schema files with typed Rust structs. One `Event` enum
with 14 variants, each variant a struct with
`#[derive(Serialize, Deserialize)]`. The enum is internally tagged on
`type` via `#[serde(tag = "type")]`. The wire format (JSONL) is
unchanged.

**Why:**

- **No `schemas_dir`.** The child config can live anywhere. No path
  derivation, no Nix install gap. The types are compiled into the
  binary.
- **No per-event I/O.** `load_schemas` disappears. Validation is
  "can serde deserialize this?" — the same cost as parsing the JSON you
  already parse.
- **Compile-time safety.** Adding a new event type requires a new
  variant and struct. The compiler checks every `match`. No "unknown
  type" at runtime.
- **The wire format is unchanged.** `LogLine::from_json` takes the
  serialized string as before. The TUI keeps reading JSONL.

The types live in `crates/rushi/src/event.rs` (new module), re-exported
from `rushi_common`. `serde` (with the `derive` feature) is added to
`rushi-common`'s `Cargo.toml`. `serde_json` is already a dependency.

The full type definitions:

```rust
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One line in `events.jsonl`. Internally tagged on `type` so the
/// wire format matches the existing JSONL.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    UserMessage(UserMessage),
    AssistantMessage(AssistantMessage),
    ToolCall(ToolCall),
    ToolResult(ToolResult),
    Error(ErrorEvent),
    ExtStatus(ExtStatus),
    CompactionStarted(CompactionStarted),
    CompactionSummary(CompactionSummary),
    CompactionFailed(CompactionFailed),
    ContextExhausted(ContextExhausted),
    ApprovalRequest(ApprovalRequest),
    Approval(Approval),
    Rewind(Rewind),
    UserMessageRetract(UserMessageRetract),
}

// ---- user-facing ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserMessage {
    pub v: u8,
    pub ts: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue: Option<Queue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Queue {
    Steer,
    Follow,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserMessageRetract {
    pub v: u8,
    pub ts: String,
    /// The `id` of the `user_message` being retracted.
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

// ---- assistant ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantMessage {
    pub v: u8,
    pub ts: String,
    pub content: String,
    #[serde(default)]
    pub tool_calls: Vec<InlineToolCall>,
    pub stop_reason: StopReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Vec<ReasoningItem>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InlineToolCall {
    pub id: String,
    pub name: String,
    /// Free-form JSON object.
    pub arguments: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    Stop,
    ToolCalls,
    Length,
    Error,
    Aborted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningItem {
    #[serde(rename = "type")]
    pub kind: String, // always "reasoning" in v1
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encrypted_content: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u64>,
}

// ---- tool I/O ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub v: u8,
    pub ts: String,
    pub id: String,
    pub name: String,
    /// Free-form JSON object.
    pub arguments: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub v: u8,
    pub ts: String,
    pub id: String,
    /// The tool's output. A JSON object with optional `text` and
    /// `details` keys, or the tool's stdout parsed as JSON.
    pub value: Value,
    pub is_error: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_log: Option<String>,
}

// ---- errors and status ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorEvent {
    pub v: u8,
    pub ts: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtStatus {
    pub v: u8,
    pub ts: String,
    pub id: String,
    /// Shared UI state. Any JSON value.
    pub value: Value,
}

// ---- compaction ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompactReason {
    Threshold,
    Overflow,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionStarted {
    pub v: u8,
    pub ts: String,
    pub reason: CompactReason,
    pub tokens_before: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionSummary {
    pub v: u8,
    pub ts: String,
    pub summary: String,
    pub first_kept_seq: u64,
    pub version: u64,
    pub parent_version: u64,
    pub diverge_seq: u64,
    pub reason: CompactReason,
    pub tokens_before: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_after: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_files: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_files: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionFailed {
    pub v: u8,
    pub ts: String,
    pub reason: CompactReason,
    pub detail: String,
    pub last_user_seq: u64,
}

// ---- context ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextExhausted {
    pub v: u8,
    pub ts: String,
    pub message: String,
    /// The handoff session seeded with the summary. Empty string when
    /// the summary call failed and no session was seeded.
    pub new_session: String,
}

// ---- approval ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub v: u8,
    pub ts: String,
    pub id: String,
    pub tool_call_id: String,
    pub prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Approval {
    pub v: u8,
    pub ts: String,
    pub id: String,
    pub decision: ApprovalDecision,
    /// Edited arguments on the edit-then-allow path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalDecision {
    Allow,
    Deny,
}

// ---- rewind ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rewind {
    pub v: u8,
    pub ts: String,
    pub target_seq: u64,
    pub mode: RewindMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RewindMode {
    Before,
    On,
}
```

The `event_validation` module is retired. Its job (parse a line,
reject unknown or malformed events) moves into `event.rs` as
`parse_event`, which deserializes straight into the `Event` enum.
The public API is:

```rust
/// Parse one JSONL line into a typed `Event`.
/// Malformed lines are the caller's error to handle.
pub fn parse_event(line: &str) -> Result<Event, serde_json::Error>;

/// The set of known event type tags, for diagnostic messages.
pub const EVENT_TYPES: &[&str] = &[/* 14 strings */];
```

`LogLine` is unchanged: it still takes a serialized JSON string.
Producers construct an `Event`, call `serde_json::to_string(&event)`,
then `LogLine::from_json(&json)`. Readers call
`serde_json::from_str::<Event>(line)` instead of parsing into a raw
`Value` and matching on the `type` string. The `schemas/events/v1/`
files stay in the repo for the TUI and any external consumers, but the
kernel no longer reads them at runtime.

## 4. The tool

`tools/spawn_agent/tool.toml`:

```toml
name = "spawn_agent"
description = "Run a blocking child rushi session with a restricted tool set and read its terminal status."

[execution]
bin = "harness-spawn-agent"
args = []

[inputSchema]
type = "object"
required = ["task", "tools"]
properties = {
  task = { type = "string", description = "The prompt the child agent starts from." },
  tools = {
    type = "array",
    items = { type = "string" },
    description = "Native tool dir paths the child may use, e.g. tools/bash. The spawn tool's own dir is never passed down."
  },
  model = { type = "string", description = "Optional model override; inherits the parent model when absent." },
  hooks = {
    type = "array",
    items = { type = "string" },
    description = "Optional extra hook command lines appended to the inherited hooks."
  },
  max_steps = { type = "integer", description = "Optional step budget for the child; inherits the parent when absent." },
}
```

Decision 2026-09-16 (human): the tool is an application, not a
kernel citizen. It is not a kernel workspace member. The tool dir
and binary live in the exts repo, registered via
`extension_tool_paths`. The kernel ships only `config_gen` and the
`rushi config-gen` subcommand.

The exts tool binary does:

1. Read `$HARNESS_SESSION_DIR`, `$CONFIG`, and `$HARNESS_BIN` (all set
   by `route`).
2. Build the child config via `config_gen::project(parent_cfg, args)`
   (D3, D5, D6). Write it to
   `<parent_session_dir>/sub-<uuid8>/config.toml`.
3. `exec` `$HARNESS_BIN run --session-dir <child_dir> --config
   <child_config> <task>` and wait for exit.
4. On exit, read the child's `events.jsonl`, find the last
   `ext_status` event, and emit a tool result:
   `{"status": "<terminal>", "summary": "<last assistant text or the error message>", "session": "<child_dir>"}`.

The tool exits 0 when the child reaches `idle`; non-zero when the
child exits `error` or times out. The kernel's `route` turns a
non-zero exit into a `tool_result` with `is_error: true`, which the
parent agent sees in its next step.

## 5. What changes in the kernel

- `[paths] native_tool_paths` and `extension_tool_paths` replace
  `tools_root` + `extra_tools_roots` (D6). `route`, `assemble`, and
  `rushi` config resolution read the two lists. The
  `RUSHI_EXTRA_TOOLS_ROOT` env fallback is retired. P6.

- New module `config_gen` (D5). It has a `project()` function and a
  `rushi config-gen` subcommand that writes a child config. P3.
  The `bin/spawn_agent` kernel workspace member named in section 4
  is superseded. The tool is exts-owned (decision 2026-09-16).

- Route tool-env exports: `HARNESS_BIN` and `CONFIG` join
  `HARNESS_SESSION_DIR` in the tool subprocess env. The exts
  `spawn_agent` tool execs the child run through them. The spec's
  section 4 claim that route already sets all three is not true
  today. Verified 2026-09-16: route sets only `HARNESS_SESSION_DIR`
  (`bin/route/src/main.rs:376`).

- New module `event.rs` with the `Event` enum and per-type structs
  (D8). It is the live validator in the loop and in every stage
  binary (`log`, `user`, `claim`, `compact`). `event_validation` is
  retired. `schemas_dir` and the `--schemas` CLI args are gone. P8.

- No depth counter or `max_depth` key is added. Depth stays 1 and is
  enforced purely by tool availability. P5.

- No changes to the loop, `route` dispatch, or the tool contract.
  The wire format is unchanged.

## 6. Out of scope for v1

- Parallel children in one step; a `fan_out` tool that merges
  results.
- A child that streams its events into the parent TUI in real time.
  The parent only sees the terminal status.
- Shared file locks or a "child is doing X" indicator in the TUI.
- Auto-tuning of the child's tool set based on the task text.
## Open questions

1. Should the child config file be kept on disk for operator
   inspection, or written to a temp path and deleted after the child
   exits? (Default: keep it, in the child dir, so a post-mortem can
   see exactly what the child ran under.)
2. Does the terminal `ext_status` read need to handle a child that
   crashed with no `ext_status` at all (process kill, OOM)? (Default:
   treat a missing terminal status as `error`, with the child's exit
   code in the summary.)
3. oes the child inherit the parent's `model` by default, or does
   the tool require an explicit `model` argument? (Default: inherit,
   with an optional override — D3.)
4. hould `config_gen` also project `[hooks]` command lines verbatim,
   or resolve them against the parent's `hooks_bin` and re-emit
   absolute paths? (Default: verbatim, since the child runs from the
   same workspace and the hook binaries are at the same relative
   paths.)

## Properties

P1 — **Extension isolation.** The `spawn_agent` tool never enters
the kernel tree (decision 2026-09-16: exts-owned application). No
kernel crate or binary references `spawn_agent` by name. The
kernel workspace builds without the exts tool present.

P2 — **Nesting and cleanup.** The child session dir is
`sessions/<parent>/sub-<uuid8>`. `rm -rf sessions/<parent>/`
removes the parent and every child it spawned. The child's
`events.jsonl`, `tools.jsonl`, `meta.json`, and `.loop.lock` are
all inside that dir.

P3 — **Deterministic child config.** Running `config_gen::project`
on the same parent config and the same arguments twice produces
byte-identical child config files (modulo the session dir uuid). No
hand-written config, no `[tui]` section in the child config.

P4 — **Headless child.** The child process starts no TUI server.
Its `config.toml` has no `[tui]` section. `$HARNESS_WS` is unset in
the child's environment. The child exits when its agent stops.
The parent does not need a socket to the child.

P5 — **Depth is 1.** The child's `native_tool_paths` never include
the `spawn_agent` tool dir, so `route` rejects a `spawn_agent` call as
an unknown tool. Depth is capped by construction. There is no counter
and no `max_depth` key.

P6 — **Tool-path list semantics.** `native_tool_paths` and
`extension_tool_paths` are lists of tool dirs, each a dir that
directly contains a `tool.toml`. One resolution function serves both.
Native entries scan before extension entries. The earlier entry wins a
tool-name collision. The `RUSHI_EXTRA_TOOLS_ROOT` env var is not read
by `route` after this change.

P7 — **Blocking result.** The tool's result contains the child's
terminal `ext_status` value. The parent step does not advance while
the child process is alive. A child that exits `error` produces a
tool result with `is_error: true`.

P8 — **Typed events.** Every line in `events.jsonl` deserializes
into the `Event` enum via `serde_json::from_str`. The 14 variant
type tags match the 14 schema filenames. A malformed line
(an unknown tag, a missing field, or a wrong type) fails
deserialization with a `serde_json::Error` naming the field. The
`Event` enum is the closed set of event types. No `Value`-based
"unknown type" path remains in the kernel.

## Verification

| # | Property | Method | Status |
|---|----------|--------|--------|
| P1 | Extension isolation | The tool lives in the exts repo. `rg spawn_agent` over the kernel tree is empty. The kernel never references it (decision 2026-09-16). | open |
| P2 | Nesting and cleanup | unit test in `config_gen`: `project` sets `sessions_root` to `<parent>/sub-<uuid8>`. e2e: `rm -rf` of the parent dir removes child state. | open |
| P3 | Deterministic child config | unit test: two `project` calls, same input, diff the output bytes (session uuid masked). | open |
| P4 | Headless child | e2e: `rg '\[tui\]'` the generated config finds nothing. run the child, assert no `$HARNESS_WS` in its env. | open |
| P5 | Depth is 1 | Child without spawn tool dir gets `unknown tool` error. No counter. | open |
| P6 | Tool-path semantics | Entry with `tool.toml` registers. Native shadows extension. Env var ignored. | open |
| P7 | Blocking result | e2e: spawn a child, assert the tool result JSON contains the child's terminal status. assert the parent step blocked until exit. | open |
| P8 | Typed events | unit tests in `event.rs`: one round-trip per event type, unknown-tag and missing-field rejection, 14-entry inventory. `event_validation` retired and `cargo test` passes with `schemas/events/v1/` moved out of the tree. | passing |

## Gate

**Blocked.** D6 (tool-path lists) and D8 (typed events) are built.
Not yet implemented: `config_gen`, the `rushi config-gen`
subcommand, the route tool-env exports, and the exts-owned
`spawn_agent` tool. P1-P7 are open. P8 (typed events) is done:
`event.rs` is wired into the loop and all stage binaries.
`event_validation` is retired, and the loop no longer reads
`schemas/events/v1/` at runtime.
