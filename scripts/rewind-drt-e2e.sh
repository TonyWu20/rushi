#!/usr/bin/env bash
# rewind-drt-e2e.sh — differential-random-test gate for the rewind/fork
# active-range recursion.
#
# Two sides must agree on every input line of the shared protocol
# (`END <pos> [<seq>:<target>:<mode> ...]`), per docs/rewind-fork-design.md
# "Verification":
#   - model  side: lean/.lake/build/bin/RewindDrt (lean/RewindDrt.lean)
#   - production: target/debug/rewind-drt (bin/rewind-drt, Rust)
#
# A green run is the regression gate between the kernel-checked Lean
# spec (RewindSpec) and the Rust implementation it mirrors.
#
# Usage:
#   scripts/rewind-drt-e2e.sh [N]
#   N = number of generated random inputs (default 500; pass a larger
#   N for a heavier regression run). Plus four fixed malformed lines
#   that must make BOTH sides print ERR and exit 1.
#
# Requires:
#   - cargo build -p rewind-drt
#   - a built `lake build` of lean/RewindDrt.lean (run
#     scripts/lean-gate.sh, or `nix develop .#lean` then `lake build`)
#   - SKIPs (exit 0) when the Lean RewindDrt executable is absent.
#
# Exit 0 = all inputs agree, 1 = a mismatch.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOT="$SCRIPT_DIR"
N="${1:-500}"

RUST_BIN="$ROOT/target/debug/rewind-drt"
LEAN_BIN="$ROOT/lean/.lake/build/bin/RewindDrt"

if [ ! -x "$RUST_BIN" ]; then
  echo "rewind-drt-e2e: building Rust DRT executable..."
  (cd "$ROOT" && cargo build --quiet -p rewind-drt) \
    || { echo "FAIL: cargo build -p rewind-drt"; exit 1; }
fi

if [ ! -x "$LEAN_BIN" ]; then
  echo "SKIP: Lean RewindDrt executable not built."
  echo "      Run scripts/lean-gate.sh (or `nix develop .#lean` then"
  echo "      `cd lean && lake build`) to produce $LEAN_BIN."
  exit 0
fi

WORK="$(mktemp -d "${TMPDIR:-/tmp}/rewind-drt-e2e.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

# Deterministic input generator (fixed-seed LCG; docs in the script).
bash "$ROOT/scripts/rewind-drt-inputs.sh" "$N" > "$WORK/inputs.txt"

# Malformed-input symmetry: every line below must make BOTH sides
# print "ERR" and exit 1. (GARBAGE / "END" alone = missing pos;
# "END 3 1:2" = short triple; "END 3 1:2:9" = bad mode bit.)
printf '%s\n' 'GARBAGE' 'END' 'END 3 1:2' 'END 3 1:2:9' >> "$WORK/inputs.txt"

TOTAL="$(wc -l < "$WORK/inputs.txt")"

FAIL=0
while IFS= read -r line; do
  L_OUT="$(DRT_INPUT="$line" "$LEAN_BIN" 2>/dev/null)"
  L_RC=$?
  R_OUT="$(DRT_INPUT="$line" "$RUST_BIN" 2>/dev/null)"
  R_RC=$?
  if [ "$L_OUT" != "$R_OUT" ] || [ "$L_RC" -ne "$R_RC" ]; then
    echo "MISMATCH on input: $line"
    echo "  lean (rc=$L_RC): $L_OUT"
    echo "  rust (rc=$R_RC): $R_OUT"
    FAIL=1
  fi
done < "$WORK/inputs.txt"

if [ "$FAIL" -ne 0 ]; then
  echo "FAIL: rewind DRT — Lean model and Rust diverged on some of $TOTAL inputs."
  exit 1
fi

echo "PASS: rewind DRT — $TOTAL/$TOTAL inputs agree (Lean model vs Rust active_ranges)."
