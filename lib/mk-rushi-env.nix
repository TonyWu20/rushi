# lib/mk-rushi-env.nix — environment variables (mkRushi step 6).
#
# Generate `export KEY=…` lines from the three supported forms:
#   "literal"        → export KEY="literal"
#   { value = "…" } → export KEY="…"
#   { file = drv }  → export KEY="$(cat <storepath>)"
#
# The file form is the sops-nix integration point: the derivation's
# store path is referenced but not expanded at build time, so
# secret material never lands in the Nix store as plain text.
#
# `environment` is `rushi.environment` (empty when unset). The
# exported lines ship in $out/rushi.env and the configured package's
# bin/rushi is wrapped to source them (lib/mk-rushi-package.nix).
#
# Plain function (not a module): the flake `lib` output must be a
# value. Used by lib/mk-rushi.nix.

{
  lib,
  pkgs,
  environment ? { },
}:

let
  envExportLines = lib.mapAttrsToList (key: val:
    if builtins.isString val then
      "export ${key}=\"${val}\""
    else if val ? value then
      "export ${key}=\"${val.value}\""
    else if val ? file then
      "export ${key}=\"\$(cat ${toString val.file})\""
    else
      ""
  ) environment;
  hasEnv    = envExportLines != [ ];
  envFile   = pkgs.writeText "rushi-env.sh"
    (if hasEnv then (builtins.concatStringsSep "\n" (lib.filter (l: l != "") envExportLines)) + "\n" else "");
  # Derivations referenced via `file =` must be build inputs so their
  # store paths are materialized before the wrapper runs.
  envFileDeps = lib.concatLists (
    lib.mapAttrsToList (_: v: if v ? file then [ v.file ] else [ ]) environment
  );
  envFileNixDeps = builtins.filter (x:
    (builtins.isAttrs x)
    || (builtins.isString x && (builtins.match ''/nix/store/.*'' x) != null)
  ) envFileDeps;
in
{
  inherit
    envExportLines
    hasEnv
    envFile
    envFileDeps
    envFileNixDeps
    ;
}
