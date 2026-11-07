# lib/mk-rushi-merge.nix — config deep-merge + eval-time fill
# (mkRushi steps 2 + 2.5).
#
# NixOS recursively merges `rushi.config` values across modules
# (different top-level keys merge. Same key: later module wins).
# The option's `default` ({}) is NOT deep-merged with module values,
# so we manually overlay the kernel defaults underneath:
#
#   kernelDefaults ← userMerged (user wins on conflict)
#
# This is the overlay semantics a user expects from a config system.
#
# A consumer-set value is authoritative as-is: it is used verbatim
# (no merge, no discovery, no dedup). Only when the consumer left
# extension_tool_paths / ui_extension_names unset does mkRushi fill
# them from the producers' meta.rushi declarations. An explicit
# `= [ ]` counts as set: the empty list stays empty, and no
# fallback discovery runs. The native_tool_paths fill follows the
# enabled native tools (the evaluated `rushi.tools` value): one
# relative `tools/<name>` entry per enabled tool. The static
# kernel-default catalog serves non-Nix users only. Filled values
# land in the returned `config` text and `configAttrs`, and the
# shipped config.toml is written once from them, byte-identical to
# the returned config, unless the build-time fallback appends names.
#
# Plain function (not a module): the flake `lib` output must be a
# value. Used by lib/mk-rushi.nix.

{
  lib,
  # Kernel defaults attrset (lib/rushi-defaults.nix), `config` key.
  defaults,
  # `rushi.config` merged across consumer modules (empty when unset).
  userConfig,
  # The module-evaluated `rushi.tools` value (the enabled native
  # tool list; the option default is the kernel's real tool set).
  tools,
  # Module-evaluated values (lib/mk-rushi-modules.nix).
  extUiExtNames,
  uiExtensions,
  # Producer-declared names (lib/mk-rushi-meta.nix).
  metaToolEntries,
  metaExtUiNames,
  metaHookBins,
  toolFallbackSources,
  uiFallbackSources,
}:

let
  # ── 2. Deep-merge rushi.config ──
  baseConfig =
    lib.recursiveUpdate defaults.config userConfig;

  # ── 2.5. Eval-time fill from `meta.rushi` (issue #13) ──
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

  # The [paths].native_tool_paths fill. A consumer-set value is
  # authoritative as-is (the prebaked absolute store paths, a
  # relative subset, or an explicit `= [ ]`). Unset, the value
  # follows the enabled native tools: one relative `tools/<name>`
  # entry per `rushi.tools` entry. The static kernel-default
  # catalog (rushi-defaults.nix) serves non-Nix users only; the
  # Nix builder never ships a path list wider than the enabled set.
  userNativeToolPathsSet =
    (userConfig ? paths) && (userConfig.paths ? native_tool_paths);
  finalNativeToolPaths =
    if userNativeToolPathsSet
    then userConfig.paths.native_tool_paths
    else
      lib.map (n: "tools/${n}") tools;

  finalExtUiNames =
    if extUiExtNames != [ ]
    then extUiExtNames
    else
      lib.unique metaExtUiNames;

  # The manifest [ui_extensions] enabled list: kernel-bundled names
  # (the `rushi.ui_extensions` option) first, then the ext names.
  finalUiNames =
    lib.unique (uiExtensions ++ finalExtUiNames);

  # Each fill applies only when the consumer left that value
  # unset. An explicit `= [ ]` counts as set.
  extPathsOverride =
    lib.optionalAttrs (!userExtToolPathsSet) {
      extension_tool_paths = finalExtToolPaths;
    }
    // lib.optionalAttrs (!userNativeToolPathsSet) {
      native_tool_paths = finalNativeToolPaths;
    };
  filledPaths =
    baseConfig.paths // extPathsOverride;
  mergedConfig =
    lib.recursiveUpdate baseConfig (lib.listToAttrs [
      (lib.nameValuePair "paths" filledPaths)
    ]);

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
in
{
  inherit
    mergedConfig
    finalExtToolPaths
    finalNativeToolPaths
    finalExtUiNames
    finalUiNames
    needToolDiscovery
    needUiDiscovery
    guardedHookCommands
    ;
}
