# lib/nixos-module.nix — NixOS module for `programs.rushi`.
#
# Mirrors pi-flake's `nixosModules.coding-agent`. The option path is
# `programs.rushi`. When enabled, the configured rushi package is
# added to `environment.systemPackages`.
#
# Usage in a NixOS configuration:
#
#   { ... }:
#   {
#     imports = [ ./rushi.nix ];   # the kernel flake's nixosModule
#
#     programs.rushi = {
#       enable = true;
#       package = rushiConfigured.package;   # from lib.mkRushi { … }
#     };
#   }
#
# Exposed via the kernel flake's `nixosModules.rushi` output.

{ config, lib, ... }:

let
  cfg = config.programs.rushi;
in
{
  options.programs.rushi = {
    enable = lib.mkEnableOption "rushi agent harness";

    package = lib.mkOption {
      type = lib.types.raw;   # a derivation (the configured rushi package)
      default = null;
      description = ''
        The configured rushi package (the `.package` field returned by
        the kernel flake's `lib.mkRushi { … }`). Added to
        `environment.systemPackages` when `programs.rushi.enable` is
        set.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages =
      if cfg.package != null then [ cfg.package ] else [ ];
  };
}
