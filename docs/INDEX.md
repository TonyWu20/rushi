# Documentation Index

Authoritative entry point for any agent starting a new session.
Read this first. It tells you what exists, what works, and what is next.

## Repo state (2026-08-28)

**Working.** The Phase 1 pipeline runs end to end:
`user` → `turn.sh` → `step.sh` → `claim` → `assemble` → `model` →
`parse` → `route` → tools → `log`. Five tools are live:
`read`, `write`, `edit`, `list`, `bash`. The session log uses
append-only JSONL with JSON Schema validation. The model adapter
speaks the DeepSeek Responses API with a Chat Completions fallback.

**New: the TUI.** `bin/tui` renders the session log, appends
`user_message`, `approval`, and `cancel` events, and supervises the
opaque `[loop]` command from `config.toml`. See `tui.md` §13 and
`tui-plan.html`.

**Next: the UI extension mechanism.** Out-of-process UI extensions
over a JSONL boundary, host-owned load order. Design in
`ui-extension.md`, staged work in `ui-extension-plan.md`.

**Not yet done.** No CI. No shared `core` crate (by design, per
Phase 1). The schema validator is now a third copy
(`notes/itches.md`). No UI extension mechanism (designed, staged,
not built).

## Doc inventory

| Doc                                           | Status              | Last updated | Purpose                                                                                                                                                    |
| --------------------------------------------- | ------------------- | ------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `architecture.md`                             | Active              | 2026-08-25   | Hexagonal architecture, phase roadmap, tool contract, guardrails                                                                                           |
| `auto-compact-plan.md`                        | Spec, not yet built | 2026-09-02   | Implementation plan for proactive auto-compact and loop continuation. Ported from the pi 0.84.2 compaction source. Reviewed and audited in three passes |
| `auto-compact-plan-review.md`                 | Review              | 2026-09-02   | Design review of `auto-compact-plan.md`: every claim verified against the repo code, config, and the pi source                                              |
| `auto-compact-plan-audit.md`                  | Review              | 2026-09-02   | Second audit: re-derives each accepted cut and safety argument from the code, not the plan wording                                                        |
| `auto-compact-plan-audit-2.md`                | Review              | 2026-09-02   | Third audit: B1-B6, all accepted. The fixes land in the plan sections 4-7                                                                                 |
| `refinement-policy.md`                        | Active              | 2026-08-21   | Rules for changing the harness: evidence bar, trigger thresholds, not-yet list                                                                             |
| `SPEC_CONTRACT_TESTS.md`                      | Active              | 2026-08-21   | Two-agent method: spec vs contract split, meaning test, mutation gate                                                                                      |
| `spec-review-criteria.md`                     | Active              | 2026-08-26   | Consolidated review checklist for any new spec document                                                                                                    |
| `loop-and-edit-tool.md`                       | Implemented         | 2026-08-24   | Deep spec: loop driver, model adapter, read/write/edit tools                                                                                               |
| `loop-and-edit-implementation.md`             | Implemented         | 2026-08-24   | Implementation proposal: wire format, stage details, session layout                                                                                        |
| `loop-and-edit-implementation-corrections.md` | Historical          | 2026-08-24   | Corrections applied after review. Read for context, not for current behavior                                                                               |
| `bash-tool.md`                                | Implemented   | 2026-08-26   | Deep spec for the `bash` tool: schema, timeout, output capping, conformance tests                                                                          |
| `bash-tool-review.md`                         | Review              | 2026-08-26   | Adversarial review of `bash-tool.md` against the review criteria. Four blocking findings. Superseded by `bash-tool-review-2.md`.                           |
| `bash-tool-review-2.md`                       | Review              | 2026-08-26   | Second review pass. Finds cap-semantics gaps A1-A6 in the fixed spec and names the review's incorrect judgments.                                           |
| `tui.md`                                      | Implemented         | 2026-08-27   | TUI design: `SessionPort`, event rendering, key bindings, daemon split, §13 implementation record                                                          |
| `tui-plan.html`                               | Implemented         | 2026-08-27   | Visual implementation plan and verification record for `tui.md`: architecture, event flow, loop lifecycle, tailer, layout, keys, dependencies, deviations  |
| `empty-turn-root-cause.md`                    | Implemented         | 2026-08-27   | Root cause and fix for `model returned an empty turn after retries`: 30 s reqwest cap on streaming SSE, silent error swallow, loop-guard misclassification |
| `tui_extension_design_questions_from_human.md` | Draft               | 2026-08-28   | Human design question: decoupling UI extensions from the Rust TUI. Seeds `ui-extension.md`                                                                 |
| `tool-interface-registry-idea_from_human.md`  | Draft v2            | 2026-08-25   | User idea: clap-based tool registration and auto-discovery via the daemon                                                                                  |
| `tui_feature_requests_from_human.md`          | Active              | 2026-09-02   | Slim index of user feature requests on the tui. One line per request. Each item links its detail doc                                                            |
| `tui-model-wait-indicator.md`                 | Implemented       | 2026-09-01   | Spec for the loop-phase indicator: the loop publishes `loop_phase` (`wait`/`tools`) as an `ext_status` event, the TUI renders it as the title bit and the working row above the input box |
| `tui-color-tones.md`                          | Spec, not yet built | 2026-09-02   | Request doc for the gray-abuse defect and the `Read` highlighting. Partial: the capability-aware tones shipped (commit `f652b89`). The reference renderer and the `Read` highlighting stay open |
| `tui-malformed-line-flash.md`                 | Implemented       | 2026-09-02   | Root cause and shipped fix for the transient `[malformed log line]` flash on live loops. `read_events` drops the in-progress tail, the tailer holds partial lines |
| `tui-pending-user-messages.md`                | Plan              | 2026-09-02   | Staged plan for the pending `user_message` lists. Stage 1 (TUI steering block) shipped in `fc51f71`. Stage 2 (the loop-side `steer`/`follow` split) stays open  |
| `tui-statusline-powerline.md`                 | Implemented       | 2026-09-02   | Record for the statusline powerline footer: rounded Nerd Font pills (`U+E0B4`/`U+E0B6`), per-span hex colors, overflow drops the lowest-priority pills |
| `tui-thinking-block.md`                       | Spec, not yet built | 2026-09-02   | Request doc for the thinking (reasoning) block. Partial: the capture into the log shipped in `61cde02`. The TUI render, the toggle, and the effort control stay open |
| `tui-thinking-level-input-box.md`             | Implemented       | 2026-09-02   | Record for docs/tui.md section 7.2: the loop publishes `model_thinking` (the resolved `reasoning_effort` mapped to 0-4, via `bin/model --describe`), the TUI colors the input-area border from the last value |
| `tui-tool-display-port.md`                    | Spec, not yet built | 2026-09-02   | Request doc for the full `pi-tool-display` style port: the lighter result box, the fold/expand control, per-tool limits, presets, and config                    |
| `tui-tool-result-truncation.md`               | Spec, not yet built | 2026-09-02   | Request doc for truncating `Read`/`Write` results and the `Edit` diff, plus the six "never truncate" comment rescopes                                        |
| `ui-extension.md`                             | Spec, not yet built | 2026-08-28   | Out-of-process UI extension design: JSONL host, five capabilities, host-owned load order. Review round 1 folded in                                        |
| `tui-extension-design-review.md`              | Review              | 2026-08-28   | Round-1 critique of `ui-extension.md`. All findings folded into the spec                                                                                    |
| `ui-extension-plan.md`                        | Plan                | 2026-08-28   | Staged work breakdown for `ui-extension.md`: stages 0-4, acceptance per stage                                                                                |
| `ft-005-logline.md`                           | Implemented         | 2026-08-29   | Record for FT-005: motivation from the ruxe type-level-disjointness post, two-writer interleave analysis, LogLine capability design and tests             |
| `skill-system.md`                               | Proposal            | 2026-09-02   | Feature request: the OS + Applications split. Base distribution (kernel: loop core + base tools + growth machinery; default-swappable `tui` front-end, the WM tier). A tool registers by being on the agent-visible PATH and self-documents via `--help` (no `SKILL.md`; a procedure is a script). Discovery is an on-demand `tools --list` catalog (reads `tool.toml` `description`), never prompt content. First application is `tui-capture` |

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
- **Plan** — a staged work breakdown of an approved spec.
  Work the stages in order. Update the status as stages land.

## Reading order for a new session

1. This file (`INDEX.md`).
2. `architecture.md` — the shape of the system and where each binary sits.
3. `refinement-policy.md` — the rules for any change you propose.
4. The spec for the feature you are working on (see the status
   column). UI extension work reads `ui-extension.md` with
   `ui-extension-plan.md`.
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
