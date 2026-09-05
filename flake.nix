{
  description = "rushi — Unix-philosophy agent harness (kernel + distribution)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-24.05";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };
        rustToolchain = pkgs.rust-bin.stable.latest.minimal;

        rushi = pkgs.rustPlatform.buildRustPackage rec {
          pname = "rushi";
          version = "0.1.0";
          src = self;
          cargoLock = {
            lockFile = ./Cargo.lock;
          };
          nativeBuildInputs = [ rustToolchain ];
          # Build all workspace members (loop stages, tools, tui, hooks).
          cargoBuildFlags = [ "--workspace" ];
        };
      in {
        packages.default = rushi;

        devShells.default = pkgs.mkShell {
          buildInputs = [
            rustToolchain
            pkgs.jq
            pkgs.python3
            pkgs.file
          ];
          packages = [ rushi ];
          shellHook = ''
            export RUSHI_KERNEL="${rushi}"
          '';
        };

        # Lean 4 shell for the formal-verification backstop (docs/lean-driven-development.md §8).
        # Provides lean + lake + z3 on PATH. Run from the repo root:
        #   nix develop --impure -I .#lean
        # then `lake build` inside ./lean/ to check the spec + proofs.
        devShells.lean = pkgs.mkShell {
          packages = [ pkgs.lean4 pkgs.z3 ];
          shellHook = ''
            echo "Lean 4 dev shell: lean + lake + z3 on PATH."
            echo "Check the formal spec + proofs with: cd lean && lake build"
          '';
        };
      });
}
