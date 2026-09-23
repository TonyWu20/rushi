{
  description = "rushi — Unix-philosophy agent harness (kernel + distribution)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # The TUI front-end repo (docs/tui-ext-repo-split.md lives there).
    # GitHub input — not a local path, since this flake is git-tracked
    # and hostable: a git tree respects rushi-tui's .gitignore (no
    # target/, sessions/, or .git copies into the store) and works on
    # any machine. Since PR #35 the kernel no longer carries a `tui`
    # subcommand, so the devShell below puts the Nix-built
    # `rushi-tui` binary on PATH next to the Nix-built `rushi`.
    # `follows` keeps its nixpkgs/fenix pinned with the top level.
    # The input name is `rushiTui` because hyphenated input names are
    # not legal Nix identifiers in the outputs destructuring.
    rushiTui = {
      url = "github:TonyWu20/rushi-tui";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.fenix.follows = "fenix";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      fenix,
      rushiTui,
      ...
    }:
    rec {

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
      #       (rushi-flake.lib.mkRushi {
      #         pkgs = import nixpkgs { system = "x86_64-linux"; overlays = [ fenix.overlays.default ]; };
      #         modules = [ { config, lib, pkgs, ... }: {
      #           rushi.config.model = { api = "responses"; max_output_tokens = 32768; };
      #           rushi.external_tools = [ ... ];
      #           # extension_tool_paths + ui_extension_names are auto-derived
      #         } ];
      #       }).package;
      #   };
      #
      # See docs/reference/nix/nix-flake-module.md for the full option
      # schema and the pi-flake → rushi capability mapping (§9).
      lib = import ./lib;

      # ── NixOS / home-manager integration (pi-flake nixosModules /
      #    homeModules equivalents). Adds the configured package to
      #    environment.systemPackages / home.packages under
      #    `programs.rushi`. ──
      nixosModules = {
        rushi = import ./lib/nixos-module.nix;
      };
      homeManagerModules = {
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
      # aarch64-darwin is kept so Mac developers get a native
      # `nix build` / `nix develop` (Mac is a first-class rushi platform,
      # decision 2026-09-18).
      supportedSystems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
      # NOTE: nixpkgs.lib is the shared Nix library (available as a flake
      # output of the nixpkgs input).
      pkgLib = nixpkgs.lib;

      packages = pkgLib.genAttrs supportedSystems (
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ fenix.overlays.default ];
          };
          rustToolchain = (
            fenix.packages.${system}.stable.withComponents [
              "cargo"
              "clippy"
              "rust-src"
              "rustc"
              "rustfmt"
              "rust-analyzer"
            ]
          );

          rushi = pkgs.rustPlatform.buildRustPackage {
            pname = "rushi";
            version = "0.1.3";
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
            meta = with pkgLib; {
              description = "rushi — Unix-philosophy agent harness (kernel + distribution)";
              homepage = "https://github.com/TonyWu20/rushi";
              license = licenses.mit;
              mainProgram = "rushi";
            };
          };

          # Auto-generated option documentation (pi-flake docs-md / docs-html
          # equivalent). Evaluates the rushi option schema via lib.evalModules
          # and renders it with nixosOptionsDoc. No rushi build required —
          # the docs build from the option declarations alone.
          optionsModule = import ./lib/rushi-options.nix { lib = pkgs.lib; };
          optionsEval = pkgs.lib.evalModules {
            specialArgs = {
              inherit pkgs;
              lib = pkgs.lib;
            };
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
          docs-md = optionsDoc.optionsCommonMark // {
            meta = with pkgLib; {
              description = "rushi option reference (Markdown, auto-generated from the option schema)";
              homepage = "https://github.com/TonyWu20/rushi";
              license = licenses.mit;
            };
          };
          # HTML option docs (pandoc).
          docs-html = pkgs.stdenv.mkDerivation {
            name = "rushi-options-docs-html";
            nativeBuildInputs = [ pkgs.pandoc ];
            src = optionsDoc.optionsCommonMark;
            installPhase = ''
              mkdir -p $out
              pandoc $src -f markdown -t html \
                --standalone --self-contained \
                --metadata title="rushi flake module — option reference" \
                -o $out/index.html
            '';
            meta = with pkgLib; {
              description = "rushi option reference (HTML, auto-generated from the option schema)";
              homepage = "https://github.com/TonyWu20/rushi";
              license = licenses.mit;
            };
          };
        }
      );

      devShells = pkgLib.genAttrs supportedSystems (
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ fenix.overlays.default ];
          };
          rustToolchain = (
            fenix.packages.${system}.stable.withComponents [
              "cargo"
              "clippy"
              "rust-src"
              "rustc"
              "rustfmt"
              "rust-analyzer"
            ]
          );

          rushi = pkgs.rustPlatform.buildRustPackage {
            pname = "rushi";
            version = "0.1.3";
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
              # The Nix-built `rushi` binary on PATH (this flake's
              # packages.default). Since PR #35 the kernel no longer
              # carries a `tui` subcommand, so the TUI front-end comes
              # from the rushi-tui repo (the `rushiTui` input) instead.
              rushi
              # The Nix-built `rushi-tui` binary on PATH (the
              # rushi-tui flake's packages.default, imported above as
              # `rushiTui`). The kernel's side-by-side check
              # (<exe_dir>/rushi-tui) misses across store paths, but
              # its PATH fallback finds the binary here.
              rushiTui.packages.${system}.default
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
        }
      );
    };
}
