# TUI and TUI-extensions repo split — research and plan

Status: research complete. The in-tree enablers are implemented
(section 4, items 4–6: `EXTS_ROOT` in `scripts/tui-pty-smoke.py`
and `scripts/ext-env.sh`, plus the quit-budget fix in the same
smoke script). The split itself is not executed: the new
repos are created when the work starts, and this doc fixes the
boundary so both move in one pass.

Prerequisite (machine-local): the plain cases open the repo's
`sessions/tui-test` session, and the scroll-burst case asserts two
markers from its `events.jsonl` (head "This is the first time",
tail "Want me to fix #1 and #2"). `sessions/` is gitignored, so
that fixture is machine-local state, not a tree file; when it is
missing the scroll-burst case fails "tail marker missing at
startup" until it is re-seeded.

Seeded by the distribution layer (docs/harness-distribution.md):
the kernel is one git repo, the TUI is the Tier-2 swappable
front-end. This doc answers two questions:

1. Can TUI development move to a dedicated git repo?
2. What changes separate the TUI and the TUI-extensions into
   different repos, while the TUI sources the extensions back at
   runtime?

## 1. Current architecture (the research)

The kernel repo (this tree) is one Cargo workspace
(`Cargo.toml` members): `bin/rushi` (the distribution totem: it
launches the TUI, runs `setup`, `run`, `step`), the loop stage
binaries (`claim`, `assemble`, `model`, `parse`, `route`, `log`,
`compact`), the `hook-*` binaries, the `tools/*` one-shot tools,
`crates/rushi` (crate `rushi-common`: `logline`, `event_validation`,
`compact_math`, `stage`, `hooks`), and `crates/goal-state`.

**The TUI is a separate binary with a process boundary.**
`rushi` spawns it: `resolve_tui_binary` (`bin/rushi/src/main.rs`
section 123) finds the `tui` executable next to the `rushi`
executable, else on `PATH`. The TUI source holds no loop
internals: the loop is the opaque `[loop]` config command
(docs/tui.md section 2.3). The TUI's kernel code surface is exactly
one path dependency (`bin/tui/Cargo.toml`):

- `rushi-common` — the TUI uses `logline::LogLine`,
  `event_validation` (producer-side validation against
  `schemas/events/v1`), and `stage::ModelDelta` (the stream
  delta lines `bin/model` emits).

(The goal UI used to be the second dependency: the TUI read the
session's goal files for the goal chrome. That coupling is
removed — the host exposes a generic `row` capability
(docs/ui-extension.md section 4) and the `goal` extension owns
the slot; `goal-state` is a dependency of `ui_extensions/goal`,
the goal hooks, and the goal tools, never of the TUI.)

**The extension host is inside the TUI binary but is
directory-agnostic at runtime** (`bin/tui/src/ext.rs`). An
extension is an external process; one JSONL boundary over stdio;
the TUI spawns, supervises, and collects styled replies. "No
extension code compiles into the TUI." Discovery is host-fixed
and purely on-disk:

- layer 1: built-in renderers (host code),
- layer 2 (global): `[ext] dir` from `config.toml`, resolved
  against the config dir, defaulting to `<config_dir>/ui_extensions`
  (`bin/tui/src/config.rs`, `bin/tui/src/ext.rs` `discover`),
- layer 3 (project): `<config_dir>/.pi/ui_extensions`, which
  overrides a global entry by name.

Manifests are `ext.toml`; `command` resolution is host-owned: a
bare name resolves on the TUI process's `PATH`, a relative path
against the entry directory (so a bundled `target/debug/<ext>`
binary needs no `PATH` export), an absolute path as-is. Missing
commands and malformed manifests refuse the start (fail-loud).
The `protocol_v` manifest field is checked before spawn: a
mismatch skips the extension with a flash; the host speaks one
version (`PROTOCOL_V = 1`). Restart budget 1 s / 2 s / 4 s, then a
dead hint; per-op G5 fallbacks keep the TUI up.

