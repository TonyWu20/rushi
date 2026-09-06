#!/usr/bin/env bash
# lean-gate.sh — Lean kernel gate for the formal specs.
#
# Builds the Lean project in lean/ and checks that every theorem
# in each spec is verified by the Lean kernel.
# A clean `lean` build is the guarantee: every proof step is
# re-checked independently. See docs/lean-driven-development.md §8.
#
# Usage:
#   scripts/lean-gate.sh
#
# Requires either:
#   - `lean` on PATH (e.g. inside `nix develop .#lean`), or
#   - Nix with the repo flake (falls back to `nix develop`).
#
# Specs checked:
#   RushiSpec.lean    — the `rushi setup` tool-set resolver
#   TuiStreamSpec.lean — streaming render of the model response
#
# Exit 0 = clean, 1 = build failed.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SPEC="$ROOT/lean"
SPECS=(RushiSpec.lean TuiStreamSpec.lean)

if command -v lean >/dev/null 2>&1; then
  # Already in the devShell; LEAN_PATH should already include Mathlib.
  cd "$SPEC"
  for spec in "${SPECS[@]}"; do
    lean "$spec"
  done
  exit 0
fi

# Fall back to the repo Nix lean devShell (flake.nix devShells.lean).
# The shellHook sets up LEAN_PATH to include Nix-provided Mathlib oleans.
cd "$ROOT"
for spec in "${SPECS[@]}"; do
  nix develop --impure .#lean --command bash -c "cd '$SPEC' && exec lean $spec"
done
