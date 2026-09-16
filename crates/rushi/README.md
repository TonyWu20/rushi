# rushi-common

Package `rushi-common`, in `crates/rushi/`.
Shared types and math for the rushi agent harness.

All the loop stage binaries
(`claim`, `assemble`, `model`, `parse`, `route`, `log`,
`compact`, `user`)
and the loop orchestrator (`rushi`)
depend on this crate.
It is the single source of truth for the
event vocabulary,
hook ABI,
compact math,
and model-settings resolution.

The crate types are pure.
No network.
The only I/O is one method:
`LogLine::commit` does a single locked `write(2)` of the whole line.
All other modules are I/O-free.
Its only external dependencies are
`serde`, `serde_json`, `toml`, `glob`, and `libc`.

---

## Modules

| Module | Purpose |
|--------|---------|
| `event` | Typed event enum. One variant per event type in `events.jsonl`. `parse_event` is the single validation step. |
| `logline` | The `LogLine` type. The only value that may write to the session log. FT-005: one `write(2)` of the whole line under an exclusive `flock`. |
| `compact_math` | Pure trigger math and cut walk for auto-compaction. `estimate_from_events`, `trigger_fired`, `find_cut`, `post_compact_sanity`. |
| `stage` | The `StageRunner` trait and all payload types (`Claim`, `ModelOutput`, `ParsedEvents`, `RouteEnv`, `CompactOpts`, `CompactStatus`, `StageError`). |
| `hooks` | Lifecycle-window dispatcher. The `Window` enum, `fire_hooks`, `fold_decision`, `hook_env`. |
| `hook_io` | Typed payload builders for hook decisions. `ToolAfterTransform` and friends. No serde derive needed. |
| `rewind` | Active-path computation over rewind events. `RewindRef`, `active_ranges`, `seq_in_ranges`. |
| `model_settings` | Shared model-section resolution. `resolve_model_settings`, `active_model_from_config`. |
| `config_check` | Legacy key detection. `legacy_key_report`. |

---

## Build

Run from the repo root or from this directory:

```bash
cargo build -p rushi-common
cargo test  -p rushi-common
```

The crate ships one example:

```bash
cargo run -p rushi-common --example est_probe -- <events.jsonl> [cpts]
```

It probes `estimate_from_events` on a real session log.

---

## Design

This crate holds **what** the loop knows.
It does not hold **how** any stage runs.

- The `event` module defines the wire format.
  Every line in `events.jsonl` is one `Event`.
  The enum is internally tagged on `"type"`.
  The serialized form is a flat JSON object with no wrapper.

- The `logline` module owns the atomic-append guarantee.
  No `impl Write` on `LogLine`.
  Only `commit` writes to the log.
  Concurrent appends from any number of processes
  serialize on the lock.

- The `compact_math` module is I/O-free.
  It operates on projected event values and token measurements.
  The harness loop and the `compact` binary both call into it.
  It keeps no persistent state file.

- The `stage` module defines the `StageRunner` trait.
  The loop state machine depends on this trait, not on process handles.
  Phase 2 ships one implementation.
  That is subprocess spawn of the stage binaries.
  Phase 3 moves this trait to a shared `core` crate
  with the state machine.

- The `hooks` module implements the lifecycle-window dispatcher.
  A hook is a short-lived command on a path.
  The harness spawns it at a fixed window.
  It feeds one JSON object on stdin.
  It reads one JSON decision from stdout.
  The hook exits 0 or 2.

- The `rewind` module computes the active path
  over a prefix of the log.
  The active path is computed recursively
  through the rewind chain.
  Every nesting depth masks the right spans.
  See `active_ranges` for the definition.

- The `model_settings` module centralizes the
  per-model/global override chain.
  The defaults and the resolution logic
  live in exactly one place.
  A change cannot desync the binaries.

---

## Dependencies

| Crate | Purpose |
|-------|---------|
| `serde` (derive) | Serialize and Deserialize the `Event` enum and payload types. |
| `serde_json` | Parse JSON lines and build hook payload JSON. |
| `toml` | Read the model section from `config.toml`. |
| `libc` | `flock` in `LogLine::commit` for cross-process atomicity. |

---

## Consumers

All stage binaries in `bin/` link the crate.
The main consumer map, from the actual `use` statements:

| Consumer | Primary crate items used |
|----------|--------------------------|
| `bin/claim` | `event` (parse log to derive state) |
| `bin/assemble` | `config_check`, `compact_math::trigger_level_for`, `model_settings`, `rewind` |
| `bin/model` | `model_settings`, `stage::ModelDelta` |
| `bin/parse` | `config_check` (legacy-key check) |
| `bin/route` | `logline::LogLine` |
| `bin/log` | `event`, `logline::LogLine` |
| `bin/compact` | `compact_math`, `model_settings`, `rewind` |
| `bin/user` | `event`, `logline::LogLine` |
| `bin/rushi` | `hooks`, `stage`, `model_settings`, `config_check`, `compact_math` |

Two notes:

- The `hooks` dispatcher is driven by `bin/rushi`.
  The loop fires hooks at each window.
- The `hook_io` payload builders serve the extension side.
  External hook binaries (in `rushi-exts`) link the crate
  and build their stdout JSON with `hook_io`.
  No in-repo binary uses `hook_io`.
- The in-repo `bin/hook-compact` is standalone.
  It links no crate.
