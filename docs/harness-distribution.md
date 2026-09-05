# Rushi: distribution and per-project setup

Status: Spec, not yet built (2026-09-13). Design only. No code yet.

Seeded by a portability gap. The harness resolves its tools by local
PATH exports (the `.envrc` in the tree). It cannot run from another
directory. The fix is a distribution layer, not a new architecture.
This doc names that layer and freezes its properties.

## Naming

The project and published binary are named `rushi`. The name combines
Rust (the implementation language) and sushi (the structural metaphor).
The event log is the shari (rice): the append-only JSONL base that
everything sits on. The detached UI is the neta (topping): swappable,
distinct from the core. Self-evolution is the omakase: the agent loops
through options and hardens what works into a permanent piece.

## 1. The frame: a distribution, not an app

The harness is already a distribution (docs/skill-remapped-to-os-apps.md).
It has a kernel (Tier 1), a swappable front-end (Tier 2), and
per-project applications. Publishing adds one layer. That layer is a
distribution layer. It moves the kernel from "built in this directory"
to "installed on a box, then registered per project." Nothing else
changes.

## 2. One kernel repo

The kernel is a single git repo. Its components share `crates/common`
and evolve in lockstep. The loop, `route`, and tool discovery are one
mechanism. Split the kernel into multiple repos and the mechanism
breaks. The repo ships: the loop stage binaries, the base tool
binaries, the extension host, the TUI, the `rushi` entry-point
binary, the config schema, and an optional flake. Per-project
applications do not ship here. They live in the project repo.

## 3. Command structure

The published binary is `rushi`. It has one subcommand and one default
behavior:

- `rushi` or `rushi tui` → open the TUI (default)
- `rushi setup` → initialize a project from `rushi.toml`

The user does not run the loop stages by hand. The TUI supervises the
opaque loop command. `rushi setup` is the only non-TUI operation the
user runs.

## 4. Global install, per-project registration

Two tiers, the distro split:

- **Global (once, on PATH):** the `rushi` binary. Install to `$PREFIX`
  (default `~/.local/bin`). Shared by every project. Like `/usr/bin`.
- **Per-project (in the project repo):** `rushi.toml`, `config.toml`,
  the `tools/` directory, and `ui_extensions/`. Versioned with the
  project. Like `/etc` plus `/opt` for one service.

## 5. The declarative manifest

One file at the project root: `rushi.toml`. It names which components
a project wants.

```toml
# rushi.toml
[rushi]
version = "0.1"

[tools]
enabled = ["read", "write", "edit", "list", "bash", "goal"]

[ui_extensions]
enabled = ["statusline-rs", "mermaid"]

[loop]
compact_reserve_tokens = 16384
```

`rushi setup` reads this file. It copies or symlinks the selected tools
into `./tools/`. It writes `.envrc` for PATH wiring. It generates
`config.toml` from the kernel defaults merged with the project
overrides. The result is plain files on the project path. No registry.
No daemon.

## 6. Two install paths, one result

- **Plain (default, for friends):** `install.sh` in the repo. It runs
  `cargo build --release` and copies `rushi` to `$PREFIX/bin`. No Nix.
  No homebrew. A friend clones the repo, runs `install.sh`, then runs
  `rushi setup` per project.
- **Nix (optional, for Nix users):** `flake.nix` in the repo. It
  builds the same binaries. It exposes a devShell. It is a parallel
  path. It is not a requirement. Both paths produce the same binary.

## 7. Per-project layout

After `rushi setup` runs in a project:

```
my-project/
  rushi.toml        # declarative selection (source of truth)
  config.toml       # generated loop config
  .envrc            # generated PATH wiring (direnv)
  tools/
    read/
      tool.toml
      read
    write/
      ...
    my-tool/        # project-specific, versioned here
      tool.toml
      my-tool
  ui_extensions/
    statusline.json
```

## 8. What publishing does not change

The approved model stands. A tool registers by being on the agent
visible path. It self-documents via `--help`. There is no SKILL.md. The
catalog is `tools --list`. The prompt prefix never mutates. A fenced
failure reports. It does not hang. Publishing adds the distribution
layer only.

## 9. Scope

No marketplace. No global extension registry. The distribution is a
git repo plus an install command. The users are the author and a small
group of friends. The harness is Unix-first by design. It uses the bash
tool, pipes, PTY, and POSIX paths. Windows is a separate porting
effort. It is not a target now.

## Properties

P1. kernel-on-path: given `rushi` installed to `$PREFIX/bin`, observe
the TUI launches from any directory and the loop completes one turn.
P2. declarative-registration: given a `rushi.toml` that names a set
of tools, observe `rushi setup` materialize `tools/` with exactly
those `tool.toml` entries and no others.
P3. per-project-isolation: given two projects with disjoint tool
selections in their `rushi.toml`, observe each resolve its own set.
A tool in one project is not callable from the other.
P4. no-nix-required: given a box with cargo and bash only, observe
`install.sh` place `rushi` on PATH and the P1 turn completes.
P5. nix-optional: given a Nix box, observe the flake devShell exposes
the same `rushi` binary. The on-PATH set equals the plain install set.
P6. idempotent-setup: given two consecutive `rushi setup` runs,
observe the second leave `tools/` byte-identical to the first.

## Verification

| P# | Property | Proof | Status |
|----|----------|-------|--------|
| P1 | kernel-on-path | e2e: install to a scratch `$PREFIX`, run `rushi` from a second directory | open |
| P2 | declarative-registration | test: give a manifest, assert the materialized `tools/` set equals the declared set | open |
| P3 | per-project-isolation | e2e: two fixture projects with disjoint selections, each resolves only its own tools | open |
| P4 | no-nix-required | e2e: a cargo-only container, run `install.sh`, complete the P1 turn | open |
| P5 | nix-optional | e2e: the flake devShell on-PATH set equals the `install.sh` on-PATH set | open |
| P6 | idempotent-setup | test: two `rushi setup` runs, assert `tools/` is byte-identical | open |

Inherited from skill-remapped-to-os-apps.md (not re-proven here):
self-doc (P2 there), catalog (P3 there), no-prompt-mutation (P4 there),
fenced-failure (P5 there).

## Lean applicability

The Lean language kernel is not the authority for this spec. This spec
governs process and layout (install paths, manifest semantics, PATH
conventions). It is not a pure algorithm. The Lean kernel proves code
properties. It cannot prove "the install script puts the binary on
PATH and the loop starts." The proof authority is the house gate.
That gate is `cargo build` plus the conformance and e2e scripts. See
docs/lean-driven-development.md.

One component is Lean-checkable. The `rushi.toml` to tool-set resolver
is a pure function. A Lean theorem can state "the resolver returns
exactly the declared set." The default proof is a conformance test.
Lean is an optional backstop, not the gate.

## Gate

Gate: blocked. The `rushi` binary, the manifest schema, and `rushi
setup` are not built. Prerequisites are named per property.

```
cargo build
cargo test
scripts/tool-conformance.sh
scripts/cache-e2e.sh
scripts/install-e2e.sh
```
