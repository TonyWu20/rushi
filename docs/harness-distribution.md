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
binary, the config schema, and the flake. Per-project
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

- **Nix (primary):** `flake.nix` in the repo. It builds `rushi` and
  the kernel stage binaries for the host's `stdenv` and exposes them
  through the devShell. The author's machines are flake-managed
  (x86_64-linux and aarch64-darwin), so the flake is the first-class
  path. The flake builds per host. The `rushi.lock` pins source
  revisions, not build artifacts.
- **Plain (deferred, for friends):** `install.sh` in the repo. It
  runs `cargo build --release` and copies `rushi` to `$PREFIX/bin`.
  No Nix. No homebrew. A friend clones the repo, runs `install.sh`,
  then runs `rushi setup` per project. Deferred: it lands when the
  harness is ready for other users to try.

Both paths produce the same binary.

## 7. Per-project layout

After `rushi setup` runs in a project:

```
my-project/
  rushi.toml        # declarative selection (source of truth)
  rushi.lock        # pinned sources: kernel commit, tool commits, hashes
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

## 10. Improving a base tool: local mask, not a kernel fork

The model already answers whether setup can improve an existing tool.
A base tool is a kernel application. A project improves it by adding
a same-name tool to its own `tools/` dir. The agent-visible path is
searched project-first. The local copy masks the kernel default.
This is the Unix `$PATH` first-match rule.

Fork and branch are source-control choices, not harness choices. The
harness registers by directory name. It does not read git history.
The project only needs one tool dir on the visible path.

Pick the source-control shape by ownership:

- **You do not own the tool repo.** Fork it. Improve the tool on the
  fork. Have the project fetch from the fork. Merge back upstream
  only when you can.
- **You own the tool repo.** Branch it. Commit the improvement. Push.
  The project consumes the branch.
- **You need it for one project only.** Add the improved tool dir to
  that project. No fork or branch.

In every case the harness sees one thing: a `tools/<name>/` dir on the
visible path. Git ownership, merge, and sync stay outside the harness.

One rule keeps this safe. `rushi setup` is additive. It adds any tool
named in `rushi.toml` that is missing from the local `tools/`. It
never overwrites or deletes a project-authored tool. A masked base
tool therefore survives repeated setup. This extends P6.

The omakase loop is the same idea. The agent hardens a working
improvement into a permanent, versioned tool dir. That dir is the
permanent piece.

## 11. Capability boundary

The friction the design must prevent: a promised tool fails to resolve
its path, and the agent cannot execute the job. The invariant is one
line: the agent can always use the tools the harness promises.

The Unix answer is composition. The `bash` tool gives the agent access
to the user shell. It can run `which jq` or `ls /usr/local/bin`.
The harness does not need a declaration file. It does not need a
catalog of the user environment.

The harness promises one set of tools. The `tools/` directory,
registered by `rushi.toml` and listed by `tools --list`, is
deterministic, versioned with the project, and always resolvable.
Everything else on the user's PATH belongs to the user. The agent
reaches it through `bash` composition. The harness does not model,
catalog, or version the user environment. Which tools the agent
discovers and composes is the agent's business, not the harness'.

The `bash` tool is the bridge. It is a promised tool whose capability
extends into the user environment.

### Restrictions: hooks, not allowlists

If a project blocks a tool or a bash pattern, it registers a
`tool.before` hook. The hook reads the pending `bash` command, matches
a pattern, and returns `block` with a reason. The loop synthesizes a
`tool_result` error. The agent sees the reason and adapts.

Existing examples from the pi extension port:

- `no-find-grep` blocks bare `find` and `grep` in `bash` commands.
  It steers the agent to `fd` and `rg`.
- `no-bare-python` blocks bare `python` and `python3`. It steers the
  agent to `uv run`.

See `docs/loop-lifecycle-hooks.md` for the hook ABI. See
`docs/pi-extension-port-investigation.md` for the port notes.

The harness does not auto-install user tools. A missing tool is a
user decision. The agent may request an install via `bash`. The
project may block that request with a hook.

### What this design does not do

It does not sandbox the shell. It does not block `bash` from probing
the user environment. It does not maintain a separate external-tool
catalog. The boundary is a promise, not a fence.

## 12. Reproducibility: the lock file

The manifest declares intent. `rushi.toml` says which tools a project
wants. It does not pin the source version. A re-run on a later day or
a different box may pull a newer kernel commit. The tool set drifts.
The agent's capability set is no longer reproducible.

The fix is a lock file. `rushi.lock` sits next to `rushi.toml` in the
project repo. It is machine-generated. The user does not edit it by
hand.

### Scope: what gets locked

Three classes of tool and extension exist. Each has a different
pinning story.

- **Kernel-shipped.** The base tools (`read`, `write`, `edit`,
  `list`, `bash`, `goal`) and bundled UI extensions ship inside the
  kernel binary. Their content is fixed by the kernel commit. The
  lock records the kernel commit. No per-tool entry is needed.
- **Project-local.** A tool or extension authored in the project
  repo. It is versioned by the project's git history. The lock does
  not record it. It is already pinned by the project commit.
- **External.** A tool or extension pulled from a fork, a branch, or
  a separate repo. Its source ref can drift between re-runs. The
  lock records the source URL, the commit, and a content hash.

The lock file has one kernel entry and one entry per external source.
Kernel-shipped and project-local items need no entry. They are pinned
transitively.

### Lock format

```toml
# rushi.lock (machine-generated, do not edit by hand)
[lock]
version = "0.1"
kernel_commit = "a3f8c21"

[[external]]
name = "my-tool"
kind = "tool"
source = "git+https://github.com/me/rushi-tools"
commit = "b7e91d4"
sha256 = "41ab..."

