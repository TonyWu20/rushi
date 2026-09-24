# `meta.rushi` — Producer-Declared Names for `lib.mkRushi` (issue #13)

Status: implemented (2026-09). Supersedes the build-time discovery
mechanism introduced in issue #10 for the *eval-time* value of
`[paths] extension_tool_paths` and the manifest `[ui_extensions] enabled`
list. The build-time discovery remains as a **fallback** for producers
that have not yet adopted `meta.rushi` and for plain-path sources.

## 1. Motivation

Issue #10 made `lib.mkRushi` fill `config.paths.extension_tool_paths` and
the manifest `[ui_extensions] enabled` list by **globbing the assembled
package at build time**. That design has three defects:

1. **Eval/build divergence.** The value the consumer can inspect with
   `nix eval` (the returned `config` / `configAttrs`) does not match what
   actually ships in `config.toml`, because the shipping value is decided
   later, inside `installPhase`.
2. **Shell-glob source of truth.** The entry names come from a
   `for d in "$out"/tools/*/tool.toml` loop in the build script, so the
   "names" are implicit, untyped, and only observable after a full build.
3. **Untyped names.** Nothing checks that a producer actually ships the
   directory it claims, until the consumer's config points at a path that
   does not exist at runtime.

Issue #13 moves the source of truth to the producer flake: each external
package declares, in standard Nix `meta`, exactly what it provides.

## 2. The `meta.rushi` contract

A producer flake sets **exactly one** of the three fields under
`meta.rushi` on each package it exposes for `rushi.external_*`:

| Package type | `meta.rushi` field | Meaning |
|---|---|---|
| Tool | `entry = "<name>"` | the entry dir name the package ships under `$out/<name>/` (i.e. the dir holding `tool.toml` + `bin/`) |
| UI ext | `ext = "<name>"` | the entry dir name the package ships under `$out/<name>/` (the dir holding `ext.toml`) |
| Hook | `bin = "<binary>"` | the bare hook binary name the package ships (lands in `$out/bin/<binary>`) |

Example (tool):

```nix
wrapAsTool {
  name = "goal";
  toolToml = "${self}/goal-app/goal-tools/goal/tool.toml";
  built = buildCrate { crateDir = "goal-app/goal-tools/goal"; crateName = "goal"; };
  # meta.rushi.entry (issue #13): the tool entry dir this package provides.
  # lib.mkRushi reads it at eval time to derive [paths] extension_tool_paths.
  meta = { rushi = { entry = "goal"; }; };
}
```

The value is the **directory name**, not the config path. `mkRushi`
expands a tool `entry` to the config-relative path `tools/<entry>` (the
copy target of `rushi.external_tools`) before filling the config.

The attribute is inert until a consumer reads it. A producer that adds
`meta.rushi` but whose consumer does not read it changes nothing.

## 3. Eval-time semantics in `lib.mkRushi`

`lib/mk-rushi.nix` (section 1.5 / 2.5) adds:

- **`rushiMeta s`** — accessor. Returns `s.meta.rushi` when the source is
  an attrset that carries it; `{ }` for plain path strings (which cannot
  carry `meta`). A *present but malformed* `meta.rushi` (not an attrset)
  is a hard `throw`, not a silent no-op.
- **`metaToolEntries`** — fold over `rushi.external_tools`. Collects
  `m.entry` from producers that declare it. Sources without
  `meta.rushi.entry` emit a `lib.warn` fallback notice and are left to
  build-time discovery.
- **`metaExtUiNames`** — same for `rushi.external_ui_extensions`,
  collecting `m.ext`.
- **`metaHookBins`** — collects `m.bin` from `rushi.external_hooks`.
  Bare hook commands matching a declared bin are *exempt* from the
  build-time drift guard.

Consumer-authority rule: when the consumer has explicitly set
`rushi.config.paths.extension_tool_paths` (including an explicitly empty
list), that value is used **verbatim** — no merge, no dedup, no discovery.
Only when the value is *unset* does `mkRushi` fill it from
`metaToolEntries`. The same rule applies to `rushi.ui_extension_names`.

The filled values land in the returned `config` (TOML text) and
`configAttrs` (pre-TOML attrset). The shipped `config.toml` is written
**once** from the eval-time value (a single `cp` of the `writeText` file),
so the returned `config` and the shipped file are byte-identical unless
the build-time fallback appends names.

