#!/usr/bin/env bash
# lean-gate.sh — Lean kernel gate for the kernel's formal specs.
#
# Builds the Lean project in lean/ and checks that every theorem is
# verified by the Lean kernel. A clean build with zero `sorry` is the
# guarantee: every proof step is re-checked independently.
# See docs/lean-driven-development.md §8.
#
# The TUI specs (TuiStreamSpec, TuiViewportSpec, TuiStreamDrt) moved
# to the rushi-tui repo at the repo split; this gate now covers only
# the kernel specs.
#
# Specs checked (all built via `lake build`):
#   RushiSpec.lean  — the `rushi setup` tool-set resolver
#   RewindSpec.lean — the rewind/fork active-range recursion
#   RewindDrt.lean  — the DRT model executable for RewindSpec
#
# Usage:
#   scripts/lean-gate.sh
#
# Requires either:
#   - `lake` on PATH (e.g. inside `nix develop .#lean`), or
#   - Nix with the repo flake (falls back to `nix develop`).
#
# Exit 0 = clean, 1 = sorry found or build failed.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SPEC="$ROOT/lean"

# Zero-sorry invariant: the gate guarantees no unproven claims.
# Search only source .lean files, not build artifacts.
if rg -n '\bsorry\b' "$SPEC"/*.lean; then
  echo "lean-gate: FAIL — unproven claims (sorry) found in lean/"
  exit 1
fi
echo "lean-gate: no sorry found in lean/*.lean"

if command -v lake >/dev/null 2>&1; then
  # Already in the devShell (or lake is on PATH).
  cd "$SPEC"
  lake build
  exit 0
fi

# Fall back to the repo Nix lean devShell (flake.nix devShells.lean).
# The shellHook sets up LEAN_PATH to include Nix-provided oleans.
cd "$ROOT"
nix develop --impure .#lean --command bash -c "cd '$SPEC' && exec lake build"
