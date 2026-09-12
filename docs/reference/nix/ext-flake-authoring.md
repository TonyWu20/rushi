# Ext-Repo `flake.nix` Authoring Guide

Status: active reference (2026-09). Companion to `nix-flake-module.md`.
Covers how an **extension repo** (a `rushi-exts`-style sibling) writes its
`flake.nix` so a consumer can plug its **tools**, **hooks**, and
**UI extensions** into a `rushi` package built by `lib.mkRushi`.

This guide is about the *producer* side (the ext repo's flake). The
*consumer* side (wiring the flake's outputs into `rushi.external_*`) is in
`nix-flake-module.md` §5. Read that first.

---

## 1. Do you need a `flake.nix` at all?

There are two ways a consumer can pull an ext into a `rushi` package:

| Mode | Producer artifact | Consumer mechanism | When |
|---|---|---|---|
| **A. No flake** | just a git repo on GitHub with the right *source* layout | `rushiFlake.lib.fetchExt { … }` / `lib.fetchTool { … }` → `fetchFromGitHub` + `buildRustPackage` | Zero-dependency exts; single-crate repos; a cargo *workspace* at the repo root |
| **B. Ext flake** | a `flake.nix` exposing `packages.<system>.<name>` | flake input → pass `extFlake.packages.<system>.<name>` into `rushi.external_tools` / `external_hooks` / `external_ui_extensions` | Multi-crate repos with no workspace root, crates that path-dep on a shared sibling crate, or any combo of tool + hook + ext in one repo |

`lib.fetchTool` builds with `cargoBuildFlags = [ "--workspace" ]` and
expects a `Cargo.toml` at the fetched *repo root*. That works for a
workspace, but **not** for a repo like `goal-app/` that has no root
`Cargo.toml` (its `goal-tools/`, `goal-hooks/`, `goal-ext/` are independent
crates that path-dep on a shared `goal-state/` crate). For those, Mode B is
the correct vehicle: one flake owns the whole source tree, and each package
builds one crate from it, so intra-repo path deps (`goal-state =
{ path = "../../goal-state" }`) resolve naturally.

> **Rule of thumb.** If the ext repo is a single crate (or a cargo
> workspace), Mode A is enough — no `flake.nix` needed. If it is several
> independent crates sharing sibling path deps, or you want to mix
> tools + hooks + UI-exts in one repo, write the ext flake (Mode B).

Both modes produce the *same* `$out` contracts (below). The flake just
makes them first-class, named, per-system packages instead of ad-hoc
`fetchFromGitHub` calls in the consumer.

---

## 2. The four `$out` contracts

`mkRushi` (the kernel flake's `lib/mk-rushi.nix`) copies each external
source into the configured package's output layout. Each source type has a
strict `$out` shape the copy step expects. This is the part of the ext
flake you must get right.

| Source option | `$out` layout the copy step expects | What ends up in the package |
|---|---|---|
| `rushi.external_tools` | `<tool>/tool.toml` + `<tool>/bin/<binary>` | `tools/<tool>/…` (additive to kernel tools) |
| `rushi.external_ui_extensions` | `<ext>/ext.toml` + `<ext>/<binDir>/<bin>` (`binDir` = the ext.toml `command` path) | `ui_extensions/<ext>/…` |
| `rushi.external_hooks` | a single executable **or** a `bin/` dir (e.g. `$out/bin/<hook>`) | `hooks/<hook>` |
| `rushi.tui` (separate repo) | `$out/bin/tui` | `bin/tui` (kernel side-by-side resolver finds it) |

Concrete groundings:

- **Tool** — `tool.toml` `command` resolves on the agent-visible `PATH`
  (`docs/reference/README.md`). `mk-rushi.nix` ships each tool as
  `tools/<tool>/tool.toml` + `tools/<tool>/bin/<binary>`. So the tool
  package's `$out` must be `<tool>/tool.toml` + `<tool>/bin/<binary>`.
- **UI extension** — `ext.toml` `command` "resolves against the entry
  directory" when it is a relative path
  (`../rushi-tui/docs/ui-extension.md` §6). The entry dir *is*
  `ui_extensions/<ext>/`. So the ext package's `$out` must be
  `<ext>/ext.toml` + `<ext>/target/release/<binary>`, where
  `ext.toml` `command = "target/release/<binary>"` is relative to
  `<ext>/`.
- **Hook** — `mk-rushi.nix` `extHookScript` handles a source that is
  `-x` (a single executable), *or* a dir with `bin/` (copies `bin/.` →
  `hooks/`), *or* a plain dir. A raw `buildRustPackage` produces
  `$out/bin/<hook>`, so **a `buildRustPackage` result is already a valid
  hook source — no repackage needed.**
- **TUI** — `rushi.tui` (see `lib/rushi-options.nix`) requires the
  binary at `$out/bin/tui` so the kernel's side-by-side resolver
  (`<exe_dir>/tui`) finds it.

The copy steps, from `lib/mk-rushi.nix` (authoritative):

```
external_tools:          cp -rL "${src}/."      "$out/tools/"
external_ui_extensions:  cp -rL "${src}/."      "$out/ui_extensions/"
external_hooks:          [ -x src ] → cp src hooks/
                         [ -d src/bin ] → cp -rL src/bin/. hooks/
                         else → cp -rL src/. hooks/
rushi.tui:               cp "${src}/bin/tui"    "$out/bin/tui"
```

Because the copy is `cp -rL "<src>/. "`, the ext package's `$out` must
*contain* the per-name subdirectory (`<tool>/`, `<ext>/`). That is the
difference between the tool/ext wrappers (which wrap in a named dir) and
the hook (which ships a bare `bin/`).

---

## 3. The ext flake skeleton

A standalone ext flake. Mirrors the kernel flake's `supportedSystems`
list and `genAttrs` (not `flake-utils.eachDefaultSystem` — the same
reason the kernel flake avoids it: `eachDefaultSystem` transposes the
result and breaks `nix develop` / per-system `devShells`), and the same
fenix toolchain.

```nix
{
  description = "goal-app — goal-mode tools, hooks, and TUI extension for rushi";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, fenix }:
    let
      supportedSystems = [ "x86_64-linux" "aarch64-linux" ];
      pkgLib = nixpkgs.lib;

      buildFor = system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ fenix.overlays.default ];
          };
          rustToolchain = fenix.packages.${system}.stable.withComponents [
            "cargo" "clippy" "rust-src" "rustc" "rustfmt" "rust-analyzer"
          ];

          # Build one standalone cargo crate from a subpath of this flake's
          # source tree. Intra-repo path deps (goal-state) resolve because
          # `src` keeps the repo layout intact. The output binary name comes
          # from the crate's [[bin]] name in Cargo.toml, not from crateName.
          buildCrate = { crateDir, crateName }:
            pkgs.rustPlatform.buildRustPackage {
              pname = crateName;
              version = "0.1.0";
              src = "${self}/${crateDir}";
              nativeBuildInputs = [ rustToolchain ];
              cargoLock = { lockFile = "${self}/${crateDir}/Cargo.lock"; };
              doCheck = false;
            };

          # ── Tool wrapper: $out/<name>/tool.toml + <name>/bin/<binary> ──
          wrapAsTool = { name, toolToml, built }:
            pkgs.stdenv.mkDerivation {
              pname = "${name}-tool";
              nativeBuildInputs = [ built ];
              installPhase = ''
                mkdir -p $out/${name}/bin
                cp -rL ${built}/bin/. $out/${name}/bin/
                cp ${toolToml} $out/${name}/tool.toml
              '';
            };

          # ── UI-ext wrapper: $out/<ext>/ext.toml + <ext>/<binDir>/<bin> ──
          # `binDir` must equal the relative `command` path in ext.toml,
          # because the TUI resolves `command` against the ext entry dir.
          # buildRustPackage is a release build, so the default is
          # "target/release". If an ext's ext.toml points at a different
          # dir (e.g. goal-ext's "target/debug/goal-ext", a dev artifact),
          # either align the ext.toml `command` to target/release/… or
          # pass binDir = "target/debug".
          wrapAsExt = { extName, extToml, built, binDir ? "target/release" }:
            pkgs.stdenv.mkDerivation {
              pname = "${extName}-ui-ext";
              nativeBuildInputs = [ built ];
              installPhase = ''
                mkdir -p $out/${extName}/${binDir}
                cp ${extToml} $out/${extName}/ext.toml
                cp -rL ${built}/bin/. $out/${extName}/${binDir}/
              '';
            };
        in
        {
          packages = rec {
            # ── Tools (wrap the cargo build into the tool contract) ──
            goal          = wrapAsTool { name = "goal";          toolToml = "${self}/goal-app/goal-tools/goal/tool.toml";         built = buildCrate { crateDir = "goal-app/goal-tools/goal";         crateName = "goal"; }; };
            goal-blocked  = wrapAsTool { name = "goal_blocked";  toolToml = "${self}/goal-app/goal-tools/goal_blocked/tool.toml";  built = buildCrate { crateDir = "goal-app/goal-tools/goal_blocked";  crateName = "goal_blocked"; }; };
            goal-complete = wrapAsTool { name = "goal_complete"; toolToml = "${self}/goal-app/goal-tools/goal_complete/tool.toml"; built = buildCrate { crateDir = "goal-app/goal-tools/goal_complete"; crateName = "goal_complete"; }; };

            # ── Hooks (a bare buildRustPackage result IS the hook source) ──
            # Binary names (harness-hook-*) come from each Cargo.toml's
            # [[bin]] name; they must match the `command` in the hook config.
            hook-goal-idle     = buildCrate { crateDir = "goal-app/goal-hooks/hook-goal-idle";     crateName = "hook-goal-idle"; };
            hook-goal-compact  = buildCrate { crateDir = "goal-app/goal-hooks/hook-goal-compact";  crateName = "hook-goal-compact"; };
            hook-goal-tools    = buildCrate { crateDir = "goal-app/goal-hooks/hook-goal-tools";    crateName = "hook-goal-tools"; };
            hook-goal-arm      = buildCrate { crateDir = "goal-app/goal-hooks/hook-goal-arm";      crateName = "hook-goal-arm"; };

            # ── UI extension (wrap into the ext contract) ──
            goal-ext = wrapAsExt { extName = "goal"; extToml = "${self}/goal-app/goal-ext/ext.toml"; built = buildCrate { crateDir = "goal-app/goal-ext"; crateName = "goal-ext"; }; };

            default = goal;
          };
        };
    in
    pkgLib.genAttrs supportedSystems (system: buildFor system);
}
```

Notes on the skeleton:

- **`buildCrate`** is the one primitive that does the heavy lifting.
  Everything else is a thin layout wrapper around it.
- **Hooks** reuse `buildCrate` directly (no wrapper) because a
  `buildRustPackage` output (`$out/bin/<binary>`) is already a valid
  `external_hooks` source. The binary name is whatever `[[bin]] name`
  is in that crate's `Cargo.toml` (e.g. `harness-hook-goal-idle`, not
  the crate name `hook-goal-idle`); it must match the `command` field
  in `config.toml`'s `[[hooks.on]]`.
- **`goal-state`** (the shared path-dep crate) is *not* a `packages.`
  entry. It is built transitively by `buildCrate`'s path-dep
  resolution. Expose it only if a consumer wants to build/link it
  directly.
- **One flake, any combination.** A repo can expose *any* mix of the
  three categories (tool / hook / ext). The flake just names each
  package; the consumer decides which go to which `rushi.external_*`
  option. A tool-only repo exposes only tools; a hook-only repo only
  hooks; `goal-app` exposes all three.

---

## 4. The three categories, in isolation

Each category is independent. If your ext repo only does one, keep only
that section of `packages`.

### 4.1 Tool

A tool is a cargo crate with a `tool.toml` at its root. The package wraps
the cargo build into `<name>/tool.toml` + `<name>/bin/<binary>`.

```
goal-app/goal-tools/goal/
  Cargo.toml      # name = "goal"
  Cargo.lock
  tool.toml       # [tool] command = "goal" …
  src/
```

`packages.goal = wrapAsTool { name = "goal"; toolToml =
"…/goal/tool.toml"; built = buildCrate { … }; }` → `$out/goal/tool.toml`
+ `$out/goal/bin/goal`.

### 4.2 Hook

A hook is a cargo crate producing one executable. No wrapper:

```
goal-app/goal-hooks/hook-goal-idle/
  Cargo.toml      # [[bin]] name = "harness-hook-goal-idle"
  Cargo.lock
  src/
```

`packages.hook-goal-idle = buildCrate { crateDir = "goal-app/goal-hooks/hook-goal-idle"; crateName = "hook-goal-idle"; }` →
`$out/bin/harness-hook-goal-idle`. `mkRushi`'s hook copy step
(`-d src/bin → cp bin/. hooks/`) drops it into `hooks/`. The binary
name (`harness-hook-goal-idle`) comes from the crate's `[[bin]] name`,
not the crate name.

### 4.3 UI extension

A UI ext is a cargo crate with an `ext.toml` at its root. The package
wraps the cargo build into `<ext>/ext.toml` + `<ext>/<binDir>/<binary>`,
where `<binDir>` is the relative dir the ext.toml `command` field points
at (the TUI resolves `command` against the ext entry dir). The **invariant**:
the Nix package must place the built binary exactly where `ext.toml`
`command` resolves, or the TUI's entry resolver (fail-loud, P1) will
refuse to start. `buildRustPackage` is a release build, so the
`command` should read `target/release/<binary>`; if an ext's `ext.toml`
points at `target/debug/…` (a dev-build artifact from `ext-env.sh`),
either align it to `target/release/…` or pass `binDir = "target/debug"`
to `wrapAsExt`.

```
goal-app/goal-ext/
  Cargo.toml      # [[bin]] name = "goal-ext"
  Cargo.lock
  ext.toml        # [ext] command = "target/release/goal-ext" …
  src/
```

`packages.goal-ext = wrapAsExt { extName = "goal"; extToml = "…/goal-ext/ext.toml"; built = buildCrate { … }; }`
→ `$out/goal/ext.toml` + `$out/goal/target/release/goal-ext`.
The `extName` (`"goal"`) is the `ui_extensions/` entry name; it matches
the `command`'s entry-dir. **Watch the `binDir` invariant**: `goal-ext`'s
`ext.toml` currently reads `command = "target/debug/goal-ext"` (a dev
artifact — `ext-env.sh` builds in debug). For the Nix package, align it
to `target/release/goal-ext` (the `buildRustPackage` release layout) so
the default `binDir = "target/release"` resolves; otherwise pass
`binDir = "target/debug"`. (For `statusline-rs` the entry name and the
binary name differ — `ext.toml` `command = "target/release/statusline-ext"`
— and `cp -rL ${built}/bin/.` copies the binary under its *binary* name,
so the relative `command` path still resolves.)

---

## 5. Combinations: any subset of {tool, hook, ext} in one flake

There is no rule that a repo does exactly one category. `goal-app` does
all three (3 tools + 4 hooks + 1 ext). The flake simply lists each
package; the **consumer** picks the subset it wants:

```nix
# consumer flake
rushiFlake.lib.mkRushi {
  inherit pkgs;
  modules = [ ({ config, lib, pkgs, ... }: {
    # Pull only what you need from the ext flake:
    rushi.external_tools       = [ goalApp.packages.${system}.goal ];
    rushi.external_hooks       = [
      goalApp.packages.${system}.hook-goal-idle
      goalApp.packages.${system}.hook-goal-compact
      goalApp.packages.${system}.hook-goal-tools
      goalApp.packages.${system}.hook-goal-arm
    ];
    rushi.external_ui_extensions = [ goalApp.packages.${system}.goal-ext ];
  }) ];
}.package;
```

A repo that is *only* hooks (e.g. `no-find-grep`) exposes one
`buildCrate` package. A repo that is *only* a tool (e.g. `lean-verify`)
exposes one `wrapAsTool` package. The flake shape is identical; only the
`packages.` keys differ.

### 5.1 The TUI case (`rushi-tui`)

The TUI is a *fourth* output, not an `external_*`. It feeds `rushi.tui`
and must expose the binary at `$out/bin/tui` (standard Nix package
layout) so the kernel's side-by-side resolver (`<exe_dir>/tui`) finds it:

```nix
# rushi-tui/flake.nix — add a packages output (it currently has devShells only)
packages = {
  default = tuiPkg;   # tuiPkg = buildRustPackage { … ; src = self; } → $out/bin/tui
};
# consumer: rushi.tui = rushiTuiFlake.packages.${system}.default;
```

`rushi-tui` also path-deps on the kernel's `rushi-common` crate, so its
flake takes a `rushi-kernel` flake input — see `rushi-tui/flake.nix`.
Today that input is a bootstrap `git+file:` URL into the sibling kernel
checkout; at hosting time it flips to a pinned `github:` ref (the same
three-mode pinning as `nix-flake-module.md` §7). The ext repos do
**not** depend on the kernel, so they need no such input.

---

## 6. Local dev shell (optional, `rushi-tui` precedent)

An ext flake may also expose a `devShells.<system>.default` for working on
the ext in-tree (cargo + the Nix-built kernel on `PATH` + any Lean toolchain
for DRT gates). This is a *developer* convenience; it does not affect how
the consumer pulls the ext into a `rushi` package. `rushi-tui/flake.nix`
is the model.

---

## 7. Choosing between Mode A and Mode B, per current ext repo

| Repo | Shape | Right mode |
|---|---|---|
| `lean-verify` | single crate + `tool.toml` | A (`lib.fetchTool { repo = "lean-verify"; build = "cargo-single"; }`) or B (one `wrapAsTool`) |
| `no-find-grep` | single hook crate | A (`lib.fetchExt`/`fetchTool`) or B (one `buildCrate` hook) |
| `ext-rs/statusline-rs`, `notify-rs`, `tool_result-rs` | single ext crate each | A (zero-dep `lib.fetchExt` + a build) or B (one `wrapAsExt` each) |
| `goal-app` | multi-crate, no workspace, shared `goal-state` path dep | **B only** — one flake, `buildCrate` per component |

`goal-app` is the one that *forces* Mode B: `lib.fetchTool`'s
`--workspace` + "root `Cargo.toml`" assumption does not hold, and the
intra-repo `goal-state` path dep needs the whole tree as one source.

---

## 8. Properties, Verification, Gate

**Properties**

- **P1. contract-layout.** Each `packages.<system>.<name>` whose
  category is *tool* has `$out/<name>/tool.toml` and
  `$out/<name>/bin/<binary>`; *ext* has `$out/<ext>/ext.toml` and
  `$out/<ext>/<binDir>/<binary>` where `<binDir>` matches the ext.toml
  `command` relative path; *hook* has `$out/bin/<binary>`; `rushi.tui`
  has `$out/bin/tui`.
- **P2. intra-repo-deps.** A package whose crate path-deps on a sibling
  crate in the same repo builds without a manual kernel input
  (`goal-state` resolves via the flake's own source tree).
- **P3. subset-composition.** The consumer can wire any subset of a repo's
  packages into `rushi.external_tools` / `external_hooks` /
  `external_ui_extensions` independently, and `mkRushi` copies each into
  the matching package dir.
- **P4. no-workspace.** A multi-crate repo with no root `Cargo.toml`
  builds correctly under Mode B (one `buildCrate` per crate), where
  `lib.fetchTool`'s `--workspace` assumption would fail.

**Verification**

- `nix build .#<name>` (or `nix build --json`) and `nix eval
  --raw .#<name>` to inspect the store path; `ls -R <store>/` against
  the P1 layout table.
- Run the kernel consumer build: `nix build .#rushi` in a consumer flake
  that wires `extFlake.packages.<system>.*` into `rushi.external_*`, then
  `ls -R <rushi-pkg>/{tools,ui_extensions,hooks,bin}`.
- `rushi setup --locked` against the generated `tools.manifest`
  materializes the external tools/hooks; `rushi docs` still renders.

**Gate**

```
nix build .#goal          # ext flake, one tool
nix build .#hook-goal-idle   # ext flake, one hook
nix build .#goal-ext       # ext flake, one UI ext
# consumer flake:
nix build .#rushi         # configured package with the exts wired in
rushi setup --locked      # materialize + verify tool/hook layout
```

A clean gate with every P-row proven is the guarantee that the ext flake
produces the `$out` contracts `mkRushi` copies correctly.
