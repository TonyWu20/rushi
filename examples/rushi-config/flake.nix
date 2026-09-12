# examples/rushi-config/flake.nix
#
# Consumer flake: declarative rushi configuration via Nix modules.
# Mirrors the pi-config ↔ pi-flake pattern.
#
# Usage:
#   nix build .#rushi         # build the configured rushi package
#   nix run .#rushi           # run rushi from the configured package
#   nix develop .#configured  # dev shell with the configured rushi
#
# To upgrade the kernel: bump the rushi-flake ref, then:
#   nix flake update rushi-flake

{
  description = "Rushi agent-harness configuration — model, tools, extensions, hooks";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    # ── Kernel pinning: three modes ──
    #
    # 1. STABLE (production / friends):
    #    Pin to a release tag. The flake.lock pins the exact commit.
    #    Upgrade: bump the ref, then `nix flake update rushi-flake`.
    #
    # 2. UNSTABLE (developer / tracking a branch):
    #    Track a branch. `nix flake update rushi-flake` re-locks to
    #    the latest commit on that branch. No version bumping needed —
    #    every push to the branch is available after a flake update.
    #    The lock still pins the exact commit, so builds are
    #    reproducible between updates.
    #
    # 3. LOCAL (in-tree development):
    #    Use a path reference. No network. The kernel source is the
    #    local checkout; changes to the kernel are picked up
    #    immediately.
    #
    # Uncomment exactly one:

    # Option A: pin to a release tag (stable / production)
    # rushi-flake.url = "github:tony/rust-unix-harness?ref=v0.1.0";

    # Option B: track a branch (unstable / development)
    #   After pushing kernel changes: `nix flake update rushi-flake`
    #   The lock re-pins to the latest commit on the branch.
    rushi-flake.url = "github:tony/rust-unix-harness?ref=main";

    # Option C: local checkout (in-tree development, no network)
    # rushi-flake.url = "path:../../";

    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, inputs, ... } @ flake:
    let
      systems = [ "x86_64-linux" "aarch64-darwin" ];
      forAllSystems = inputs.nixpkgs.lib.genAttrs systems;
      rushiFlake = inputs."rushi-flake";
      nixpkgs    = inputs.nixpkgs;

      forSystem = system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ inputs.fenix.overlays.default ];
          };

          # ── The configured rushi package ──
          #
          # `modules` is a list of NixOS-style module functions, merged
          # left-to-right. Each module is:
          #   { config, lib, pkgs, ... } → { rushi = { ... }; }
          #
          # See docs/reference/nix/nix-flake-module.md §3 for the full
          # option schema.
          #
          rushiConfigured = rushiFlake.lib.mkRushi {
            inherit pkgs;
            modules = [

              # ══════════════════════════════════════════════
              # Module 1: model + loop configuration
              # ══════════════════════════════════════════════
              ({ config, lib, pkgs, ... }:
              {
                rushi.version = "0.1";

                rushi.config.model = {
                  api = "responses";
                  max_output_tokens = 32768;
                  reasoning_effort = "xhigh";

                  # Per-model overrides (dotted names are quoted in TOML).
                  "deepseek" = {
                    model_id = "deepseek-v4-flash";
                    base_url = "https://api.deepseek.com";
                    api_key_env = "DEEPSEEK_API_KEY";
                    context_tokens = 131072;
                  };
                };

                rushi.config.active.model = "deepseek";

                rushi.config.limits = {
                  context_budget_tokens = 262144;
                  compact_reserve_tokens = 16384;
                  bash_timeout_max = 300;
                };

                rushi.config.loop = {
                  command = "rushi";
                  args = [ "run" ];
                  arg_style = "append_session";
                };
              })

              # ══════════════════════════════════════════════
              # Module 2: tools + extensions
              #
              # Kernel tools: whitelist from the kernel's bundled set.
              # External tools / UI extensions / hooks: Nix derivations
              # (fetchFromGitHub, cargo packages, local paths, …).
              # ══════════════════════════════════════════════
              ({ config, lib, pkgs, ... }:
              {
                # Kernel tools to ship (must exist in the kernel's tools/).
                rushi.tools = [ "read" "write" "edit" "bash" ];

                # UI extensions to enable (names in the ext dir).
                rushi.ui_extensions = [ "statusline-rs" "mermaid" ];

                # External tool sources (Nix derivations). Each derivation's
                # output must contain <tool-name>/tool.toml + binary.
                # rushi.external_tools = [
                #   (pkgs.fetchFromGitHub {
                #     owner = "tony";
                #     repo = "rushi-exts";
                #     rev = "abc1234";
                #     hash = "sha256-...";
                #   })
                # ];

                # External UI extension sources.
                # rushi.external_ui_extensions = [
                #   (pkgs.fetchFromGitHub {
                #     owner = "tony";
                #     repo = "rushi-exts";
                #     rev = "abc1234";
                #     hash = "sha256-...";
                #   })
                # ];

                # Hook binaries (goal-continuation, lean-verify, …).
                # rushi.external_hooks = [
                #   (pkgs.fetchFromGitHub {
                #     owner = "tony";
                #     repo = "rushi-exts";
                #     rev = "abc1234";
                #     hash = "sha256-...";
                #   })
                # ];
              })

              # ══════════════════════════════════════════════
              # Module 3: hooks + system prompt
              # ══════════════════════════════════════════════
              ({ config, lib, pkgs, ... }:
              {
                rushi.config.hooks = {
                  timeout_ms = 30000;
                  on = [
                    {
                      window = "exhausted.handle";
                      command = "harness-hook-compact";
                      args = [ ];
                    }
                    {
                      window = "overflow.resolve";
                      command = "harness-hook-compact";
                      args = [ ];
                    }
                    # Goal-continuation hooks (from rushi-exts).
                    # Uncomment when the exts hook binaries are shipped
                    # via rushi.external_hooks.
                    # {
                    #   window = "run.idle";
                    #   command = "harness-hook-goal-idle";
                    #   args = [ ];
                    # }
                  ];
                };

                # rushi.config.system_prompt.text = ''
                #   You are an expert coding assistant in `rushi`, a Unix
                #   agent harness. …
                # '';
              })

              # ══════════════════════════════════════════════
              # Module 4: environment (optional)
              #
              # Exported into the rushi process at runtime. The
              # `file` form keeps secrets out of the Nix store (a
              # sops-nix derivation read at runtime by the wrapper).
              # Threaded via `extraSpecialArgs = { inherit secrets; }`
              # on the mkRushi call.
              # ══════════════════════════════════════════════
              # ({ config, lib, pkgs, secrets, ... }: {
              #   rushi.environment = {
              #     DEEPSEEK_API_KEY = { file = secrets.deepseekKey; };
              #     RUSHI_LOG = "debug";
              #   };
              # })
            ];
          };
        in
        {
          # Fully configured rushi (kernel + config + tools + exts).
          default = rushiConfigured.package;
          rushi = rushiConfigured.package;
          # Raw kernel binary (no user config).
          rushi-kernel = rushiFlake.packages.${system}.rushi;
        };

    in
    {
      packages = forAllSystems forSystem;

      # ── Dev shell with the configured rushi ──
      devShells = forAllSystems (system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ inputs.fenix.overlays.default ];
          };
          configured = self.packages.${system}.default;
        in
        {
          configured = pkgs.mkShell {
            buildInputs = [ configured ];
            shellHook = ''
              echo "Configured rushi (v${configured.version})."
              echo "  bin/rushi       kernel binary"
              echo "  tools/         kernel + external tool manifests"
              echo "  config.toml    generated from Nix module options"
              echo "  ui_extensions/ UI extension files"
              echo "  hooks/         hook binaries"
              echo "  tools.manifest rushi.toml-equivalent"
            '';
          };
        }
      );
    };
}
