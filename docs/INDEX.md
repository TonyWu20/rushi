# Documentation Index

Authoritative entry point for any agent starting a new session.
Read this first. It tells you what exists, what works, and what is next.

## Repo state (2026-08-26)

**Working.** The Phase 1 pipeline runs end to end:
`user` → `turn.sh` → `step.sh` → `claim` → `assemble` → `model` →
`parse` → `route` → tools → `log`. Four tools are live:
`read`, `write`, `edit`, `list`. The session log uses append-only
JSONL with JSON Schema validation. The model adapter speaks the
DeepSeek Responses API with a Chat Completions fallback.

**Not yet done.** No `bash` tool (spec exists, see below).
No TUI. No Rust unit tests. No CI. No shared `core` crate
(by design, per Phase 1). No approval flow.

## Doc inventory

| Doc | Status | Last updated | Purpose |
|---|---|---|---|
| `architecture.md` | Active | 2026-08-25 | Hexagonal architecture, phase roadmap, tool contract, guardrails |
| `refinement-policy.md` | Active | 2026-08-21 | Rules for changing the harness: evidence bar, trigger thresholds, not-yet list |
| `SPEC_CONTRACT_TESTS.md` | Active | 2026-08-21 | Two-agent method: spec vs contract split, meaning test, mutation gate |
| `spec-review-criteria.md` | Active | 2026-08-26 | Consolidated review checklist for any new spec document |
| `loop-and-edit-tool.md` | Implemented | 2026-08-24 | Deep spec: loop driver, model adapter, read/write/edit tools |
| `loop-and-edit-implementation.md` | Implemented | 2026-08-24 | Implementation proposal: wire format, stage details, session layout |
| `loop-and-edit-implementation-corrections.md` | Historical | 2026-08-24 | Corrections applied after review. Read for context, not for current behavior |
| `bash-tool.md` | Spec, not yet built | 2026-08-26 | Deep spec for the `bash` tool: schema, timeout, output capping, conformance tests |
| `bash-tool-review.md` | Review | 2026-08-26 | Adversarial review of `bash-tool.md` against the review criteria. Four blocking findings. Superseded by `bash-tool-review-2.md`. |
| `bash-tool-review-2.md` | Review | 2026-08-26 | Second review pass. Finds cap-semantics gaps A1-A6 in the fixed spec and names the review's incorrect judgments. |
| `tui.md` | Proposal | 2026-08-21 | TUI design: `SessionPort`, event rendering, key bindings, daemon split |
| `tool-interface-registry-idea_from_human.md` | Draft v2 | 2026-08-25 | User idea: clap-based tool registration and auto-discovery via the daemon |

Status legend:

- **Active** — standing reference. Read it before touching the area it covers.
- **Implemented** — the described system exists in the codebase.
  The doc remains the design record. Update it when behavior changes.
- **Spec, not yet built** — approved to build. The next work item.
- **Proposal** — under consideration. Do not build until the human approves.
- **Draft** — early exploration. May change shape before it becomes a spec.
- **Historical** — records a past decision or correction.
  Do not treat as a current specification.
- **Review** — a critique of another doc. It names findings and
  fixes. It is not a specification.

## Reading order for a new session

1. This file (`INDEX.md`).
2. `architecture.md` — the shape of the system and where each binary sits.
3. `refinement-policy.md` — the rules for any change you propose.
4. The spec for the feature you are working on (see the status column).
5. `spec-review-criteria.md` — check your spec against these before implementing.
6. `SPEC_CONTRACT_TESTS.md` — how to split the implementer and tester roles.

## Maintenance rules

- Update the status column when a doc changes state.
  A doc moves from "Spec, not yet built" to "Implemented" when its
  conformance tests pass and the feature is exercised in a real session.
- Date the "Last updated" column on every edit.
- When a doc is superseded, mark it "Superseded by `<new doc>`" and
  leave it in place. Do not delete. Agents may need the history.
- Keep this file under 150 lines. If it grows past that, split the
  doc inventory into a sub-index and keep only the state summary here.
