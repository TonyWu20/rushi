# Image Read — KISS Decision and Implementation

This document records the decision to implement image reading as a
**kernel + extension** split. The kernel owns only what is needed to
accept image input, send it to the model, and keep the token budget
honest. All image-specific logic (detection, compression, re-encode)
lives in a `tool.after` extension hook.

## 1. The Core Split

The original plan (`docs/image-read-plan.md`) put image decode, resize,
and re-encode in the `read` tool (kernel). That pulled the heavy
`image` codec dependency and a complex compression pipeline into the
kernel. The KISS decision moves all of that to an extension.

**Kernel (this repo, `rust-unix-harness`):**

- Accept image tool results and carry them to the model.
- A per-model `vision` flag gates whether image data is sent.
- The array `output` wire form (`input_text` + `input_image`) for the
  Responses API, and the `image_url` user-message form for Chat
  Completions.
- A flat per-image token estimate so the trigger and the compact cut
  count image tokens.
- A `tool.after` `transform` decision that lets an extension rewrite
  tool results by call id.
- The `hook_io` helper crate so extensions build the decision JSON
  without hand-rolling the envelope.

**Extension (out of the kernel):**

- A `tool.after` hook that catches a failed read of an image file.
- Magic-byte detection, decode, resize, re-encode, the 3-retry
  compression budget, and temp-file lifecycle.
- It builds a success result carrying the compressed base64 and emits a
  `transform` decision.

## 2. The Flow

1. The agent calls `read("/path/to/big.png")`.
2. The (text-only) `read` tool rejects the binary file. The routed
   result is an error.
3. The `tool.after` window fires. The extension hook reads the call's
   `file_path`, sniffs the magic bytes, confirms it is an image.
4. The hook runs the compression pipeline (3-retry budget) to bring the
   file under the size cap.
5. The hook builds a success `tool_result` with
   `value.details = {type: "image", mime_type, data (base64), path}`
   and emits a `transform` decision.
6. The kernel splices the rewrites into the routed results by call
   id and appends it to the log.
7. `assemble` sees `value.details.type == "image"` with non-empty
   `data` and, when `vision` is on, emits the array `output` with an
   `input_image` part. The model sees the compressed image.

The read tool stays text-only. The image support is an extension
capability. Future formats (PDF, audio, video) are new hook logic, not
kernel changes.

## 3. What Was Reverted From The Read Tool

- The image branch (magic-byte sniff, base64 encode, 10 MB cap) in
  `tools/read/src/main.rs` is removed.
- The `base64` dependency is removed from `tools/read/Cargo.toml`.
- The `tool.toml` description is back to the text-only wording.

The read tool reads UTF-8 text only. Any binary file produces the
existing "not a UTF-8 text file" error. That error is the trigger the
extension hook keys on.

## 4. Kernel Changes

### 4.1 `crates/rushi/src/model_settings.rs` — `vision` flag

`ModelSettings` carries `vision: bool` (default `false`), resolved from
the per-model TOML section with a fallback to the global `[model]`
section. `config-real.toml` sets `vision = true` on the local Qwen
models. `config.toml` documents the knob.

### 4.2 `bin/assemble` — image carry + array output

- `Ev::ToolResult` gains `image: Option<Value>`, populated from
  `value.details` when `type == "image"` and `data` is non-empty.
- `full_items` emits the array `output` form when `vision` is true,
  and the plain string form with an "Image omitted" note when false.
- `compact_items` drops the image (compacted regions never re-send
  images).
- `estimate_ev_tokens` adds a flat 4800 chars per image-carrying event.
- The `vision` flag is threaded through `build_items` /
  `build_items_off` / `full_items`.

### 4.3 `bin/model` — Chat-Completions conversion

`convert_to_chat_format` detects an array `output` with `input_image`
parts. It emits a text-only `role: "tool"` message, then a
`role: "user"` message with `image_url` parts. This prevents the
silent image drop on the Chat-Completions path, which both configured
models use.

### 4.4 `crates/rushi/src/compact_math.rs` — image-aware estimate

`project_event` adds a flat 4800-char estimate when a `tool_result`
carries an image in `value.details`. This keeps the loop, the compact
binary, and the assemble estimator consistent. It is the token-budget
safeline on the compaction side.

### 4.5 `bin/rushi/src/step.rs` — `tool.after` `transform`

The `tool.after` window now carries the routed `calls` (with their
arguments) alongside the `results`. A hook may emit a `transform`
decision with `payload.results`, a keyed map from call id to new
`tool_result` JSON. The kernel splices each entry into the routed
results by matching the call id. Unmentioned calls keep their routed
result. A transform that lists every call id is a whole swap.

The `transform` word is shared with `model.before`, where the payload
is the whole request object instead of a keyed map. One word, two
payload shapes. Each window knows its own shape.

The transformed results are what get appended to the log.

### 4.6 `crates/rushi/src/hook_io.rs` — extension helper

Typed builders for the hook decision envelope. An extension calls
`ToolAfterTransform::new().rewrite(id, result_json).with_reason(...)
 and prints `to_stdout_json()`. The kernel is the
single source of truth for the envelope. A kernel contract change is
caught at the extension's compile time, not at runtime.

## 5. What Stays Out Of The Kernel

- Magic-byte detection, decode, resize, re-encode (the `image` crate).
- The 3-retry compression budget (resize + quality stepping).
- Temp-file lifecycle under the session dir.
- The 10 MB file-size cap is enforced by the extension's compression
  budget, not by the read tool. The kernel's token-budget estimate is the
  safeline: image tokens are counted, so an image-heavy context triggers
  compaction, and compaction drops old images.

## 6. Files Touched

| File | Change |
|---|---|
| `tools/read/src/main.rs` | Reverted to text-only (image branch removed) |
| `tools/read/Cargo.toml` | Removed `base64` dependency |
| `tools/read/tool.toml` | Reverted to text-only description |
| `crates/rushi/src/model_settings.rs` | `vision: bool` field, resolved from config |
| `bin/assemble/src/main.rs` | `image` field, `vision` threading, array output, compact drop, +4800 estimate |
| `bin/model/src/main.rs` | Chat-Completions array `output` -> `image_url` user message |
| `crates/rushi/src/compact_math.rs` | `project_event` image estimate |
| `crates/rushi/src/hook_io.rs` | New: `ToolAfterTransform` builder |
| `crates/rushi/src/lib.rs` | Export `hook_io` |
| `bin/rushi/src/step.rs` | `tool.after` `transform` splice + `calls` in payload |
| `config-real.toml` | `vision = true` on Qwen models |
| `config.toml` | Commented `vision` example |

## 7. Verification

- `cargo build` succeeds across the workspace.
- `cargo test` passes: 42 assemble, 47 rushi-common (includes the 2 new
  `hook_io` tests), 28 route, 27 model, 20 read, 19 log, and the rest,
  with 0 failures.
