# Documentation Index

Authoritative entry point for any agent starting a new session.
Read this first. It tells you what exists, what works, and what is next.

## Repo state (2026-09-08)

**Working.** The Phase 2 pipeline runs end to end:
`rushi` (loop) → `claim` → `assemble` → `model` →
`parse` → `route` → tools → `log`. Base tools live in
`tools/`: `read`, `write`, `edit`, `list`, `bash`. The session log
uses append-only JSONL with JSON Schema validation. The model adapter
speaks the DeepSeek Responses API with a Chat Completions fallback.

**Post-split.** At the 2026-09-08 split this repo is the kernel
(loop, base tools, extension host, hook ABI, distribution). The TUI
and UI-extension layers moved to `../rushi-tui`; goal tools, goal
hooks, and `lean-verify` moved to `../rushi-exts`. Their docs live in
those repos. The kernel ships a default config with no sibling
dependency; `config-exts.example.toml` shows how to re-enable the
exts wiring.

**Not yet done.** No CI. No shared `core` crate (by design, per
Phase 3). The `LogLine` and validator duplication itches are closed:
both now live once in `crates/rushi/` (`notes/itches.md`).

## Doc inventory

| Doc                                           | Status              | Last updated | Purpose                                                                                                                                                    |
| --------------------------------------------- | ------------------- | ------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `architecture.md`                             | Active              | 2026-09-04   | Hexagonal architecture, phase roadmap, tool contract, guardrails. Lean sections: Properties P1-P8, Verification, Gate                                                                               |
| `lean-driven-development.md`                  | Active              | 2026-09-04   | Lean-driven development workflow adapted to this repo: the Properties / Verification / Gate contract for spec docs, the no-unproven-claim rule, the build gate as acceptance authority, and the `scripts/verify-specs.sh` doc gate |
| `auto-compact-plan.md`                        | Implemented       | 2026-09-16   | In-session auto-compaction: the `bin/compact` binary, the threshold trigger, the overflow and length-stop recovery in `step.sh`, the `context_exhausted` last-resort. Ported from the pi 0.84.2 compaction source. Reviewed, audited in three passes, and shipped: 62 e2e scenarios in `scripts/compact-e2e.sh`. The `compact_trigger_base` knob adds the pi-parity `context_budget` trigger base; the stale post-compact reading guard (sections 9, 9.5) keeps the last-resort path recoverable; the `context_budget` base unclamps the wire budget to the model window (section 9.6); reserve size controls next-response headroom and `model_timeout_s` bounds a stalled model call (section 9.7); the hard-trim backstop mechanically cuts the request to fit when the LLM compact lags (section 9.8); the parse length-stop gate lets truncated tool calls recover instead of hard-failing (section 9.9) |
| `auto-compact-plan-review.md`                 | Review              | 2026-09-02   | Design review of `auto-compact-plan.md`: every claim verified against the repo code, config, and the pi source                                              |
| `auto-compact-plan-audit.md`                  | Review              | 2026-09-02   | Second audit: re-derives each accepted cut and safety argument from the code, not the plan wording                                                        |
| `auto-compact-plan-audit-2.md`                | Review              | 2026-09-02   | Third audit: B1-B6, all accepted. The fixes land in the plan sections 4-7                                                                                 |
| `refinement-policy.md`                        | Active              | 2026-08-21   | Rules for changing the harness: evidence bar, trigger thresholds, not-yet list                                                                             |
| `coding-conventions.md`                       | Active              | 2026-09-03   | Standing code rules for the Rust in this repo: the `bon` builder for any function with 8+ parameters, its `Option` and lifetime rules, the `#[allow]` ban on `too_many_arguments` |
| `SPEC_CONTRACT_TESTS.md`                      | Active              | 2026-08-21   | Two-agent method: spec vs contract split, meaning test, mutation gate                                                                                      |
| `spec-review-criteria.md`                     | Active              | 2026-08-26   | Consolidated review checklist for any new spec document                                                                                                    |
| `loop-and-edit-tool.md`                       | Implemented         | 2026-08-24   | Deep spec: loop driver, model adapter, read/write/edit tools                                                                                               |
| `loop-and-edit-implementation.md`             | Implemented         | 2026-08-24   | Implementation proposal: wire format, stage details, session layout                                                                                        |
| `loop-and-edit-implementation-corrections.md` | Historical          | 2026-08-24   | Corrections applied after review. Read for context, not for current behavior                                                                               |
| `phase-2-readiness.md`                        | Review            | 2026-09-03   | Readiness review for `architecture.md` Phase 2 (the loop into a Rust binary): the loop behavior list, the stability evidence, ten findings (R1-R10), the verdict with six conditions |
| `phase-2-plan.md`                             | Implemented       | 2026-09-13   | The Phase 2 spec and plan: the `harness` loop binary (`run`/`step`, the session lock, the cancellation handles, the ported retry and compact-branch semantics), the `harness-common` utility crate, the approval round-trip (`awaiting_approval` state, two new event types), the `run.idle` goal-continuation window, the `model.before` transform window, tool sidecar boundary, five stages with gates |
| `phase-2-crate-research.md`                   | Proposal          | 2026-09-07   | Crate research for the Phase 2 core: maps the shell glue of `step.sh`/`turn.sh` and the bash-tool command surface to Rust crates (`grep` family, `jaq`, `jsonschema`, `toml`, `regex`, `chrono`), with a placement map and the deferred list |
| `handoff-strategy.md`                           | Implemented | 2026-09-13   | The handoff context strategy: replaces the in-session last-resort compact with a summary handoff that seeds and auto-starts a new session. The `SessionStore` port, the handoff doc format (both session dirs), and the log-index convention. Strategy selection is a hook registration on the `exhausted.handle` window (see `loop-lifecycle-hooks.md`) |
| `loop-lifecycle-hooks.md`                        | Implemented | 2026-09-13   | The lifecycle-window hook design: fine-grained control points on the loop (`session.start/end`, `step.start/end`, `model.before/after`, `compact.before/after`, `overflow.resolve`, `exhausted.handle`, `tool.before/after`, `run.idle`), a Unix-style hook ABI (command on a path, JSON stdin/stdout, exit-code decision with `block`/`approve` on `tool.before`, `transform` on `model.before`, and `continue` on `run.idle`), the two built-in overflow strategies as swappable hook registrations, and the Unix-philosophy reframe of "hooks" as applications on a path. Built: `crates/rushi/src/hooks.rs`, `bin/hook-compact`, `bin/hook-handoff` |
| `bash-tool.md`                                | Implemented   | 2026-08-26   | Deep spec for the `bash` tool: schema, timeout, output capping, conformance tests                                                                          |
| `bash-tool-review.md`                         | Review              | 2026-08-26   | Adversarial review of `bash-tool.md` against the review criteria. Four blocking findings. Superseded by `bash-tool-review-2.md`.                           |
| `bash-tool-review-2.md`                       | Review              | 2026-08-26   | Second review pass. Finds cap-semantics gaps A1-A6 in the fixed spec and names the review's incorrect judgments.                                           |
| `empty-turn-root-cause.md`                    | Implemented         | 2026-08-27   | Root cause and fix for `model returned an empty turn after retries`: 30 s reqwest cap on streaming SSE, silent error swallow, loop-guard misclassification |
| `tool-interface-registry-idea_from_human.md`  | Draft v2            | 2026-08-25   | User idea: clap-based tool registration and auto-discovery via the daemon                                                                                  |
| `ft-005-logline.md`                           | Implemented         | 2026-08-29   | Record for FT-005: motivation from the ruxe type-level-disjointness post, two-writer interleave analysis, LogLine capability design and tests             |
| `rewind-fork-design.md`                        | Implemented       | 2026-09-08   | Session rewind and fork (pi `/tree` style): the `rewind` event on the append-only log, the active-path mask in `bin/assemble` (the recursive computation that masks abandoned branches at every nesting depth), the rewind-aware `bin/claim` state, and the review of the proposed checkpoint+mask design (I1-I12). Lean backstop: `lean/RewindSpec.lean` + DRT gate in `scripts/rewind-drt-e2e.sh`. The TUI picker stage lives in `rushi-tui` |
| `skill-remapped-to-os-apps.md`                  | Proposal            | 2026-09-02   | Feature request: the OS + Applications split. Base distribution (kernel: loop core + base tools + growth machinery; default-swappable `tui` front-end, the WM tier). A tool registers by being on the agent-visible PATH and self-documents via `--help` (no `SKILL.md`; a procedure is a script). Discovery is an on-demand `tools --list` catalog (reads `tool.toml` `description`), never prompt content. First application is `tui-capture` |
| `pi-extension-port-investigation.md`            | Investigation       | 2026-09-13   | Portability assessment of the `pi-config` extension set (pi-goal, rpiv-ask-user-question, pi-fff, agent-simple-english, no-find-grep, no-bare-python, pi-lynx, pi-terminal-browser, pi-automode) onto the harness: tool/hook/UI/loop mapping, difficulty verdict, blockers, and suggested order. pi-goal verdict updated to "Ready" (2026-09-13). |
| `pi-goal-readiness.md`                           | Investigation     | 2026-09-13   | Readiness assessment for porting `pi-goal` as a harness extension: all three loop seams (`run.idle`, `compact.before`, `model.before` transform) are built and gated; the remaining work is application-level (goal tools, hook binaries, state file); gaps G2-G4 with workarounds |
| `goal-rule-reinject.md`                           | Proposal          | 2026-09-08   | Supersedes the idle-rules-only proposal: inject the goal block (goal_id, objective, rules, trust boundary, goal-tool hint) into `request.instructions` once per goal lifetime via a byte-stable `model.before` transform (append on open, strip on close). The `goal` tool schema is invisible to the agent in every state. `goal_complete` and `goal_blocked` are present only while a goal is open. `goal resume` targets blocked goals only. `run.idle` (hook-goal-idle) keeps the idle nudge. Cache tradeoff: two re-prefills per goal (set + close), for no per-turn re-acknowledgement. Superseded proposal archived in git history (2026-09-07) |
| `system-prompt-generation.md`                    | Proposal          | 2026-09-13   | How the system prompt is built: the kernel starts from the user config `[system_prompt]` field, adds a generated tool list and cwd line, then extension hooks add or remove named fragments (e.g. goal block). The kernel joins all parts into `request.instructions`. Supersedes the mechanism in `goal-rule-reinject.md` (keeps D1-D3 and cache analysis). |
| `deepseek-harness-compaction-research.md`       | Investigation       | 2026-09-04   | Cross-codebase research: how the dsh compaction system keeps the session log append-only while replacing surface ranges with summary checkpoints, and how the stable request prefix and prefix-aligned summarization call maximize KV cache prefix hits |
| `user-message-editing.md`                      | Implemented | 2026-09-11   | Spec for recalling and editing pending user messages: `Alt+Up` bulk recall and the `:edit-queue` palette entry, the `user_message_retract` event and optional `id` field on `user_message`, the loop-side skip rules in `bin/claim` and `bin/assemble`, and the idle-on-retract behavior. Kernel-side retract event and skip rules are in the kernel; the TUI side is in `rushi-tui` |
| `harness-distribution.md`                        | Implemented       | 2026-09-05   | Distribution model: `rushi setup`, `rushi.toml` manifest, `rushi.lock` pinning, global install + per-project registration, `install.sh` plain path, Nix flake primary path. Properties P1-P10 with verification table and gate |
| `aeneas-rust-to-lean.md`                       | Investigation       | 2026-09-04   | Aeneas Rust-to-Lean toolchain: charon MIR extraction + aeneas translation, the functional-core/imperative-shell pattern shared with this harness |
| `better-ui-root-cause.md`                        | Historical          | 2026-08-27   | Root-cause analysis for TUI responsiveness issues (pre-split; TUI now in `rushi-tui`) |
| `failure-tracking.md`                           | Active              | 2026-08-29   | How to track and report harness failures: the `error` event, `hook.<window>.error` markers, and the fenced-failure report format |
| `goal-ui_feedback_from_human.md`                | Historical          | 2026-09-05   | Feedback notes on the goal UX redesign (pre-split; goal-ux.md now lives in `rushi-tui/docs/`) |
| `handoff-compaction-request-format.md`          | Active              | 2026-09-06   | Wire format for the handoff/compact request: the summary-call shape, token budget, and the boundary summary fallback |
| `tool-log-design_from_human.md`                  | Active              | 2026-08-25   | Per-session tool log design: full output in `tools.jsonl`, slim index in the session log |
| `vim-editor-design.md`                           | Implemented         | 2026-09-02   | Design for the TUI's built-in vim-like editor: keymap, mode machine, register model. TUI code lives in `rushi-tui` |

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
- **Investigation** — a research doc that surveys an external codebase or
  design space to inform a decision. Not a spec; not a review of a doc.
