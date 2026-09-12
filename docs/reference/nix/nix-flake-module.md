# Rushi Nix Flaking Module

Status: design (2026-09-09). Mirrors the `pi-flake` pattern used by
`pi-config/flake.nix` (github:lukasl-dev/pi.nix) for the rushi kernel.

## 1. Motivation

Today `rushi` ships as a bare `buildRustPackage` in `flake.nix` with no
configuration surface: the user must hand-write `config.toml`, manage tool
dirs, and wire hooks manually. The `pi-flake` pattern shows how to turn a
kernel flake into a **configurable distribution unit**:

```
rushi-flake (kernel)          rushi-config (consumer)
  lib.mkRushi ──────────────────►  modules: [ myConfig ]
  (builds binary,                (declares version, config, tools,
   packages tools + exts,          extensions, hooks, model settings)
   generates config.toml)
```

The consumer never touches the build system. It writes one Nix attrset
(per-module) and gets a ready-to-run `rushi` package with all config,
tools, extensions, and hooks baked in.

## 2. Two-repo split (matching pi-flake / pi-config)

| Role | Repo / flake | Responsibility |
|---|---|---|
| **Kernel** | `rushi-flake` (= this repo's `flake.nix`, published as tag `vX.Y.Z`) | `lib.mkRushi`, builds `rushi` binary, ships kernel tools, exposes module schema |
| **Consumer** | `rushi-config` (any user repo) | pins `rushi-flake` to a tag, declares modules (config + extensions), produces final package |

This mirrors `pi-flake` (lukasl-dev/pi.nix) ↔ `pi-config` (user repo).
Each kernel release tag pins the source rev via a `VERSION.json`
(or `Cargo.lock` hash). The consumer bumps its `ref` to upgrade.

## 3. Module option schema

The module system is the **full NixOS module system** (`lib.evalModules`
+ `lib.mkOption`), mirroring pi-flake's `coding-agent/options.nix`.
Each consumer module is a standard NixOS-style module function
`{ config, lib, pkgs, ... } → attrset` that sets values under the
`rushi` option path. All NixOS module features work:
`lib.mkIf`, `lib.mkMerge`, `lib.mkOverride`, `lib.mkForce`,
`lib.mkOption` in sub-modules, `config.` access for inter-module
conditionals, and `extraSpecialArgs` for injecting custom evaluation
context.

Option declarations live in `lib/rushi-options.nix`; kernel defaults
live in `lib/rushi-defaults.nix`. The option schema:

```nix
rushi = {
  # ── Version ──
  # Kernel version to target. The kernel flake tag must match.
  # The consumer pins this in the flake input ref; this field is
  # informational (recorded into the generated config.toml header).
  version = "0.1";

  # ── config.toml fields ──
  # Declarative mirror of config.toml. Every field is optional;
  # absent fields use the kernel default (the config.toml shipped
  # with the kernel binary is the fallback).
  config = {
    # [active]
    active = {
      model = "";            # name of the active model (key into config.model)
    };

    # [model] — global model settings
    model = {
      api = "chat";          # "chat" | "responses"
      max_output_tokens = 8192;
      reasoning_effort = "medium";  # "low"|"medium"|"high"|"xhigh"|"max"
      model_timeout_s = 0;   # 0 = no cap
      # Per-model overrides: attrset keyed by model display name.
      # Each value is an attrset:
      #   model_id, base_url, api_key_env, context_tokens, timeout_s,
      #   reasoning_effort
      # Example:
      # "deepseek" = {
      #   model_id = "deepseek-v4-flash";
      #   base_url = "https://api.deepseek.com";
      #   api_key_env = "DEEPSEEK_API_KEY";
      #   context_tokens = 131072;
      # };
    };

    # [paths]
    paths = {
      sessions_root = "sessions";
      tools_root = "tools";
      extra_tools_roots = [ ];   # list of paths (relative to CWD or abs)
    };

    # [limits]
    limits = {
      read_limit = 2000;
      read_max_line_length = 2000;
      read_max_bytes = 51200;
      read_stream_min_size = 10485760;
      write_max_bytes = 1048576;
      tool_result_max_chars = 20000;
      bash_max_output_bytes = 16000;
      bash_timeout_default = 60;
      bash_timeout_max = 300;
      compact_enabled = true;
      compact_reserve_tokens = 16384;
      compact_keep_tokens = 20000;
      compact_strategy = "compact";
      context_budget_tokens = 0;   # 0 = auto (context_tokens - max_output_tokens)
      approval_timeout_s = 0;      # 0 = wait forever
      compact_reasoning_effort = "";  # "" = inherit session effort
    };

    # [hooks]
    hooks = {
      timeout_ms = 30000;
      on = [ ];   # list of { window = "exhausted.handle"; command = "..."; args = [ ]; }
    };

    # [loop]
    loop = {
      command = "rushi";
      args = [ "run" ];
      arg_style = "append_session";
    };

    # [system_prompt]
    system_prompt = {
      text = "";   # empty = kernel default
    };

    # [tui]
    tui = {
      binary = "";        # path to TUI binary ("" = resolve on PATH)
      color = "";         # "" = auto-detect
      color_scheme = "";
      tool_display = {
        preset = "";
        preview_lines = 0;
        bash_collapsed_lines = 0;
        diff_collapsed_lines = 0;
        expanded_preview_max_lines = 0;
        diff_view = "auto";
      };
    };
    # ext_dirs (TUI global UI-extension layer) lives in the [tui]
    # table; an empty list falls back to <config dir>/ui_extensions.
  };

  # ── Tools ──
  # Kernel tool names to enable. Each must exist in the kernel's
  # tools/ dir. The final package ships tool.toml + binary for each.
  tools = [ "read" "write" "edit" "bash" ];

  # ── Extensions ──
  # UI extension names (resolved from the exts dir bundled with the
  # package, or fetched via external_ui_extensions below).
  ui_extensions = [ ];

  # External tool sources: Nix derivations (fetchFromGitHub, built
  # cargo packages, paths, etc.). Each derivation's output must
  # contain a <name>/tool.toml + binary layout.
  # The last path component of the derivation's out is used as the
  # tool name unless overridden.
  external_tools = [ ];

  # External UI extension sources: same pattern as external_tools.
  external_ui_extensions = [ ];

  # External hook binaries: list of Nix derivations or paths.
  # Each must produce a single executable (or a bin/ dir).
  external_hooks = [ ];

  # ── Environment variables ──
  # Exported into the rushi process at runtime. The configured package
  # wraps bin/rushi in a shell script that exports these before
  # exec'ing the real binary. Mirrors pi-flake's `environment` option.
  #
  # Three forms per key:
  #   KEY = "value"                  literal
  #   KEY = { value = "…"; }        literal (explicit tag)
  #   KEY = { file = <derivation>; } value read from file at runtime
  #
  # The `file` form supports sops-nix secrets: pass a sops-nix
  # derivation and its content is read at runtime, never stored in
  # the Nix store as plain text.
  environment = {
    # DEEPSEEK_API_KEY = { file = secrets.deepseekKey; };  # sops-nix
    # RUSHI_LOG = "debug";
  };
};
```

### Option semantics

- **Additive merge.** `mkRushi` merges all modules in order. Later
  modules override earlier ones for the same key. This is the same
  NixOS module-merge semantics the pi-flake uses. For `rushi.config`
  specifically, `mkRushi` deep-merges the NixOS-merged value on top of
  kernel defaults (`lib.recursiveUpdate`), so setting one field
  preserves all others.
- **Config is an overlay.** `rushi.config` is not a full replacement;
  it overlays on top of the kernel default `config.toml` shipped in
  the package. Unset fields fall through to the kernel default.
- **Tools are a whitelist.** Only tools listed in `rushi.tools`
  (kernel) + `rushi.external_tools` (external) appear in the final
  package. This is the same as `rushi.toml [tools] enabled`.
- **Extensions are fetched, not embedded in the kernel.** The kernel
  flake does NOT ship extensions. The consumer flake fetches them
  (exactly like pi-config fetches `pi-automode`, `pi-lynx`, etc.)
  and passes them as Nix derivations. The kernel flake just packages
  them into the output.
- **Environment vars are exported, not embedded.** `rushi.environment`
  generates a `rushi.env` file and a wrapper script. The `file` form
  keeps secrets out of the Nix store (sops-nix integration point).
- **NixOS / home-manager integration.** The kernel flake exposes
  `nixosModules.rushi` and `homeManagerConfig.rushi` (option path
  `programs.rushi`). Set `programs.rushi.enable = true` and
  `programs.rushi.package = rushiConfigured.package` to add the
  configured rushi to `environment.systemPackages` / `home.packages`.

## 4. `lib.mkRushi` — the builder function

```
lib.mkRushi {
  pkgs,                # nixpkgs for the target system (fenix overlay for Rust)
  modules,             # list of NixOS-style module functions (lib.evalModules)
  rustToolchain ? null, # optional pre-resolved toolchain (auto-resolved if null)
  extraSpecialArgs ? {}, # extra args injected into every module's scope
} → {
  package,             # Nix derivation: bin/ + tools/ + config.toml + ui_extensions/ + hooks/
  config,             # generated config.toml (as a Nix string, for inspection)
  version,            # the resolved kernel version string
  options,            # full lib.mkOption schema (for nixosOptionsDoc / docs generation)
  configAttrs,        # deep-merged Nix attrset (pre-TOML), for programmatic access
}
```

### NixOS module features available inside consumer modules

Because `mkRushi` uses `lib.evalModules`, every NixOS module feature
works inside a consumer module (`{ config, lib, pkgs, ... } → …`):

| Feature | Use case |
|---|---|
| `lib.mkIf cond { … }` | Conditionally set options (e.g. only enable bash tool on Linux) |
| `lib.mkMerge [ {…} {…} ]` | Merge multiple partial configs in one module |
| `lib.mkOverride level val` | Override a value from an earlier module |
| `lib.mkForce val` | Override even `lib.mkOverride` (highest priority) |
| `config.foo` access | Branch on another module's values |
| `lib.mkOption` in sub-modules | Declare new sub-options with types + defaults |
| `extraSpecialArgs` | Inject custom evaluation context (e.g. sops-nix secrets) |

Example — conditional config:

```nix
{ config, lib, pkgs, ... }:
lib.mkMerge [
  {
    rushi.config.model = { api = "responses"; };
  }
  (lib.mkIf (config.rushi.tools ? "bash") {
    rushi.config.limits = { bash_timeout_max = 300; };
  })
  (lib.mkIf (pkgs.stdenv.hostPlatform.isLinux) {
    rushi.environment = { RUSHI_LOG = "debug"; };
  })
]
```

### Package layout

```
$out/
  bin/
    rushi              # kernel binary (loop engine + tui launcher).
                       # When rushi.environment is set, this is a thin
                       # shell wrapper that exports the vars from
                       # $out/rushi.env and exec's the real binary at
                       # bin/.rushi-real; otherwise it is the binary.
    read               # tool binary (if enabled)
    write
    edit
    harness-bash
    harness-hook-compact
    <ext-tool>         # external tool binaries
  tools/
    read/
      tool.toml
    write/
      tool.toml
    ...
    <ext-tool>/
      tool.toml
  ui_extensions/
    statusline.json
    mermaid/
    ...
  hooks/
    harness-hook-compact
    harness-hook-goal-idle
    ...
  config.toml          # generated from the merged module options
  rushi.env            # shell exports for rushi.environment (only when set)
  tools.manifest       # rushi.toml-equivalent (for rushi setup --locked)
```

### config.toml generation

`mkRushi` serializes the merged `rushi.config` attrset into TOML.
The mapping is direct:

| Nix attrset key | config.toml section |
|---|---|
| `config.active.model` | `[active] model = "..."` |
| `config.model.api` | `[model] api = "..."` |
| `config.model."deepseek".base_url` | `[model.deepseek] base_url = "..."` |
| `config.limits.read_limit` | `[limits] read_limit = 2000` |
| `config.hooks.on[0].window` | `[[hooks.on]] window = "..."` |
| `config.hooks.on[0].command` | `[[hooks.on]] command = "..."` |
| `config.loop.command` | `[loop] command = "..."` |
| `config.system_prompt.text` | `[system_prompt] text = "..."` |
| `config.tui.tool_display.preset` | `[tui.tool_display] preset = "..."` |
| `config.paths.extra_tools_roots` | `[paths] extra_tools_roots = [ ... ]` |

Empty strings / zero values / empty lists are **omitted** from the
generated TOML (the kernel default stands). This keeps the config
minimal and readable.

## 5. Consumer example (rushi-config)

See `examples/rushi-config/flake.nix` for the full working example.
Key excerpts:

```nix
# Consumer flake inputs
inputs = {
  nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  rushi-flake.url = "github:tony/rust-unix-harness?ref=v0.1.0";  # stable
  # rushi-flake.url = "github:tony/rust-unix-harness?ref=main";  # unstable
  # rushi-flake.url = "path:../../";                              # local dev
  fenix = {
    url = "github:nix-community/fenix";
    inputs.nixpkgs.follows = "nixpkgs";
  };
};
```

```nix
# Consumer modules
rushiConfigured = rushiFlake.lib.mkRushi {
  inherit pkgs;
  # Thread extra evaluation context into every module's scope
  # (here: the sops-nix secrets set).
  extraSpecialArgs = { inherit secrets; };
  modules = [
    # Module 1: model + loop configuration
    ({ config, lib, pkgs, ... }: {
      rushi.version = "0.1";
      rushi.config.model = {
        api = "responses";
        max_output_tokens = 32768;
        reasoning_effort = "xhigh";
        "deepseek" = {
          model_id = "deepseek-v4-flash";
          base_url = "https://api.deepseek.com";
          api_key_env = "DEEPSEEK_API_KEY";
          context_tokens = 131072;
        };
      };
      rushi.config.active.model = "deepseek";
      rushi.config.limits = {
        context_budget_tokens = 262144;
      };
      rushi.config.loop = {
        command = "rushi";
        args = [ "run" ];
        arg_style = "append_session";
      };
    })

    # Module 2: tools + extensions
    ({ config, lib, pkgs, ... }: {
      rushi.tools = [ "read" "write" "edit" "bash" ];
      rushi.ui_extensions = [ "statusline-rs" "mermaid" ];
      # rushi.external_tools = [
      #   rushiFlake.lib.fetchTool {
      #     inherit pkgs;
      #     owner = "tony"; repo = "rushi-exts";
      #     rev = "abc1234"; hash = "sha256-...";
      #     build = "cargo";
      #   }
      # ];
      rushi.config.hooks = {
        timeout_ms = 30000;
        on = [
          { window = "exhausted.handle"; command = "harness-hook-compact"; args = [ ]; }
          { window = "overflow.resolve"; command = "harness-hook-compact"; args = [ ]; }
        ];
      };
    })

    # Module 3: environment (sops-nix secret, threaded via extraSpecialArgs)
    # The `file` form keeps the secret out of the Nix store: the
    # sops-nix derivation is a build input, its content is read at
    # runtime by the `bin/rushi` wrapper, never embedded as plain text.
    ({ config, lib, pkgs, secrets, ... }: {
      rushi.environment = {
        DEEPSEEK_API_KEY = { file = secrets.deepseekKey; };
        RUSHI_LOG = "debug";
      };
    })
  ];
};
```

## 6. Kernel flake changes (this repo)

The rushi `flake.nix` gains:

1. **`lib.mkRushi`** in the outputs — the builder function described
   above (full `lib.evalModules` + `lib.mkOption` schema).
2. **`lib.defaults`** — the default option values (the "kernel
   defaults" that config.toml-overlay merges on top of).
3. **`lib.fetchExt` / `lib.fetchTool` / `lib.resolvePlatformHash`** —
   extension / tool source helpers (pi-flake `extNoDeps` /
   `extWithDeps` / `resolvePlatformHash` equivalents), so consumers
   don't hand-roll `fetchFromGitHub` + `buildRustPackage`. (Producer
   side — how an ext repo authors its own `flake.nix` against these
   contracts: `docs/reference/nix/ext-flake-authoring.md`.)
4. **`nixosModules.rushi`** and **`homeManagerConfig.rushi`** — the
   `programs.rushi` NixOS / home-manager module (adds the configured
   package to `environment.systemPackages` / `home.packages`).
5. **`packages.docs-md` / `packages.docs-html`** — auto-generated
   option reference (pi-flake `docs-md` / `docs-html` equivalents, via
   `pkgs.nixosOptionsDoc`).
6. The existing `packages.default` is unchanged (it is the raw kernel
   build with no config, for `rushi setup` / dev use).

The consumer flake calls `rushi-flake.lib.mkRushi` to get a fully
configured package. The kernel flake stays self-contained; the
configuration is external, versioned, and composable.

## 7. Version pinning — three modes

The `rushi-flake` input URL controls what kernel source the consumer
builds. Three modes cover every workflow:

| Mode | `url` | When to use | How to update |
|---|---|---|---|
| **Stable** | `github:tony/rust-unix-harness?ref=v0.1.0` | Production, shared config, friends | Bump ref tag, `nix flake update rushi-flake` |
| **Unstable / branch** | `github:tony/rust-unix-harness?ref=main` (or any branch) | Developer tracking kernel changes | `nix flake update rushi-flake` re-locks to latest commit on that branch |
| **Local** | `path:../../` (or `path:.` for in-tree) | In-tree development, no network | Edits to local source picked up immediately, no lock refresh needed |

Key points:

- **No version-bump flooding.** In unstable mode you never edit the
  ref string. You push to `main` (or a feature branch), run
  `nix flake update rushi-flake`, and the lock re-pins to the latest
  commit on that branch. Each build is reproducible because the lock
  records the exact commit SHA; you only re-lock when you want the
  new code.

- **Stable consumers** pin to release tags (`v0.1.0`, `v0.2.0`, …).
  Each tag's `flake.lock` pins `Cargo.lock` (transitively pinning all
  Rust deps) and all flake inputs. Upgrading is a one-line ref bump.

