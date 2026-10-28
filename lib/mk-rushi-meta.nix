# lib/mk-rushi-meta.nix — producer-declared names (mkRushi step 1.5).
#
# Each external package may carry the standard `meta` attribute
# declaring what it provides, like `meta.mainProgram` does. A
# package sets exactly one field of `meta.rushi`:
#
#   entry  tool package: the entry dir copied into <pkg>/tools/
#   ext    UI-ext package: the entry dir copied into <pkg>/ui_extensions/
#   bin    hook package: the binary name copied into <pkg>/hooks/
#
# mkRushi reads these at eval time and fills
# config.paths.extension_tool_paths plus the manifest [ui_extensions]
# enabled list when the consumer has not set them. A non-empty
# consumer value stays authoritative. Sources without a usable
# meta.rushi field (plain paths cannot carry meta) keep the #10
# build-time discovery as a fallback, with an eval-time warning.
#
# Plain function (not a module): the flake `lib` output must be a
# value. Used by lib/mk-rushi.nix.

{
  lib,
  extTools,
  extUiExts,
  extHooks,
}:

let
  # `meta.rushi` for a source. `{ }` when absent, which is the case
  # for plain path strings. A present-but-malformed `meta.rushi` is an
  # error, not a silent no-op.
  rushiMeta = s:
    if builtins.isAttrs s && s ? meta && s.meta ? rushi then
      let m = s.meta.rushi; in
      if builtins.isAttrs m then m
      else
        throw "rushi.mkRushi: meta.rushi on ${toString s} must be an attrset with one of entry / ext / bin, got ${builtins.typeOf m}"
    else { };

  # `meta.rushi.entry` per external_tools source: the tool entry dir
  # name that the source ships under its $out (it is copied into the
  # configured package at <pkg>/tools/<entry>). Sources without the
  # field fall back to build-time discovery. The warning fires at eval
  # time when this value is forced, i.e. when the fallback kicks in.
  metaToolEntries =
    lib.foldl
      (acc: s:
        let m = rushiMeta s; in
        if m ? entry then
          acc ++ [ m.entry ]
        else
          lib.warn "rushi.mkRushi: external_tools source ${toString s} has no meta.rushi.entry — falling back to build-time discovery. Declare meta.rushi.entry in the producer flake (issue #13)" acc)
      [ ]
      extTools;

  # `meta.rushi.ext` per external_ui_extensions source: the UI-ext
  # entry dir that the source ships under $out.
  metaExtUiNames =
    lib.foldl
      (acc: s:
        let m = rushiMeta s; in
        if m ? ext then
          acc ++ [ m.ext ]
        else
          lib.warn "rushi.mkRushi: external_ui_extensions source ${toString s} has no meta.rushi.ext — falling back to build-time discovery. Declare meta.rushi.ext in the producer flake (issue #13)" acc)
      [ ]
      extUiExts;

  # `meta.rushi.bin` per external_hooks source: the hook binary name
  # shipped under $out/hooks/. A bare hook command matching one of
  # these is statically known-bundled, so the build-time guard only
  # covers the rest (plain-path sources, kernel bins, typos).
  metaHookBins =
    lib.map (s: (rushiMeta s).bin) (lib.filter (s: rushiMeta s ? bin) extHooks);

  # The sources that keep the #10 build-time discovery fallback.
  toolFallbackSources =
    lib.filter (s: !(rushiMeta s ? entry)) extTools;
  uiFallbackSources =
    lib.filter (s: !(rushiMeta s ? ext)) extUiExts;
in
{
  inherit
    metaToolEntries
    metaExtUiNames
    metaHookBins
    toolFallbackSources
    uiFallbackSources
    ;
}
