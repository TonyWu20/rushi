# rushi — Rust + Unix agent harness (kernel)

`rushi` = Rust + Unix philosophy + sushi (my favorite food, ;)).

A hobby-driven project.
It experiments with a Unix-philosophy agent harness.
Each pipeline stage is a one-shot CLI binary.
The ABI is bytes over pipes (JSON on stdin/stdout).
The harness never imports tool or hook code.
The core is an event-sourced,
append-only session log.

Heavily inspired by [pi](https://github.com/earendil-works/pi)
and [deepseek-harness](https://github.com/deepseek-ai/deepseek-harness),
but simpler and stricter about the Unix contract.

This repo is the **kernel**:
the agent-loop pipeline,
base tools,
extension host,
hook ABI,
and distribution layer.

| Layer              | Contents                                            | Location                                         |
| ------------------ | --------------------------------------------------- | ------------------------------------------------ |
| Kernel (Tier 1)    | Loop core, base tools, extension host, distribution | this repo                                        |
| Front-end (Tier 2) | TUI + UI extensions (swappable)                     | `rushi-tui` repo                                 |
| Applications       | Project-specific tools, hooks, extensions           | per-project `tools/`, `hooks/`, `ui_extensions/` |

The kernel has **no build-time dependency** on sibling repos.
`config-exts.example.toml` shows how to wire the kernel to `rushi-exts`
for development.

---

## Pipeline

One agent-loop turn is a pipeline
of one-shot stage binaries:

```
rushi run <session>
  ┌─────────────────────────────────────────────────────────┐
  │  claim → assemble → model → parse → route → log        │
  └─────────────────────────────────────────────────────────┘
```

| Stage      | Binary         | Responsibility                                                  |
| ---------- | -------------- | --------------------------------------------------------------- |
| `claim`    | `bin/claim`    | Acquire session lock, select next user message (or idle claim). |
| `assemble` | `bin/assemble` | Project the session log into a `ModelRequest` JSON.             |
| `model`    | `bin/model`    | Call the LLM. Stream the response.                              |
| `parse`    | `bin/parse`    | Parse the model response into tool-call actions.                |
| `route`    | `bin/route`    | Execute tool calls in parallel. Apply caps.                     |
| `log`      | `bin/log`      | Append events to `events.jsonl`.                                |
| `compact`  | `bin/compact`  | Run a summary call. Write handoff doc.                          |
| `user`     | `bin/user`     | Read user message. Emit `user_message` event.                   |

The `rushi` binary (`bin/rushi`) orchestrates these stages.
It handles cancellation,
manages the session lock,
and provides the CLI entry point.

Subcommands:

| Command                                      | Purpose                                |
| -------------------------------------------- | -------------------------------------- |
| `rushi` _(default)_ or `rushi tui [SESSION]` | Launch the TUI                         |
| `rushi setup [--locked]`                     | Initialize a project from `rushi.toml` |
| `rushi run SESSION`                          | Run the full turn loop                 |
| `rushi step SESSION`                         | Run a single step                      |
| `rushi docs [SECTION\|DOC]`                  | Print the embedded harness reference   |

---

## Layout

- `crates/rushi/` — `rushi-common`: shared types
  (typed `Event` enum,
  config,
  hook ABI,
  compact math,
  rewind/fork active-path math).
- `bin/rushi/` — the `rushi` loop orchestrator
  and CLI (`run`, `step`, `setup`, `docs`, `tui`).
- `bin/{claim,assemble,model,parse,route,log,compact,user}/`
  — stage binaries the loop spawns.
- `bin/hook-compact/` — built-in hook binary
  registered on `overflow.resolve` and `exhausted.handle`.
- `tools/` — base tools (`read`, `write`, `edit`, `bash`),
  each a directory with a `tool.toml` manifest.
- `lib/` — Nix flake modules
  (`mkRushi`, `fetchExt`, `fetchTool`,
  NixOS/home-manager modules, option schema).
- `docs/` — specs, reviews, and the doc index (`docs/INDEX.md`).
- `docs/reference/` — the stable harness reference,
  embedded in the binary via `rushi docs`.
- `config.toml` / `config-bash-only.toml` —
  kernel-only default configs.
  `config-exts.example.toml` shows the `rushi-exts` wiring.
- `rushi.toml` / `rushi.lock` —
  declarative project manifest and lock.
- `scripts/` — gate scripts and e2e test suites.
- `examples/rushi-config/` — example consumer flake
  for `lib.mkRushi`.

---

## Base Tools

| Tool    | Directory      | Notes                                      |
| ------- | -------------- | ------------------------------------------ |
| `read`  | `tools/read/`  | Read a file. Supports `offset`/`limit`.    |
| `write` | `tools/write/` | Write content to a file.                   |
| `edit`  | `tools/edit/`  | String replacement in a file.              |
| `bash`  | `tools/bash/`  | Run a shell command. 60 s default timeout. |

Tools are one-shot processes. The contract:

- **stdin:** one JSON object (the tool call args).
- **stdout:** one JSON object (the result).
- **stderr:** diagnostics. On non-zero exit,
  forwarded to the model as an error.
- **Exit code:** `0` success, non-zero failure.

A tool registers by sitting on the agent-visible PATH
(the `tools/` directory).
`route` and `bash` search that directory.
No registry. No install step.
Each tool self-documents via `--help`.

See `docs/reference/README.md` §5 for the full
tool manifest and execution contract.

---

## Hooks

A hook is a short-lived command on a path.
The harness spawns it at a lifecycle window.
The ABI is bytes over pipes.

| Window                          | Decisions                      | Default           |
| ------------------------------- | ------------------------------ | ----------------- |
| `session.start` / `session.end` | _(obs. only)_                  | —                 |
| `step.start` / `step.end`       | _(obs. only)_                  | —                 |
| `model.before`                  | `transform`                    | proceed unchanged |
| `model.after`                   | _(obs. only)_                  | —                 |
| `compact.before`                | `proceed`, `cancel`, `replace` | proceed           |
| `compact.after`                 | _(obs. only)_                  | —                 |
| `overflow.resolve`              | `stay_compact`, `stop`         | `stay_compact`    |
| `exhausted.handle`              | `stay_compact`, `stop`         | `stay_compact`    |
| `tool.before`                   | `proceed`, `block`, `approve`  | `proceed`         |
| `tool.after`                    | _(obs. only)_                  | —                 |
| `run.idle`                      | `stop`, `continue`             | `stop`            |

The built-in `bin/hook-compact` is a reference implementation.
See `docs/reference/README.md` §6 for the full hook ABI.

---

## Session Log

The session log is `sessions/<name>/events.jsonl`,
append-only JSONL.
All state is a projection of this log.
**Model-visible means logged.**

Events are validated by the typed `Event` enum
in `crates/rushi` (`docs/typed-events.md`).
The log stage assigns each event a `seq` number.

Key event types: `user_message`, `assistant_message`,
`tool_call`, `tool_result`, `compaction_summary`,
`compaction_started`, `compaction_failed`,
`context_exhausted`, `approval_request`, `approval`,
`rewind`, `ext_status`, `error`.

---

## Build

### Nix (primary path)

On a flake-managed machine:

```bash
nix develop .#dev        # Rust toolchain on PATH
cargo build
```

The flake (`flake.nix`) also exposes `lib.mkRushi`
for declarative rushi config from consumer flakes.
See `docs/reference/nix/nix-flake-module.md`
for the option schema.

### Plain cargo

```bash
cargo build --release
bash install.sh          # copies rushi to $PREFIX/bin
```

`install.sh` is a deferred path for non-Nix users.
The Nix flake is the primary install path.

---

## Gate

The house gate, run from the repo root:

```bash
cargo build
cargo test
bash scripts/verify-specs.sh
bash scripts/e2e-rewind.sh
bash scripts/compact-e2e.sh
bash scripts/tool-conformance.sh
```

More e2e suites: `crash-e2e.sh`, `install-e2e.sh`,
`model-before-transform-e2e.sh`,
`run-idle-log-message-e2e.sh`,
`steer-inflight-e2e.sh`,
`tool-after-transform-e2e.sh`.

CI (`.github/workflows/ci.yml`) runs the gate
on push/PR across `x86_64-linux`,
`aarch64-linux`, and `aarch64-darwin`.

The Lean backstop was retired 2026-09-17.
Invariants are proven in Rust
(unit tests + e2e scripts).

`scripts/cache-e2e.sh` requires a live model API key
(`DEEPSEEK_API_KEY`) and skips when it is absent.

---

## Configuration

- **`config.toml`** — kernel config: model, limits,
  hooks, paths, system prompt, TUI settings.
- **`rushi.toml`** — declarative project manifest.
  `rushi setup` reads it to materialize tools,
  write `rushi.lock`, `.envrc`, and `config.toml`.
- **`rushi.lock`** — pins the kernel commit
  and external source revisions.
- **`config-exts.example.toml`** — shows how to wire
  the kernel to the sibling `rushi-exts` repo.

See `docs/reference/README.md` §4
for the full key reference.

---

## Docs

Start at `docs/INDEX.md`:
repo state,
doc inventory,
and reading order for a new session.

The stable harness reference lives in
`docs/reference/README.md`
and is embedded in the `rushi` binary:

```bash
rushi docs              # full reference
rushi docs <section>    # one section
rushi docs --list       # list sections and bundled docs
rushi docs nix-flake-module
rushi docs ext-flake-authoring
```

Key docs:

- `docs/architecture.md` — hexagonal architecture,
  phase roadmap, tool contract, guardrails.
- `docs/refinement-policy.md` — rules for any change:
  evidence bar, trigger thresholds, not-yet list.
- `docs/lean-driven-development.md` — spec-driven workflow:
  properties before code, proof per property,
  build gate as acceptance authority.
- `docs/coding-conventions.md` — standing code rules
  for the Rust.

---

## Distribution

`rushi setup` initializes a project:

- Reads `rushi.toml` from the project root.
- Materializes selected kernel tools into `./tools/`.
- Writes `rushi.lock`
  (pins kernel commit + external source revisions).
- Writes `.envrc` (PATH wiring via direnv).
- Generates `config.toml` from kernel defaults
  merged with project overrides.
- `--locked` verifies against an existing `rushi.lock`.

Full distribution model: `docs/harness-distribution.md`.

---

## Sibling Repos

| Repo          | Contents                                 |
| ------------- | ---------------------------------------- |
| `rushi-tui`   | TUI binary, UI extensions, TUI docs      |
| `rushi-exts`  | Goal tools, goal hooks, extension hooks  |
| **this repo** | Kernel: loop, base tools, extension host |

The kernel does not depend on the siblings at build time.
