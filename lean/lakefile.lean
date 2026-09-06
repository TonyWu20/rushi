import Lake
open Lake DSL

/-- Formal backstop for the `rushi setup` tool-set resolver
    (bin/rushi/src/setup.rs, function `resolve_tools`).

    `lake build` re-checks every theorem in `RushiSpec.lean` with the
    Lean kernel. A clean build with zero unproven claims is the
    guarantee. See docs/lean-driven-development.md §8. -/
package «rushiSpec» where

lean_lib «RushiSpec» where
  -- module RushiSpec lives at ./RushiSpec.lean

/-- Formal specification for streaming render of the model response
    (docs/tui_feature_requests_from_human.md, "Stream rendering of the
    model response").

    The Lean kernel re-checks every invariant theorem and concrete
    example. A clean build with zero unproven claims is the guarantee.
    See docs/lean-driven-development.md §8. -/
package «tuiStreamSpec» where

lean_lib «TuiStreamSpec» where
  -- module TuiStreamSpec lives at ./TuiStreamSpec.lean
