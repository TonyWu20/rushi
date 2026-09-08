# rushi — Unix agent harness (kernel)

A Unix-first agent harness: an event-sourced session log, a one-shot
stage binary per pipeline stage, and a hook-based loop. This repo is
the **kernel** — the loop core, base tools, extension host, and
distribution layer. The TUI front-end lives in a sibling
`rushi-tui` repo; goal-mode tools and hooks live in a sibling
`rushi-exts` repo. Neither is a build dependency of the kernel.

## Layout

- `crates/rushi/` — `rushi-common`: shared types (event log line,
  config, hook ABI, compact math, rewind/fork active-path math).
- `bin/rushi/` — the `rushi` loop binary (`run`, `step`, `setup`).
- `bin/{claim,assemble,model,parse,route,log,compact,user}/` — the
  stage binaries the loop spawns.
- `bin/hook-compact/`, `bin/hook-handoff/` — built-in hook binaries
  registered on the lifecycle windows.
- `bin/rewind-drt/` — the Rust DRT production side for the rewind
  fork spec (see `lean/RewindDrt.lean`).
- `tools/` — the base tool set (`read`, `write`, `edit`, `list`,
  `bash`), each a directory with a `tool.toml` manifest.
- `lean/` — Lean 4 backstop specs: `RushiSpec` (the `setup` tool-set
  resolver), `RewindSpec` (the fork active-path recursion), and the
  `RewindDrt` differential-random-test model executable.
- `schemas/events/v1/` — the JSON Schema vocabulary for the session log.
- `docs/` — specs, reviews, and the doc index (`docs/INDEX.md`).
- `config.toml` / `config-low.toml` — kernel-only default configs.
  `config-exts.example.toml` shows the sibling `rushi-exts` wiring.

## Build

The Nix flake is the primary path on flake-managed machines:

    nix develop .#dev        # Rust toolchain on PATH
    cargo build

Plain path: `cargo build` then `bash install.sh` (copies `rushi` to
`$PREFIX/bin`).

## Gate

The house gate, run from the repo root:

    cargo build
    cargo test
    bash scripts/verify-specs.sh
    bash scripts/lean-gate.sh
    bash scripts/e2e-rewind.sh
    bash scripts/compact-e2e.sh
    bash scripts/tool-conformance.sh

`scripts/lean-gate.sh` runs `lake build` over the Lean specs and
fails on any `sorry`. `scripts/rewind-drt-e2e.sh` differentially
random-tests the fork active-path math between the Lean model
executable and `bin/rewind-drt`.

`scripts/cache-e2e.sh` requires a live model API key
(`DEEPSEEK_API_KEY`) and skips when it is absent.

## Docs

Start at `docs/INDEX.md`: repo state, the doc inventory, and the
reading order for a new session.

## Distribution

See `docs/harness-distribution.md`: `rushi setup` initializes a
project from `rushi.toml`, `rushi.lock` pins the kernel commit and
external tool/extension sources, and global install plus per-project
registration follow the distro split.
