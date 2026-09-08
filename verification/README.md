# verification/

Formal-verification tooling. This directory is **not** part of the rushi
runtime: nothing in `bin/`, `crates/`, or the installed harness loads or
spawns these executables. They exist only to feed the differential-random
test (DRT) gate defined in `docs/rewind-fork-design.md` ("Verification").

- `rewind-drt/` — Rust production-side DRT executable. Mirrors
  `lean/RewindDrt.lean` (the Lean model CLI) on the shared line protocol and
  calls the real `rushi_common::rewind::active_ranges`. The gate
  (`scripts/rewind-drt-e2e.sh`) diffs the two sides on generated inputs
  (`scripts/rewind-drt-inputs.sh`).

Run the gate with `bash scripts/rewind-drt-e2e.sh [N]`.
