# lib/ext-contract.nix — the rushiExt ext-flake contract.
#
# The typed placement bridge between ext flakes and the rushi
# builders. Two helpers:
#
#   rushiExtBuckets { packages; drop ? [ "default" ]; }
#     Scan an attrset of ext packages and bucket them by their
#     meta.rushi declaration (issue #13): entry ⇒ tools,
#     ext ⇒ ui, bin ⇒ hooks. Packages without a meta.rushi
#     declaration (e.g. a bare-binary package the ext repo ships
#     alongside) are ignored. Alias keys are dropped. The ext
#     flake exposes the result per system as `rushiExt` (see
#     docs/reference/nix/ext-flake-authoring.md):
#
#       rushiExt = pkgLib.genAttrs supportedSystems (system:
#         rushiFlake.lib.rushiExtBuckets { packages = buildFor system; });
#
#   unionRushiExt { system; extFlakes; }
#     The consumer-side inverse. Fold a list of ext flake inputs
#     into a bucket table by reading each flake's
#     `rushiExt.<system>` and concatenating. The result maps
#     directly onto `rushi.external_tools` / `external_ui_extensions`
#     / `external_hooks`, or onto a deployment builder's `exts`
#     parameter. The consumer declares exts in their own flake
#     inputs and passes the flake inputs straight — no
#     packages.<system>.<name> selection, no bucket routing.

rec {
  # Bucket an attrset of ext packages by meta.rushi (issue #13).
  # `drop` removes alias keys (a `default` attrset duplicates the
  # package it aliases; the flake passes its concrete package
  # attrset, which still carries the alias key).
  rushiExtBuckets =
    {
      packages,
      drop ? [ "default" ],
    }:
    let
      concrete = builtins.removeAttrs packages drop;
      # Guarded access: `m ? rushi` short-circuits without
      # evaluating the attribute, so packages with no meta (or
      # no meta.rushi) are ignored, not thrown on.
      bucketOf =
        p:
        let
          m = p.meta or { };
        in
        if m ? rushi
        then
        let
          r = m.rushi;
        in
        if r ? entry then "tools"
        else if r ? ext then "ui"
        else if r ? bin then "hooks"
        else null
        else null;
      # List flattening without nixpkgs lib (flake `lib` value
      # context): fold `++` over the per-name single-or-empty
      # lists.
      pick =
        b:
        builtins.foldl' (a: l: a ++ l) [ ] (
          builtins.map (n:
            if bucketOf concrete.${n} == b
            then [ concrete.${n} ]
            else [ ]
          ) (builtins.attrNames concrete)
        );
    in
    {
      tools = pick "tools";
      ui = pick "ui";
      hooks = pick "hooks";
    };

  # Fold a list of ext flake inputs into a bucket table. Each
  # flake must expose `rushiExt.<system> = { tools; ui; hooks; }`
  # (the contract above).
  unionRushiExt =
    {
      system,
      extFlakes,
    }:
    let
      bucketsOf =
        f:
        if f ? rushiExt
        then ((f.rushiExt or {}).${system} or (
               throw "rushiExt: flake has no ${system} entry"
             ))
        else throw "rushiExt: an ext flake in the list does not expose the rushiExt attribute (see docs/reference/nix/ext-flake-authoring.md)";
      fold =
        acc:
        f:
        let
          b = bucketsOf f;
        in
        {
          extTools = acc.extTools ++ (b.tools or [ ]);
          extUi = acc.extUi ++ (b.ui or [ ]);
          extHooks = acc.extHooks ++ (b.hooks or [ ]);
        };
    in
    builtins.foldl' fold {
      extTools = [ ];
      extUi = [ ];
      extHooks = [ ];
    } extFlakes;
}
