# lib/rushi-options.nix — NixOS-style option schema for the rushi module.
#
# Analogous to pi-flake's `coding-agent/options.nix`. Declares the
# `rushi.*` options with `lib.mkOption` types, defaults, descriptions,
# and examples. Passed to `lib.evalModules` by `mk-rushi.nix`.
#
# Design notes:
#
#   * `rushi.config` is `lib.types.attrs` with default `{}`. Kernel
#     defaults live in `rushi-defaults.nix` and are deep-merged
#     underneath by `mk-rushi.nix` (NixOS does not deep-merge an
#     option's `default` with module values for `attrs` types).
#
#   * Consumer modules use sub-key syntax for partial overrides:
#
#       { config, lib, pkgs, ... }: {
#         rushi.config.limits = { context_budget_tokens = 262144; };
#         rushi.config.model  = { api = "responses"; };
#         rushi.tools        = [ "read" "write" "edit" "bash" ];
#       }
#
#     Different modules can set different top-level keys under
#     `rushi.config` — they recursive-merge in `evalModules`.
#
#   * See docs/reference/nix/nix-flake-module.md §3 for the full schema
#     reference.

{ lib, kernelTools ? null, ... }:

let
  # Convenience: "free-form" list type for external sources
  # (derivations, store paths, or plain strings).
  rawList = lib.types.listOf lib.types.raw;

  # Kernel tool names for the `tools` default. When the caller passes
  # a precomputed `kernelTools` list (derived at eval time from the
  # kernel's own `tools/*/tool.toml`, via `builtins.readDir` +
  # `pathExists` in mk-rushi.nix), use it so the consumer can omit
  # `rushi.tools` and get the full kernel tool set. When null (e.g.
  # the docs-generation import from flake.nix, which does not have the
  # kernel path), fall back to the known set so the option still
  # evaluates.
  kernelToolNames =
    if kernelTools != null then kernelTools
    else [ "read" "write" "edit" "bash" ];
