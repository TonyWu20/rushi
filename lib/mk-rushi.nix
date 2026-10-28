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
#   composable modules, option types, `lib.mkIf` / `lib.mkMerge` /
#   `lib.mkOverride` / `lib.mkForce`, `config` access, `specialArgs`,
#   and option introspection (the result's `.options` field, usable
#   with `nixosOptionsDoc`). See lib/mk-rushi-modules.nix.
#
# ── Config deep-merge semantics ────────────────────────────────────
#   `rushi.config` is a free-form `attrs` option (mirrors the
#   `config.toml` section layout). NixOS recursively merges
#   `rushi.config` values across modules, but does NOT deep-merge
#   the option's `default` with module values. `mkRushi` overlays the
#   kernel defaults (lib/rushi-defaults.nix) underneath the NixOS-
#   merged result via `lib.recursiveUpdate`:
#
#       merged = recursiveUpdate(kernelDefaults, userMerged)
#
#   This gives users the expected overlay behaviour: set one field,
#   the rest stay at kernel defaults. See lib/mk-rushi-merge.nix.
#
# ── Pipeline (purpose-grouped helper files) ────────────────────────
#   step  0  lib/rushi-src-tools.nix   kernel tool names from src/tools
#   step  1  lib/mk-rushi-modules.nix  NixOS module evaluation
#   step 1.5 lib/mk-rushi-meta.nix     producer-declared names (meta.rushi)
#   step 2   lib/mk-rushi-merge.nix    config deep-merge + eval-time fill
#   step 3   lib/mk-rushi-kernel.nix   kernel binary (buildRustPackage)
#   step 4   (here)                    config.toml text from merged config
#   step 6   lib/mk-rushi-env.nix      rushi.environment export lines
#
# Wiring order note: step 6 computes before step 5, because the
# package derivation consumes the env file and its Nix inputs.
#   step 5   lib/mk-rushi-package.nix  configured package derivation
#
# The helpers are plain Nix functions (a flake `lib` output must be
# a value, not a NixOS module). This file wires them in pipeline
# order and returns the documented result attrset.
#
# Mirrors the pi-flake.lib.mkCodingAgent pattern: the kernel flake
# provides the builder; the consumer flake provides configuration
# modules. See docs/reference/nix/nix-flake-module.md for the full
# option schema.

{
  pkgs,
  modules ? [ ],
  src,
  rustToolchain ? null,
  extraSpecialArgs ? { },
  cargoLockContents ? null,
  kernelCargoLock ? null,
  ...
}:

let
  lib = pkgs.lib;

  # ── TOML serializer ──
  toTomlDocument = import ./to-toml.nix;

  # ── 0. Kernel tool names (eval-time, from the kernel's own tools/) ──
  srcTools = (import ./rushi-src-tools.nix) { inherit src; };

  # ── 1. Evaluate the NixOS module system ──
  moduleEval = (import ./mk-rushi-modules.nix) {
    inherit lib pkgs modules extraSpecialArgs;
    kernelTools = srcTools.nativeToolNames;
  };

  # ── 1.5. Producer-declared names: `meta.rushi` (issue #13) ──
  producerNames = (import ./mk-rushi-meta.nix) {
    inherit lib;
    extTools = moduleEval.extTools;
    extUiExts = moduleEval.extUiExts;
    extHooks = moduleEval.extHooks;
  };

  # ── 2. Deep-merge rushi.config (kernel defaults under user values) ──
  userConfig = moduleEval.evaluated.config.rushi.config or { };
  configMerge = (import ./mk-rushi-merge.nix) {
    inherit lib userConfig;
    defaults = import ./rushi-defaults.nix;
    extUiExtNames = moduleEval.extUiExtNames;
    uiExtensions = moduleEval.uiExtensions;
    inherit
      (producerNames)
      metaToolEntries
      metaExtUiNames
      metaHookBins
      toolFallbackSources
      uiFallbackSources
      ;
  };

  # ── 3. Build the rushi kernel binary ──
  kernel = (import ./mk-rushi-kernel.nix) {
    inherit pkgs src rustToolchain cargoLockContents kernelCargoLock;
    version = moduleEval.version;
  };

  # ── 4. Generate config.toml from the deep-merged options ──
  # Byte-identical to the returned `config`, unless the build-time
  # fallback appends ext names (lib/mk-rushi-package.nix).
  configTomlText = toTomlDocument configMerge.mergedConfig;

  # ── 6. Environment variables (rushi.environment) ──
  env = (import ./mk-rushi-env.nix) {
    inherit lib pkgs;
    environment = moduleEval.evaluated.config.rushi.environment or { };
  };

  # ── 5. Assemble the configured package ──
  package = (import ./mk-rushi-package.nix) {
    inherit lib pkgs configTomlText;
    version = moduleEval.version;
    rushi = kernel.rushi;
    nativeToolNames = srcTools.nativeToolNames;
    tools = moduleEval.tools;
    uiExtensions = moduleEval.uiExtensions;
    extTools = moduleEval.extTools;
    extUiExts = moduleEval.extUiExts;
    extUiExtNames = moduleEval.extUiExtNames;
    extHooks = moduleEval.extHooks;
    tuiPkg = moduleEval.tuiPkg;
    inherit
      (producerNames)
      metaToolEntries
      metaExtUiNames
      metaHookBins
      ;
    inherit
      (configMerge)
      finalExtToolPaths
      finalExtUiNames
      needToolDiscovery
      needUiDiscovery
      guardedHookCommands
      ;
    inherit (env) envFile envFileNixDeps;
  };
in
{
  inherit package;
  config = configTomlText;
  version = moduleEval.version;
  # Expose the full option schema for documentation / introspection
  # (analogous to pi-flake's `options` output, usable with nixosOptionsDoc).
  inherit (moduleEval.evaluated) options;
  # Expose the raw evaluated config (pre-TOML) for programmatic access.
  # This is the deep-merged Nix attrset (defaults + user overlays +
  # meta.rushi fill), identical to what ships in config.toml when no
  # build-time fallback is in play.
  configAttrs = configMerge.mergedConfig;
  # issue #13: final eval-time values exposed for `nix eval`.
  extensionToolPaths =
    configMerge.finalExtToolPaths;
  uiExtensionNames =
    configMerge.finalUiNames;
}
