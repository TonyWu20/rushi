# lib/mk-rushi.nix — `lib.mkRushi`: build a fully configured rushi package.
#
# Usage (consumer flake, mirroring the pi-flake.mkCodingAgent pattern):
#
#   rushi = rushi-flake.lib.mkRushi {
#     inherit pkgs;
#     modules = [
#       { config, lib, pkgs, ... }: {
#         rushi.config.model = { api = "responses"; max_output_tokens = 32768; };
#         rushi.config.limits = { context_budget_tokens = 262144; };
#         rushi.tools = [ "read" "write" "edit" "bash" ];
#       }
#     ];
#     # extraSpecialArgs lets you inject custom args into every module:
#     # extraSpecialArgs = { inherit mySecrets; };
#   };
#   → { package, config, version, options }
#
# ── Module system (full NixOS) ─────────────────────────────────────
#   Built on `lib.evalModules`, so every NixOS module feature works:
#
#     * Composable modules  — pass a list of module functions; they
#       merge left-to-right. Different modules can set different
#       top-level keys under `rushi.config` (e.g. one sets
#       `rushi.config.model`, another sets `rushi.config.hooks`) and
#       they recursive-merge together.
#
#     * Option types        — every `rushi.*` option is declared with
#       `lib.mkOption { type, default, description, example }`, so
#       type errors surface at eval time with clear messages.
#
#     * Conditionals        — `lib.mkIf`, `lib.mkMerge`,
#       `lib.mkOverride`, `lib.mkForce` are all available inside
#       consumer modules (they're NixOS builtins).
#
#     * `config` access     — modules receive the accumulated
#       `config` attribute, so a module can branch on another
#       module's values:
#
#           { config, lib, pkgs, ... }:
#           lib.mkIf (config.rushi.tools ? "bash") {
#             rushi.config.limits = { bash_timeout_max = 300; };
#           }
#
#     * `specialArgs`       — `pkgs` and `lib` are always available in
#       module scope. Pass `extraSpecialArgs` to add your own:
#
#           extraSpecialArgs = { inherit mySecrets; };
#           # module can then use: `mySecrets.apiKey`
#
#     * Option introspection — the result's `.options` field exposes
#       the full option schema (usable with `nixosOptionsDoc` for
#       auto-generated docs, like pi-flake's `docs-md`/`docs-html`).
#
# ── Config deep-merge semantics ────────────────────────────────────
#   `rushi.config` is a free-form `attrs` option (mirrors the
#   `config.toml` section layout). NixOS recursively merges
#   `rushi.config` values across modules, but does NOT deep-merge
#   the option's `default` with module values. `mkRushi` handles this
#   by deep-overlaying the kernel defaults (`lib/rushi-defaults.nix`)
#   underneath the NixOS-merged result via `lib.recursiveUpdate`:
#
#       merged = recursiveUpdate(kernelDefaults, userMerged)
#
#   This gives users the expected overlay behaviour: set one field,
#   the rest stay at kernel defaults.
#
# ── Package layout ($out) ──────────────────────────────────────────
#   bin/             rushi + all tool/hook/stage binaries
#   tools/           tool manifests (tool.toml per enabled tool)
#   ui_extensions/   UI extension files
#   hooks/           hook binaries (if any)
#   config.toml      generated from the deep-merged options
#   tools.manifest   rushi.toml-equivalent for `rushi setup --locked`
#
# Mirrors the pi-flake.lib.mkCodingAgent pattern: the kernel flake
# provides the builder; the consumer flake provides configuration
# modules. See docs/reference/nix/nix-flake-module.md for the full
# option schema.

{ pkgs
, modules ? [ ]
, src
, rustToolchain ? null
, extraSpecialArgs ? { }
, ...
}:

