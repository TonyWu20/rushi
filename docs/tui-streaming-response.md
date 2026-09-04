# TUI streaming response rendering

Status: Spec (2026-09-05). Depends on Phase 2 (`docs/phase-2-plan.md`).
The `harness` binary must exist before this feature lands. The
streaming channel is the TUI-side consumer; `harness` and `bin/model`
are the producers. This doc is the contract for all three.

## 1. Purpose and motivation

Today the TUI shows a frozen screen for the entire model call. The
`loop_phase=wait` marker (docs/tui-model-wait-indicator.md) names
the phase and shows a timer. The user sees the next complete
`assistant_message` appear only after the model finishes, the
response is parsed, and the event lands in the log. On long
reasoning or long text responses this is a multi-minute gap with
zero feedback.

The model binary already consumes the SSE stream internally
(`stream: true`, delta accumulation in `bin/model`). The
infrastructure exists. The missing piece is a channel that carries
the in-progress text to the TUI before the final event is written.

## 2. Design summary

Three components change. No new event type enters the log. The
durable log stays an audit record of completed events only.

| Component | Change |
|---|---|
| `bin/model` | New `--delta-file <path>` flag. When set, the model writes one JSON line to that file for every SSE delta it receives, in arrival order. The stdout contract (one final JSON object) is unchanged. |
| `harness` (Phase 2) | Before spawning the model stage, create `sessions/<n>/.model-stream` (empty). Pass its path to the model binary via `--delta-file`. After the model call completes, delete the file. On a model-call error the file is also deleted. |
| TUI | Each frame, if the active session has a `.model-stream` file, read it (or the portion after the last byte offset) and render the accumulated text in a live block below the transcript. When the matching `assistant_message` event arrives via the log tailer, clear the live block. The transcript then renders the final event normally. |

## 3. The stream file

### 3.1 Location and lifecycle

`sessions/<n>/.model-stream` is a session-local, ephemeral artifact.
It is created and deleted by `harness` around each model call. It
never appears in `claim`'s state derivation, the compact trigger
math, or the TUI's log replay. Old sessions that predate this
feature have no `.model-stream` file; the TUI treats a missing file
as "no live stream."

The dot-prefix follows the existing session-dir pattern: `compact.json`,
`.loop.lock`, `loop.pid`. The TUI's log tailer watches `events.jsonl`
only, so the new file disturbs no existing render path
(phase-2-plan §3.5: "The TUI tailer watches events.jsonl only").

### 3.2 Line protocol

Each line is a compact JSON object. One delta event per line. The
lines are newline-delimited; the reader does not need to parse
partial lines (the model binary writes one complete line per `write`,
and `flock` serializes concurrent writers, same as `LogLine`).

```json
{"kind":"text","delta":"Hello "}
{"kind":"reasoning","item_id":"rs_1","delta":"Let me think about this..."}
{"kind":"tool_call_delta","call_id":"c1","name":"bash","args_delta":"ls -la"}
{"kind":"done","stop_reason":"stop"}
```

| `kind` | Fields | Meaning |
|---|---|---|
| `text` | `delta` (string) | A chunk of the assistant's output text. |
| `reasoning` | `item_id` (string), `delta` (string) | A chunk of a reasoning item's text. `item_id` matches the id the final `assistant_message` will carry in its `reasoning` array. |
| `tool_call_delta` | `call_id` (string), `name` (string), `args_delta` (string) | A chunk of a tool-call's argument JSON. The TUI shows the call name immediately and the arguments as they stream in. |
| `done` | `stop_reason` (string) | The model call finished. No further lines follow. |

The `done` line is written by the model binary after the SSE stream
closes. It is the signal that the stream file is complete. The TUI
does not wait for `done` before finalizing: the `assistant_message`
event in the log is the authoritative final state. The `done` line
lets the TUI stop polling early if the log event has not arrived
yet (a brief window where the stream is done but the log append has
not landed).

### 3.3 Size bound

A typical model response produces 200–2000 delta lines. Each line
is at most ~500 bytes. The file is at most ~1 MB for a very long
response. The TUI reads it in full each frame while the file exists.
No tailer is needed: the file is small and short-lived. The read is
`std::fs::read` on a regular file, not a `poll`-based tailer. This
avoids a second file-descriptor and a second inotify watch in the
TUI event loop.

## 4. `bin/model` changes