**The extension layers live in this repo today:**

- `ui_extensions/` — the global reference layer: `statusline`,
  `notify`, `frame` (bash, zero compiled binaries) and `mermaid`,
  `goal` (standalone cargo packages with an empty `[workspace]`
  table — the isolation pattern this plan reuses; the host
  resolves their binaries by entry-relative path).
- `ext-rs/` — the Rust ports (`statusline-rs`, `tool_result-rs`,
  `notify-rs`): bare command names resolved on `PATH` via
  `scripts/ext-env.sh` and the `.envrc` exports.
- `ui_extensions-demos/` — opt-in demo layers (`tool_result`,
  `cmd_palette`), activated by pointing `[ext] dir` at them.
- `scripts/ext-fixture/` — broken-behavior layers (`dying`,
  `badjsonl`, `append-reject`, `stub`, `frame`) that are test
  *inputs to the host*, consumed by the PTY smoke.

**Cross-layer compile-time coupling is exactly one.**
`ui_extensions/goal/Cargo.toml` names
`goal-state = { path = "../../crates/goal-state" }`. Every other
extension entry depends only on published crates
(`mermaid-text`, `serde`, `serde_json`, `toml`).

**Distribution and dev wiring:**

- `flake.nix` is the canonical build path: the `rushi` package
  builds the whole workspace (`cargoBuildFlags = ["--workspace"]`),
  which includes `tui` and `tui-stream-drt`. `install.sh` is the
  deferred plain path and copies only `rushi` to `$PREFIX/bin` —
  `resolve_tui_binary` then falls back to a `tui` on `PATH`.
- `rushi setup` + `rushi.lock` already model external extension
  sources: `[[external]]` entries with `kind = "ui_extension"`,
  a git `source`, a `commit`, and a `sha256`
  (docs/harness-distribution.md section 12). The fetch and
  materialize path for that entry is designed but not yet
  implemented (that doc's Gate section names it as the open
  external-source work).
- Dev-time coupling that keeps TUI and extensions in one tree:
  `scripts/tui-pty-smoke.py` (the acceptance gate of
  docs/ui-extension.md and ui-extension-plan.md; it hardcodes
  `REPO + "/ui_extensions"` / `REPO + "/ext-rs"` / `REPO +
  "/ui_extensions-demos/..."` paths, builds the five Rust
  extension packages in place, and kills stray extension
  processes by their layer-dir cwd prefixes), `scripts/ext-env.sh`
  (builds the five packages from the in-repo root and prints their
  `target/debug` dirs for `PATH`), the `.envrc` `PATH` exports, and
  the Lean DRT pair: `lean/TuiStreamSpec.lean` +
  `lean/TuiStreamDrt.lean` (+ `TuiViewportSpec.lean`) in one lake
  project with the kernel's `RushiSpec`, the std-only Rust mirror
  `bin/tui-stream-drt`, and the generator `scripts/tui-stream-drt-
  inputs.sh`.

## 2. Verdict: a dedicated TUI repo is supported

Yes, with no mechanism work in the TUI binary. The boundaries the
split relies on already exist:

- **Process boundary.** `rushi` spawns `tui`; the two never share
  a compile unit. A TUI repo builds `tui` against the kernel as a
  git dependency and keeps the existing launcher contract
  (`resolve_tui_binary`: side-by-side, then `PATH`).
- **Small, stable code surface.** Two crates (`rushi-common`,
  `goal-state`). Re-point the path dependencies to git
  dependencies pinned in `Cargo.lock` (or publish both crates and
  depend by version). Nothing else crosses the repo line at
  compile time.
- **Runtime boundary is on-disk.** The host discovers extension
  layers from directories (`[ext] dir`, the default config-dir
  layer, the project `.pi` layer). `ext.rs` makes no assumption
  about which git repo a layer came from. "The TUI sources the
  extensions back at runtime" is therefore already true of any
  checkout: point the layer at the exts repo and build the Rust
  entries in place.
