# Reasoning Log Size — Drop the Redundant `summary`

This document records a log-size decision. It strips the `summary` field from
logged `reasoning` items. The goal is a leaner session log. It does not change
what the model reasons about.

## 1. The Measurement

Scanning every `events.jsonl` under the `rust-unix-harness` and `rushi-tui`
session trees (125 files):

- Reasoning `content` bytes: **71.57 MB**
- Reasoning `summary` bytes: **71.52 MB**
- The two overlap at **99.9%**. `summary` is a near copy of the thinking that
  already lives in `content`.

Carrying both doubles the reasoning payload with no replay benefit. This is the
dominant reasoning-side cost. The tool-log `details` split
(`tool-log-design_from_human.md`) already handles the tool side.

## 2. The Decision

Strip `summary` at the point reasoning items enter the log in `bin/parse`.
Keep `content` whole. No capping, trimming, or per-leaf elision of `content`.

Capping or trimming reasoning text or tool `details` adds new elision rules.
Those rules cost more mind to hold than the bytes they save. `summary` is the
one redundant copy. Drop only that field and leave the rest intact.

## 3. Change

`bin/parse/src/main.rs`, in `process()`:

- Each `reasoning` item is cloned. Its `summary` field is removed before the
  item attaches to the `assistant_message` event.
- The fields `content`, `encrypted_content`, `id`, `type`, and `status` stay.
- A new unit test `reasoning_summary_field_is_stripped` asserts the field is
  gone and `content` is preserved.

## 4. Contract Note

`docs/typed-events.md` and `loop-and-edit-implementation-corrections.md`
describe `reasoning` as forwarded verbatim. This change breaks that contract
for `summary` only. The log no longer carries it.

This is safe on replay. `bin/assemble` re-sends each reasoning item on the next
request. The model API treats `summary` as optional on the request. `content`
and `encrypted_content` carry the substance. `bin/model` still asks the server
for `summary: "auto"` on generation. It just does not echo the summary back.

## 5. Scope

- Forward-only. Existing logs keep their `summary` fields. `assemble` still
  replays them verbatim. New sessions log reasoning without `summary`.
- Does not touch `tool_result.details`, `tools.jsonl`, the head/tail preview,
  or the image-payload exception.

## 6. Files Touched

| File | Change |
|---|---|
| `bin/parse/src/main.rs` | `process()` strips `summary`; adds the strip test |

## 7. Verification

- `cargo build` is clean with no warnings.
- `cargo test` passes 252 tests with 0 failures. The new
  `reasoning_summary_field_is_stripped` test passes.