[[external]]
name = "statusline-rs"
kind = "ui_extension"
source = "git+https://github.com/me/rushi-exts"
commit = "c8d0e5f"
sha256 = "f39c..."
```

### Commands

`rushi setup` resolves the manifest, writes the lock, and
materializes `tools/`. `rushi setup --locked` reads the lock,
materializes `tools/` to match it, and fails if the lock is stale or
missing.

### Why Cargo.lock, not flake.lock or uv.lock

We adopt the `Cargo.lock` model: one committed lock file pins the
resolved source state. The manifest declares intent. The lock records
reality.

`flake.lock` would work for the Nix path, but the plain path (P4)
has no Nix. `uv.lock` pins registry packages. Our tools are git
checkouts or local dirs. A per-source commit hash plus content hash
fits.

The lock file makes the project's tool set reproducible across boxes.
Two boxes with the same `rushi.toml` and `rushi.lock` produce
byte-identical `tools/`.

## 13. Transferability

The distribution unit is the project repo. `rushi.toml`, `rushi.lock`,
and `tools/` travel together in git. Clone the repo, run
`rushi setup --locked`, and the same tool set materializes.

To share a tool with another project, the tool must be an external
source: a git repo that both projects reference in their
`rushi.toml`. The lock pins it in both. Three sharing classes:

- **Kernel tool.** Pinned by the kernel commit. Every project on the
  same kernel gets the same base tool. No extra step.
- **External source.** A git repo referenced by both projects.
The lock pins the commit and hash in each. `rushi setup --locked`
fetches the same revision in both.
- **Project-local tool.** Lives in one project's `tools/` dir. It
  travels only with that project's git history. To share it, promote
  it to a git source. The lock then covers it.

Gap: a project-local tool has no integrity pin outside git. Copying
the directory by hand passes no check. The fix is to promote the
tool to an external source; a git commit is the integrity pin for
project-local tools.

Permission gaps are git's problem, not the harness'. But the harness
must not hide the failure: a fetch error names the tool, the source
URL, and the git error, and setup stops rather than leaving a
partial `tools/` dir (P10).

Kernel compatibility: the lock pins `kernel_commit` per project. A
tool built for one kernel version might not work with another. An
optional `requires_kernel` field in `tool.toml` (a single optional
string field, checked once at setup; not a versioning scheme) would
let setup reject an incompatible tool. This is a future constraint,
not a current gap.

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
P5. nix-primary: given a flake-managed box, observe the flake devShell
places `rushi` on PATH and the P1 turn completes. The on-PATH set
equals the plain install set.
P6. idempotent-setup: given two consecutive `rushi setup` runs on the
same `rushi.toml`, observe the second leave `tools/` byte-identical to
the first. Setup is non-destructive. It adds missing named tools and
leaves project-authored tools untouched.
P7. tool-shadow: given a local `tools/<name>/` that matches a
base-tool name, observe the agent resolve that tool to the local
dir. Re-running `rushi setup` leaves the local dir byte-identical.
P8. tool-promise: given a project whose `rushi.toml` declares a tool
set, observe every agent call to a promised tool resolve to a working
binary. No call fails because the tool cannot be found.
P9. reproducible-setup: given a committed `rushi.lock`, observe two
`rushi setup --locked` runs on different boxes produce byte-identical
`tools/` dirs. A kernel update without a lock refresh makes
`--locked` fail.
P10. explicit-failure: given a `rushi.toml` that names a tool from
a private repo the user cannot access, observe `rushi setup` fail
with an explicit error. The error names the tool and the git error.
Setup does not skip the tool or leave a partial `tools/` dir.

## Verification

| P# | Property | Proof | Status |
|----|----------|-------|--------|
| P1 | kernel-on-path | e2e: install to a scratch `$PREFIX`, run `rushi` from a second directory | open |
| P2 | declarative-registration | test: give a manifest, assert the materialized `tools/` set equals the declared set | open |
| P3 | per-project-isolation | e2e: two fixture projects with disjoint selections, each resolves only its own tools | open |
| P4 | no-nix-required | e2e: a cargo-only container, run `install.sh`, complete the P1 turn | open |
| P5 | nix-primary | e2e: the flake devShell places `rushi` on PATH and completes the P1 turn. The on-PATH set equals the `install.sh` on-PATH set | open |
| P6 | idempotent-setup | test: two `rushi setup` runs, assert `tools/` is byte-identical and project-authored tools stay untouched | open |
| P7 | tool-shadow | e2e: with a local `tools/read/` override, a session call to `read` runs the local binary, not the kernel one. A second `rushi setup` leaves the local dir untouched | open |
| P8 | tool-promise | e2e: a session calls every declared tool. Every call resolves and executes. No call returns a not-found error | open |
| P9 | reproducible-setup | e2e: two boxes share `rushi.toml` and `rushi.lock`. `rushi setup --locked` on each produces byte-identical `tools/`. A kernel update without a lock refresh makes `--locked` fail | open |
| P10 | explicit-failure | e2e: a `rushi.toml` names a tool from a private repo. The user has no access. `rushi setup` fails with an error naming the tool and the git error. No partial `tools/` dir | open |

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
setup` are not built. Prerequisites are named per property. P6 and P7
gate the tool-shadow rule. P8 gates the tool-promise invariant. P9
gates reproducibility. P10 gates explicit failure on fetch errors. `rushi setup` materializes the full declared
set. Every promised tool resolves to a working binary. `rushi.lock`
pins the resolved source state. Restrictions on what the agent may
invoke are expressed through `tool.before` hooks, not through the
distribution layer.

```
cargo build
cargo test
scripts/tool-conformance.sh
scripts/cache-e2e.sh
scripts/install-e2e.sh
```