- **In-tree precedent.** The extension entries are already
  standalone cargo packages with an empty `[workspace]` table
  (`ui_extensions/mermaid`, `ui_extensions/goal`, `ext-rs/*`).
  The isolation pattern is proven; the split generalizes it.
- **Distribution intent.** docs/harness-distribution.md section 14
  fixes the TUI as a Tier-2 front-end with its own name, and
  section 12 already pins external `ui_extension` sources in
  `rushi.lock` by git source + commit + content hash. A dedicated
  TUI repo and a dedicated exts repo are the natural consumers of
  that lock design.

## 3. Runtime sourcing: what already works, what is new

**Works today (no host changes):**

- Layering: `[ext] dir` (absolute, or relative to the config
  dir) → `<config_dir>/ui_extensions`; the project
  `.pi/ui_extensions/` layer overrides by name. Any checkout of
  the exts repo is a valid global layer.
- Bundled binaries: entry-relative `command` paths keep working
  after the move, because the host resolves them against the
  entry dir; `cargo build` in the exts entry produces
  `target/debug/<ext>` next to the manifest.
- `PATH`-style commands: the `ext-rs` ports and any future
  binary extension; the exts repo owns the `ext-env.sh` / `.envrc`
  wiring for them.
- Versioning across repos: the `protocol_v` handshake (skip and
  flash on mismatch, docs/ui-extension.md P3). The host stays the
  single protocol owner; the exts repo declares the host
  protocol it targets in every `ext.toml` and pins the host
  revision it tested against in CI.

**New at split time (one kernel-side feature):**

- The distribution-side fetch: `rushi setup` resolves an
  `[[external]]` `kind = "ui_extension"` source (git URL + commit
  + `sha256`, docs/harness-distribution.md section 12),
  materializes the named entries into the project layer, and
  builds Rust entries in place (bash entries need no build).
  Until that lands, the user flow is: check out the exts repo
  once and point `[ext] dir` at it (development), or copy entries
  into the project `.pi/ui_extensions/` layer (per-project).

**No change needed in `bin/tui/src/ext.rs` for the split.** The
only optional host hardening (not a split requirement): report
the host protocol version in the `tick` / `frame` payloads so a
well-behaved extension can self-degrade against a newer host.
Today the check is one-way (host validates the manifest), which
is sufficient.

## 4. The split: layout and the changes

Three repos; the kernel stays this tree.

| Repo | Contents after the split |
|---|---|
| kernel (this repo) | loop stages, `hook-*`, `tools/*`, `bin/rushi` (with the new `[[external]]` fetch), `crates/rushi`, `crates/goal-state`, `schemas/`, `lean/RushiSpec.lean` (the lakefile sheds the TUI libs), `rushi.toml` / `rushi.lock` design |
| `rushi-tui` (new) | `bin/tui`, `bin/tui-stream-drt`, the TUI half of `lean/` (`TuiStreamSpec`, `TuiStreamDrt`, `TuiViewportSpec` + the lake plumbing they need), `docs/tui*.md`, `docs/ui-extension*.md`, `docs/goal-ux.md`, `scripts/tui-pty-smoke.py`, `scripts/tui-capture.py`, `scripts/tui-stream-drt-inputs.sh`, its own flake (git dep on the kernel for `rushi-common`; Lean toolchain inputs for the DRT gate), its `.envrc` |
| `rushi-exts` (new) | `ui_extensions/`, `ext-rs/`, `ui_extensions-demos/`, `ext-fixture/` (the fixture entries, out of `scripts/`), `scripts/ext-env.sh`, the two READMEs |

Changes, by area:

**A. Dependency re-pointing (at split time).**

1. `bin/tui/Cargo.toml`: the `rushi-common` path dep becomes a
   git dep on the kernel repo (rev pinned in the TUI repo's
   `Cargo.lock`), or a versioned published crate. The TUI no
   longer names `goal-state` — the goal UI moved to the goal
   extension's row slot, so item 2 carries that dependency.