New return attributes: `extensionToolPaths` and `uiExtensionNames`,
exposed for `nix eval`.

## 4. The build-time fallback (unchanged for legacy sources)

For any source that lacks a usable `meta.rushi` field, and any *plain
path string* source (which structurally cannot carry `meta`), the #10
build-time discovery still runs in `installPhase`:

- `[paths] extension_tool_paths` — glob `"$out"/tools/*/tool.toml`,
  exclude native kernel tool dirs, dedup against the eval-time value, and
  rewrite the shipped `config.toml` line via `sed`.
- `[ui_extensions] enabled` — glob `"$out"/ui_extensions/*/ext.toml` and
  append the discovered entry names.
- Hook drift guard — still verifies every bare `config.hooks.defs.*.command`
  that is **not** covered by a `meta.rushi.bin` resolves to a file in
  `$out/bin/` or `$out/hooks/`.

The eval-time `lib.warn` tells the consumer *which* source still needs
migration. The fallback keeps pre-#13 producers and plain-path sources
working, so migration is additive and backwards-compatible.

## 5. Migration table (producers)

| Producer | Package | `meta.rushi` |
|---|---|---|
| `rushi-web-access` | tool | `entry = "<tool>"` |
| `rushi-simple-english` | ext | `ext = "<ext>"` |
| `rushi-exts` (`goal-app`) | `goal`, `goal_blocked`, `goal_complete` tools | `entry = "goal"` / `"goal_blocked"` / `"goal_complete"` |
| `rushi-exts` | `goal-ext` UI ext | `ext = "goal"` |
| `rushi-exts` | `hook-goal-{idle,compact,tools,arm}` | `bin = "harness-hook-goal-<suffix>"` |

`rushi-web-access` and `rushi-simple-english` already carry
`meta.rushi` (the reference shape). `rushi-exts`, `no-find-grep`, and
`rushi-statusline` migrate in their own repos.

## Properties

P1. **Eval-time identity.** For a fully-migrated producer set (every
source carries `meta.rushi`), the eval-time `extensionToolPaths` /
`uiExtensionNames` equal what #10 build-time discovery would have
produced for the same sources.

P2. **Byte identity.** With no fallback in play, the returned `config`
string and the shipped `$out/config.toml` are byte-identical.

P3. **nix-eval visibility.** `nix eval <consumer>#<case>.extToolPaths`
(and `.uiNames`) returns the final eval-time value without building.

P4. **Legacy fallback.** A source without `meta.rushi` (a derivation)
still works: its name is discovered at build time, the eval-time value
covers only the meta sources, and an eval-time `lib.warn` fires.

P5. **Plain-path fallback.** A plain path string source (no flake, no
`meta` possible) keeps working through the #10 build-time discovery,
with a warning.

P6. **Consumer authority.** A consumer-set
`rushi.config.paths.extension_tool_paths` (even explicitly empty) is
used verbatim; no merge and no discovery runs.

P7. **Meta check.** A producer that declares `meta.rushi.entry` but
does not ship that entry dir fails the configured build with a clear
error.

P8. **Reduced hook guard.** A bare hook command covered by a
`meta.rushi.bin` is exempt from the build-time drift guard; uncovered
bare commands are still guarded and fail the build when missing.

## Verification

| Property | Check |
|---|---|
| P1 | `bash tests/nix/run.sh` — `case-legacy` asserts the shipped `extension_tool_paths` contains both the meta-derived and the discovered entry |
| P2 | `run.sh` byte-diffs `nix eval … .configText --raw` against the shipped `config.toml` for `case-meta` and `case-user-auth` |
| P3 | `run.sh` asserts `evals.meta.extToolPaths` / `uiNames` via `nix eval … --json` |
| P4 | `run.sh` asserts `evals.legacy.extToolPaths` == meta-only value and that a fallback warning is emitted |
| P5 | `run.sh` asserts `case-plain-path` ships `tools/plain-tool` via discovery and the eval-time value is empty |
| P6 | `run.sh` asserts `case-user-auth` ships the consumer value verbatim and omits the meta-derived entry |
| P7 | `run.sh` asserts `case-meta-mismatch` build fails with the meta-check error |
| P8 | `run.sh` asserts `case-hook-guard` build fails naming the uncovered hook |

## Gate

```
bash tests/nix/run.sh        # 30 assertions: eval + build
bash scripts/verify-specs.sh # doc gate
cargo build --workspace && cargo test --workspace
```