- **Local mode** is for the kernel developer working inside the rushi
  repo itself. The `path:` reference means no network fetch; `nix
  build` sees the working tree directly. No lock refresh needed — the
  build tracks the local source.

- `rushi.lock` (the Cargo-lock-style tool manifest) is a separate
  concern generated by `rushi setup` from the materialized tool set.
  The Nix `flake.lock` is the build-reproducibility pin;
  `rushi.lock` is the tool-set reproducibility pin. They are
  independent.

### Example: switching between modes

```nix
# Production: pinned to a release
rushi-flake.url = "github:tony/rust-unix-harness?ref=v0.1.0";

# Development: track main, re-lock after each push
rushi-flake.url = "github:tony/rust-unix-harness?ref=main";
# then: nix flake update rushi-flake && nix build .#rushi

# In-tree: no lock, no network
rushi-flake.url = "path:../../";
```

The flake.lock always records the resolved commit for whichever mode
is active. Switching modes changes the lock entry but the consumer
flake code stays the same.

## 8. What this does NOT do (by design)

- Does not replace `rushi.toml` / `rushi.lock` / `rushi setup`. Those
  are the **plain-install** path (P4 in harness-distribution.md). The
  Nix module is the **Nix-primary** path (P5). They produce the same
  tool set; the Nix path just does it declaratively.