2. `ui_extensions/goal/Cargo.toml`: the one cross-layer path dep
   becomes the same git dep. The exts repo pins the kernel rev in
   its own lock; `goal-state` stays a kernel-owned source of
   truth (never vendored into the exts repo).
3. `bin/tui-stream-drt`: std-only, moves as-is.

**B. In-tree enablers (implemented in this worktree; defaults
behave exactly as today).**

4. `scripts/tui-pty-smoke.py`: an `EXTS_ROOT` environment
   override (default: the `REPO` argument) re-points every
   ext-repo-owned path: the five Rust package dirs
   (`ui_extensions/mermaid`, `ui_extensions/goal` build inputs,
   `ext-rs/{statusline,tool_result,notify}-rs`), the
   `ui_extensions/` + `ui_extensions-demos/` layer dirs, and the
   stray-process kill prefixes. Kernel-side paths (sessions,
   the `scripts/ext-fixture/` host-test inputs, temp configs)
   stay under `REPO`. Post-split, CI runs the gate against kernel
   + tui + exts checkouts with `EXTS_ROOT` at the exts checkout.
5. `scripts/ext-env.sh`: the same `EXTS_ROOT` override on the
   build root (default: the tree the script lives in). Today it
   can build a separate exts checkout; post-split the script
   lives in the exts repo and the default is enough.
6. `scripts/tui-pty-smoke.py` `case()` quit budget: the deadline
   was 3.0 s, but the TUI quit path runs the stop sequence with a
   3 s SIGTERM->SIGKILL grace (docs/ui-extension.md section 7),
   so on this box a healthy TUI exits at ~4.5 s and the three
   plain cases failed "still running after double-q (hang)" on
   every run (pre-existing: both pre-edit baselines failed the
   same three cases). The budget is now 8.0 s: above the
   documented grace with margin. The property is unchanged
   ("double-q exits the TUI"), only the test budget moved.

**C. Build / CI wiring (at split time).**

6. Kernel flake: drop `bin/tui` and `bin/tui-stream-drt` from the
   workspace members; the `rushi` package keeps the stages,
   tools, hooks, and crates. Optionally add the two new repos as
   flake inputs for a combined devShell.
7. TUI repo flake: builds `tui` + `tui-stream-drt`; the kernel
   git source feeds the two crate deps; the Lean toolchain
   (fenix / lean4 / z3 / mathlib inputs, the same pattern as the
   kernel flake) runs the DRT gate: `lake build` + `lean-verify`
   `op=drt` against `bin/tui-stream-drt` with
   `scripts/tui-stream-drt-inputs.sh`.
8. Ext repo: no flake required. Bash entries are zero-build; Rust
   entries build per directory with the entry-relative binary
   paths the host already resolves. A small flake is optional.
9. `.envrc`: the kernel keeps `target/release`; the TUI repo adds
   its own `target/release` plus the exts `ext-env.sh` PATH for
   the `ext-rs` binaries.
10. `install.sh` (deferred path; the flake is canonical):
    co-install `tui` next to `rushi` (the launcher prefers the
    side-by-side exe) or ship a TUI-repo installer. On the Nix
    side, the TUI repo exposes the `tui` package.