### 4.1 New flag

```
model --config <path> [--delta-file <path>]
```

`--delta-file` is optional. When absent, behavior is unchanged
(phase-2 parity). When present, the model opens the file at start
(truncate, create) and appends one line per SSE delta event.

The flag is additive. The existing e2e overrides (`MODEL_BIN`
stub, `cache-e2e.sh`) do not pass it. The stub model in
`compact-e2e.sh` does not emit deltas. The stream file is simply
empty. This is correct: a stub model has no real stream.

### 4.2 Where deltas are emitted

`bin/model` already parses the SSE stream line by line
(`parse_sse_response`). The delta events it already accumulates
(`response.output_text.delta`, `response.reasoning_text.delta`,
`response.function_call_arguments.delta`) are the source. The
change is to write a line to the delta file for each of these
events, in addition to (not instead of) the existing accumulation.
The final JSON object on stdout is built the same way as today.

The `done` line is written after the SSE stream closes and before
the final JSON is printed to stdout.

### 4.3 No change to the stdout contract

The model binary's stdout still carries exactly one JSON object:
the complete model response. `harness`'s subprocess `StageRunner`
implementation reads that object from stdout and passes it to
`parse`. The delta file is a side channel. The two paths are
independent: if the delta file write fails (disk full, permission),
the model call still succeeds. The TUI simply does not show a live
stream for that response. A stderr warning is sufficient.

## 5. `harness` changes

### 5.1 Stream-file management

In the `awaiting_model` branch (phase-2-plan §4.3, step 6), before
spawning the model stage:

1. Create `sessions/<n>/.model-stream` (empty). The file is owned
   by the harness process. `flock` is not needed: only the harness
   and the model child write to it, and the model child inherits
   the harness's working directory. A concurrent reader (the TUI)
   only reads, so no write lock is required.
2. Pass `--delta-file <path>` to the model stage argv.
3. After the model call returns (success or error), delete the file.
   If the call failed, the file may contain partial deltas. The
   `error` event in the log is the authoritative record. The TUI
   sees the error and clears its live buffer.

The file path is session-scoped: `sessions/<n>/.model-stream`.
One model call at a time per session (the loop is sequential), so
no contention.

### 5.2 `StageRunner` trait change

The `model` method gains one parameter:

```rust
fn model(
    &self,
    request: &RequestFile,
    delta_file: Option<&std::path::Path>,
) -> Result<ModelOutput>;
```

The subprocess implementation passes `--delta-file <path>` when
`delta_file` is `Some`. The `harness` loop calls it with the
session's stream-file path. A `None` argument (the `harness step`
CLI, where no TUI is watching) skips the flag. The method's return
type is unchanged: the deltas are a side effect on the file, not
part of the return value. This keeps the trait synchronous and the
state machine simple. In Phase 3 (in-process runner), the same
signature works: the in-process model writes to the file directly.
A later optimization can replace the file with an in-memory
channel without changing the trait.

### 5.3 No change to the log

The log gets the same events as today: one `assistant_message`,
N `tool_call` events, one `tool_result` per call. No new event
type. The `assistant_delta` event does not exist. The stream file
is a side channel. This respects the phase-2-plan §9 constraint:
"No new event type, no `v` bump."

## 6. TUI changes

### 6.1 New state on `App`

```rust
pub struct App {
    // ... existing fields ...

    /// The live stream buffer. `None` when no model call is in
    /// progress or the stream file does not exist.
    stream_buf: Option<StreamBuf>,
    /// The byte offset the TUI has read from the stream file.
    stream_offset: u64,
}

pub struct StreamBuf {
    /// Accumulated assistant text (from `text` deltas).
    text: String,
    /// Accumulated reasoning text, keyed by item id.
    reasoning: HashMap<String, String>,
    /// Partial tool-call arguments, keyed by call id.
    tool_args: HashMap<String, (String, String)>, // (name, partial-args)
    /// Set when the `done` line is read.
    done: bool,
}
```

`StreamBuf` is `None` while no stream is active. It is created on
the first frame that finds a non-empty `.model-stream` file, and
is cleared (set to `None`) when the matching `assistant_message`
event arrives via the log tailer.

### 6.2 Per-frame read

In the main loop (after the log tailer drain, before the draw):

