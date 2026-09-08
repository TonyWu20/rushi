# rushi — Rust + Unix agent harness (kernel)

`rushi` = Rust + Unix philosophy + sushi (my favorite food, ;)).

This is a hobby-driven project to experiment my
idea on agent harness. Highly personal, not battery-included and out-of-the-box
for everyone.

## Foreword

Heavily inspired by [pi](https://github.com/earendil-works/pi)
and [deepseek-harness](https://github.com/deepseek-ai/deepseek-harness), but
goes deeper and simpler, adhere to the Unix philosophy: do one thing well, and
work together. The core breaks down to: an event-sourced session log, a one-shot
stage binary per pipeline stage, and a hook-based loop. This repo is
the **kernel** — the loop core, base tools, extension host, and
distribution layer.

The TUI front-end lives in a sibling
`rushi-tui` repo. It is not a build dependency of the kernel.
And it is not even the necessary component for driving a session: you can use
`user` to send your message and start the loop in CLI. Of course, that is the
"emergency mode" usually if only you break your TUI. But it also demonstrate the
charm of our Unix-style architecture.

## Layout

- `crates/rushi/` — `rushi-common`: shared types (event log line,
  config, hook ABI, compact math, rewind/fork active-path math).
- `bin/rushi/` — the `rushi` loop binary (`run`, `step`, `setup`).
- `bin/{claim,assemble,model,parse,route,log,compact,user}/` — the
  stage binaries the loop spawns.
- `bin/hook-compact/` — built-in hook binary
  registered on the lifecycle windows.
- `verification/rewind-drt/` — DRT verification tooling (not part of
  the rushi runtime): the Rust production-side executable that the
  gate compares against the Lean model `lean/RewindDrt.lean`.
- `tools/` — the base tool set (`read`, `write`, `edit`, `list`,
  `bash`), each a directory with a `tool.toml` manifest.
- `lean/` — Lean 4 backstop specs: `RushiSpec` (the `setup` tool-set
  resolver), `RewindSpec` (the fork active-path recursion), and the
  `RewindDrt` differential-random-test model executable.
- `schemas/events/v1/` — the JSON Schema vocabulary for the session log.
- `docs/` — specs, reviews, and the doc index (`docs/INDEX.md`).
- `config.toml` / `config-low.toml` — kernel-only default configs.
  `config-exts.example.toml` shows the sibling `rushi-exts` wiring.

## Extensions (OS + Applications model)

The harness is a **distribution** with two layers.

- **Tier 1 — the kernel:** the agent-loop core
  (`assemble`, `model`, `parse`, `route`, `user`, `log`,
  `claim`, `compact`), the base tools under `tools/`, and the
  growth machinery (the `tools/` directory, `route` execution,
  the extension host). Without these layers the distribution
  stops.
- **Tier 2 — the default front-end:** the TUI and its
  `ui_extensions` JSON-render layer. It is swappable. The
  kernel runs without it.

Everything past the two tiers is an **application**. It is
project-specific and installs per-project. It is not a build
dependency of the kernel.

Registration: a tool or extension registers by sitting on the
agent-visible PATH, the `tools/` directory. `route` and `bash`
search that directory. No manifest index. No registry. No
install step.

Self-documentation: each tool documents itself via `--help`.
No `SKILL.md` exists. A multi-step procedure is a short-lived
script, not a doc file.

Cache rule: `assemble` renders the tool list into the prompt prefix
from the discovered manifests. The prefix stays byte-stable in
steady state. A tool-set or fragment change costs one cache
rebuild. `tools --list` remains a TUI catalog and human aid.

Trust model: a tool is trusted because it is in the reviewed
tree. Being on the path makes it runnable. `route` applies caps
and a timeout. A failure reports to the agent. It never hangs.

See `docs/skill-remapped-to-os-apps.md` for the full spec.

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
executable and `verification/rewind-drt`.

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
