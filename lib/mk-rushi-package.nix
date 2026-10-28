# lib/mk-rushi-package.nix — configured package assembly
# (mkRushi step 5).
#
# Assembles the rushi-configured stdenv derivation ($out layout):
#
#   bin/             rushi + all tool/hook/stage binaries
#   tools/           tool manifests (tool.toml per enabled tool)
#   ui_extensions/   UI extension files
#   hooks/           hook binaries (if any)
#   config.toml      generated from the deep-merged options
#   tools.manifest   rushi.toml-equivalent for `rushi setup --locked`
#
# The ext copy scripts and the build-time guards below are generated
# from the eval-time inputs. See lib/mk-rushi-meta.nix (producer
# declarations) and lib/mk-rushi-merge.nix (eval-time fill +
# discovery flags) for their provenance.
#
# Plain function (not a module): the flake `lib` output must be a
# value. Used by lib/mk-rushi.nix.

{
  lib,
  pkgs,
  version,
  # Kernel binary (provides all stage + tool binaries and tool
  # manifests). lib/mk-rushi-kernel.nix.
  rushi,
  # Generated config.toml text (byte-identical to the returned
  # `config`).
  configTomlText,
  # Kernel tool names (eval-time, from the kernel's own tools/).
  # lib/rushi-src-tools.nix.
  nativeToolNames,
  # Module-evaluated config fields. lib/mk-rushi-modules.nix.
  tools,
  uiExtensions,
  extTools,
  extUiExts,
  extUiExtNames,
  extHooks,
  tuiPkg,
  # Producer-declared names. lib/mk-rushi-meta.nix.
  metaToolEntries,
  metaExtUiNames,
  metaHookBins,
  # Eval-time fill + discovery flags. lib/mk-rushi-merge.nix.
  finalExtToolPaths,
  finalExtUiNames,
  needToolDiscovery,
  needUiDiscovery,
  guardedHookCommands,
  # Environment. lib/mk-rushi-env.nix.
  envFile,
  envFileNixDeps,
}:

let
  # All external Nix values that must be in the store before we build.
  allExternalDeps = extTools ++ extUiExts ++ extHooks;

  # Convert a Nix value to a shell-safe path string.
  # Nix derivations expand to their store path; string paths pass through.
  toShellPath = v: if builtins.isString v then v else toString v;

  # External tool sources. Each is copied into $out/tools/ and must
  # ship at least one tool manifest (a tool.toml at the source top
  # level or in a sub-dir). A source with none is a broken ext and
  # fails the build with a clear error. This is the declare-once
  # guarantee: the source is the single declaration of this ext tool.
  extToolScript = builtins.concatStringsSep "\n" (
    map (s:
      let sp = toShellPath s; in ''
        # External tool source: ${sp}
        if [ ! -d "${sp}" ]; then
          echo "mkRushi: external tool source ${sp} is missing or not a directory" >&2
          exit 1
        fi
        ext_toml=""
        for t in "${sp}"/tool.toml "${sp}"/*/tool.toml; do
          if [ -f "$t" ]; then ext_toml="$t"; break; fi
        done
        if [ -z "$ext_toml" ]; then
          echo "mkRushi: external tool source ${sp} ships no tool.toml (broken ext)" >&2
          exit 1
        fi
        cp -rL "${sp}/." "$out/tools/"
      ''
    ) extTools
  );

  # External UI extension sources. Each is copied into
  # $out/ui_extensions/. Each source must ship at least one entry dir
  # with an ext.toml. A source with none is a broken ext and fails
  # the build with a clear error. Entry dir names are auto-discovered
  # at build time (see installPhase) unless ui_extension_names
  # overrides them.
  extUiScript = builtins.concatStringsSep "\n" (
    map (s:
      let sp = toShellPath s; in ''
        # External UI extension source: ${sp}
        if [ ! -d "${sp}" ]; then
          echo "mkRushi: external UI ext source ${sp} is missing or not a directory" >&2
          exit 1
        fi
        ext_toml=""
        for t in "${sp}"/ext.toml "${sp}"/*/ext.toml; do
          if [ -f "$t" ]; then ext_toml="$t"; break; fi
        done
        if [ -z "$ext_toml" ]; then
          echo "mkRushi: external UI ext source ${sp} ships no ext.toml (broken ext)" >&2
          exit 1
        fi
        cp -rL "${sp}/." "$out/ui_extensions/"
      ''
    ) extUiExts
  );

  extHookScript = builtins.concatStringsSep "\n" (
    map (s:
      let sp = toShellPath s; in ''
        # External hook source: ${sp}
        # Order matters: a store-dir (buildRustPackage) is a directory
        # that is -x but must be recursed into, not cp'd as a file.
        if [ -d "${sp}/bin" ]; then
          cp -rL "${sp}/bin/." "$out/hooks/" 2>/dev/null || true
        elif [ -d "${sp}" ]; then
          cp -rL "${sp}/." "$out/hooks/" 2>/dev/null || true
        elif [ -e "${sp}" ]; then
          cp "${sp}" "$out/hooks/"
        fi
      ''
    ) extHooks
  );

  # tools.manifest is generated at build time in installPhase so the
  # [ui_extensions] enabled list can include ext entry names
  # auto-discovered from the bundled UI ext packages. The [tools]
  # enabled list and [rushi] version are static, interpolated from
  # Nix into the shell at build time.

  # Embed generated files as Nix text files (referenced via passAsFile).
  # (tools.manifest is generated at build time in installPhase, not
  # embedded, because its [ui_extensions] list depends on build-time
  # discovery of the bundled ext entry dirs.)
  configTomlFile  = pkgs.writeText "rushi-config.toml" configTomlText;

  # Only Nix values (derivations, paths) need to be build inputs;
  # plain string paths are used directly in the shell.
  nixDeps = builtins.filter (x:
    (builtins.isAttrs x)
    || (builtins.isString x && (builtins.match ''/nix/store/.*'' x) != null)
  ) allExternalDeps;

  # TUI binary (rushi.tui): add Nix derivation to build inputs.
  tuiNixDep = if tuiPkg != null && (
      (builtins.isAttrs tuiPkg)
      || (builtins.isString tuiPkg && (builtins.match ''/nix/store/.*'' tuiPkg) != null)
    ) then [ tuiPkg ] else [ ];

  # Shell script to copy the TUI binary into the package.
  #
  # The TUI flake (rushi-tui#22, `0996b52`) renamed the entry binary
  # `tui` → `rushi-tui` and now ships only `$out/bin/rushi-tui`
  # (issue #31). Clean cut-over: no `bin/tui` alias, no old-name
  # fallback — a configured package carries only `bin/rushi-tui`.
  tuiInstallScript = if tuiPkg != null then
    let tuiPath = toShellPath tuiPkg; in
    ''
      # ── TUI binary (rushi.tui) ──
      TUI_SRC="${tuiPath}"
      if [ -f "$TUI_SRC/bin/rushi-tui" ]; then
        cp "$TUI_SRC/bin/rushi-tui" "$out/bin/rushi-tui"
        chmod +x "$out/bin/rushi-tui"
      else
        echo "WARNING: rushi.tui (${tuiPath}) has no bin/rushi-tui; TUI unavailable." >&2
      fi
    ''
  else "";

