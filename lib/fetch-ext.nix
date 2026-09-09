# lib/fetch-ext.nix — extension / tool source helpers.
#
# Mirrors pi-flake's `extNoDeps` / `extWithDeps` / `resolvePlatformHash`
# (see pi-config's flake.nix). The file is a single *recursive* attrset
# of three helpers; each takes `pkgs` as its first argument so the
# flake's system-independent `lib` output can re-export them and the
# consumer supplies the target pkgs at call time:
#
#   lib.fetchExt   { pkgs; owner; repo; rev; hash; subpath ? null; extraBuild ? ""; }
#   lib.fetchTool  { pkgs; owner; repo; rev; hash; build ? "cargo"; subpath ? null; rustToolchain ? null; }
#   lib.resolvePlatformHash { pkgs; hash; }
#
# fetchExt  → derivation whose $out mirrors the repo layout (zero-build
#             exts: JS/TS exts run in the kernel's own runtime, so no
#             compile step — just fetch + copy).
#
# fetchTool → derivation whose $out carries a `<name>/tool.toml` +
#             binary layout, what mk-rushi's external_tools copy step
#             expects. build = "cargo" (workspace) | "cargo-single" |
#             "none" (prebuilt layout).
#
# resolvePlatformHash → hash may be a string (shared) or a
#             { x86_64-linux = "…"; aarch64-darwin = "…"; } attrset
#             (per-platform), like pi-flake's helper.

rec {
  # ── Hash resolver (pi-flake `resolvePlatformHash` equivalent) ──
  # hash: a sha256 string, or an attrset keyed by system (with an
  # optional `default`). Resolves the entry for the target pkgs.system.
  resolvePlatformHash =
    { pkgs, hash }:
    if builtins.isAttrs hash
    then (hash.${pkgs.system} or hash.default or (
           throw "rushi.resolvePlatformHash: no hash for system ${pkgs.system} in ${builtins.toJSON hash}"
         ))
    else hash;

  # ── Zero-dependency extension fetch (pi-flake `extNoDeps` equivalent) ──
  #
  #   ext = rushiFlake.lib.fetchExt {
  #     pkgs = …;
  #     owner = "rushi-exts"; repo = "statusline-rs";
  #     rev = "abc1234";
  #     hash = "sha256-…";            # or { x86_64-linux = "…"; …; }
  #     # subpath = "ui/statusline-rs";   # monorepo subdirectory
  #   };
  #
  # $out mirrors the repo layout so the consumer's package can copy
  # subdirs into ui_extensions/ or tools/.
  fetchExt =
    { pkgs
    , owner
    , repo
    , rev
    , hash
    , subpath ? null
    , extraBuild ? ""
    }:
    let
      resolvedHash = resolvePlatformHash { inherit pkgs hash; };
      src = pkgs.fetchFromGitHub {
        inherit owner repo rev;
        hash = resolvedHash;
        # Monorepo: pin a subdirectory instead of the whole repo.
        subpath = if subpath == null then null else subpath;
      };
    in
    pkgs.stdenv.mkDerivation {
      pname = "${repo}-${rev}";
      inherit src;
      # No build: ship the files as-is.
      buildPhase = ''
        mkdir -p $out
        cp -rL $src/. $out/
        ${extraBuild}
      '';
    };

  # ── Tool source fetch + build (pi-flake `extWithDeps` equivalent) ──
  #
  #   tool = rushiFlake.lib.fetchTool {
  #     pkgs = …;
  #     owner = "rushi-exts"; repo = "my-cargo-tool";
  #     rev = "abc1234"; hash = "sha256-…";
  #     build = "cargo";            # workspace cargo build
  #     # subpath = "tools/mytool"; # monorepo subdirectory
  #   };
  #
  # $out carries `<name>/tool.toml` + binary so mk-rushi's copy step
  # can drop it into the package's tools/ dir.
  fetchTool =
    { pkgs
    , owner
    , repo
    , rev
    , hash
    , build ? "cargo"
    , subpath ? null
    , rustToolchain ? null
    }:
    let
      resolvedHash = resolvePlatformHash { inherit pkgs hash; };
      fenix = pkgs.fenix or null;
      toolchain =
        if rustToolchain != null then rustToolchain
        else if fenix != null then
          fenix.stable.withComponents [ "cargo" "rust-src" "rustc" "rustfmt" ]
        else
          throw "rushi.fetchTool: need a Rust toolchain (fenix overlay or rustToolchain arg)";

      src = pkgs.fetchFromGitHub {
        inherit owner repo rev;
        hash = resolvedHash;
      };

      # Where the tool manifest lives in the source tree.
      tomlSrc = if subpath == null then src else "${src}/${subpath}";

      built =
        if build == "cargo" || build == "cargo-single" then
          pkgs.rustPlatform.buildRustPackage {
            pname = repo;
            inherit src;
            version = "1.0.0";
            nativeBuildInputs = [ toolchain ];
            cargoLock = if builtins.pathExists (src/Cargo.lock)
              then { lockFile = src/Cargo.lock; } else { };
            # cargo-single: build one crate; cargo: whole workspace.
            cargoBuildFlags = if build == "cargo-single" then [ ] else [ "--workspace" ];
            doCheck = false;
            # Repackage into the <name>/tool.toml + binary layout the
            # consumer's tools/ copy step expects.
            postInstall = ''
              mkdir -p $out
              tool_src="${tomlSrc}"
              if [ -f "$tool_src/tool.toml" ]; then
                tool_name=$(basename "$tool_src")
                mkdir -p "$out/$tool_name"
                cp "$tool_src/tool.toml" "$out/$tool_name/tool.toml"
                # Ship built binaries alongside the manifest.
                if [ -d "$out/bin" ]; then
                  cp -rL "$out/bin" "$out/$tool_name/bin" 2>/dev/null || true
                fi
              fi
            '';
          }
        else if build == "none" then
          # Prebuilt layout: $out already has <name>/tool.toml + binary.
          src
        else
          throw "rushi.fetchTool: unknown build mode '${build}' (use cargo | cargo-single | none)";
    in
    built;
}