11. The Lean DRT gate moves with the TUI repo
    (items 7); `scripts/lean-verify-drt-e2e.sh` (the offline test
    of the `lean-verify` tool's DRT machinery, a kernel tool)
    stays in the kernel.

**D. Docs and references (at split time).**

12. `docs/ui-extension.md` section 3 ("a global directory ships
    with the harness") gains the exts-repo wording: the global
    layer is any directory — the installed exts layer or a project
    layer. Section 9 (distribution) gains the exts-repo flow.
13. `ui_extensions/README.md` and `ext-rs/README.md`: paths and
    build instructions.
14. TUI docs move with the TUI code; the kernel keeps
    `harness-distribution.md` and the loop/kernel docs.
15. The TUI config contract (`[ext] dir`, `[loop]`, `[paths]`,
    `[active]`, `[tui]`) is unchanged: the config file stays a
    kernel project file the TUI reads.

## 5. Cross-repo invariants

- The host owns the protocol: `PROTOCOL_V` lives in `ext.rs`;
  `protocol_v` in `ext.toml` is the extension side of the
  handshake; skip-and-flash is the only coupling. An exts
  release pins the host revision it was smoke-tested against.
- `goal-state` is kernel-owned; the exts repo consumes it by git
  dep, pinned per exts lock (the TUI itself has no goal-state
  dependency).
- The event vocabulary (`schemas/events/v1`) and the extension
  JSONL protocol are the shared ABIs, both kernel-owned; every
  repo pins the kernel revision its CI proves against.

## 6. Execution order (one pass)

1. Create `rushi-tui` and `rushi-exts`; move the trees per
   section 4; apply the dependency re-pointing (A1–A3).
2. Land the `[[external]]` `ui_extension` fetch in `rushi setup`
   (the designed-but-unimplemented distribution path).
3. Re-point CI: kernel (workspace minus the TUI), TUI repo (build
   + unit tests + PTY smoke with `EXTS_ROOT` at the exts
   checkout), exts (per-entry builds).
4. Extend the distribution doc's Gate with an exts-reproducible
   property: two boxes with the same `rushi.toml` + `rushi.lock`
   materialize byte-identical `ui_extensions/` from the exts
   source.

## Properties

P1. same-repo-default: given the PTY smoke with `EXTS_ROOT`
unset, observe every ext-side path resolve under the kernel repo
exactly as before the change.

P2. exts-root-override: given `EXTS_ROOT` set to a tree carrying
the ext layers, observe the smoke's package builds, layer loads,
demo copy, and stray-kill prefixes resolve under that tree while
kernel-side paths stay under the `REPO` argument.

P3. env-root-override: given `ext-env.sh` with `EXTS_ROOT` set,
observe the entries build under that root; the default (the
script's own tree) is unchanged and the printed `PATH` line is
identical for identical roots.

P4. host-invariant: given the split, observe no change to
`bin/tui/src/ext.rs`: runtime sourcing is directory-agnostic
(already proven by the layer properties of docs/ui-extension.md,
P1–P3).

## Verification

| P# | Property | Proof | Status |
|----|----------|-------|--------|
| P1 | same-repo-default | `scripts/tui-pty-smoke.py target/debug/tui "$PWD"` with `EXTS_ROOT` unset: all 15 cases OK, "ALL SMOKE CASES PASSED", rc 0 | proven |
| P2 | exts-root-override | the same gate with `EXTS_ROOT="$PWD"` (a same-tree run proving the override plumbing; a true two-repo run lands with the split): all 15 cases OK, rc 0 | proven |
| P3 | env-root-override | `bash scripts/ext-env.sh` vs `EXTS_ROOT="$PWD" bash scripts/ext-env.sh` print the identical colon-joined `target/debug` line (diff clean), rc 0 | proven |
| P4 | host-invariant | no `ext.rs` diff in the split-enabling worktree; layer properties stand from docs/ui-extension.md | proven |

Record (this worktree): the two pre-edit baselines each failed
four cases — the three plain double-q cases (the quit budget, item
6) and scroll-burst (the missing machine-local `sessions/tui-test`
fixture, the prerequisite note above). After the enablers, the
budget fix, and the fixture re-seed: the default run and the
`EXTS_ROOT` run are 15/15; `ext-env.sh` prints the same line in
both modes.

## Gate

```
cargo build
cargo test -p tui
scripts/tui-pty-smoke.py target/debug/tui "$PWD"
EXTS_ROOT="$PWD" scripts/tui-pty-smoke.py target/debug/tui "$PWD"
bash scripts/ext-env.sh
EXTS_ROOT="$PWD" bash scripts/ext-env.sh
```