- Does not auto-install user tools. The `bash` tool is the bridge to
  the user environment (harness-distribution.md §11).
- Does not sandbox or restrict. Hooks are the restriction mechanism.

## 9. Full capability surface (pi-flake → rushi mapping)

Read from the actual pi-flake source (github:lukasl-dev/pi.nix, v0.85.1),
the module system exposes far more than "version + config + extensions".

### 9.1 Core builder: `lib.mkCodingAgent`

```
mkCodingAgent { pkgs; modules ? []; extraSpecialArgs ? {}; }
  → { config, options, coding-agent, package, rules, args }
```

- Uses **`lib.evalModules`** (the full NixOS module system), not a
  simple attrset merge. Modules can declare options with
  `lib.mkOption` (type, default, description, example) and get
  automatic type-checking and merging.
- `extraSpecialArgs` allows injecting additional evaluation context
  (e.g. `jail-nix` library) into every module.
- Returns both `config` (evaluated) and `options` (declarations),
  enabling self-documentation.

### 9.2 Option schema (`pi.coding-agent.*`)

13 declared options, each with `lib.mkOption` type + default:

| Option | Type | Purpose |
|---|---|---|
| `package` | `package` | Base pi package to wrap |
| `wsl` | `bool` | WSL detection (auto-set from NixOS `config.wsl.enable`) |
| `jail.enable` | `bool` | **Bubblewrap sandbox** via jail-nix |
| `jail.permissions` | `functionTo (listOf raw)` | Sandbox permission combinators (network, mount-cwd, add-pkg-deps, try-readonly, …) |
| `models` | `nullOr path` | models.json to install into agent config dir |
| `rules` | `nullOr (lines \| path)` | System prompt text or AGENTS.md path |
| `extensions` | `listOf (path \| str)` | Extension files via `--extension` |
| `skills` | `listOf path` | Skill directories via `--skill` |
| `themes` | `listOf path` | Theme JSONs via `--theme` |
| `promptTemplates` | `listOf path` | Prompt templates via `--prompt-template` |
| `extraArgs` | `listOf str` | Raw CLI args (e.g. `["--provider" "openai"]`) |
| `environment` | `nullOr (path \| taggedAttrs)` | Env vars: `{ value = "…" }` or `{ file = path }` (sops-nix secret support) |
| `settings` | `attrs` | Contents of `settings.json` (deep-merged at runtime via `jq`) |

