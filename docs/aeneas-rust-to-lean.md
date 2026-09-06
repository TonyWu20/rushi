# Aeneas Rust->Lean: what we learned and borrowed

Source repos: [e6qu/rust-lean-aeneas](https://github.com/e6qu/rust-lean-aeneas)
(tutorials running the pipeline) and
[AeneasVerif/aeneas](https://github.com/AeneasVerif/aeneas) (the translator
itself, flake-based). The Aeneas pipeline turns Rust into a *pure Lean model*
so the Lean kernel can re-check properties of it:

```
cargo crate ──charon──> <crate>.llbc ──aeneas -backend lean──> <Crate>.lean
             (MIR extraction)        (LLBC -> pure Lean model)
```

## The pipeline, pinned

- `charon cargo --preset=aeneas` (run in the crate root) extracts the crate's
  MIR to `<crate>.llbc`.
- `aeneas -backend lean <crate>.llbc` (run in the crate root) writes
  `<Crate>.lean` next to it: a `noncomputable section` in
  `namespace <crate>` that `import Aeneas` opens (`Aeneas.Std`,
  `Result`, `ControlFlow`, `Error`), one definition per function with a
  source-location doc comment (`/-- [crate::fn]: Source: 'src/lib.rs',
  lines ... -/`).
- Charon's coverage limits (their tutorial matrix): `unreachable!`,
  shallow-init-box, nested borrows, `filter`/`collect` iterators, and
  `break`-to-outer-loop all stop a clean translation — 10/11 of their
  tutorials translate, several only partially.

## What we learned (the caveats)

1. **Translation is not verification.** Their own `STATUS.md`: all theorem
   statements in the tutorial proofs were `axiom` declarations — "Lean
   accepts them but verifies nothing about correctness"; the standalone
   Aeneas prelude used by their Lean CI was a fake (simplified types),
   and the hand-written proof files never used the real generated code.
   A passing `lake build` over that setup proves nothing.
2. **Generated code cannot be build-gated in-repo here.** The generated
   file `import Aeneas` — the real Aeneas Lean library, which we do not
   vendor (it is not part of the aeneas flake's build, only its
   translator). Our kernel gate (`lean-verify` op=build) therefore
   applies to specs we write, and the `translate` op reports the model
   plus how many `axiom`s aeneas emitted — an axiom asserts nothing and
   must be treated as an open hole, not a result.
3. **Version drift (watch item).** Their tutorial repos pin
   `leanprover/lean4:v4.28.0` (elan); upstream aeneas pins
   `leanprover/lean4:v4.31.0` in `tests/lean/lean-toolchain`; this repo
   pins 4.30.0 (`lean/lean-toolchain` + the flake's `pkgs.lean4`). The
   translator output is version-sensitive; re-check when any pin moves.
4. **Toolchain pairing.** Aeneas's test envs use the charon flake's
   rust toolchain (nightly). We verified empirically that charon works
   with the *stable* rustc carried by `devShells.aeneas` (live
   translate of a scratch crate succeeded end-to-end), so no nightly
   pairing is needed in our flake today.

## What we borrowed

- **The flake pattern: a whole toolchain as a flake input.**
  `flake.nix` carries `inputs.aeneas` (github:AeneasVerif/aeneas) as a
  single hermetic input: it locks its own nixpkgs revision (deliberate —
  OCaml 5.2), and its lock nests the charon pin, guarded upstream by
  their `charon-pin` + `check-charon-pin` check. Both binaries reach us
  through `aeneas.packages.<system>.{aeneas,charon}` (charon is
  symlinked into the aeneas package's bin/ by their flake). No
  top-level charon pin to drift.
- **A devShell, not an install.** `devShells.aeneas` (charon + aeneas +
  a rust toolchain + elan + jq + the Lean stack) is the only entry
  point for the toolchain; environment stays managed in `flake.nix`
  (docs/skill-remapped-to-os-apps.md). Enter it with
  `nix develop .#aeneas`.
- **The procedure as one self-documenting command.** Their
  two-step `charon` + `aeneas` recipe is exactly the kind of
  multi-step workflow the OS+applications model remaps to *one*
  short-lived tool op: `lean-verify` with
  `{"op":"translate","dir":"<crate-root>"}`. No SKILL.md; the op
  documents itself via `lean-verify --help`.
- **The workflow, with the guarantee kept separate.** `translate`
  produces the model; `build` (zero-sorry kernel gate) and
  `drt` (differential regression gate against the real Rust binary)
  remain the guarantees. The e2e asserts the translation output
  markers only (`import Aeneas`, the generated definition) and does
  not build-gate the generated file, for the reasons in section 2.
  The full spec-driven loop — including the "follow the proven spec to
  implement the Rust" step (`docs/lean-driven-development.md` §3.3) —
  is carried in the tool's self-doc (`lean-verify --help`, the
  `Workflow` section), not a SKILL.md.

## Using it

```
nix develop .#aeneas            # charon + aeneas on PATH
# or, without entering the shell, the tool's own fallback:
echo '{"op":"translate","dir":"mycrate"}' | target/debug/lean-verify
```

Then: state your specification as theorems over the generated
definitions, run `{"op":"build"}` (the zero-sorry kernel gate), and
finish with `{"op":"drt"}` against the real Rust binary.
