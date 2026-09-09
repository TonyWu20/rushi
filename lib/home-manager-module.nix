# lib/home-manager-module.nix — home-manager module for `programs.rushi`.
#
# Mirrors pi-flake's `homeModules.coding-agent`. When enabled and
# `package` is set, adds the configured rushi package to `home.packages`.
#
# Usage in a home-manager configuration:
#
#   { ... }:
#   {
#     imports = [ ./rushi.nix ];   # the kernel flake's homeManagerModule
#
#     programs.rushi = {
#       enable = true;
#       package = rushiConfigured.package;   # from lib.mkRushi { … }
#     };
#   }
#
# Exposed via the kernel flake's `homeManagerConfig.rushi` output.

{ config, lib, ... }:

let
  cfg = config.programs.rushi;
in
{
  options.programs.rushi = {
    enable = lib.mkEnableOption "rushi agent harness";

    package = lib.mkOption {
      type = lib.types.raw;
      default = null;
      description = ''
        The configured rushi package (the `.package` field returned by
        the kernel flake's `lib.mkRushi { … }`). Added to `home.packages`
        when `programs.rushi.enable` is set.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages =
      if cfg.package != null then [ cfg.package ] else [ ];
  };
}