### 9.3 Package construction pipeline (3 stages)

1. **Base**: `buildNpmPackage` (nodejs, typescript, cairo, …) → raw pi
2. **Wrapped**: `writeShellScriptBin "pi"` that:
   - Sources environment (tagged attrs or shell file)
   - Sets `PI_CODING_AGENT_DIR`
   - Installs `models.json` (no-clobber: skips if symlink or file exists)
   - Merges `settings.json` via `jq -s '.[0] * .[1]'` (runtime deep merge)
   - Appends resource flags (`--append-system-prompt`, `--extension`, `--skill`, `--theme`, `--prompt-template`)
   - Appends `extraArgs`
   - **Delegates** `install|remove|uninstall|update|list|config` to the real binary
3. **Jailed** (optional): wraps in `jail-nix` bubblewrap sandbox with
   configurable permissions (bind-mounts agent config dir, forwards env)

### 9.4 Multiple integration contexts

The same option schema is parameterized by `optionPath`:

| Context | Entry point | Option path | Effect |
|---|---|---|---|
| Standalone | `lib.mkCodingAgent` | `pi.coding-agent` | Consumer flake builds configured package |
| NixOS | `nixosModules.coding-agent` | `programs.pi.coding-agent` | Adds `finalPackage` to `environment.systemPackages` |
| home-manager | `homeModules.coding-agent` | `programs.pi.coding-agent` | Adds `finalPackage` to `home.packages` |

