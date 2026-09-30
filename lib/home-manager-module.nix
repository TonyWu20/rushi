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
# `enableTelevisionIntegration` writes the `rushi-sessions` channel into
# `programs.television.channels` (the home-manager television module
# serializes it to `~/.config/television/cable/rushi-sessions.toml`).
# The channel's source/preview commands run the `rushi-sessions` binary
# (kernel bin/rushi-sessions), so enabling the integration also adds
# the rushi `package` to `home.packages` when it is set. The channel
# needs the home-manager television module loaded.
#
# Exposed via the kernel flake's `homeManagerModules.rushi` output.

{ config, lib, ... }:

let
  cfg = config.programs.rushi;
  # The rushi-sessions channel, as a TOML-serializable value. This is the
  # Nix source of truth for the channel. The home-manager television
  # module renders it to cable/rushi-sessions.toml.
  defaultChannel = import ./rushi-sessions-channel.nix;
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

    enableTelevisionIntegration = lib.mkEnableOption ''
      rushi-sessions television channel. When enabled, writes
      `programs.television.channels.rushi-sessions`. The channel peeks at
      rushi sessions: status, loop phase, last activity, last messages.
      Its commands run the `rushi-sessions` binary, which ships in the
      rushi `package`; when this option is set, that package is added
      to `home.packages` (alongside `enable`). It needs the
      home-manager `programs.television` module loaded.
    '';

    televisionChannel = lib.mkOption {
      type = lib.types.raw;
      default = null;
      description = ''
        The value written to `programs.television.channels.rushi-sessions`
        when `enableTelevisionIntegration` is set. Defaults to the
        built-in `rushi-sessions` channel. Set a custom channel attrset to
        override it.
      '';
    };
  };

  config = lib.mkMerge [
    (lib.mkIf (cfg.enable || cfg.enableTelevisionIntegration) {
      home.packages =
        if cfg.package != null then [ cfg.package ] else [ ];
    })
    (lib.mkIf cfg.enableTelevisionIntegration {
      programs.television.channels.rushi-sessions =
        if cfg.package == null
        then builtins.throw "programs.rushi.enableTelevisionIntegration needs `package` set: the channel commands run the `rushi-sessions` binary, which ships in the rushi package"
        else if cfg.televisionChannel != null
        then cfg.televisionChannel
        else defaultChannel;
    })
  ];
}