- **Plan** — a staged work breakdown of an approved spec.
  Work the stages in order. Update the status as stages land.

## Sibling repos

The TUI and UI-extension layers live in `../rushi-tui`. Goal tools,
goal hooks, and `lean-verify` live in `../rushi-exts`. Their docs
(`tui.md`, `ui-extension.md`, `goal-ux.md`, etc.) live in those repos.
The kernel does not depend on them at build time. `config-exts.example.toml`
shows how to wire the kernel to the exts tree for development.

## Reading order for a new session

1. This file (`INDEX.md`).
2. `architecture.md` — the shape of the system and where each binary sits.
3. `refinement-policy.md` — the rules for any change you propose.
4. `lean-driven-development.md` — the spec-driven workflow: properties
   before code, a proof per property, the build gate as acceptance.
5. `coding-conventions.md` — the standing code rules for the Rust.
6. The spec for the feature you are working on (see the status
   column). Every spec doc ends with its Properties,
   Verification, and Gate sections.
7. `spec-review-criteria.md` — check your spec against these before implementing.
8. `SPEC_CONTRACT_TESTS.md` — how to split the implementer and tester roles.

## The Lean gate

The acceptance gate for a spec is the `## Gate` section at the end of
the spec doc. The doc gate is `scripts/verify-specs.sh` (structure and
cross-reference checks on the docs). The code gate is the command list
in each Gate section: `cargo build`, `cargo test`, and the named e2e
scripts. A clean gate with zero open properties is the guarantee.

