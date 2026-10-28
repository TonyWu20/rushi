# lib/rushi-src-tools.nix — kernel source inspection (mkRushi step 0).
#
# Reads the kernel tool names from the kernel's own tools/ tree at
# eval time. A dir counts as a kernel tool when it ships a tool.toml.
#
# Two consumers:
#   * `rushi.tools` option default (full kernel tool set, so the
#     consumer can omit it entirely)
#   * build-time ext-tool discovery filter (subdirs of $out/tools
#     that are NOT in this set are extension tool dirs)
#
# Plain function (not a module): the flake `lib` output must be a
# value. Used by lib/mk-rushi.nix.

{ src }:

let
  toolsDir = src + "/tools";
  dirNames = builtins.attrNames (builtins.readDir toolsDir);
in
{
  nativeToolNames =
    builtins.filter (n:
      builtins.pathExists (toolsDir + "/${n}/tool.toml")
    ) dirNames;
}
