# Itches

Problems to solve.

## Event schema validator is now a third copy (2026-08-27)

Producer-side G3 validation (check the event against
`schemas/events/v1/<type>.json` before append) exists in three
places:

1. `bin/user/src/main.rs` — `validate_event` (user_message producer).
2. `bin/log/src/main.rs` — loads all schemas, validates every line.
3. `bin/tui/src/port_file.rs` — minimal local validator used by
   `append_event`.

The duplication is intentional for Phase 1 (no shared `core` crate,
per `architecture.md`), but the third copy crosses the promotion
threshold in `refinement-policy.md`. Candidates for one shared
validator:

- a small `schemars`-free JSON-Schema subset crate (`core` or
  `bin/common`), or
- one binary that owns validation and the producers call it.

Blocker to watch: the validator subset must stay small enough to be
portable. The TUI copy (`port_file.rs`) supports only `const`,
`required`, `properties`, `items`, and primitive `type` checks
(string/integer/number/boolean/array/object). Do not grow the three
copies in parallel.