```text
if active_session_has(.model-stream):
    read the file (or the portion after stream_offset)
    parse new lines into StreamBuf
    if the matching assistant_message event landed this frame:
        clear stream_buf
```

The read is a plain `fs::read` to a `u64` offset. The file is small
(§3.3). No async, no inotify. The TUI's event loop already runs at
~10 fps (the 100 ms poll). Reading a sub-MB file at that rate is
negligible.

### 6.3 Render

In `render.rs`, after the cached transcript lines and before the
input box, render the live block when `stream_buf` is `Some`:

```
  > assistant …
    <accumulated text, same wrapping and syntax highlighting as
     a final assistant_message>
    ▊
```

The `…` after "assistant" signals "in progress." The `▊` is a
blinking cursor (toggled on alternating frames). The text uses the
same `render_message_content` path as the final message, so syntax
highlighting and markdown rendering are consistent. The reasoning
and tool-call deltas render in the same style as their final
forms, but with a "streaming" indicator.

When `stream_buf` is `None` (no stream, or the final event has
landed), the live block is absent. The transcript renders the
final `assistant_message` as usual.

### 6.4 Restart and edge cases

| Case | Behavior |
|---|---|
| TUI restarts while the model call is still in progress | The `.model-stream` file exists. The TUI reads it from byte 0. The live block shows the accumulated text. When the `assistant_message` event arrives, the block clears. |
| TUI restarts after the model call finished | The `.model-stream` file is gone (deleted by `harness`). No live block. The `assistant_message` is already in the log and renders normally. |
| Model call errors | `harness` deletes the stream file. The TUI sees the file disappear. The `error` event in the log renders. The live block (if any partial text was shown) is cleared on the next frame. |
| The user scrolls up while streaming | The live block is pinned to the viewport bottom (above the input box). It does not scroll with the transcript. This matches the "model wait indicator" behavior: the indicator is a working-row element, not a transcript entry. |
| Multiple sessions | The stream file is per-session (`sessions/<n>/.model-stream`). The TUI only reads the active session's file. Switching sessions clears the live block. |

## 7. What this does not change

- No new event type in the log. No schema file. No `v` bump.
- No change to `claim`, `assemble`, `parse`, `route`, or `compact`.
- No change to the log format. Old sessions replay unchanged.
- No change to the `StageRunner` trait's method set. One parameter
  added to `model`.
- The TUI's log tailer is unchanged. The stream file is a separate,
  simpler read path.
- The `loop.pid` probe, `flock` check, and `loop_phase` marker are
  all unchanged.

## 8. Phase 3 evolution

When the `StageRunner` moves in-process (phase 2 §3.3, Phase 3),
the `model` method can return a `Vec<ModelDelta>` alongside the
final `ModelOutput`. The file channel is replaced by an in-memory
channel (a `tokio::sync::mpsc::Receiver<ModelDelta>` that the TUI
polls). The trait signature changes from `model(&self, request,
delta_file) -> Result<ModelOutput>` to:

```rust
fn model(
    &self,
    request: &RequestFile,
    stream: Option<&mut dyn FnMut(&ModelDelta)>,
) -> Result<ModelOutput>;
```

The callback is invoked for each delta. The TUI's implementation
appends to `StreamBuf`. The file-based implementation (Phase 2)
ignores the callback and writes to the file instead. The state
machine is unchanged.

The `ModelDelta` type is the same shape as the JSON lines in §3.2:

```rust
pub enum ModelDelta {
    Text(String),
    Reasoning { item_id: String, delta: String },
    ToolCallDelta { call_id: String, name: String, args_delta: String },
    Done { stop_reason: String },
}
```

This type lives in `harness-common::stage` alongside the existing
payload types. It is added in Phase 2 (the file-based runner uses
it to serialize to JSON lines) so the Phase 3 in-process runner
reuses it without a wire-protocol break.

## 9. Test plan

| Test | What it verifies |
|---|---|
| `bin/model` with `--delta-file` writes one line per SSE delta | The delta file contains the expected lines in order. The stdout JSON is byte-identical to the no-flag case. |
| `bin/model` without `--delta-file` is unchanged | The e2e suite passes. No side file is created. |
| `harness step` with a TUI watching | The stream file appears during the model call, is deleted after. The TUI shows the live block. |
| `harness step` without a TUI (CLI) | The stream file is created and deleted. No reader. No effect. |
| Model call error with `--delta-file` | The partial delta file is deleted. The error event is in the log. |
| TUI restart mid-stream | The TUI reads the existing stream file and shows the accumulated text. |
| TUI restart after stream completes | No stream file. No live block. Normal render. |
| Scroll-up during stream | The live block stays pinned. The transcript scrolls independently. |
| The 12-scenario `compact-e2e.sh` gate | All 12 pass. The stream file is not part of the compact path. |

