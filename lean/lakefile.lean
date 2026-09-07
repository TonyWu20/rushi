import Lake
open Lake DSL

/-- Formal backstop for the `rushi setup` tool-set resolver
    (bin/rushi/src/setup.rs, function `resolve_tools`).

    The TUI half of the Lean DRT backstop (TuiStreamSpec,
    TuiViewportSpec, TuiStreamDrt) moved to the TUI repo at the split
    (docs/tui-ext-repo-split.md section 4); the kernel keeps only
    RushiSpec.

    `lake build` re-checks every theorem in `RushiSpec.lean` with the
    Lean kernel. A clean build with zero unproven claims is the
    guarantee. See docs/lean-driven-development.md §8. -/
package «rushiSpec» where

lean_lib «RushiSpec» where
  -- module RushiSpec lives at ./RushiSpec.lean
