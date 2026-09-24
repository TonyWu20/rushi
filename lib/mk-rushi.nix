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

  # ── 1.5. Producer-declared names: `meta.rushi` (issue #13) ──
  #
  # Each external package may carry the standard `meta` attribute
  # declaring what it provides, like `meta.mainProgram` does. A
  # package sets exactly one field of `meta.rushi`:
  #
  #   entry  tool package: the entry dir copied into <pkg>/tools/
  #   ext    UI-ext package: the entry dir copied into <pkg>/ui_extensions/
  #   bin    hook package: the binary name copied into <pkg>/hooks/
  #
  # mkRushi reads these at eval time and fills
  # config.paths.extension_tool_paths plus the manifest [ui_extensions]
  # enabled list when the consumer has not set them. A non-empty
  # consumer value stays authoritative. Sources without a usable
  # meta.rushi field (plain paths cannot carry meta) keep the #10
  # build-time discovery as a fallback, with an eval-time warning.

  # `meta.rushi` for a source. `{ }` when absent, which is the case
  # for plain path strings. A present-but-malformed `meta.rushi` is an
  # error, not a silent no-op.
  rushiMeta = s:
    if builtins.isAttrs s && s ? meta && s.meta ? rushi then
      let m = s.meta.rushi; in
      if builtins.isAttrs m then m
      else
        throw "rushi.mkRushi: meta.rushi on ${toString s} must be an attrset with one of entry / ext / bin, got ${builtins.typeOf m}"
    else { };

  # `meta.rushi.entry` per external_tools source: the tool entry dir
  # name that the source ships under its $out (it is copied into the
  # configured package at <pkg>/tools/<entry>). Sources without the
  # field fall back to build-time discovery. The warning fires at eval
  # time when this value is forced, i.e. when the fallback kicks in.
  metaToolEntries =
    lib.foldl
      (acc: s:
        let m = rushiMeta s; in
        if m ? entry then
          acc ++ [ m.entry ]
        else
          lib.warn "rushi.mkRushi: external_tools source ${toString s} has no meta.rushi.entry — falling back to build-time discovery. Declare meta.rushi.entry in the producer flake (issue #13)" acc)
      [ ]
      extTools;

  # `meta.rushi.ext` per external_ui_extensions source: the UI-ext
  # entry dir that the source ships under $out.
  metaExtUiNames =
    lib.foldl
      (acc: s:
        let m = rushiMeta s; in
        if m ? ext then
          acc ++ [ m.ext ]
        else
          lib.warn "rushi.mkRushi: external_ui_extensions source ${toString s} has no meta.rushi.ext — falling back to build-time discovery. Declare meta.rushi.ext in the producer flake (issue #13)" acc)
      [ ]
      extUiExts;

  # `meta.rushi.bin` per external_hooks source: the hook binary name
  # shipped under $out/hooks/. A bare hook command matching one of
  # these is statically known-bundled, so the build-time guard only
  # covers the rest (plain-path sources, kernel bins, typos).
  metaHookBins =
    lib.map (s: (rushiMeta s).bin) (lib.filter (s: rushiMeta s ? bin) extHooks);

  # The sources that keep the #10 build-time discovery fallback.
  toolFallbackSources =
    lib.filter (s: !(rushiMeta s ? entry)) extTools;
  uiFallbackSources =
    lib.filter (s: !(rushiMeta s ? ext)) extUiExts;

  # ── 2. Deep-merge rushi.config ──
  #
  # NixOS recursively merges `rushi.config` values across modules
  # (different top-level keys merge. Same key: later module wins).
  # The option's `default` ({}) is NOT deep-merged with module values,
  # so we manually overlay the kernel defaults underneath:
  #
  #   kernelDefaults ← userMerged (user wins on conflict)
  #
  # This is the overlay semantics a user expects from a config system.
  userConfig   = evaluated.config.rushi.config or { };
  baseConfig =
    lib.recursiveUpdate defaults.config userConfig;

  # ── 2.5. Eval-time fill from `meta.rushi` (issue #13) ──
  #
  # A consumer-set value is authoritative as-is: it is used verbatim
  # (no merge, no discovery, no dedup). Only when the consumer left
  # extension_tool_paths / ui_extension_names unset does mkRushi fill
  # them from the producers' meta.rushi declarations. An explicit
  # `= [ ]` counts as set: the empty list stays empty, and no
  # fallback discovery runs. Filled values land in the returned
  # `config` text and `configAttrs`, and the shipped config.toml is
  # written once from them, byte-identical to the returned config,
  # unless the build-time fallback below appends names.
  userExtToolPathsSet =
    (userConfig ? paths) && (userConfig.paths ? extension_tool_paths);
  userExtToolPaths =
    if userExtToolPathsSet then
      userConfig.paths.extension_tool_paths
    else
      [ ];

  # Config paths carry the "tools/" prefix (the ext copy target), so
  # entry dir names are expanded here. This matches the shape of the
  # #10 build-time discovery output.
  finalExtToolPaths =
    if userExtToolPathsSet
    then userExtToolPaths
    else
      lib.unique (lib.map (e: "tools/${e}") metaToolEntries);

  finalExtUiNames =
    if extUiExtNames != [ ]
    then extUiExtNames
    else
      lib.unique metaExtUiNames;

  # The manifest [ui_extensions] enabled list: kernel-bundled names
  # (the `rushi.ui_extensions` option) first, then the ext names.
  finalUiNames =
    lib.unique (uiExtensions ++ finalExtUiNames);

  extPathsOverride =
    lib.listToAttrs [
      (lib.nameValuePair "extension_tool_paths" finalExtToolPaths)
    ];
  filledPaths =
    baseConfig.paths // extPathsOverride;
  filledConfig =
    lib.recursiveUpdate baseConfig (lib.listToAttrs [
      (lib.nameValuePair "paths" filledPaths)
    ]);
  mergedConfig =
    if userExtToolPathsSet then
      baseConfig
    else
      filledConfig;

  # Build-time discovery runs only when a source lacks a meta.rushi
  # field AND the consumer did not set the value themselves.
  needToolDiscovery =
    !userExtToolPathsSet
    && toolFallbackSources != [ ];
  needUiDiscovery =
    extUiExtNames == [ ]
    && uiFallbackSources != [ ];

  # Bare hook commands the build-time guard still checks: every bare
  # command of a `[hooks.defs.<name>]` entry that is not statically
  # known from a bundled hook package's `meta.rushi.bin` (issue #13).
  hookDefs =
    mergedConfig.hooks.defs or { };
  hookCommands =
    builtins.map (d: if builtins.isAttrs d then d.command or "" else "")
      (builtins.attrValues hookDefs);
  bareHookCommands =
    builtins.filter (c: c != "" && builtins.match "^[^/]+$" c != null)
      hookCommands;
  guardedHookCommands =
    builtins.filter (c: !(builtins.elem c metaHookBins))
      bareHookCommands;

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
  #
  # The TUI flake (rushi-tui#22, `0996b52`) renamed the entry binary
  # `tui` → `rushi-tui` and now ships only `$out/bin/rushi-tui`
  # (issue #31). Clean cut-over: no `bin/tui` alias, no old-name
  # fallback — a configured package carries only `bin/rushi-tui`.
  tuiInstallScript = if tuiPkg != null then
    let tuiPath = toShellPath tuiPkg; in
    ''
      # ── TUI binary (rushi.tui) ──
      TUI_SRC="${tuiPath}"
      if [ -f "$TUI_SRC/bin/rushi-tui" ]; then
        cp "$TUI_SRC/bin/rushi-tui" "$out/bin/rushi-tui"
        chmod +x "$out/bin/rushi-tui"
      else
        echo "WARNING: rushi.tui (${tuiPath}) has no bin/rushi-tui; TUI unavailable." >&2
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
      # ── Eval-time + build-time inputs (Nix-interpolated) ──
      # NEED_*: 1 only when a source lacks a meta.rushi field and the
      # consumer did not set the value themselves (issue #13).
      # EVAL_*: the eval-time derived lists. The shipped config.toml is
      # written once from these, and discovery only appends names for
      # sources without meta.
      # META_*: producer-declared dirs/bins to verify after the copy.
      # GUARDED_HOOK_COMMANDS: bare commands the build-time hook drift
      # guard still checks. meta.rushi.bin-covered ones are exempt
      # (issue #13).
      NEED_TOOL_DISCOVERY="${if needToolDiscovery then "1" else "0"}"
      NEED_UI_DISCOVERY="${if needUiDiscovery then "1" else "0"}"
      NATIVE_TOOLS="${builtins.concatStringsSep " " nativeToolNames}"
      EVAL_EXT_TOOL_PATHS="${builtins.concatStringsSep " " finalExtToolPaths}"
      EVAL_EXT_UI_NAMES="${builtins.concatStringsSep " " finalExtUiNames}"
      META_TOOL_DIRS="${builtins.concatStringsSep " " metaToolEntries}"
      META_EXT_DIRS="${builtins.concatStringsSep " " metaExtUiNames}"
      META_HOOK_BINS="${builtins.concatStringsSep " " metaHookBins}"
      USER_UI_EXT_NAMES="${builtins.concatStringsSep " " extUiExtNames}"
      UI_EXT_BASE_NAMES="${builtins.concatStringsSep " " uiExtensions}"
      GUARDED_HOOK_COMMANDS="${builtins.concatStringsSep " " guardedHookCommands}"
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

      # ── meta.rushi declaration check (issue #13) ──
      # What a producer declared must exist in the assembled package;
      # a lying producer fails the build here instead of breaking at
      # runtime.
      for d in $META_TOOL_DIRS; do
        if [ ! -f "$out/tools/$d/tool.toml" ]; then
          echo "mkRushi: meta.rushi.entry '$d' declared but $out/tools/$d/tool.toml is missing (broken ext)" >&2
          exit 1
        fi
      done
      for d in $META_EXT_DIRS; do
        if [ ! -f "$out/ui_extensions/$d/ext.toml" ]; then
          echo "mkRushi: meta.rushi.ext '$d' declared but $out/ui_extensions/$d/ext.toml is missing (broken ext)" >&2
          exit 1
        fi
      done
      for b in $META_HOOK_BINS; do
        if [ ! -f "$out/hooks/$b" ] && [ ! -f "$out/bin/$b" ]; then
          echo "mkRushi: meta.rushi.bin '$b' declared but $b is missing from $out/hooks/ and $out/bin/ (broken ext)" >&2
          exit 1
        fi
      done

      # ── Generated config.toml: written once, from the eval-time
      # value (issue #13). The returned `config` and this file are
      # byte-identical unless the fallback below appends. ──
      cp "${configTomlFile}" $out/config.toml

      # ── Build-time discovery fallback for [paths] (issue #13) ──
      # Runs only when a tool source lacks meta.rushi.entry AND the
      # consumer did not set extension_tool_paths. Discover ext tool
      # dirs (subdirs of $out/tools/ holding a tool.toml, not native
      # kernel tools) and append them to the shipped config line.
      # Meta-covered dirs are already in the eval-time list; dedup.
      if [ "$NEED_TOOL_DISCOVERY" = "1" ]; then
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
        all_ext_paths="$EVAL_EXT_TOOL_PATHS$ext_tool_paths"
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
        sed -i "s|^extension_tool_paths = .*|extension_tool_paths = [ $ext_paths_toml ]|" $out/config.toml
      fi

      # ── [ui_extensions] enabled list for the manifest (issue #13) ──
      # Kernel-bundled names first, then the ext names: the consumer's
      # override (USER_UI_EXT_NAMES, drift-guarded) or the
      # meta.rushi.ext declarations (EVAL_EXT_UI_NAMES), plus
      # build-time discovery when a source lacks meta.
      ext_ui_names="$EVAL_EXT_UI_NAMES"
      if [ "$NEED_UI_DISCOVERY" = "1" ]; then
        for d in "$out"/ui_extensions/*/ext.toml; do
          if [ -f "$d" ]; then
            ext_ui_names="$ext_ui_names $(basename "$(dirname "$d")")"
          fi
        done
      fi
      if [ -n "$USER_UI_EXT_NAMES" ]; then
        for n in $USER_UI_EXT_NAMES; do
          if [ ! -d "$out/ui_extensions/$n" ]; then
            echo "mkRushi: ui_extension_names '$n' has no entry in $out/ui_extensions/ (drift)" >&2
            exit 1
          fi
        done
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

      # ── Hook command drift guard (issue #13) ──
      # Every bare command NOT statically known from a bundled hook
      # package's meta.rushi.bin must resolve to a file in $out/bin/
      # or $out/hooks/ (the kernel's runtime resolution for bare
      # names, per resolve_hook_command). Meta-covered commands were
      # already verified by the meta-declaration check above.
      missing_hooks=""
      for cmd in $GUARDED_HOOK_COMMANDS; do
        if [ ! -f "$out/bin/$cmd" ] && [ ! -f "$out/hooks/$cmd" ]; then
          missing_hooks="$missing_hooks $cmd"
        fi
      done
      if [ -n "$missing_hooks" ]; then
        echo "mkRushi: hook command(s) not found in $out/bin/ or $out/hooks/:$missing_hooks" >&2
        echo "  Bundle them via rushi.external_hooks (meta.rushi.bin recommended), or set a full path in [hooks.defs.<name>].command." >&2
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
      # Single MIT license, matching the repo's LICENSE file
      # (decision 2026-09-18, docs/itches.md).
      license = licenses.mit;
      # Keep in sync with the flake's supportedSystems: aarch64-darwin
      # is a first-class rushi platform (decision 2026-09-17, see
      # docs/itches.md and flake.nix). Nixpkgs 26.11 dropped
      # x86_64-darwin, so the supported set is exactly these three.
      platforms = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" ];
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
  # This is the deep-merged Nix attrset (defaults + user overlays +
  # meta.rushi fill), identical to what ships in config.toml when no
  # build-time fallback is in play.
  configAttrs = mergedConfig;
  # issue #13: final eval-time values exposed for `nix eval`.
  extensionToolPaths =
    finalExtToolPaths;
  uiExtensionNames =
    finalUiNames;
}