in
pkgs.stdenv.mkDerivation {
  pname = "rushi-configured";
  inherit version;
  # No source unpacking: installPhase copies from pre-built packages
  # (rushi, exts, TUI), not from $src.
  dontUnpack = 1;

  # Kernel binary (provides all stage + tool binaries and tool manifests).
  buildInputs = [ rushi ];

  # External sources (derivations only; string paths are used as-is).
  nativeBuildInputs = nixDeps ++ envFileNixDeps ++ tuiNixDep;

  # Pass generated text files via file descriptors (avoids long
  # Nix store paths in the shell command).
  passAsFile = [ configTomlFile envFile ];

  installPhase = ''
    # ── Eval-time + build-time inputs (Nix-interpolated) ──
    # NEED_*: 1 only when a source lacks a meta.rushi field and the
    # consumer did not set the value themselves (issue #13).
    # EVAL_*: the eval-time derived lists. The shipped config.toml is
    # written once from these, and discovery only appends names for
    # sources without meta.
    # META_*: producer-declared dirs/bins to verify after the copy.
    # GUARDED_HOOK_COMMANDS: bare commands the build-time hook drift
    # guard still checks. meta.rushi.bin-covered ones are exempt
    # (issue #13).
    NEED_TOOL_DISCOVERY="${if needToolDiscovery then "1" else "0"}"
    NEED_UI_DISCOVERY="${if needUiDiscovery then "1" else "0"}"
    NATIVE_TOOLS="${builtins.concatStringsSep " " nativeToolNames}"
    EVAL_EXT_TOOL_PATHS="${builtins.concatStringsSep " " finalExtToolPaths}"
    EVAL_EXT_UI_NAMES="${builtins.concatStringsSep " " finalExtUiNames}"
    META_TOOL_DIRS="${builtins.concatStringsSep " " metaToolEntries}"
    META_EXT_DIRS="${builtins.concatStringsSep " " metaExtUiNames}"
    META_HOOK_BINS="${builtins.concatStringsSep " " metaHookBins}"
    USER_UI_EXT_NAMES="${builtins.concatStringsSep " " extUiExtNames}"
    UI_EXT_BASE_NAMES="${builtins.concatStringsSep " " uiExtensions}"
    GUARDED_HOOK_COMMANDS="${builtins.concatStringsSep " " guardedHookCommands}"
    TOOLS_RAW="${builtins.concatStringsSep " " tools}"

    # ── Directory layout ──
    mkdir -p $out/bin $out/tools $out/ui_extensions $out/hooks

    # ── All kernel binaries (rushi, tools, hooks, stage CLIs) ──
    # The kernel build's bin/ contains:
    #   rushi, read, write, edit, harness-bash, harness-hook-compact,
    #   claim, assemble, compact, model, parse, route, rushi-log, user,
    #   rewind-drt
    # Tool resolution at runtime uses the `command` field from
    # tool.toml (e.g. "harness-bash" for the bash tool), so all
    # binaries must be in $out/bin/.
    cp -rL ${rushi}/bin/. $out/bin/
    chmod -R +x $out/bin/

    # ── Tool manifests (tool.toml only, skip source/Cargo noise) ──
    if [ -d "${rushi}/tools" ]; then
      for toml in "${rushi}"/tools/*/tool.toml; do
        [ -f "$toml" ] || continue
        name=$(basename "$(dirname "$toml")")
        mkdir -p "$out/tools/$name"
        cp "$toml" "$out/tools/$name/tool.toml"
      done
    fi

    # ── External tool sources (add to tools/) ──
    ${extToolScript}

    # ── External UI extension sources (add to ui_extensions/) ──
    ${extUiScript}

    # ── External hook binaries (add to hooks/) ──
    ${extHookScript}

    # ── TUI binary (rushi.tui) ──
    ${tuiInstallScript}

    # ── meta.rushi declaration check (issue #13) ──
    # What a producer declared must exist in the assembled package;
    # a lying producer fails the build here instead of breaking at
    # runtime.
    for d in $META_TOOL_DIRS; do
      if [ ! -f "$out/tools/$d/tool.toml" ]; then
        echo "mkRushi: meta.rushi.entry '$d' declared but $out/tools/$d/tool.toml is missing (broken ext)" >&2
        exit 1
      fi
    done
    for d in $META_EXT_DIRS; do
      if [ ! -f "$out/ui_extensions/$d/ext.toml" ]; then
        echo "mkRushi: meta.rushi.ext '$d' declared but $out/ui_extensions/$d/ext.toml is missing (broken ext)" >&2
        exit 1
      fi
    done
    for b in $META_HOOK_BINS; do
      if [ ! -f "$out/hooks/$b" ] && [ ! -f "$out/bin/$b" ]; then
        echo "mkRushi: meta.rushi.bin '$b' declared but $b is missing from $out/hooks/ and $out/bin/ (broken ext)" >&2
        exit 1
      fi
    done

    # ── Generated config.toml: written once, from the eval-time
    # value (issue #13). The returned `config` and this file are
    # byte-identical unless the fallback below appends. ──
    cp "${configTomlFile}" $out/config.toml

    # ── Build-time discovery fallback for [paths] (issue #13) ──
    # Runs only when a tool source lacks meta.rushi.entry AND the
    # consumer did not set extension_tool_paths. Discover ext tool
    # dirs (subdirs of $out/tools/ holding a tool.toml, not native
    # kernel tools) and append them to the shipped config line.
    # Meta-covered dirs are already in the eval-time list; dedup.
    if [ "$NEED_TOOL_DISCOVERY" = "1" ]; then
      ext_tool_paths=""
      for d in "$out"/tools/*/tool.toml; do
        if [ -f "$d" ]; then
          name=$(basename "$(dirname "$d")")
          is_native=0
          for nt in $NATIVE_TOOLS; do
            if [ "$name" = "$nt" ]; then is_native=1; break; fi
          done
          if [ "$is_native" -eq 0 ]; then
            ext_tool_paths="$ext_tool_paths tools/$name"
          fi
        fi
      done
      all_ext_paths="$EVAL_EXT_TOOL_PATHS$ext_tool_paths"
      final_ext_paths=""
      for p in $all_ext_paths; do
        case " $final_ext_paths " in *" $p "*) continue ;; esac
        final_ext_paths="$final_ext_paths $p"
      done
      ext_paths_toml=""
      for p in $final_ext_paths; do
        if [ -z "$ext_paths_toml" ]; then
          ext_paths_toml=$(printf '"%s"' "$p")
        else
          ext_paths_toml="$ext_paths_toml, $(printf '"%s"' "$p")"
        fi
      done
      sed -i "s|^extension_tool_paths = .*|extension_tool_paths = [ $ext_paths_toml ]|" $out/config.toml
    fi

    # ── [ui_extensions] enabled list for the manifest (issue #13) ──
    # Kernel-bundled names first, then the ext names: the consumer's
    # override (USER_UI_EXT_NAMES, drift-guarded) or the
    # meta.rushi.ext declarations (EVAL_EXT_UI_NAMES), plus
    # build-time discovery when a source lacks meta.
    ext_ui_names="$EVAL_EXT_UI_NAMES"
    if [ "$NEED_UI_DISCOVERY" = "1" ]; then
      for d in "$out"/ui_extensions/*/ext.toml; do
        if [ -f "$d" ]; then
          ext_ui_names="$ext_ui_names $(basename "$(dirname "$d")")"
        fi
      done
    fi
    if [ -n "$USER_UI_EXT_NAMES" ]; then
      for n in $USER_UI_EXT_NAMES; do
        if [ ! -d "$out/ui_extensions/$n" ]; then
          echo "mkRushi: ui_extension_names '$n' has no entry in $out/ui_extensions/ (drift)" >&2
          exit 1
        fi
      done
    fi
    all_ui_names="$UI_EXT_BASE_NAMES $ext_ui_names"
    final_ui_names=""
    for n in $all_ui_names; do
      case " $final_ui_names " in *" $n "*) continue ;; esac
      final_ui_names="$final_ui_names $n"
    done
    ui_ext_toml=""
    for n in $final_ui_names; do
      if [ -z "$ui_ext_toml" ]; then
        ui_ext_toml=$(printf '"%s"' "$n")
      else
        ui_ext_toml="$ui_ext_toml, $(printf '"%s"' "$n")"
      fi
    done

    # ── Hook command drift guard (issue #13) ──
    # Every bare command NOT statically known from a bundled hook
    # package's meta.rushi.bin must resolve to a file in $out/bin/
    # or $out/hooks/ (the kernel's runtime resolution for bare
    # names, per resolve_hook_command). Meta-covered commands were
    # already verified by the meta-declaration check above.
    missing_hooks=""
    for cmd in $GUARDED_HOOK_COMMANDS; do
      if [ ! -f "$out/bin/$cmd" ] && [ ! -f "$out/hooks/$cmd" ]; then
        missing_hooks="$missing_hooks $cmd"
      fi
    done
    if [ -n "$missing_hooks" ]; then
      echo "mkRushi: hook command(s) not found in $out/bin/ or $out/hooks/:$missing_hooks" >&2
      echo "  Bundle them via rushi.external_hooks (meta.rushi.bin recommended), or set a full path in [hooks.defs.<name>].command." >&2
      exit 1
    fi

    # ── Manifest [tools] enabled list (rushi.tools value) ──
    tools_toml=""
    for t in $TOOLS_RAW; do
      if [ -z "$tools_toml" ]; then
        tools_toml=$(printf '"%s"' "$t")
      else
        tools_toml="$tools_toml, $(printf '"%s"' "$t")"
      fi
    done

    # ── tools.manifest (rushi.toml equivalent, built at build time) ──
    {
      printf '[rushi]\n'
      printf 'version = "%s"\n' "$version"
      printf '\n'
      printf '[tools]\n'
      printf 'enabled = [ %s ]\n' "$tools_toml"
      printf '\n'
      printf '[ui_extensions]\n'
      printf 'enabled = [ %s ]\n' "$ui_ext_toml"
    } > $out/tools.manifest

    # ── Environment variables (rushi.environment) ──
    # If env vars were declared, wrap bin/rushi so they are
    # exported before the real binary runs. The export lines live
    # in $out/rushi.env (also sourceable manually).
    if [ -s "${envFile}" ]; then
      cp "${envFile}" $out/rushi.env
      mv $out/bin/rushi $out/bin/.rushi-real
      printf '%s\n' \
        '#!/bin/sh' \
        '# Auto-generated by rushi.mkRushi — exports rushi.environment' \
        'BIN_DIR="$(cd "$(dirname "$0")" && pwd)"' \
        '. "$BIN_DIR/../rushi.env"' \
        'exec "$BIN_DIR/.rushi-real" "$@"' \
        > $out/bin/rushi
      chmod +x $out/bin/rushi
    fi

    # ── Sanity check ──
    test -x $out/bin/rushi || { echo "ERROR: rushi binary missing"; exit 1; }
  '';

  meta = with lib; {
    description = "Configured rushi agent harness (v${version})";
    # Single MIT license, matching the repo's LICENSE file
    # (decision 2026-09-18, docs/itches.md).
    license = licenses.mit;
    # Keep in sync with the flake's supportedSystems: aarch64-darwin
    # is a first-class rushi platform (decision 2026-09-17, see
    # docs/itches.md and flake.nix). Nixpkgs 26.11 dropped
    # x86_64-darwin, so the supported set is exactly these three.
    platforms = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" ];
    mainProgram = "rushi";
  };
}
