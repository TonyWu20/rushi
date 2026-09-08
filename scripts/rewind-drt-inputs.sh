#!/usr/bin/env bash
# Deterministic input generator for the RewindSpec DRT gate — the
# `lean-verify` op=drt / op=check-inputs `input_gen` for the
# lean/RewindDrt.lean vs bin/rewind-drt pair (docs/rewind-fork-design.md
# "Verification").
#
# Emits N well-formed one-line scenarios of the shared protocol:
#   END <pos> [<seq>:<target>:<mode> ...]
#
# Determinism: a fixed-seed Park-Miller LCG (no external randomness, no
# $RANDOM), so any two runs agree line for line and a re-run of the
# gate reproduces its inputs exactly.
#
# Domain: each line is a random "rewind chain" for active_ranges. The
# triples are in strictly-increasing seq order and every one satisfies
# target < seq (a rewind points at an earlier event), so the lines are
# exactly the valid inputs of the recursion the spec proves. `pos`
# (the prefix end) varies across and past the seqs so the cases
# "no rewind in play", "rewind lands at the prefix", and "deep nested
# chain" all occur. `mode` is a random bit (0 = on, 1 = before).
#
# Usage: scripts/rewind-drt-inputs.sh [N]   (default N=100000)
set -euo pipefail
n="${1:-100000}"
awk -v n="$n" '
BEGIN {
  st = 123456789
  for (i = 0; i < n; i++) {
    pos = r(60)                       # prefix end 0..59
    k   = r(7)                        # rewind count 0..6
    prev = 0
    line = "END " pos
    for (j = 0; j < k; j++) {
      seq    = prev + 1 + r(6)        # strictly-increasing seq, >= 1
      prev   = seq
      target = r(seq)                 # 0..seq-1, so target < seq
      mode   = r(2)                   # 0 = on, 1 = before
      line = line " " seq ":" target ":" mode
    }
    printf "%s\n", line
  }
}
# Park-Miller LCG, period 2^31-1; exact under IEEE-754 doubles.
function r(m) { st = (st * 48271) % 2147483647; return int(st / 2147483647 * m) }
'
