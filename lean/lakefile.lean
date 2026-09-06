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
lean_lib «TuiStreamSpec» where
  -- module TuiStreamSpec lives at ./TuiStreamSpec.lean

/-- Formal specification for the viewport-based scrollback design
    (docs/tui_feature_requests_from_human.md, "viewport-based
    scrollback").

    The Lean kernel re-checks every invariant theorem and concrete
    example. A clean build with zero unproven claims is the guarantee.
    See docs/lean-driven-development.md §8. -/
lean_lib «TuiViewportSpec» where
  -- module TuiViewportSpec lives at ./TuiViewportSpec.lean

/-- Differential-random-testing CLI over the TuiStreamSpec reference
    renderer (the lean-verify op=drt model executable): one scenario
    line in, the rendered view out. The Rust production mirror is
    bin/tui-stream-drt; the shared line protocol is documented in
    lean/TuiStreamDrt.lean. -/
lean_exe «TuiStreamDrt» where
  -- module TuiStreamDrt lives at ./TuiStreamDrt.lean
