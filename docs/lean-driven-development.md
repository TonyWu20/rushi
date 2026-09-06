# Lean-Driven Development — Adapted Workflow

This document defines how the Lean-4 spec-driven workflow adapts to this
repo. It is the process spec: every spec doc in `docs/` must carry the
three sections it defines. The Lean kernel is the acceptance authority.
A passing gate is the guarantee. No unproven claim survives in a
checked-in spec.

## 1. Why Lean-style

The Lean-4 method (Varun Prant, "Prove It. Don't Guess It.") gives us
four properties this repo needs:

- **The specification is the source of truth.** It defines correctness.
  It is frozen before implementation. We do not weaken it to make a
  proof pass.
- **Every property is a theorem.** One invariant per named property.
  Each property is an observable input-to-output guarantee.
- **The proof is the test.** Each property maps to a concrete test or
  manual procedure. No property is "assumed true."
- **The build is the gate.** A clean build with zero unproven claims
  is the acceptance criterion. The kernel (here: `cargo build` +
  conformance scripts) re-checks every step independently.

Mapped to this repo:

| Lean concept | This repo |
|---|---|
| Spec (theorem statements) | Design docs in `docs/` |
| Property (`theorem P`) | Named invariant in a `## Properties` section |
| Proof (tactic sequence) | Conformance test, `cargo test`, e2e script |
| `sorry` / `admit` | `todo!()`, unaddressed finding, untested path |
| `lake build` (kernel check) | `cargo build` + `scripts/tool-conformance.sh` + e2e scripts |
| Clean build = guarantee | All gates green, zero open properties = feature proven |

## 2. The three required sections

Every spec doc (status `Spec, not yet built` or `Implemented`) must
end with these three sections. Review, audit, record, and investigation
docs are exempt.

### 2.1 `## Properties`

One numbered invariant per line. Format:

```
P1. <short-name>: given <input condition>, observe <output guarantee>.
P2. <short-name>: given <input condition>, observe <output guarantee>.
```

Rules:

- One property per non-trivial invariant. Do not bundle unrelated
  guarantees.
- Each property is observable: "given X input, observe Y output."
- No implementation words (store, cache, loop, index, hash, database,
  algorithm). Use input-to-output form.
- Type parameters are named. Use the most general form.
- Minimum: one property per spec. A spec with zero properties is not a
  spec; it is a design note.

### 2.2 `## Verification`

A table mapping each property to its proof. Format:

```
| P# | Property | Proof | Status |
|----|----------|-------|--------|
| P1 | <name>   | `test_name` in `path::to::module` | proven |
| P2 | <name>   | e2e scenario 3 in `scripts/foo-e2e.sh` | proven |
| P3 | <name>   | Blocked: requires Phase 2 `harness` binary | open |
```

Rules:

- Every property in `## Properties` has a row. No orphan properties.
- `proven` — the named test or script exists and passes. Cite the
  test name and file path.
- `open` — the property is not yet discharged. State the exact
  blocker and what unblocks it. An `open` row is a tracked TODO, not
  a `sorry`.
- No `sorry`, no `admit`. A property without a proof row is a
  violation. The build gate rejects it.

### 2.3 `## Gate`

The acceptance command list. All commands must exit 0 for the doc to
be marked proven. Format:

```
cargo build
cargo test
scripts/tool-conformance.sh
scripts/compact-e2e.sh
```

Rules:

- List every command that must pass. Include `cargo build` and
  `cargo test` as the base gate for all docs.
- Add doc-specific conformance or e2e scripts.
- For specs not yet built: list the gate commands that will apply.
  Mark the gate `blocked` with the prerequisite named.
- The gate is the build. A clean gate is the guarantee. No property
  is "done" until the gate passes.

## 3. Process

The workflow for any feature, mapped from the Lean-4 pipeline:

1. **Write the spec.** State the properties. Freeze the spec.
2. **Validate the spec against known inputs.** Run quick checks
   (`cargo test`, manual e2e) against known-good behavior. If the
   spec or the implementation is wrong, fix that before writing the
   proof.
3. **Implement from the spec.** The code implements the properties,
   not the other way around.
4. **Prove each property.** Write the test or script. Each test
   discharges one property.
5. **Run the gate.** `cargo build` + conformance + e2e. A clean gate
   with zero open properties is the acceptance guarantee.

## 4. Rules

- **The specification is the source of truth.** Do not weaken it to
  make a proof pass. If the spec is wrong, fix the spec first, then
  re-prove.
- **No `sorry`, no `admit`, no `todo!()`** in checked-in code that
  a spec property depends on. An unimplemented path is an `open`
  property, not a checked-in `todo!()`.
- **Run the gate after every proof edit.** A clean build is the
  acceptance gate. If the gate is red, the work is not done.
- **The kernel is the final authority.** A proof that type-checks
  (the test passes) is a guarantee. A proof that does not type-check
  (the test fails) is not a guarantee.
- **One property per invariant.** Do not bundle. One theorem, one
  proof.
