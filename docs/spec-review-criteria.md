# Spec Review Criteria

Checklist for reviewing any spec document in this repo (tool specs, event
specs, pipeline specs, architecture changes). Derived from
`refinement-policy.md`, `SPEC_CONTRACT_TESTS.md`, and `architecture.md`.

A spec passes review when it satisfies every criterion below.

## 1. Justification

- Cites a triggering episode (session id, step id), reproducer command, or
  correctness bug.
- Passes the necessary-change checklist (P4). It unblocks a current task,
  fixes a correctness bug, removes a recurring manual step, or moves an
  invariant from human discipline to code.
- Passes the pre-test: the change cannot be achieved in config or a script
  without changing the protocol.
- Fails the pre-test and still proceeds only when the episode is recorded.

## 2. Testability (meaning test)

- Every line is a check: "given X input, observe Y output."
- No line survives that a tester cannot turn into a test without
  clarification.
- No implementation words: store, cache, loop, index, hash, database,
  algorithm. Rewrite as input-to-output rules.
- Edge cases are enumerated: empty input, max size, missing fields, invalid
  values, duplicates, zero, negative values, timeout.
- States and transitions are named. States are observable. Internal
  mechanisms are not stated.
- A mutation gate is defined: removing the target behavior makes at least
  one conformance test fail.

## 3. Contract precision

- The contract (boundary) is separable from the spec (whole task).
  The contract names entry points, boundary types, and behavior rules in
  input-to-output form.
- Tool specs conform to the tool contract (architecture.md section 3).
  Input: one JSON object on stdin. Output: one JSON object on stdout with a
  `text` field. Stderr is for diagnostics only. Exit code 0 means success.
- Event specs conform to the event contract (architecture.md section 5.2):
  one event per line, versioned envelope (`v`, `type`, `ts`), no embedded
  newlines.
- Every input field is typed. Every output field is typed. No
  "some metadata" or "various fields."
- The JSON Schema (for events) or TOML manifest (for tools) is included
  or referenced.

## 4. Minimalism and scope

- No speculative features. Every section traces back to the triggering
  episode.
- Deferred items are named explicitly in a "What this does not do" or
  "Deferred" section.
- Rule of three is respected: no new endpoint, flag, or binary without a
  third real caller.
- No versioning for imagined futures.
- The spec adds the minimum surface that unblocks the episode.

## 5. Boundary and phase alignment

- The change respects the hexagonal split: core owns what, adapters own how.
- The change is placed in the correct phase (1 through 4 in
  architecture.md section 6).
- No decision logic leaks into the wrong layer.
  The TUI does not validate tools.
  Tools do not decide policy.
  Assemble does not call the model.
- Phase 1 constraint: no shared Rust crate. Shared types are JSON
  Schemas.
- The spec states which binaries are affected and which are not.

## 6. Conformance and acceptance

- Each spec section maps to at least one conformance test case.
- Tests are runnable without the full pipeline: a single tool or binary
  with fixed stdin produces the expected stdout and exit code.
- Acceptance criteria are observable: "run command X, verify output
  contains Y" rather than "the system handles Z correctly."
- The conformance test list is complete against the edge-case
  enumeration in section 2.

## 7. Reversibility and migration

- The spec states whether the change is additive or breaking.
- Old sessions containing the old format still replay without error.
- If breaking, the `v` bump is justified (P1b).
- Consumers of the changed contract are listed. Each consumer's fallback
  or migration is stated.

## 8. Format and completeness

- The spec contains these sections (omit only when genuinely not
  applicable, and state why):

| Section | Purpose |
|---|---|
| Purpose | One paragraph: what and why, citing the episode |
| Schema or contract | Typed inputs, typed outputs, JSON Schema or TOML manifest |
| Behavior | Input-to-output rules, no implementation detail |
| Failure modes | Table: condition, exit code, `is_error`, `text` content |
| Conformance tests | Table: test name, input, expected output |
| Properties | Numbered Lean-style invariants (P1, P2, ...) in input-to-output form |
| Verification | Table mapping each property to its proof and status |
| Gate | The acceptance command list that must pass for the spec to be proven |
| What this does not do | Explicit non-goals and deferrals |
| Impact | Which binaries, event types, schemas are affected |
| Migration | Additive or breaking. Old-session replay guarantee |

- No section is left as a placeholder or "TBD."
- The spec is self-contained. A reader who has not seen the triggering
  episode can understand the change from the document alone.

## 9. Consistency with existing artifacts

- New event types do not duplicate existing types (P1a condition 4).
- New tool names do not collide with existing tools under `tools/`.
- New config keys do not conflict with existing keys in `config.toml`.
- The system prompt change (if any) keeps the prefix cache-stable: no
  timestamps, PIDs, or temp paths in the added text.
- The spec does not contradict a rule in `refinement-policy.md`.

## 10. Lean spec contract (Properties, Verification, Gate)

Per `lean-driven-development.md`, every spec doc ends with three
sections. Review rejects a spec that lacks them:

- **Properties.** One numbered invariant per non-trivial guarantee.
  Each property is observable: "given X input, observe Y output."
  No property bundles two unrelated invariants.
- **Verification.** One table row per property. A `proven` row cites
  the existing test or script that discharges it. An `open` row names
  the exact blocker and what unblocks it. A property with no row is a
  rejection.
- **Gate.** The command list that constitutes acceptance. `cargo build`
  and `cargo test` plus the doc-specific conformance or e2e scripts.
  For unbuilt specs, the gate is marked blocked with the prerequisite.

The spec author runs `scripts/verify-specs.sh` before the tester pass.
A failing doc gate returns the spec to the author.

## Review procedure

1. **Freeze.** The spec author marks the doc as review-ready.
2. **Tester pass.** A second reader checks criteria 2, 3, and 6. Every
   line is either testable or rejected.
3. **Architecture pass.** A second reader (may be the same) checks
   criteria 5 and 9 against `architecture.md` and
   `refinement-policy.md`.
4. **Human pass.** The human checks intent: is this the right thing to
   build now, or is it an itch that belongs in `docs/itches.md`?

A spec that fails any criterion is not implementable. Return it to the
author with the failing criterion named.
