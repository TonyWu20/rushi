{
  description = "rushi — Unix-philosophy agent harness (kernel + distribution)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # Rust->Lean verification toolchain (AeneasVerif). The flake builds
    # `aeneas` (OCaml) + `charon` (Rust) and ships the per-target
    # backends in `aeneas-release`. Borrowed flake pattern: a whole
    # toolchain as a flake input, consumed via `packages.` (their
    # devShell does the same with `inputsFrom`). No `nixpkgs.follows`:
    # Aeneas deliberately pins its own nixpkgs revision (OCaml 5.2), so
    # keep the input tree hermetic with its own lock, like everything
    # else in this flake. Charon is nested inside aeneas's lock (their
    # charon-pin + check-charon-pin guard that pairing), so both
    # binaries reach us through the single `aeneas` input — hermetic,
    # no top-level charon pin to drift.
    aeneas = {
      url = "github:AeneasVerif/aeneas";
    };
  };

  outputs = { self, nixpkgs, flake-utils, fenix, aeneas, ... }: rec {

    # ── Shared library (system-independent) ──
    #
    # `lib.mkRushi` is the Nix flake module for declarative rushi
    # configuration, following the pi-flake `mkCodingAgent` pattern.
    #
    # Usage from a consumer flake:
    #
    #   inputs = {
    #     nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    #     rushi-flake.url = "github:tony/rust-unix-harness?ref=v0.1.0";
    #   };
    #   outputs = { self, nixpkgs, rushi-flake, ... }: {
    #     packages.x86_64-linux.default =
    #       rushi-flake.lib.mkRushi {
    #         pkgs = import nixpkgs { system = "x86_64-linux"; overlays = [ fenix.overlays.default ]; };
    #         modules = [ { config, lib, pkgs, ... }: {
    #           rushi.config.model = { api = "responses"; max_output_tokens = 32768; };
    #           rushi.tools = [ "read" "write" "edit" "bash" ];
    #         } ];
    #       }.package;
    #   };
    #
    # See docs/nix-flake-module.md for the full option schema and the
    # pi-flake → rushi capability mapping (§9).
    lib = {
      # Build a fully configured rushi package from Nix modules.
      #
      # Parameters:
      #   pkgs              — nixpkgs for the target system (fenix overlay for Rust)
      #   modules           — list of NixOS-style module functions (lib.evalModules)
      #   rustToolchain     — optional pre-resolved toolchain (auto-resolved if null)
      #   extraSpecialArgs  — extra args injected into every module's scope
      #
      # Returns: { package, config, version, options, configAttrs }
      #   config      = generated config.toml text
      #   options     = full lib.mkOption schema (for nixosOptionsDoc)
      #   configAttrs = deep-merged Nix attrset (pre-TOML)
      mkRushi =
        { pkgs
        , modules ? [ ]
        , rustToolchain ? null
        , extraSpecialArgs ? { }
        , ...
        }:
        (import ./lib/mk-rushi.nix) {
          inherit pkgs modules rustToolchain extraSpecialArgs;
          src = self;
        };

      # Kernel defaults (for inspection or custom overlay construction).
      defaults = import ./lib/rushi-defaults.nix;

      # Extension / tool source helpers (pi-flake extNoDeps / extWithDeps
      # equivalents). Each takes { pkgs, … } and returns a derivation
      # suitable for rushi.external_tools / external_ui_extensions.
      fetchExt = args: (import ./lib/fetch-ext.nix).fetchExt args;

      fetchTool = args: (import ./lib/fetch-ext.nix).fetchTool args;

      resolvePlatformHash = args: (import ./lib/fetch-ext.nix).resolvePlatformHash args;
    };

    # ── NixOS / home-manager integration (pi-flake nixosModules /
    #    homeModules equivalents). Adds the configured package to
    #    environment.systemPackages / home.packages under
    #    `programs.rushi`. ──
    nixosModules = {
      rushi = import ./lib/nixos-module.nix;
    };
    homeManagerConfig = {
      rushi = import ./lib/home-manager-module.nix;
    };

    # ── Per-system packages and devShells ──
    #
    # Use builtins.genAttrs over an explicit supported-systems list instead
    # of flake-utils.lib.eachDefaultSystem:
    #   1. eachDefaultSystem in the pinned flake-utils (11707dc) transposes
    #      the result to { shell-name = { system = …; }; } which breaks
    #      `nix develop .` (it expects devShells.<system>.<name>).
    #   2. Nixpkgs 26.11 dropped x86_64-darwin; evaluating all 4 default
    #      systems fails on the darwin entry and poisons the whole attrset.
    supportedSystems = [ "x86_64-linux" "aarch64-linux" ];
    # NOTE: nixpkgs.lib is the shared Nix library (available as a flake
    # output of the nixpkgs input).
    pkgLib = nixpkgs.lib;

    packages = pkgLib.genAttrs supportedSystems (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ fenix.overlays.default ];
        };
        rustToolchain = (fenix.packages.${system}.stable.withComponents [
          "cargo"
          "clippy"
          "rust-src"
          "rustc"
          "rustfmt"
          "rust-analyzer"
        ]);

        rushi = pkgs.rustPlatform.buildRustPackage rec {
          pname = "rushi";
          version = "0.1.0";
          src = self;
          cargoLock = {
            lockFile = ./Cargo.lock;
          };
          nativeBuildInputs = [ rustToolchain ];
          # Build all workspace members (loop stages, tools, hooks).
          # (The TUI + TUI-stream-drt moved to the rushi-tui repo at the
          # split; docs/tui-ext-repo-split.md section 4, item 6.)
          cargoBuildFlags = [ "--workspace" ];
          doCheck = false;
          # Ship tools/ alongside bin/ so the side-by-side check in
          # resolve_kernel_tools_dir (<exe>/../tools) finds them without
          # needing RUSHI_KERNEL.
          postInstall = ''
            mkdir -p $out/tools
            cp -r $src/tools/. $out/tools/
          '';
        };

        # Auto-generated option documentation (pi-flake docs-md / docs-html
        # equivalent). Evaluates the rushi option schema via lib.evalModules
        # and renders it with nixosOptionsDoc. No rushi build required —
        # the docs build from the option declarations alone.
        optionsModule = import ./lib/rushi-options.nix { lib = pkgs.lib; };
        optionsEval = pkgs.lib.evalModules {
          specialArgs = { inherit pkgs; lib = pkgs.lib; };
          modules = [ optionsModule ];
        };
        optionsDoc = pkgs.nixosOptionsDoc {
          inherit (optionsEval) options;
          documentType = "none";
          warningsAreErrors = false;
        };
      in
      {
        default = rushi;
        # Raw kernel build (no user config). For `rushi setup` / dev use.
        # Use lib.mkRushi for a fully configured package.
        rushi = rushi;
        # Markdown option docs (auto-generated from the option schema).
        docs-md = optionsDoc.optionsCommonMark;
        # HTML option docs (pandoc).
        docs-html = pkgs.stdenv.mkDerivation {
          pname = "rushi-options-docs-html";
          nativeBuildInputs = [ pkgs.pandoc ];
          src = optionsDoc.optionsCommonMark;
          installPhase = ''
            mkdir -p $out
            pandoc $src -f markdown -t html \
              --standalone --self-contained \
              --metadata title="rushi flake module — option reference" \
              -o $out/index.html
          '';
        };
      }
    );

    devShells = pkgLib.genAttrs supportedSystems (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ fenix.overlays.default ];
        };
        rustToolchain = (fenix.packages.${system}.stable.withComponents [
          "cargo"
          "clippy"
          "rust-src"
          "rustc"
          "rustfmt"
          "rust-analyzer"
        ]);

        rushi = pkgs.rustPlatform.buildRustPackage rec {
          pname = "rushi";
          version = "0.1.0";
          src = self;
          cargoLock = {
            lockFile = ./Cargo.lock;
          };
          nativeBuildInputs = [ rustToolchain ];
          cargoBuildFlags = [ "--workspace" ];
          doCheck = false;
          postInstall = ''
            mkdir -p $out/tools
            cp -r $src/tools/. $out/tools/
          '';
        };
      in
      {
        default = pkgs.mkShell {
          buildInputs = [
            rustToolchain
            pkgs.jq
            pkgs.python3
            pkgs.file
            # Lean toolchain for the Lean 4 backstop (`lake build
            # RushiSpec`) and the exts-owned lean-verify tool
            # (rushi-exts/goal-tools/lean-verify/): `lake`, `lean`, and
            # `z3` on PATH. `leanPackages.mathlib`
            # exports LEAN_PATH with the Nix-prebuilt Mathlib oleans, so
            # the tool's `lake build` kernel gate and DRT work without a
            # separate `nix develop .#lean` step. Environment is managed
            # here, never installed by hand (docs/skill-remapped-to-os-apps.md).
            pkgs.lean4
            pkgs.z3
            pkgs.leanPackages.mathlib
            # The Nix-built `rushi` binary on PATH (this flake's
            # packages.default). The launcher finds `tui` on PATH after
            # the side-by-side check (bin/rushi/src/main.rs, function
            # resolve_tui_binary). The rushi-tui .envrc puts its
            # target/release on PATH for that lookup.
            rushi
          ];
          # Do not export RUSHI_KERNEL to the Nix store path: the
          # built package has no tools/ dir, and `rushi setup` would
          # materialize zero tools. From a dev checkout, `rushi setup`
          # falls back to CWD/tools, which works without the variable.
          # Uncomment only when a store copy carries tools/:
          # shellHook = ''
          #   export RUSHI_KERNEL="${rushi}"
          # '';
        };

        # Lean 4 shell for the formal-verification backstop (docs/lean-driven-development.md §8).
        # Provides lean + lake + z3 + mathlib on PATH. Run from the repo root:
        #   nix develop .#lean
        # then `lake build RushiSpec` inside ./lean/ to check the spec + proofs.
        # Or run the gate script directly: scripts/lean-gate.sh
        lean = pkgs.mkShell {
          packages = [ pkgs.lean4 pkgs.z3 pkgs.leanPackages.mathlib ];
          shellHook = ''
            echo "Lean 4 dev shell: lean + lake + z3 + mathlib on PATH."
            echo "Check the formal spec + proofs with: cd lean && lean RushiSpec.lean"
          '';
        };

        # Aeneas Rust->Lean toolchain (github:AeneasVerif/aeneas, flake-based).
        # `charon` extracts a cargo crate's MIR to LLBC; `aeneas` translates
        # the LLBC into pure Lean (the functional core of the "functional
        # core, imperative shell" pattern — the same split as this harness).
        # The aeneas package's bin/ carries both binaries (charon is
        # symlinked in by their flake). The rust toolchain is what charon
        # invokes on your crate; elan is what Aeneas's own devShell uses
        # to let a generated project fetch its pinned lean-toolchain.
        aeneas = pkgs.mkShell {
          packages = [
            rustToolchain
            pkgs.jq
            pkgs.lean4
            pkgs.z3
            pkgs.leanPackages.mathlib
            pkgs.elan
            aeneas.packages.${system}.aeneas
            aeneas.packages.${system}.charon
          ];
          shellHook = ''
            echo "Aeneas dev shell: charon + aeneas (Rust->Lean) + lean + lake + z3 + mathlib on PATH."
            echo "Translate a crate: charon cargo --preset=aeneas && aeneas -backend lean <crate>.llbc"
            echo "Then prove the generated Lean model with the lean-verify tool (op=build), and"
            echo "regression-gate it against the real Rust binary (op=drt)."
          '';
        };
      });
  };
}