let
  lib = pkgs.lib;

  # ── TOML serializer ──
  toTomlDocument = import ./to-toml.nix;

  # ── Kernel defaults ──
  defaults = import ./rushi-defaults.nix;

  # ── 1. Evaluate the NixOS module system ──
  #
  # optionsModule declares the rushi.* schema with lib.mkOption types,
  # defaults, descriptions, and examples.  User modules (from the
  # `modules` parameter) set values.  evalModules type-checks all
  # option values and exposes the option schema for docs generation.
  #
  # extraSpecialArgs (like pi-flake) lets the caller inject additional
  # special args into every module's scope (e.g. sops-nix secrets,
  # environment-specific overrides).
  optionsModule = import ./rushi-options.nix { inherit lib; };

  evaluated = lib.evalModules {
    specialArgs = {
      inherit pkgs lib;
    } // extraSpecialArgs;
    modules = [ optionsModule ] ++ modules;
  };

  # Simple (non-nested-attrs) options: NixOS merge handles these
  # correctly (str, listOf str, listOf raw — last module wins).
  version      = evaluated.config.rushi.version;
  tools        = evaluated.config.rushi.tools;
  uiExtensions = evaluated.config.rushi.ui_extensions;
  extTools     = evaluated.config.rushi.external_tools;
  extUiExts    = evaluated.config.rushi.external_ui_extensions;
  extHooks     = evaluated.config.rushi.external_hooks;
  tuiPkg       = evaluated.config.rushi.tui or null;

  # ── 2. Deep-merge rushi.config ──
  #
  # NixOS recursively merges `rushi.config` values across modules
  # (different top-level keys merge; same keys, later module wins).
  # The option's `default` ({}) is NOT deep-merged with module values,
  # so we manually overlay the kernel defaults underneath:
  #
  #   kernelDefaults ← userMerged (user wins on conflict)
  #
  # This is the overlay semantics a user expects from a config system.
  userConfig   = evaluated.config.rushi.config or { };
  mergedConfig = lib.recursiveUpdate defaults.config userConfig;

  # ── 3. Build the rushi kernel binary ──
  fenixInput = pkgs.fenix or null;
  toolchain = if rustToolchain != null then rustToolchain
    else if fenixInput != null then
      fenixInput.stable.withComponents [ "cargo" "rust-src" "rustc" "rustfmt" ]
    else
      throw "rushi.mkRushi: fenix not in pkgs and no rustToolchain passed. "
        + "Use an overlay that adds fenix (see examples/rushi-config/flake.nix).";

  rushi = pkgs.rustPlatform.buildRustPackage rec {
    pname = "rushi";
    inherit version src;
    cargoLock = {
      lockFile = src/Cargo.lock;
    };
    nativeBuildInputs = [ toolchain ];
    # Build all workspace members (loop stages, tools, hooks).
    cargoBuildFlags = [ "--workspace" ];
    doCheck = false;
    # Ship tool manifests alongside bin/ so resolve_kernel_tools_dir
    # (<exe>/../tools) works without RUSHI_KERNEL.
    postInstall = ''
      mkdir -p $out/tools
      cp -r $src/tools/. $out/tools/
    '';
  };

  # ── 4. Generate config.toml from the deep-merged options ──
  configTomlText = toTomlDocument mergedConfig;

  # ── 5. Assemble the configured package ──

  # All external Nix values that must be in the store before we build.
  allExternalDeps = extTools ++ extUiExts ++ extHooks;

  # Convert a Nix value to a shell-safe path string.
  # Nix derivations expand to their store path; string paths pass through.
  toShellPath = v: if builtins.isString v then v else toString v;

  extToolScript = builtins.concatStringsSep "\n" (
    map (s:
      let sp = toShellPath s; in ''
        # External tool source: ${sp}
        if [ -d "${sp}" ]; then
          cp -rL "${sp}/." "$out/tools/" 2>/dev/null || true
        fi
      ''
    ) extTools
  );

  extUiScript = builtins.concatStringsSep "\n" (
    map (s:
      let sp = toShellPath s; in ''
        # External UI extension source: ${sp}
        if [ -d "${sp}" ]; then
          cp -rL "${sp}/." "$out/ui_extensions/" 2>/dev/null || true
        fi
      ''
    ) extUiExts
  );

  extHookScript = builtins.concatStringsSep "\n" (
    map (s:
      let sp = toShellPath s; in ''
        # External hook source: ${sp}
        if [ -x "${sp}" ]; then
          cp "${sp}" "$out/hooks/"
        elif [ -d "${sp}/bin" ]; then
          cp -rL "${sp}/bin/." "$out/hooks/" 2>/dev/null || true
        elif [ -d "${sp}" ]; then
          cp -rL "${sp}/." "$out/hooks/" 2>/dev/null || true
        fi
      ''
    ) extHooks
  );

  # tools.manifest: rushi.toml-equivalent for `rushi setup --locked`.
  toolListStr  = builtins.concatStringsSep ", " (map (t: "\"${t}\"") tools);
  extListStr   = builtins.concatStringsSep ", " (map (t: "\"${t}\"") uiExtensions);
  toolsManifestText = ''
    [rushi]
    version = "${version}"

    [tools]
    enabled = [ ${toolListStr} ]

    [ui_extensions]
    enabled = [ ${extListStr} ]
  '';

  # ── 6. Environment variables (rushi.environment) ──
  #
  # Generate `export KEY=…` lines from the three supported forms:
  #   "literal"        → export KEY="literal"
  #   { value = "…" } → export KEY="…"
  #   { file = drv }  → export KEY="$(cat <storepath>)"
  #
  # The file form is the sops-nix integration point: the derivation's
  # store path is referenced but not expanded at build time, so
  # secret material never lands in the Nix store as plain text.
  envVars        = evaluated.config.rushi.environment or { };
  envExportLines = lib.mapAttrsToList (key: val:
    if builtins.isString val then
      "export ${key}=\"${val}\""
    else if val ? value then
      "export ${key}=\"${val.value}\""
    else if val ? file then
      "export ${key}=\"\$(cat ${toString val.file})\""
    else
      ""
  ) envVars;
  hasEnv    = envExportLines != [ ];
  envFile   = pkgs.writeText "rushi-env.sh"
    (if hasEnv then (builtins.concatStringsSep "\n" (lib.filter (l: l != "") envExportLines)) + "\n" else "");
  # Derivations referenced via `file =` must be build inputs so their
  # store paths are materialized before the wrapper runs.
  envFileDeps = lib.concatLists (
    lib.mapAttrsToList (_: v: if v ? file then [ v.file ] else [ ]) envVars
  );
  envFileNixDeps = builtins.filter (x:
    (builtins.isAttrs x)
    || (builtins.isString x && (builtins.match ''/nix/store/.*'' x) != null)
  ) envFileDeps;

  # Embed generated files as Nix text files (referenced via passAsFile).
  configTomlFile  = pkgs.writeText "rushi-config.toml" configTomlText;
  manifestFile    = pkgs.writeText "rushi-tools-manifest" toolsManifestText;

  # Only Nix values (derivations, paths) need to be build inputs;
  # plain string paths are used directly in the shell.
  nixDeps = builtins.filter (x:
    (builtins.isAttrs x)
    || (builtins.isString x && (builtins.match ''/nix/store/.*'' x) != null)
  ) allExternalDeps;

  # TUI binary (rushi.tui): add Nix derivation to build inputs.
  tuiNixDep = if tuiPkg != null && (
      (builtins.isAttrs tuiPkg)
      || (builtins.isString tuiPkg && (builtins.match ''/nix/store/.*'' tuiPkg) != null)
    ) then [ tuiPkg ] else [ ];

  # Shell script to copy the TUI binary into the package.
  tuiInstallScript = if tuiPkg != null then
    let tuiPath = toShellPath tuiPkg; in
    ''
      # ── TUI binary (rushi.tui) ──
      TUI_SRC="${tuiPath}"
      if [ -f "$TUI_SRC/bin/tui" ]; then
        cp "$TUI_SRC/bin/tui" "$out/bin/tui"
        chmod +x "$out/bin/tui"
      else
        echo "WARNING: rushi.tui (${tuiPath}) has no bin/tui; TUI unavailable." >&2
      fi
    ''
  else "";

  package = pkgs.stdenv.mkDerivation {
    pname = "rushi-configured";
    inherit version;

    # Kernel binary (provides all stage + tool binaries and tool manifests).
    buildInputs = [ rushi ];

    # External sources (derivations only; string paths are used as-is).
    nativeBuildInputs = nixDeps ++ envFileNixDeps ++ tuiNixDep;

    # Pass generated text files via file descriptors (avoids long
    # Nix store paths in the shell command).
    passAsFile = [ configTomlFile manifestFile envFile ];

    installPhase = ''
      # ── Directory layout ──
      mkdir -p $out/bin $out/tools $out/ui_extensions $out/hooks

      # ── All kernel binaries (rushi, tools, hooks, stage CLIs) ──
      # The kernel build's bin/ contains:
      #   rushi, read, write, edit, harness-bash, harness-hook-compact,
      #   claim, assemble, compact, model, parse, route, log, user,
      #   rewind-drt
      # Tool resolution at runtime uses the `command` field from
      # tool.toml (e.g. "harness-bash" for the bash tool), so all
      # binaries must be in $out/bin/.
      cp -rL ${rushi}/bin/. $out/bin/
      chmod -R +x $out/bin/

      # ── Tool manifests (tool.toml only, skip source/Cargo noise) ──
      if [ -d "${rushi}/tools" ]; then
        for toml in "${rushi}"/tools/*/tool.toml; do
          [ -f "$toml" ] || continue
          name=$(basename "$(dirname "$toml")")
          mkdir -p "$out/tools/$name"
          cp "$toml" "$out/tools/$name/tool.toml"
        done
      fi

      # ── External tool sources (add to tools/) ──
      ${extToolScript}

      # ── External UI extension sources (add to ui_extensions/) ──
      ${extUiScript}

      # ── External hook binaries (add to hooks/) ──
      ${extHookScript}

      # ── TUI binary (rushi.tui) ──
      ${tuiInstallScript}

      # ── Generated config.toml ──
      cp "${configTomlFile}" $out/config.toml

      # ── tools.manifest (rushi.toml equivalent) ──
      cp "${manifestFile}" $out/tools.manifest

      # ── Environment variables (rushi.environment) ──
      # If env vars were declared, wrap bin/rushi so they are
      # exported before the real binary runs. The export lines live
      # in $out/rushi.env (also sourceable manually).
      if [ -s "${envFile}" ]; then
        cp "${envFile}" $out/rushi.env
        mv $out/bin/rushi $out/bin/.rushi-real
        printf '%s\n' \
          '#!/bin/sh' \
          '# Auto-generated by rushi.mkRushi — exports rushi.environment' \
          'BIN_DIR="$(cd "$(dirname "$0")" && pwd)"' \
          '. "$BIN_DIR/../rushi.env"' \
          'exec "$BIN_DIR/.rushi-real" "$@"' \
          > $out/bin/rushi
        chmod +x $out/bin/rushi
      fi

      # ── Sanity check ──
      test -x $out/bin/rushi || { echo "ERROR: rushi binary missing"; exit 1; }
    '';

    meta = with lib; {
      description = "Configured rushi agent harness (v${version})";
      license = licenses.asl20;
      platforms = platforms.linux;
      mainProgram = "rushi";
    };
  };

in
{
  inherit package;
  config = configTomlText;
  inherit version;
  # Expose the full option schema for documentation / introspection
  # (analogous to pi-flake's `options` output, usable with nixosOptionsDoc).
  inherit (evaluated) options;
  # Expose the raw evaluated config (pre-TOML) for programmatic access.
  # This is the deep-merged Nix attrset (defaults + user overlays).
  configAttrs = mergedConfig;
}