in
{
  # ── Option declarations ──
  options.rushi = {

    version = lib.mkOption {
      type = lib.types.str;
      default = "0.1";
      description = ''
        Rushi kernel version to target. Informational: recorded in
        the generated `tools.manifest` for `rushi setup --locked`.
        In practice the version is pinned by the flake ref (tag or
        branch) used to fetch the kernel flake.
      '';
    };

    config = lib.mkOption {
      type = lib.types.attrs;
      default = { };
      description = ''
        Contents of `config.toml` in Nix attrset form. Serialized to
        TOML at build time by the kernel flake's `lib/to-toml.nix`.

        Known sections (see `rushi config --help` for the runtime
        reference):

          [active]        active.model — name of the active model
          [model]         api, max_output_tokens, reasoning_effort,
                         plus per-model nested tables (e.g. model."deepseek")
          [paths]         sessions_root, native_tool_paths, extension_tool_paths
          [limits]        read/write/bash/compact limits
          [hooks]         timeout_ms, on = [ { window, command, args } ]
          [loop]          command, args, arg_style
          [system_prompt] text (empty = kernel default)
          [tui]           binary, color, color_scheme, ext_dirs,
                         tool_display.*   (TUI-owned; the harness
                         ignores these)

        Nested attrsets are deep-merged with the kernel defaults
        (via `lib.recursiveUpdate` in mk-rushi.nix); lists and
        scalars replace the default.
      '';
      example = {
        model = {
          api = "responses";
          max_output_tokens = 32768;
          "deepseek" = {
            model_id = "deepseek-v4-flash";
            base_url = "https://api.deepseek.com";
            api_key_env = "DEEPSEEK_API_KEY";
          };
        };
        active = { model = "deepseek"; };
      };
    };

    tools = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = kernelToolNames;
      description = ''
        Kernel tool names to ship in the package's `tools/` directory.
        Each name must correspond to a tool in the kernel's `tools/`
        dir (has a `tool.toml`). The generated `tools.manifest`
        records this list for `rushi setup --locked`.

        Defaults to the full kernel tool set, derived at eval time
        from the kernel's own `tools/*/tool.toml` (via
        `builtins.readDir` + `pathExists`). Override with a subset
        to restrict which kernel tools are bundled.

        The bash tool's binary is `harness-bash` (not `bash`), so
        it does not shadow the system shell.
      '';
      example = [ "read" "write" "edit" "bash" ];
    };

    ui_extensions = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      description = ''
        UI extension names to ship in the package's `ui_extensions/`
        directory. Names are matched against entries in the exts
        `ui_extensions/` dir. The generated `tools.manifest` records
        this list.
      '';
      example = [ "statusline-rs" "mermaid" ];
    };

    external_tools = lib.mkOption {
      type = rawList;
      default = [ ];
      description = ''
        External tool sources. Each entry is a Nix derivation (e.g.
        `pkgs.fetchFromGitHub { … }`, a cargo-built tool) or a path
        string. The derivation's `$out` must contain a
        `<tool-name>/tool.toml` + binary layout (a source with no
        `tool.toml` fails the build with a clear error).

        At build time the entries are copied into the package's
        `tools/` directory (additive to the kernel tools). When the
        source declares `meta.rushi = { entry = "…" }` (a producer
        flake attribute, issue #13), the path `tools/<entry>` is
        filled at eval time into the generated `config.toml` `[paths]
        extension_tool_paths`. The consumer declares each ext source
        once here, and the name rides on the producer's `meta`
        instead of a second hand-typed copy in the consumer config.
        Sources without `meta.rushi.entry` (including plain path
        strings, which cannot carry meta) fall back to build-time
        discovery with an eval-time warning. A consumer-set
        `rushi.config.paths.extension_tool_paths` (including an
        explicitly empty list) is authoritative and wins over the
        derived list.
      '';
    };

    external_ui_extensions = lib.mkOption {
      type = rawList;
      default = [ ];
      description = ''
        External UI extension sources (Nix derivations or path
        strings). Each source's `$out` must contain one or more
        entry dirs, each holding an `ext.toml` (a source with no
        `ext.toml` fails the build with a clear error). Copied into
        the package's `ui_extensions/` dir.

        When the source declares `meta.rushi = { ext = "…" }`
        (issue #13), the entry name is filled at eval time into the
        generated `tools.manifest` `[ui_extensions] enabled` list —
        the consumer declares each ext source once here. Sources
        without `meta.rushi.ext` fall back to build-time discovery
        with an eval-time warning. `ui_extension_names` overrides
        the derived list and drift-guards it.
      '';
    };

    # Optional override / drift-guard for the external UI extension
    # entry names. When empty (the default), the names come from the
    # producers' `meta.rushi.ext` declarations at eval time (issue
    # #13), and only sources lacking `meta.rushi.ext` fall back to
    # build-time discovery with an eval-time warning, so the consumer
    # declares each ext source once. When non-empty, the listed names
    # are used as-is in the generated `tools.manifest` and the build
    # verifies each name has a matching directory in the assembled
    # `ui_extensions/` dir (drift guard).
    ui_extension_names = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      description = ''
        Optional override / drift-guard for the external UI extension
        entry names (the top-level directory in each
        `external_ui_extensions` package's `$out` that holds
        `ext.toml`).

        When empty (default): names come from the producers'
        `meta.rushi.ext` declarations at eval time (issue #13), and
        only sources lacking meta.rushi.ext fall back to build-time
        discovery with a warning.

        When set: these names are used in the generated
        `tools.manifest`'s `[ui_extensions] enabled` list, and the
        build fails if any name has no matching entry directory in
        the assembled `ui_extensions/` dir (drift guard).
      '';
      example = [ "statusline" "goal" "simple-english" ];
    };

    external_hooks = lib.mkOption {
      type = rawList;
      default = [ ];
      description = ''
        External hook binary sources (Nix derivations or path
        strings). Each source must produce an executable or a `bin/`
        directory. Copied into the package's `hooks/` dir.

        Hook binaries are referenced by bare name in
        `config.hooks.on[].command`. A source declaring
        `meta.rushi = { bin = "…" }` (issue #13) is checked at eval
        time: bare commands matching a declared bin are trusted, and
        the build verifies the binary actually landed in `hooks/`.
        Bare commands not covered by any `meta.rushi.bin` are still
        verified at build time against `$out/bin/` and `$out/hooks/`.
        Commands written as explicit paths are not guarded (they are
        resolved verbatim at runtime).
      '';
    };

    # TUI binary (separate from the kernel; lives in the rushi-tui
    # repo). When set, the binary is shipped at $out/bin/tui so the
    # kernel's side-by-side resolver finds it. When null, the user
    # must provide a TUI on PATH or via [tui].binary in config.
    tui = lib.mkOption {
      type = lib.types.raw;
      default = null;
      description = ''
        Nix derivation (or store-path string) for the TUI binary.
        The derivation must expose the binary at `$out/bin/tui`
        (standard Nix package layout).

        When set, the `tui` binary is copied into the package at
        `$out/bin/tui`, so the kernel's side-by-side resolver
        (`<exe_dir>/tui`) finds it automatically.

        When `null` (the default), no TUI is shipped. The user must
        either place a `tui` binary on `PATH` or set
        `[tui].binary` in `rushi.config` to a resolvable path.

        Example (consumer flake):
          rushi.tui = rushi-tui-flake.packages.<system>.default;
      '';
      example = null;
    };

    # Environment variables exported into the rushi process at
    # runtime (mirrors pi-flake's `pi.coding-agent.environment`).
    #
    # Each value is one of three forms (free-form type; interpreted
    # at build time by mk-rushi.nix):
    #   "literal"                    → export KEY=literal
    #   { value = "literal"; }      → export KEY=literal
    #   { file = <derivation>; }    → export KEY="$(cat <storepath>)"
    #
    # The `file` form supports sops-nix secrets: pass a sops-nix
    # derivation and its content is read at runtime, never stored in
    # the Nix store as plain text.
    environment = lib.mkOption {
      type = lib.types.attrsOf lib.types.raw;
      default = { };
      description = ''
        Environment variables exported into the `rushi` process at
        runtime. The configured package wraps `bin/rushi` in a shell
        script that exports these variables before exec'ing the real
        binary.

        Model API keys reference env-var *names* in
        `config.model.<name>.api_key_env`; use this option to provide
        the values (including sops-nix secrets via the `file` form).

        Forms (per key):
          `KEY = "value"`              literal string
          `KEY = { value = "…"; }`    literal (explicit tag)
          `KEY = { file = drv; }`     value read from `drv` at runtime

        The `file` form is the sops-nix integration point: pass a
        sops-nix derivation and the secret is decrypted at runtime,
        never stored in the Nix store as plain text.
      '';
      example = {
        DEEPSEEK_API_KEY = { file = "sopsSecret"; };
        RUSHI_LOG = "debug";
        RUSHI_TUI = { value = "true"; };
      };
    };
  };
}
