# tests/nix/flake.nix — issue #13 acceptance tests for lib.mkRushi.
#
# Covers the meta.rushi eval-time contract and its fallbacks:
#   * evals.*            — pure-eval assertions (no building)
#   * packages.<sys>.*   — built configured packages. case-hook-guard and
#                          case-meta-mismatch are expected to FAIL their
#                          builds (run.sh asserts the failure mode)
#
# Run: bash tests/nix/run.sh

{
  description = "mkRushi meta.rushi eval-time tests";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, fenix }:
    let
      system = "x86_64-linux";

      pkgs = import nixpkgs { inherit system; };

      # This test flake lives inside the kernel checkout.
      kernelRoot = ../..;

      # Same toolchain the root flake builds the kernel with, so the
      # kernel drv is shared with .#packages.x86_64-linux.rushi.
      rustToolchain = fenix.packages.${system}.stable.withComponents [
        "cargo"
        "clippy"
        "rust-src"
        "rustc"
        "rustfmt"
        "rust-analyzer"
      ];

      mkCase =
        { modules }:
        (import (kernelRoot + "/lib/mk-rushi.nix")) {
          inherit pkgs;
          inherit modules;
          src = kernelRoot;
          inherit rustToolchain;
          kernelCargoLock = { lockFile = kernelRoot + "/Cargo.lock"; };
        };

      # ── Fake producer packages ────────────────────────────────────
      # Fast stdenv stand-ins mirroring the $out contract of real
      # producer flakes (docs/reference/nix/ext-flake-authoring.md §2).
      # No Rust involved, so the gate stays quick.

      placeholderSrc =
        pkgs.writeTextFile {
          name = "fixture-src";
          destination = "/placeholder";
          text = "";
        };

      goalTool =
        pkgs.stdenv.mkDerivation {
          pname = "fixture-goal-tool";
          version = "0";
          src = placeholderSrc;
          dontUnpack = true;
          installPhase = ''
            mkdir -p $out/goal
            cp -rL ${self}/fixtures/fake-goal/. $out/goal/
            chmod +x $out/goal/bin/goal
          '';
          meta = {
            description = "Fixture. goal tool package with meta.rushi.entry";
            rushi = { entry = "goal"; };
          };
        };

      # Same layout, but the declared entry name does not exist.
      lyingTool =
        pkgs.stdenv.mkDerivation {
          pname = "fixture-lying-tool";
          version = "0";
          src = placeholderSrc;
          dontUnpack = true;
          installPhase = ''
            mkdir -p $out/goal
            cp -rL ${self}/fixtures/fake-goal/. $out/goal/
            chmod +x $out/goal/bin/goal
          '';
          meta = {
            description = "Fixture. tool package declaring a wrong entry";
            rushi = { entry = "nope"; };
          };
        };

      legacyTool =
        pkgs.stdenv.mkDerivation {
          pname = "fixture-legacy-tool";
          version = "0";
          src = placeholderSrc;
          dontUnpack = true;
          installPhase = ''
            mkdir -p $out/legacy-tool
            cp -rL ${self}/fixtures/legacy/. $out/legacy-tool/
            chmod +x $out/legacy-tool/bin/legacy-tool
          '';
          meta.description = "Fixture. pre-issue-13 producer without meta.rushi";
        };

      goalExt =
        pkgs.stdenv.mkDerivation {
          pname = "fixture-goal-ext";
          version = "0";
          src = placeholderSrc;
          dontUnpack = true;
          installPhase = ''
            mkdir -p $out/goal/target/release
            cp ${self}/fixtures/fake-goal-ext/ext.toml $out/goal/ext.toml
            cp ${self}/fixtures/fake-goal-ext/bin/fake-goal-ext $out/goal/target/release/
            chmod +x $out/goal/target/release/fake-goal-ext
          '';
          meta = {
            description = "Fixture. goal UI ext package with meta.rushi.ext";
            rushi = { ext = "goal"; };
          };
        };

      legacyExt =
        pkgs.stdenv.mkDerivation {
          pname = "fixture-legacy-ext";
          version = "0";
          src = placeholderSrc;
          dontUnpack = true;
          installPhase = ''
            mkdir -p $out/legacy-ext/target/release
            cp ${self}/fixtures/legacy-ext/ext.toml $out/legacy-ext/ext.toml
            cp ${self}/fixtures/legacy-ext/bin/legacy-ext $out/legacy-ext/target/release/
            chmod +x $out/legacy-ext/target/release/legacy-ext
          '';
          meta.description = "Fixture. pre-issue-13 ext producer without meta.rushi";
        };

      fakeHook =
        pkgs.stdenv.mkDerivation {
          pname = "fixture-hook";
          version = "0";
          src = placeholderSrc;
          dontUnpack = true;
          installPhase = ''
            mkdir -p $out/bin
            cp ${self}/fixtures/fake-hooks/bin/harness-hook-fake $out/bin/
            chmod +x $out/bin/harness-hook-fake
          '';
          meta = {
            description = "Fixture. hook package with meta.rushi.bin";
            rushi = { bin = "harness-hook-fake"; };
          };
        };

      # ── Test cases ────────────────────────────────────────────────

      # Fully migrated producer set. The consumer sets no paths or
      # names, so everything must come from meta.rushi at eval time.
      caseMeta =
        mkCase {
          modules = [ ({ config, lib, pkgs, ... }: {
            rushi.external_tools = [ goalTool ];
            rushi.external_ui_extensions = [ goalExt ];
            rushi.external_hooks = [ fakeHook ];
            rushi.config.hooks = {
              on = [
                {
                  window = "run.idle";
                  command = "harness-hook-fake";
                  args = [ ];
                }
              ];
            };
          }) ];
        };

      # Mixed. One migrated (meta) and one pre-#13 producer each.
      # Eval-time values cover the meta source only. Build-time
      # discovery appends the legacy ones.
      caseLegacy =
        mkCase {
          modules = [ ({ config, lib, pkgs, ... }: {
            rushi.external_tools = [ goalTool legacyTool ];
            rushi.external_ui_extensions = [ goalExt legacyExt ];
          }) ];
        };

      # Consumer explicitly sets extension_tool_paths. It is used
      # verbatim, the meta-derived value is ignored, and no
      # discovery runs.
      caseUserAuth =
        mkCase {
          modules = [ ({ config, lib, pkgs, ... }: {
            rushi.external_tools = [ goalTool ];
            rushi.config.paths.extension_tool_paths = [ "tools/custom" ];
          }) ];
        };

      # Plain-path source (no flake, no meta possible). The #10
      # fallback must keep it working, with an eval-time warning.
      casePlainPath =
        mkCase {
          modules = [ ({ config, lib, pkgs, ... }: {
            rushi.external_tools = [ (self + "/fixtures/plain-tool") ];
          }) ];
        };

      # A bare hook command no meta.rushi.bin covers and that is not a
      # kernel bin. The build-time guard must reject the build.
      caseHookGuard =
        mkCase {
          modules = [ ({ config, lib, pkgs, ... }: {
            rushi.external_hooks = [ fakeHook ];
            rushi.config.hooks = {
              on = [
                {
                  window = "run.idle";
                  command = "harness-hook-never-shipped";
                  args = [ ];
                }
              ];
            };
          }) ];
        };

      # A lying producer must fail the build with a clear error.
      caseMetaMismatch =
        mkCase {
          modules = [ ({ config, lib, pkgs, ... }: {
            rushi.external_tools = [ lyingTool ];
          }) ];
        };

      # ── Eval-time values (nix eval tests/nix#evals.<case>.<field>) ──
      evals = {
        meta = {
          extToolPaths = caseMeta.extensionToolPaths;
          uiNames = caseMeta.uiExtensionNames;
          configText = caseMeta.config;
        };
        legacy = {
          extToolPaths = caseLegacy.extensionToolPaths;
          uiNames = caseLegacy.uiExtensionNames;
        };
        userAuth = {
          extToolPaths = caseUserAuth.extensionToolPaths;
          configText = caseUserAuth.config;
        };
        plainPath = {
          extToolPaths = casePlainPath.extensionToolPaths;
        };
      };

      # Verifies lib.warn exists in the pinned nixpkgs. Fails eval if
      # the function was removed.
      probe = pkgs.lib.warn "rushi-issue-13 lib.warn probe (issue #13)" 42;



    in
    {
      inherit probe;
      inherit evals;
      packages.${system} = {
        case-meta = caseMeta.package;
        case-legacy = caseLegacy.package;
        case-user-auth = caseUserAuth.package;
        case-plain-path = casePlainPath.package;
        case-hook-guard = caseHookGuard.package;
        case-meta-mismatch = caseMetaMismatch.package;
      };
    };
}
