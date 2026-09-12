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

{ lib, ... }:

let
  # Convenience: "free-form" list type for external sources
  # (derivations, store paths, or plain strings).
  rawList = lib.types.listOf lib.types.raw;
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
          [paths]         sessions_root, tools_root, extra_tools_roots
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
      default = [ "read" "write" "edit" "bash" ];
      description = ''
        Kernel tool names to ship in the package's `tools/` directory.
        Each name must correspond to a tool in the kernel's `tools/`
        dir (has a `tool.toml`). The generated `tools.manifest`
        records this list for `rushi setup --locked`.

        Kernel tools: `read`, `write`, `edit`, `bash`.
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
        `<tool-name>/tool.toml` + binary layout.

        At build time the entries are copied into the package's
        `tools/` directory (additive to the kernel tools).
      '';
    };

    external_ui_extensions = lib.mkOption {
      type = rawList;
      default = [ ];
      description = ''
        External UI extension sources (Nix derivations or path
        strings). Copied into the package's `ui_extensions/` dir.
      '';
    };

    external_hooks = lib.mkOption {
      type = rawList;
      default = [ ];
      description = ''
        External hook binary sources (Nix derivations or path
        strings). Each source must produce an executable or a `bin/`
        directory. Copied into the package's `hooks/` dir.

        Hook binaries are referenced by name in
        `config.hooks.on[].command`.
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
