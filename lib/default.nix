# The rushi flake `lib` output: Nix-side helpers for declarative rushi
# configuration (the pi-flake `mkCodingAgent` equivalent). flake.nix
# re-exports this file as `lib = import ./lib;`, so consumers call
# `flake.lib.mkRushi { pkgs = …; modules = [ … ]; }`.
#
# Plain attribute set — NOT a module lambda: a flake `lib` output must
# be a value, and `flake.lib` attribute access is the documented API
# (examples/rushi-config, tests/nix, docs/reference/nix/nix-flake-module.md).
{

  # Build a fully configured rushi package from Nix modules.
  #
  # Parameters:
  #   pkgs              — nixpkgs for the target system (fenix overlay for Rust)
  #   modules           — list of NixOS-style module functions (lib.evalModules)
  #   rustToolchain     — optional pre-resolved toolchain (auto-resolved if null)
  #   extraSpecialArgs  — extra args injected into every module's scope
  #
  # Returns: { package, config, version, options, configAttrs,
  #   extensionToolPaths, uiExtensionNames }
  #   config      = generated config.toml text
  #   options     = full lib.mkOption schema (for nixosOptionsDoc)
  #   configAttrs = deep-merged Nix attrset (pre-TOML)
  #   extensionToolPaths = final [paths] extension_tool_paths list
  #   uiExtensionNames   = final [ui_extensions] enabled list
  #   (the two lists are filled from meta.rushi at eval time when
  #   the consumer leaves them unset; issue #13)
  mkRushi =
    {
      pkgs,
      modules ? [ ],
      rustToolchain ? null,
      extraSpecialArgs ? { },
      ...
    }:
    (import ./mk-rushi.nix) {
      inherit
        pkgs
        modules
        rustToolchain
        extraSpecialArgs
        ;
      # The flake source as Nix sees it. Kernel consumers pin with
      # git-scheme inputs (github: / git:), not path inputs, so the
      # source is always the git-tracked tree and no source cleaning
      # is needed. (The pinned nixpkgs has no lib.gitCleanSource,
      # only lib.cleanSource.)
      src = ../.;
      # Pass the kernel's Cargo.lock as a Nix path reference. `../.` and
      # `../Cargo.lock` resolve relative to this file's directory
      # (lib/), i.e. the flake root, so it works whether the kernel is
      # the root flake or a flake path input. The contents form
      # (lockFileContents) does not resolve reliably in the path-input
      # context; a Nix path reference does.
      kernelCargoLock = {
        lockFile = ../Cargo.lock;
      };
    };

  # Kernel defaults (for inspection or custom overlay construction).
  defaults = import ./rushi-defaults.nix;

  # Extension / tool source helpers (pi-flake extNoDeps / extWithDeps
  # equivalents). Each takes { pkgs, … } and returns a derivation
  # suitable for rushi.external_tools / external_ui_extensions.
  fetchExt = args: (import ./fetch-ext.nix).fetchExt args;

  fetchTool = args: (import ./fetch-ext.nix).fetchTool args;

  resolvePlatformHash = args: (import ./fetch-ext.nix).resolvePlatformHash args;
}
