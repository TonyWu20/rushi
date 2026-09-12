# Typed Event Vocabulary (serde)

Status: **Implemented and wired.** The module is built, unit-tested,
and is the live validator in the loop and every stage binary.

Replaces the runtime JSON-Schema validation in `event_validation` with
a typed `Event` enum in `rushi-common`. Each of the 14 event types in
`schemas/events/v1/` becomes a Rust struct with
`#[derive(Serialize, Deserialize)]`. The enum is internally tagged on
`"type"` so the JSONL wire format is byte-identical.
`serde_json::from_str::<Event>(line)` is the single validation step.
No file I/O on every append. No `schemas_dir` config.

## 1. Motivation

- The Nix build does not ship `schemas/events/v1/`. A Nix-installed
  binary silently runs with no validation (an empty schema list is a
  pass-through).
- `event_validation::load_schemas` is called on every event append
  (`step.rs:201`, `step.rs:222`), doing 14 file reads and 14 JSON
  parses per event.
- The subagent design (D9 in `subagent-design.md`) needs a child
  config that can live anywhere. A `schemas_dir` path breaks that.
- Adding a new event type currently means adding a JSON file. The
  validator has no compile-time knowledge of the type set.

## 2. Design

### 2.1 The `Event` enum

`crates/rushi/src/event.rs` defines:

- **`Event`** — internally tagged
  (`#[serde(tag = "type", rename_all = "snake_case")]`), 14 variants
  matching the 14 schema filenames.
- **14 payload structs** — `UserMessage`, `AssistantMessage`,
  `ToolCall`, `ToolResult`, `ErrorEvent`, `ExtStatus`,
  `CompactionStarted`, `CompactionSummary`, `CompactionFailed`,
  `ContextExhausted`, `ApprovalRequest`, `Approval`, `Rewind`,
  `UserMessageRetract`.
- **Helper enums** — `StopReason` (5 values), `Queue` (2),
  `CompactReason` (3), `ApprovalDecision` (2), `RewindMode` (2).
- **Helper structs** — `InlineToolCall`, `ReasoningItem`, `Usage`.
- **`parse_event(line: &str) -> Result<Event, serde_json::Error>`**
  — the single validation entry point.
- **`EVENT_TYPES: &[&str]`** — the 14 type tags, for diagnostics.

All free-form JSON fields (`arguments`, `value`, `ext_status.value`,
reasoning `content`/`summary`/`encrypted_content`) use
`serde_json::Value`. Optional fields carry
`#[serde(skip_serializing_if = "Option::is_none")]` so serialization
omits absent keys, matching the existing JSONL.

### 2.2 Wire format compatibility

The internally-tagged enum flattens variant fields into the JSON
object. `{"type":"user_message","v":1,"ts":"...","content":"..."}`
deserializes to `Event::UserMessage(UserMessage { .. })` and serializes
back to the same JSON. `LogLine::from_json` is unchanged — it takes a
raw string. Producers call `serde_json::to_string(&event)` instead of
hand-building the JSON string.

### 2.3 What changes in the kernel

| Item | Before | After |
|------|--------|-------|
| Validation | `load_schemas(dir)` + `validate(json)` | `parse_event(line)` (serde) |
| Schema source | 14 JSON files in `schemas/events/v1/` | Compiled-in `Event` types |
| `schemas_dir` in config | `config_dir.join("schemas/events/v1")` | Removed |
| `--schemas` CLI args | `user`, `claim`, `log` binaries | Removed |
| `event_validation` module | JSON Schema validator | Retired |

The `schemas/events/v1/` files stay in the repo for the TUI and
external consumers. The kernel no longer reads them at runtime.

## 3. Status

The module is built, tested, and wired in. It replaced
`event_validation` as the live validator in every caller: the loop
(`step.rs`) and the `user`, `claim`, `log`, and `compact` stage
binaries. `event_validation` is retired and `schemas_dir` is gone from
`HarnessConfig` and the `--schemas` CLI args.

- `crates/rushi/src/event.rs` — typed module, 20 unit tests.
- `crates/rushi/Cargo.toml` — `serde` with `derive` feature added.
- `crates/rushi/src/lib.rs` — `pub mod event;` registered.
- Callers: `parse_event` in `step.rs`, `user`, `claim`, `log`.
- `event_validation` retired. `schemas_dir` removed. `cargo build`
  and `cargo test` pass with no warnings.

## Properties

P1 — **Round-trip stability.** For every event type, serialize a
constructed value, deserialize it back, serialize again. The second
serialized form equals the first. 14 tests, one per type.

P2 — **Unknown type tag rejected.** A JSON object with
`"type":"bogus"` fails `parse_event` with an "unknown variant" error.

P3 — **Missing field rejected.** A `user_message` line missing
`content` fails with an error naming `content`.

P4 — **Type-tag inventory.** `EVENT_TYPES` has exactly 14 entries.
They are the same set as the 14 filenames in `schemas/events/v1/`.

P5 — **Wire format unchanged.** No existing JSONL line that is valid
per the JSON schemas will fail `parse_event`. The set of valid lines
is the same.

P6 — **No per-event file I/O.** `parse_event` does no file reads.
It is the live validator in the loop and every stage binary, so no
schema files are read at runtime.

## Verification

| # | Property | Method | Status |
|---|----------|--------|--------|
| P1 | Round-trip stability | 14 unit tests in `event.rs::tests` (one per event type) | passing |
| P2 | Unknown type tag rejected | `unknown_type_tag_fails` test in `event.rs::tests` | passing |
| P3 | Missing field rejected | `missing_field_fails` test in `event.rs::tests` | passing |
| P4 | Type-tag inventory | `event_types_constant_has_14_entries` + `event_types_match_schema_filenames` | passing |
| P5 | Wire format unchanged | Round-trip tests cover this. Existing JSONL lines parse identically | passing |
| P6 | No per-event file I/O | Code inspection. `parse_event` calls `serde_json::from_str`, no `fs::` calls | passing |

## Gate

**Passing.** The module is wired into every caller and
`event_validation` is retired. The loop and stage binaries build and
test clean.

- `cargo build` (no warnings)
- `cargo test`
- `bash scripts/e2e-rewind.sh`