All three share `options.nix`; the module just re-serves it at the
correct option path.

### 9.5 Flake outputs (beyond the package)

| Output | Purpose |
|---|---|
| `packages.coding-agent` | Node.js runtime (default) |
| `packages.coding-agent-bun` | **Bun runtime** (via bun2nix overlay) |
| `packages.docs-md` | Auto-generated option docs (markdown, via `nixosOptionsDoc`) |
| `packages.docs-html` | Option docs as HTML (pandoc) |
| `overlays.default` | Exposes `pi-coding-agent` / `pi-coding-agent-bun` as nixpkgs overlays |
| `apps.update` | `pi-update` — sync VERSION.json + re-lock |
| `apps.sync-upstream` | `pi-sync-upstream` — sync with upstream earendil-works/pi |
| `apps.regenerate-models` | `pi-regenerate-models` — regenerate AI provider model catalogs |
| `apps.scan` | `pi-scan` — security scan |

### 9.6 Version pinning

`VERSION.json` in the pi-flake repo pins:
- `rev` + `hash` → source commit (github:earendil-works/pi)
- `npmDepsHash` → npm dependency lock hash

Consumer pins `ref=vX.Y.Z` (tag) → `flake.lock` pins the exact commit
of pi.nix. `nix flake update pi-flake` re-locks.