- **The spec is frozen before implementation.** Changes to the spec
  during implementation require a re-review pass.

## 5. Verification script

`scripts/verify-specs.sh` checks that every in-scope spec doc has the
three sections, that every property has a verification row, and that
no doc contains a bare `sorry` or `admit` token. Run it before
pushing. It exits 0 on clean, 1 on any failure.

```bash
scripts/verify-specs.sh
```

Exit codes:

- `0` — all in-scope docs pass. Gate is clean.
- `1` — one or more docs fail. The output names the doc and the
  missing section or unproven property.

## 6. Migration

To add the three sections to an existing spec doc:

1. Read the doc. Identify every non-trivial invariant.
2. Write the `## Properties` section. Name each invariant P1, P2, …
3. For each property, identify the existing test or write the test.
   Fill the `## Verification` table.
4. Write the `## Gate` section. List the commands.
5. Run `scripts/verify-specs.sh`. Fix any missing rows.
6. Run the gate commands. All must pass.

A doc that has no properties (pure design discussion, no behavioral
invariant) may omit the three sections. State why in a one-line note
at the end: `No behavioral properties; design discussion only.`

## 7. Relationship to existing process docs

- `refinement-policy.md` — the evidence bar. A spec change must cite
  an episode. The Lean workflow adds the property-verification layer
  on top of that evidence bar.
- `spec-review-criteria.md` — the review checklist. Sections 2
  (Testability) and 6 (Conformance) already require input-to-output
  checks and conformance tests. The Lean sections make this
  mechanical: every property has a named proof.
- `SPEC_CONTRACT_TESTS.md` — the two-agent method. The tester's job
  is to write the proofs (tests) for each property. The mutation
  gate (remove the behavior, the test must fail) is the Lean-4
  "the kernel rejects the proof" step.
- `coding-conventions.md` — the standing code rules. The `bon`
  builder rule and the type-system guarantees are part of the "kernel"
  that re-checks every proof step.

## 8. Real Lean backstop

This repo has a real Lean 4 kernel check, not just the adapted
workflow above. The Lean kernel re-checks the invariants of every
formal spec under `lean/`.

- **The Lean specs.**
  - `lean/RushiSpec.lean` mirrors the resolver in
    `bin/rushi/src/setup.rs` (`classify`, `step`, `resolve`) and
    states invariants as theorems.
  - `lean/TuiStreamSpec.lean` is a forward spec for the TUI feature
    request "Stream rendering of the model response"
    (`docs/tui_feature_requests_from_human.md`). It models the
    streaming renderer state machine and proves its invariants.
  - `lean/TuiViewportSpec.lean` specifies the viewport-based
    scrollback invariants: no-flush while pinned, tail-following,
    eviction safety, chunked reachability, and bounded window size.
  All use core Lean only (no Mathlib), so the build is hermetic
  and needs no network.
- **The devShell.** `flake.nix` defines `devShells.lean`
  (Lean 4.30.0 + Z3). Enter it with
  `nix develop .#lean` from the repo root.
- **The Rust->Lean toolchain.** `flake.nix` also defines
  `devShells.aeneas` (charon + aeneas, from the hermetic
  AeneasVerif/aeneas flake input). The `lean-verify` tool's
  `translate` op runs the charon -> aeneas pipeline (Rust MIR ->
  LLBC -> pure Lean) and reports the generated model. The
  translation is the model half of the workflow only — the `build`
  kernel gate and the `drt` regression gate remain the guarantees
  (docs/aeneas-rust-to-lean.md).
- **The gate.** `scripts/lean-gate.sh` runs the Lean compiler over
  every spec in `lean/`. Exit 0 = every proof re-checked by the
  kernel. It is an optional backstop, not part of the main gate
  (`scripts/verify-specs.sh`), matching the stance in
  `docs/harness-distribution.md`.
- **The DRT regression gate.** `lean/TuiStreamDrt.lean` is a pure
  CLI executable over the frozen `TuiStreamSpec` reference renderer
  (`View`, `step`, `runResponse`, `runResponses`). The production
  mirror is `bin/tui-stream-drt` (pure-Rust, std-only). Both share a
  one-line scenario protocol and are differential-random-tested via
  `lean-verify` `op=drt` with a deterministic input generator
  (`scripts/tui-stream-drt-inputs.sh`, fixed-seed LCG, alphabet
  a-j 0-9). The `lean_exe «TuiStreamDrt»` target in `lakefile.lean`
  puts the executable inside the zero-sorry build gate, so the DRT
  model is kernel-checked alongside the theorems.
- **Pinned toolchain.** `lean/lean-toolchain` pins
  `leanprover/lean4:v4.30.0`, matching the flake's `pkgs.lean4`.
  The Lean project uses core Lean only (no Mathlib), so the build
  is hermetic and needs no network.

Running the backstop:

```bash
scripts/lean-gate.sh
# or, step by step:
nix develop .#lean
cd lean && lean RushiSpec.lean TuiStreamSpec.lean TuiViewportSpec.lean
```

