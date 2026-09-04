# Spec, Contract, Tests — The Two-Agent Method

Use this as a skill when you split a task into an implementer and a tester.

## The split

- The implementer writes code. It never writes tests.
- The tester writes and runs tests. It uses only the public interface.
- Neither agent reads the other's work.

## Spec vs contract

- Contract: the boundary. It names the entry points, the boundary types, and the behavior rules in input-to-output form. No internal detail. The signature is the contract. The body is the implementation.
- Spec: the whole task. The contract is its precise core. The spec adds intent, constraints, and out-of-scope items.

## The meaning test

A spec line is good only when the tester can turn it into a test. An unwritable line is an idea, not a spec. It returns to the spec author. Zero clarifications is the target.

## Six rules for a code-free spec

- Observable behavior only. Ban store, cache, loop, index, hash, database, algorithm. Rewrite as input-to-output rules.
- Use given-and-returns rows. A table of examples is the strongest form.
- Enumerate edge cases: empty, max size, missing fields, invalid values, duplicates, zero, negatives, timeouts.
- Name states and transitions. States are observable. Implementation is not.
- State every requirement as a check. If you cannot say "observe X, verify Y," the line does not belong.
- Use domain values, not placeholders.

## The pipeline (Rust order)

Rust changes the test-first order. The compiler is the first test. A test against a missing interface does not compile. The interface must compile before the tests exist.

- design spec and contract
- build the interface skeleton, compile it clean (implementer)
- write tests against the compiled interface (tester)
- implement the behavior until the tests pass (implementer)
- audit

The compiler gates the interface. The tests gate the behavior. A red test means behavior, never plumbing.

## Anti-placebo gates

A blind tester can write placebo tests. Two gates stop that.

- The compiler gate. The interface must compile before the tests
  exist. A test against a missing interface does not compile. The
  compiler is the first red gate.
- The mutation gate. Remove the target behavior. The suite must
  fail. A test that still passes is a placebo. The mutation gate is
  the primary gate for Rust.
- The citation gate. A test changes only with a written spec
  reference. A change without a citation is invalid.

Red before green does not fit Rust. In a dynamic language, a test
against an empty stub proves the test touches the interface. In Rust,
a stub must be a full trait implementation before the test compiles.

A red-before-green stub is a second implementation, not a gate. The
compiler runs before any test. There is no empty run. The mutation
gate covers it.

A behavior may resist observation through the public interface. The
fsync-to-platter is one example. No in-process test tells the page
cache from the disk. The spec then states the limitation. The
mutation gate runs as a scripted CI step. The step removes the
behavior, runs the suite, and requires failure.

## Two hard rules

- Grade the tester on mutation survival, never on "tests pass."
- Give the implementer no write access to the test files.

## The external-fact rule

Assumptions about the outside world are empirical. Only real execution verifies them. No review or gate does. Run against the real system as a required audit stage. Prefer "arrives at some point" over "arrives first."

## The audit

The spec author never audits the spec. The tester audits while it writes tests. A third agent audits the three artifacts. The human audits intent. Order: freeze, tester, failing-test, final.

## When it helps

Only large or critical tasks justify the cost. Small tasks do not.

## The Lean stage (spec properties to gate)

The two-agent method feeds the Lean-driven workflow defined in
`lean-driven-development.md`. The pipeline runs in this order:

1. The spec freezes its properties (the P1...Pn invariants).
2. The tester writes one proof per property. Each proof is a test
   that discharges exactly one property. No proof may leave its
   property open without a named blocker.
3. The implementer builds the interface first. The compiler is the
   first gate. A test against a missing interface does not compile.
4. The gate runs: `cargo build`, `cargo test`, and the e2e scripts
   named in the spec's `## Gate` section. A clean gate with zero open
   properties is the acceptance guarantee.
5. The mutation gate still applies: removing the target behavior must
   make at least one proof fail. A proof that survives mutation is a
   placebo. Reject it.
