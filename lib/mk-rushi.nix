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
, cargoLockContents ? null
, kernelCargoLock ? null
, ...
}:

let
  lib = pkgs.lib;

  # ── TOML serializer ──
  toTomlDocument = import ./to-toml.nix;

  # ── Kernel defaults ──
  defaults = import ./rushi-defaults.nix;

  # ── 0. Kernel tool names (eval-time, from the kernel's own tools/) ──
  #
  # Derived from src/tools/*/tool.toml. Two consumers:
  #   * `rushi.tools` option default (full kernel tool set, so the
  #     consumer can omit it entirely)
  #   * build-time ext-tool discovery filter (subdirs of $out/tools
  #     that are NOT in this set are extension tool dirs)
  nativeToolNames =
    let
      toolsDir = src + "/tools";
      dirNames = builtins.attrNames (builtins.readDir toolsDir);
    in
    builtins.filter (n:
      builtins.pathExists (toolsDir + "/${n}/tool.toml")
    ) dirNames;

  # ── 1. Evaluate the NixOS module system ──
  #
  # optionsModule declares the rushi.* schema with lib.mkOption types,
  # defaults, descriptions, and examples.  User modules (from the
  # `modules` parameter) set values.  evalModules type-checks all
  # option values and exposes the option schema for docs generation.
  #
  # nativeToolNames is threaded in as `kernelTools` so the `tools`
  # option default is the kernel's real tool set, not a hand-maintained
  # list. extraSpecialArgs (like pi-flake) lets the caller pass more
  # special args into every module's scope (e.g. sops-nix secrets,
  # environment-specific overrides).
  optionsModule =
    import ./rushi-options.nix { kernelTools = nativeToolNames; inherit lib; };

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
  extUiExtNames = evaluated.config.rushi.ui_extension_names;
  extHooks     = evaluated.config.rushi.external_hooks;
  tuiPkg       = evaluated.config.rushi.tui or null;

  # Inputs for build-time discovery and drift guards, interpolated into
  # installPhase as shell variables.
  discoveryInputs =
    let
      userExtToolPaths = mergedConfig.paths.extension_tool_paths or [ ];
      bareHookCommands =
        builtins.filter (c: c != "" && builtins.match "^[^/]+$" c != null)
          (builtins.map (e:
             if builtins.isAttrs e then e.command or "" else ""
           ) (mergedConfig.hooks.on or [ ]));
    in
    { inherit userExtToolPaths bareHookCommands; };

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
    # The kernel's Cargo.lock, in order of preference:
    #   1. kernelCargoLock — a Nix path reference to the kernel's
    #      Cargo.lock, supplied by the flake wrapper where `./Cargo.lock`
    #      resolves correctly (even when the kernel is a flake path input).
    #   2. cargoLockContents — the lock text read by the flake, fed
    #      directly to importCargoLock.
    #   3. src/Cargo.lock — the path form for direct (non-flake) callers.
    cargoLock = if kernelCargoLock != null then
      kernelCargoLock
    else if cargoLockContents != null then
      { lockFileContents = cargoLockContents; }
    else
      { lockFile = src/Cargo.lock; };
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

  # External tool sources. Each is copied into $out/tools/ and must
  # ship at least one tool manifest (a tool.toml at the source top
  # level or in a sub-dir). A source with none is a broken ext and
  # fails the build with a clear error. This is the declare-once
  # guarantee: the source is the single declaration of this ext tool.
  extToolScript = builtins.concatStringsSep "\n" (
    map (s:
      let sp = toShellPath s; in ''
        # External tool source: ${sp}
        if [ ! -d "${sp}" ]; then
          echo "mkRushi: external tool source ${sp} is missing or not a directory" >&2
          exit 1
        fi
        ext_toml=""
        for t in "${sp}"/tool.toml "${sp}"/*/tool.toml; do
          if [ -f "$t" ]; then ext_toml="$t"; break; fi
        done
        if [ -z "$ext_toml" ]; then
          echo "mkRushi: external tool source ${sp} ships no tool.toml (broken ext)" >&2
          exit 1
        fi
        cp -rL "${sp}/." "$out/tools/"
      ''
    ) extTools
  );

  # External UI extension sources. Each is copied into
  # $out/ui_extensions/. Each source must ship at least one entry dir
  # with an ext.toml. A source with none is a broken ext and fails
  # the build with a clear error. Entry dir names are auto-discovered
  # at build time (see installPhase) unless ui_extension_names
  # overrides them.
  extUiScript = builtins.concatStringsSep "\n" (
    map (s:
      let sp = toShellPath s; in ''
        # External UI extension source: ${sp}
        if [ ! -d "${sp}" ]; then
          echo "mkRushi: external UI ext source ${sp} is missing or not a directory" >&2
          exit 1
        fi
        ext_toml=""
        for t in "${sp}"/ext.toml "${sp}"/*/ext.toml; do
          if [ -f "$t" ]; then ext_toml="$t"; break; fi
        done
        if [ -z "$ext_toml" ]; then
          echo "mkRushi: external UI ext source ${sp} ships no ext.toml (broken ext)" >&2
          exit 1
        fi
        cp -rL "${sp}/." "$out/ui_extensions/"
      ''
    ) extUiExts
  );

  extHookScript = builtins.concatStringsSep "\n" (
    map (s:
      let sp = toShellPath s; in ''
        # External hook source: ${sp}
        # Order matters: a store-dir (buildRustPackage) is a directory
        # that is -x but must be recursed into, not cp'd as a file.
        if [ -d "${sp}/bin" ]; then
          cp -rL "${sp}/bin/." "$out/hooks/" 2>/dev/null || true
        elif [ -d "${sp}" ]; then
          cp -rL "${sp}/." "$out/hooks/" 2>/dev/null || true
        elif [ -e "${sp}" ]; then
          cp "${sp}" "$out/hooks/"
        fi
      ''
    ) extHooks
  );

  # tools.manifest is generated at build time in installPhase so the
  # [ui_extensions] enabled list can include ext entry names
  # auto-discovered from the bundled UI ext packages. The [tools]
  # enabled list and [rushi] version are static, interpolated from
  # Nix into the shell at build time.

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
  # (tools.manifest is generated at build time in installPhase, not
  # embedded, because its [ui_extensions] list depends on build-time
  # discovery of the bundled ext entry dirs.)
  configTomlFile  = pkgs.writeText "rushi-config.toml" configTomlText;

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
    # No source unpacking: installPhase copies from pre-built packages
    # (rushi, exts, TUI), not from $src.
    dontUnpack = 1;

    # Kernel binary (provides all stage + tool binaries and tool manifests).
    buildInputs = [ rushi ];

    # External sources (derivations only; string paths are used as-is).
    nativeBuildInputs = nixDeps ++ envFileNixDeps ++ tuiNixDep;

    # Pass generated text files via file descriptors (avoids long
    # Nix store paths in the shell command).
    passAsFile = [ configTomlFile envFile ];

    installPhase = ''
      # ── Build-time discovery inputs (Nix-interpolated) ──
      # NATIVE_TOOLS: kernel tool dir names (excluded from ext discovery).
      # USER_EXT_TOOL_PATHS: user-set [paths] extension_tool_paths entries.
      # USER_UI_EXT_NAMES: ui_extension_names override (drift-guarded).
      # UI_EXT_BASE_NAMES: kernel-bundled ui extension names.
      # BARE_HOOK_COMMANDS: bare config.hooks.on commands to verify.
      # TOOLS_RAW: the [tools] enabled list (rushi.tools option value).
      NATIVE_TOOLS="${builtins.concatStringsSep " " nativeToolNames}"
      USER_EXT_TOOL_PATHS="${builtins.concatStringsSep " " discoveryInputs.userExtToolPaths}"
      USER_UI_EXT_NAMES="${builtins.concatStringsSep " " extUiExtNames}"
      UI_EXT_BASE_NAMES="${builtins.concatStringsSep " " uiExtensions}"
      BARE_HOOK_COMMANDS="${builtins.concatStringsSep " " discoveryInputs.bareHookCommands}"
      TOOLS_RAW="${builtins.concatStringsSep " " tools}"

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

      # ── Auto-derive [paths] extension_tool_paths ──
      # Discover ext tool dirs: subdirs of $out/tools/ that hold a
      # tool.toml and are not native kernel tools. Merge with the
      # user-set entries (deduped) and rewrite the line in the
      # generated config.toml below.
      ext_tool_paths=""
      for d in "$out"/tools/*/tool.toml; do
        if [ -f "$d" ]; then
          name=$(basename "$(dirname "$d")")
          is_native=0
          for nt in $NATIVE_TOOLS; do
            if [ "$name" = "$nt" ]; then is_native=1; break; fi
          done
          if [ "$is_native" -eq 0 ]; then
            ext_tool_paths="$ext_tool_paths tools/$name"
          fi
        fi
      done
      all_ext_paths="$USER_EXT_TOOL_PATHS $ext_tool_paths"
      final_ext_paths=""
      for p in $all_ext_paths; do
        case " $final_ext_paths " in *" $p "*) continue ;; esac
        final_ext_paths="$final_ext_paths $p"
      done
      ext_paths_toml=""
      for p in $final_ext_paths; do
        if [ -z "$ext_paths_toml" ]; then
          ext_paths_toml=$(printf '"%s"' "$p")
        else
          ext_paths_toml="$ext_paths_toml, $(printf '"%s"' "$p")"
        fi
      done

      # ── Auto-discover [ui_extensions] entry names ──
      # Each entry dir in $out/ui_extensions/ holds an ext.toml. When
      # ui_extension_names is set it overrides discovery and is
      # drift-guarded (every name must have a dir). When empty, the
      # discovered names are used. Kernel-bundled names (ui_extensions)
      # are always listed first.
      discovered_ui=""
      for d in "$out"/ui_extensions/*/ext.toml; do
        if [ -f "$d" ]; then
          discovered_ui="$discovered_ui $(basename "$(dirname "$d")")"
        fi
      done
      if [ -n "$USER_UI_EXT_NAMES" ]; then
        for n in $USER_UI_EXT_NAMES; do
          if [ ! -d "$out/ui_extensions/$n" ]; then
            echo "mkRushi: ui_extension_names '$n' has no entry in $out/ui_extensions/ (drift)" >&2
            exit 1
          fi
        done
        ext_ui_names="$USER_UI_EXT_NAMES"
      else
        ext_ui_names="$discovered_ui"
      fi
      all_ui_names="$UI_EXT_BASE_NAMES $ext_ui_names"
      final_ui_names=""
      for n in $all_ui_names; do
        case " $final_ui_names " in *" $n "*) continue ;; esac
        final_ui_names="$final_ui_names $n"
      done
      ui_ext_toml=""
      for n in $final_ui_names; do
        if [ -z "$ui_ext_toml" ]; then
          ui_ext_toml=$(printf '"%s"' "$n")
        else
          ui_ext_toml="$ui_ext_toml, $(printf '"%s"' "$n")"
        fi
      done

      # ── Hook command drift guard ──
      # Every bare command in config.hooks.on must resolve to a file
      # in $out/bin/ or $out/hooks/ (the kernel's runtime resolution
      # for bare names, per resolve_hook_command). A typo or an
      # unbundled hook fails the build with a clear message.
      missing_hooks=""
      for cmd in $BARE_HOOK_COMMANDS; do
        if [ ! -f "$out/bin/$cmd" ] && [ ! -f "$out/hooks/$cmd" ]; then
          missing_hooks="$missing_hooks $cmd"
        fi
      done
      if [ -n "$missing_hooks" ]; then
        echo "mkRushi: hook command(s) not found in $out/bin/ or $out/hooks/:$missing_hooks" >&2
        echo "  Bundle them via rushi.external_hooks, or set a full path in config.hooks.on[].command." >&2
        exit 1
      fi

      # ── Manifest [tools] enabled list (rushi.tools value) ──
      tools_toml=""
      for t in $TOOLS_RAW; do
        if [ -z "$tools_toml" ]; then
          tools_toml=$(printf '"%s"' "$t")
        else
          tools_toml="$tools_toml, $(printf '"%s"' "$t")"
        fi
      done

      # ── Generated config.toml (extension_tool_paths filled in) ──
      cp "${configTomlFile}" $out/config.toml
      sed -i "s|^extension_tool_paths = .*|extension_tool_paths = [ $ext_paths_toml ]|" $out/config.toml

      # ── tools.manifest (rushi.toml equivalent, built at build time) ──
      {
        printf '[rushi]\n'
        printf 'version = "%s"\n' "$version"
        printf '\n'
        printf '[tools]\n'
        printf 'enabled = [ %s ]\n' "$tools_toml"
        printf '\n'
        printf '[ui_extensions]\n'
        printf 'enabled = [ %s ]\n' "$ui_ext_toml"
      } > $out/tools.manifest

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
