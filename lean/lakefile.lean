import Lake
open Lake DSL

/-- Formal backstop for the `rushi setup` tool-set resolver
    (bin/rushi/src/setup.rs, function `resolve_tools`).

    The TUI half of the Lean DRT backstop (TuiStreamSpec,
    TuiViewportSpec, TuiStreamDrt) moved to the TUI repo at the split
    (docs/tui-ext-repo-split.md section 4); the kernel keeps RushiSpec
    plus the rewind fork-recursion spec (RewindSpec, RewindDrt).

    `lake build` re-checks every theorem in `RushiSpec.lean` and
    `RewindSpec.lean` with the Lean kernel. A clean build with zero
    unproven claims is the guarantee. See
    docs/lean-driven-development.md §8. -/
package «rushiSpec» where

lean_lib «RushiSpec» where
  -- module RushiSpec lives at ./RushiSpec.lean

/-- Formal specification of the fork-recursion semantics of
    `rushi_common::rewind::active_ranges` (docs/rewind-fork-design.md
    section 3 and 5, "the active path").

    The Lean kernel re-checks the independent reference semantics
    (`walkChain` / `activeSeqs`), the invariants of the active-path
    recursion (P0 chain-termination, P1 fork-mask, P2 nested-fork,
    P3 branch-reentry, shape), and the concrete worked examples. A
    clean build with zero unproven claims is the guarantee. -/
lean_lib «RewindSpec» where
  -- module RewindSpec lives at ./RewindSpec.lean

/-- Differential-random-testing CLI over the RewindSpec mirror of
    `rushi_common::rewind::active_ranges` (the lean-verify op=drt
    model executable): one `(pos, rewinds)` scenario in, the active
    range list out. The Rust production mirror is bin/rewind-drt; the
    shared line protocol is documented in lean/RewindDrt.lean. -/
lean_exe «RewindDrt» where
  -- module RewindDrt lives at ./RewindDrt.lean