Lean backstops live in `lean/`: `RushiSpec.lean` mirrors the `rushi setup`
resolver; `RewindSpec.lean` mirrors the fork-recursion active-path
computation; `RewindDrt.lean` is the DRT model executable.
`scripts/lean-gate.sh` runs `lake build` over all three and enforces
zero `sorry`. The Lean backstop is optional; the house gate remains
the conformance and e2e scripts. See `docs/lean-driven-development.md` §8.

- `scripts/verify-specs.sh` — run before pushing doc changes. Exit 0
  is clean; exit 1 names the failing doc and section.
- `scripts/rewind-drt-e2e.sh` — differential-random-test gate comparing
  the Lean `RewindDrt` model against `bin/rewind-drt` on generated
  inputs. Run after any change to `crates/rushi/src/rewind.rs` or
  `lean/RewindSpec.lean`.

## Maintenance rules

- Update the status column when a doc changes state.
  A doc moves from "Spec, not yet built" to "Implemented" when its
  conformance tests pass and the feature is exercised in a real session.
- Date the "Last updated" column on every edit.
- When a doc is superseded, mark it "Superseded by `<new doc>`" and
  leave it in place. Do not delete. Agents may need the history.
- Keep this file under 150 lines. If it grows past that, split the
  doc inventory into a sub-index and keep only the state summary here.
- New spec docs end with the three Lean sections (Properties,
  Verification, Gate) per `lean-driven-development.md`. Run
  `scripts/verify-specs.sh` before pushing. The property rows close
  one by one as their proofs land; the gate command list must pass
  before a feature is marked proven.
