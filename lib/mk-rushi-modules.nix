# lib/mk-rushi-modules.nix — NixOS module evaluation (mkRushi step 1).
#
# Evaluates the NixOS module system: the rushi option schema
# (lib/rushi-options.nix) plus the consumer's `modules`, via
# lib.evalModules. evalModules type-checks all option values and
# exposes the option schema for docs generation.
#
# `kernelTools` is threaded in so the `tools` option default is the
# kernel's real tool set, not a hand-maintained list. `extraSpecialArgs`
# (like pi-flake) lets the caller pass more special args into every
# module's scope (e.g. sops-nix secrets, environment-specific
# overrides).
#
# Plain function (not a module): the flake `lib` output must be a
# value. Used by lib/mk-rushi.nix.

{
  lib,
  pkgs,
  modules ? [ ],
  extraSpecialArgs ? { },
  kernelTools,
}:

let
  optionsModule =
    import ./rushi-options.nix { kernelTools = kernelTools; inherit lib; };

  evaluated = lib.evalModules {
    specialArgs = {
      inherit pkgs lib;
    } // extraSpecialArgs;
    modules = [ optionsModule ] ++ modules;
  };
in
{
  inherit evaluated;
  # Simple (non-nested-attrs) options: NixOS merge handles these
  # correctly (str, listOf str, listOf raw — last module wins).
  version       = evaluated.config.rushi.version;
  tools         = evaluated.config.rushi.tools;
  uiExtensions  = evaluated.config.rushi.ui_extensions;
  extTools      = evaluated.config.rushi.external_tools;
  extUiExts     = evaluated.config.rushi.external_ui_extensions;
  extUiExtNames = evaluated.config.rushi.ui_extension_names;
  extHooks      = evaluated.config.rushi.external_hooks;
  tuiPkg        = evaluated.config.rushi.tui or null;
}