### 9.7 Extension helpers (consumer-side, in pi-config flake)

pi-flake does NOT ship extensions. The consumer flake provides helpers:

- **`extNoDeps`** — `fetchFromGitHub` for zero-dependency extension
  repos. Params: `{ owner, repo, rev, hash, extraPatch? }`
- **`extWithDeps`** — `fetchFromGitHub` + `buildNpmPackage` for
  extensions with npm dependencies. Params:
  `{ owner, repo, rev, hash, npmDepsHash, sourceRoot?, lockfile?, extraPatch? }`
  - `sourceRoot`: for monorepo subdirectories (e.g. `extensions/pi-goal`)
  - `lockfile`: inject a lockfile into the build (jq del devDeps,
    add missing integrity hashes)
  - `extraPatch`: shell snippet run during buildPhase
- **`resolvePlatformHash`** — accepts either a shared hash string or a
  per-system attrset (`{ x86_64-linux = "…"; aarch64-darwin = "…"; }`)

### 9.8 Full capability mapping (pi → rushi)

| # | pi-flake capability | rushi equivalent | Status |
|---|---|---|---|
| 1 | `lib.mkCodingAgent` (full `lib.evalModules`) | `lib.mkRushi` (full `lib.evalModules` + `lib.mkOption` schema) | ✅ implemented |
| 2 | `rules` — system prompt text/path | `rushi.config.system_prompt.text` | ✅ covered |
| 3 | `settings` — flat agent settings (JSON) | `rushi.config.model` + `rushi.config.limits` + `rushi.config.tui` | ✅ covered |
| 4 | `themes` — theme files | `rushi.config.tui.color_scheme` + `rushi.config.tui.color` | ✅ covered |
| 5 | `skills` — skill directories | No analog yet | ⏳ future |
| 6 | `models` — model catalog file | `rushi.config.model` (inline attrset, not separate file) | ✅ covered |
| 7 | `environment` — env var overrides (tagged) | `rushi.environment` (value/file tagged, sops-nix support) | ✅ implemented |
| 8 | `extensions` — extension list | `rushi.tools` + `rushi.external_tools` + `rushi.ui_extensions` + `rushi.external_ui_extensions` + `rushi.external_hooks` | ✅ covered |
| 9 | `extNoDeps` — zero-dep GitHub fetch | `lib.fetchExt` (kernel flake helper) | ✅ implemented |
| 10 | `extWithDeps` — npm-dep GitHub fetch | `lib.fetchTool` (kernel flake helper, cargo build) | ✅ implemented |
| 11 | `resolvePlatformHash` — per-system hashes | `lib.resolvePlatformHash` (kernel flake helper) | ✅ implemented |
| 12 | Monorepo subdirectory fetch | `lib.fetchExt { subpath = …; }` | ✅ covered |
| 13 | `jail` — bubblewrap sandbox | Intentionally omitted (rushi uses hooks for restriction) | ❌ by design |
| 14 | `extraArgs` — raw CLI args | `rushi.config.loop.args` (loop engine args) | ✅ covered |
| 15 | `promptTemplates` — template files | No analog yet | ⏳ future |
| 16 | NixOS/home-manager modules | `nixosModules.rushi` + `homeManagerConfig.rushi` (`programs.rushi`) | ✅ implemented |
| 17 | `devShells` | Kernel flake has `devShells.{default,lean,aeneas}` | ✅ covered |
| 18 | `overlays` — expose as nixpkgs packages | Not needed (consumer imports kernel flake directly) | ⏳ optional |
| 19 | `docs-md` / `docs-html` — auto option docs | `packages.docs-md` / `packages.docs-html` via `nixosOptionsDoc` | ✅ implemented |
| 20 | `apps.update` / `sync-upstream` | `nix flake update rushi-flake` (no custom app needed) | ✅ covered |
| 21 | Dual runtime (Node/Bun) | Single runtime (Rust) — not applicable | N/A |
| 22 | `runCommand` wrapper (PATH augmentation) | Consumer can `pkgs.runCommand "rushi-with-X"` with `makeWrapper` | ✅ available via nixpkgs |