## 10. Open questions

- **Delta granularity.** The model binary currently accumulates
  SSE deltas. The `response.output_text.delta` event carries
  arbitrary text chunks. The TUI reads the file at ~10 fps. The
  question is whether to forward every SSE delta as a file line or
  batch them (e.g., every 50 ms). Forwarding every delta is
  simplest and the file is small. Batching reduces the number of
  file writes. Decision: forward every delta. The file is small
  enough that write cost is negligible. Revisit if profiling shows
  I/O pressure.
- **Cancellation.** If the user presses `Ctrl+C` during a model
  call, `harness` receives `SIGINT`, cancels the in-flight stage
  child (phase-2-plan §4.7), and deletes the stream file. The TUI
  shows the `cancel` event. No partial `assistant_message` is
  written. This is the same behavior as today: a killed model call
  leaves no `assistant_message` in the log.

## Properties

Lean-style invariants for this spec (see `lean-driven-development.md`).
One property per non-trivial invariant. Each property is observable:
given an input, an output guarantee.

P1. delta-file-lines: given `bin/model` run with `--delta-file <path>`,
    observe one JSON line per SSE delta written to that file in
    arrival order, and the stdout JSON object is byte-identical to
    the no-flag case.
P2. flag-absent-unchanged: given `bin/model` run without
    `--delta-file`, observe the behavior is unchanged and no side
    file is created.
P3. stream-file-lifecycle: given the `harness` spawns the model
    stage, observe `sessions/<n>/.model-stream` exists and is empty
    before the model call starts, and is gone after the call
    returns, on success or error.
P4. live-block: given the active session has a non-empty
    `.model-stream` file, observe the TUI renders the accumulated
    text in a live block below the transcript, and the block
    clears when the matching `assistant_message` event arrives.
P5. restart-midstream: given a TUI restart while a model call is
    in progress, observe the TUI reads the stream file from byte 0
    and shows the accumulated text.
P6. error-clear: given a model-call error, observe the `harness`
    deletes the stream file, the TUI clears its live block, and the
    `error` event in the log is the authoritative record.
P7. missing-file: given the session has no `.model-stream` file,
    observe the TUI treats it as no live stream and draws no live
    block.

## Verification

Each property maps to its proof. `proven` means the cited test or
script exists and passes. `open` names the blocker and what unblocks
it.

| P# | Property | Proof | Status |
|----|----------|-------|--------|
| P1 | delta-file-lines | Blocked: `--delta-file` flag and delta writer are not implemented in `bin/model`. Unblock by implementing the flag and adding a test that the delta file lines match the SSE delta events and the stdout JSON is unchanged | open |
| P2 | flag-absent-unchanged | Blocked: same as P1. Unblock with the existing e2e suite (`scripts/cache-e2e.sh`) run after the flag lands, confirming no side file appears | open |
| P3 | stream-file-lifecycle | Blocked: `harness` stream-file create/delete is not implemented. Unblock by implementing the lifecycle in `bin/harness` and adding a test that the file exists between spawn and completion | open |
| P4 | live-block | Blocked: TUI stream buffer and live-block render are not implemented. Unblock by implementing `StreamBuf` and the render path, plus a render test that the live block shows and clears | open |
| P5 | restart-midstream | Blocked: same as P4. Unblock with a TUI test that a restart onto an existing stream file rebuilds the live block | open |
| P6 | error-clear | Blocked: same as P3. Unblock with a test that a failed model call leaves no stream file and the TUI clears the block | open |
| P7 | missing-file | Blocked: same as P4. Unblock with a TUI test that a session without a stream file draws no live block | open |

## Gate

Gate: blocked — the `--delta-file` streaming channel is not implemented (no `delta-file` or `.model-stream` code in `bin/model`, `bin/harness`, or `bin/tui`).

The acceptance commands that will apply. All must exit 0 for this spec to be proven.

```
cargo build
cargo test
scripts/compact-e2e.sh
```
