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

  outputs = { self, nixpkgs, flake-utils, fenix, aeneas, ... }:
    flake-utils.lib.eachDefaultSystem (system:
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
        };
      in
      {
        packages.default = rushi;

        devShells.default = pkgs.mkShell {
          buildInputs = [
            rustToolchain
            pkgs.jq
            pkgs.python3
            pkgs.file
            # Lean toolchain for the lean-verify tool (tools/lean-verify):
            # `lake`, `lean`, and `z3` on PATH. `leanPackages.mathlib`
            # exports LEAN_PATH with the Nix-prebuilt Mathlib oleans, so
            # the tool's `lake build` kernel gate and DRT work without a
            # separate `nix develop .#lean` step. Environment is managed
            # here, never installed by hand (docs/skill-remapped-to-os-apps.md).
            pkgs.lean4
            pkgs.z3
            pkgs.leanPackages.mathlib
          ];
          #packages = [ rushi ];
          #shellHook = ''
          #  export RUSHI_KERNEL="${rushi}"
          #'';
        };

        # Lean 4 shell for the formal-verification backstop (docs/lean-driven-development.md §8).
        # Provides lean + lake + z3 + mathlib on PATH. Run from the repo root:
        #   nix develop .#lean
        # then `lake build RushiSpec` inside ./lean/ to check the spec + proofs.
        # Or run the gate script directly: scripts/lean-gate.sh
        devShells.lean = pkgs.mkShell {
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
        devShells.aeneas = pkgs.mkShell {
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
}