### 9.9 Summary: in-scope vs. future

**In scope (implemented):**
- `lib.mkRushi` builder function (full `lib.evalModules` + `lib.mkOption`)
- Full `config.toml` option schema (active, model, paths, limits, hooks,
  loop, system_prompt, tui, ext)
- Kernel tool whitelist (`rushi.tools`)
- External tools / UI extensions / hooks as Nix derivation lists
- TOML serialization from Nix attrsets
- Three version-pinning modes (stable tag, unstable branch, local path)
- Consumer example flake with 3-mode pinning
- `rushi.environment` — env-var injection (literal / file / sops-nix)
- `lib.fetchExt` / `lib.fetchTool` / `lib.resolvePlatformHash` helpers
- NixOS + home-manager modules (`programs.rushi`)
- Auto-generated option docs (`docs-md` / `docs-html` via `nixosOptionsDoc`)

**Future (deferred):**
- `rushi.skills` — skill directory support
- `rushi.config.prompt_templates` — prompt template file support
- `overlays` — expose rushi as nixpkgs packages
- `VERSION.json` in kernel repo for release-tag pinning
- `rushi.lock` integration with Nix (auto-generate from the Nix-built tool set)
- Runtime config merge (like pi's `jq` settings merge) — rushi
  generates config.toml at build time, not merged at runtime
