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
| `handoff-strategy.md`                           | Spec, not yet built | 2026-09-08   | The handoff context strategy: replaces the in-session last-resort compact with a summary handoff that seeds and auto-starts a new session. The `SessionStore` port, the handoff doc format (both session dirs), and the log-index convention. Strategy selection is a hook registration on the `exhausted.handle` window (see `loop-lifecycle-hooks.md`) |
| `loop-lifecycle-hooks.md`                        | Spec, not yet built | 2026-09-13   | The lifecycle-window hook design: fine-grained control points on the loop (`session.start/end`, `step.start/end`, `model.before/after`, `compact.before/after`, `overflow.resolve`, `exhausted.handle`, `tool.before/after`, `run.idle`), a Unix-style hook ABI (command on a path, JSON stdin/stdout, exit-code decision with `block`/`approve` on `tool.before`, `transform` on `model.before`, and `continue` on `run.idle`), the two built-in overflow strategies as swappable hook registrations, and the Unix-philosophy reframe of "hooks" as applications on a path |
| `bash-tool.md`                                | Implemented   | 2026-08-26   | Deep spec for the `bash` tool: schema, timeout, output capping, conformance tests                                                                          |
| `bash-tool-review.md`                         | Review              | 2026-08-26   | Adversarial review of `bash-tool.md` against the review criteria. Four blocking findings. Superseded by `bash-tool-review-2.md`.                           |
| `bash-tool-review-2.md`                       | Review              | 2026-08-26   | Second review pass. Finds cap-semantics gaps A1-A6 in the fixed spec and names the review's incorrect judgments.                                           |
| `tui.md`                                      | Implemented         | 2026-08-27   | TUI design: `SessionPort`, event rendering, key bindings, daemon split, §13 implementation record                                                          |
| `tui-plan.html`                               | Implemented         | 2026-08-27   | Visual implementation plan and verification record for `tui.md`: architecture, event flow, loop lifecycle, tailer, layout, keys, dependencies, deviations  |
| `empty-turn-root-cause.md`                    | Implemented         | 2026-08-27   | Root cause and fix for `model returned an empty turn after retries`: 30 s reqwest cap on streaming SSE, silent error swallow, loop-guard misclassification |
| `tui_extension_design_questions_from_human.md` | Draft               | 2026-08-28   | Human design question: decoupling UI extensions from the Rust TUI. Seeds `ui-extension.md`                                                                 |
| `tool-interface-registry-idea_from_human.md`  | Draft v2            | 2026-08-25   | User idea: clap-based tool registration and auto-discovery via the daemon                                                                                  |
| `tui_feature_requests_from_human.md`          | Active              | 2026-09-08   | Slim index of user feature requests on the tui. One line per request. Each item links its detail doc                                                            |
| `tui-model-wait-indicator.md`                 | Implemented       | 2026-09-01   | Spec for the loop-phase indicator: the loop publishes `loop_phase` (`wait`/`tools`) as an `ext_status` event, the TUI renders it as the title bit and the working row above the input box |
| `tui-color-scheme.md`                         | Spec, not yet built | 2026-09-02   | Request doc for the custom TUI color scheme: `catppuccin macchiato` as the first internal scheme, a user-supplied custom scheme, capability lowering on every scheme  |
| `tui-color-tones.md`                          | Spec, not yet built | 2026-09-02   | Request doc for the gray-abuse defect and the `Read` highlighting. Partial: the capability-aware tones shipped (commit `f652b89`). The reference renderer and the `Read` highlighting stay open |
| `tui-color-pi-alignment.md`                   | Implemented         | 2026-09-05   | Case-by-case comparison of this TUI's color scheme against the `pi` TUI and the `pi-tool-display` extension: the 38-role table, the pi `syntax*` token mapping, the rebased `catppuccin macchiato` scheme and built-in palette, the frame and statusline alignment        |
| `tui-command-palette.md`                       | Spec, not yet built | 2026-09-11   | Spec for the `:` command palette in normal mode: a floating two-pane window reusing the picker's fuzzy ranker and layout, the v1 command set (toggles, effort setter, session buffers b/bn/bp, new-session, edit-queue, quit), and the extension `commands` cap with an `invoke` op for extension-provided commands   |
| `tui-conversation-browsing.md`                 | Spec, not yet built | 2026-09-05   | Spec for the 2026-09-05 requests: the right-edge position bar over the transcript, and the conversation browse mode. The double-`s` entry gate, the neovim-matched motions, the hybrid number gutter, `gg`/`G`. The regex log search discussion is stage 2 |
| `tui-file-picker.md`                           | Implemented         | 2026-09-06   | Spec for the `@` file picker and the reusable completion window widget: fuzzy from day 0 via `frizbee`, the `picker/` middle layer (items, matcher, state, render, preview), the `@` trigger, the settled floating display with a file content preview pane, the `Ctrl+I`/`Tab` file-scope cycle (P9: default → git-ignored → also hidden → default; `Ctrl+I` and `Tab` are the same terminal key, byte 0x09), and the phased build plan |
| `tui-file-picker-research.md`                  | Active              | 2026-09-08   | Library research for the file picker: `frizbee` (matcher API and scoring), `television` (Rust, background-worker snapshot model), `telescope.nvim` (floating-window UX and layout modes) |
| `tui-conversation-browsing-review.md`          | Review              | 2026-09-05   | Spec review of `tui-conversation-browsing.md`: no YAGNI abuse, no harmful scope shortcutting; five findings (F1 the `s` disarm description is wrong against `vim_editor.rs`, F2 the wrap-cache pointers are off, F3 the transcript-cap gap, F4-F5 wording). Fixed in commit `8c7cb6a` |
| `tui-malformed-line-flash.md`                 | Implemented       | 2026-09-02   | Root cause and shipped fix for the transient `[malformed log line]` flash on live loops. `read_events` drops the in-progress tail, the tailer holds partial lines |
| `tui-markdown-render.md`                      | Spec, not yet built | 2026-09-02   | Request doc for marker-free markdown rendering in user and assistant messages: styled text, drawn grid tables, the `|` pipes out of the output                  |
| `tui-pending-user-messages.md`                | Implemented       | 2026-09-03   | Staged plan for the pending `user_message` lists. Stage 1 (TUI steering block) shipped in `fc51f71`. Stage 2 (the loop-side `steer`/`follow` split) shipped in `cef8496` |
| `tui-statusline-powerline.md`                 | Implemented       | 2026-09-02   | Record for the statusline powerline footer: rounded Nerd Font pills (`U+E0B4`/`U+E0B6`), per-span hex colors, overflow drops the lowest-priority pills |
| `tui-thinking-block.md`                       | Spec, not yet built | 2026-09-02   | Request doc for the thinking (reasoning) block. Partial: the capture into the log shipped in `61cde02`. The TUI render, the toggle, and the effort control stay open |
| `tui-thinking-level-input-box.md`             | Implemented       | 2026-09-02   | Record for docs/tui.md section 7.2: the loop publishes `model_thinking` (the resolved `reasoning_effort` mapped to 0-4, via `bin/model --describe`), the TUI colors the input-area border from the last value |
| `tui-streaming-response.md`                   | Implemented       | 2026-09-05   | Spec for live streaming of the model response to the TUI: a session-local `.model-stream` file the model binary writes to during the SSE call, the TUI polls it each frame and renders a growing live block, cleared when the final `assistant_message` lands. Depends on Phase 2 (`harness` binary). No new log event type |
| `tui-tool-display-port.md`                    | Spec, not yet built | 2026-09-02   | Request doc for the full `pi-tool-display` style port: the lighter result box, the fold/expand control, per-tool limits, presets, and config                    |
| `tui-tool-result-truncation.md`               | Spec, not yet built | 2026-09-02   | Request doc for truncating `Read`/`Write` results and the `Edit` diff, plus the six "never truncate" comment rescopes                                        |
| `tui-syntax-highlighting.md`                  | Implemented       | 2026-09-08   | Decision record for the shared syntax-highlight engine (`highlight.rs`): `language_from_path`, the stateful `CodeHighlighter`, the `highlight_text_lines` entry point, the two consumers (picker preview + `Read` body), and the hand-rolled vs `syntect` vs tree-sitter tradeoff |
| `ui-extension.md`                             | Spec, not yet built | 2026-08-28   | Out-of-process UI extension design: JSONL host, five capabilities, host-owned load order. Review round 1 folded in                                        |
| `tui-extension-design-review.md`              | Review              | 2026-08-28   | Round-1 critique of `ui-extension.md`. All findings folded into the spec                                                                                    |
| `ui-extension-plan.md`                        | Plan                | 2026-08-28   | Staged work breakdown for `ui-extension.md`: stages 0-4, acceptance per stage                                                                                |
| `user-message-editing.md`                      | Spec, not yet built | 2026-09-11   | Spec for recalling and editing pending user messages: `Alt+Up` bulk recall and the `:edit-queue` palette entry, the `user_message_retract` event and optional `id` field on `user_message`, the loop-side skip rules in `bin/claim` and `bin/assemble`, and the idle-on-retract behavior   |
| `rewind-fork-design.md`                        | Implemented       | 2026-07-17   | Session rewind and fork (pi `/tree` style): the `rewind` event on the append-only log, the active-path mask in `bin/assemble` (the recursive computation that masks abandoned branches at every nesting depth), the rewind-aware `bin/claim` state, the TUI marker rendering, and the review of the proposed checkpoint+mask design (I1-I12). The TUI picker stage is spec'd, not built |
| `ft-005-logline.md`                           | Implemented         | 2026-08-29   | Record for FT-005: motivation from the ruxe type-level-disjointness post, two-writer interleave analysis, LogLine capability design and tests             |
| `skill-remapped-to-os-apps.md`                  | Proposal            | 2026-09-02   | Feature request: the OS + Applications split. Base distribution (kernel: loop core + base tools + growth machinery; default-swappable `tui` front-end, the WM tier). A tool registers by being on the agent-visible PATH and self-documents via `--help` (no `SKILL.md`; a procedure is a script). Discovery is an on-demand `tools --list` catalog (reads `tool.toml` `description`), never prompt content. First application is `tui-capture` |
| `pi-extension-port-investigation.md`            | Investigation       | 2026-09-13   | Portability assessment of the `pi-config` extension set (pi-goal, rpiv-ask-user-question, pi-fff, agent-simple-english, no-find-grep, no-bare-python, pi-lynx, pi-terminal-browser, pi-automode) onto the harness: tool/hook/UI/loop mapping, difficulty verdict, blockers, and suggested order. pi-goal verdict updated to "Ready" (2026-09-13). |
| `pi-goal-readiness.md`                           | Investigation     | 2026-09-13   | Readiness assessment for porting `pi-goal` as a harness extension: all three loop seams (`run.idle`, `compact.before`, `model.before` transform) are built and gated; the remaining work is application-level (goal tools, hook binaries, state file); gaps G2-G4 with workarounds |
| `goal-ux.md`                                     | Spec, not yet built | 2026-09-05 | Goal UX redesign: goal set by user action (TUI ext writes goal.json on send, not agent tool call), pi-goal prompt template port (goal-mode rules, trust boundary, goal_id stale-turn guard), completion guard, `goal pause` / `goal clear` commands, goal status display via the goal extension's row slot (goal status line + armed hint; the TUI itself has no goal-state coupling). The goal block is a byte-stable pure function of (goal, goal_id), injected as a trailing `input` item (after the conversation) so the `[system][history…]` cache prefix survives goal set/clear. Properties P1-P17 with verification table and gate |
| `goal-rule-reinject.md`                           | Proposal          | 2026-09-07   | Follow-up proposal: move the 10 goal-mode rules out of the per-call `model.before` block into the `run.idle` continuation prompt only, to reduce per-call token overhead. Tradeoff analysis and open questions |
| `deepseek-harness-compaction-research.md`       | Investigation       | 2026-09-04   | Cross-codebase research: how the dsh compaction system keeps the session log append-only while replacing surface ranges with summary checkpoints, and how the stable request prefix and prefix-aligned summarization call maximize KV cache prefix hits |

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

## Reading order for a new session

1. This file (`INDEX.md`).
2. `architecture.md` — the shape of the system and where each binary sits.
3. `refinement-policy.md` — the rules for any change you propose.
4. `lean-driven-development.md` — the spec-driven workflow: properties
   before code, a proof per property, the build gate as acceptance.
5. `coding-conventions.md` — the standing code rules for the Rust.
6. The spec for the feature you are working on (see the status
   column). UI extension work reads `ui-extension.md` with
   `ui-extension-plan.md`. Every spec doc ends with its Properties,
   Verification, and Gate sections.
7. `spec-review-criteria.md` — check your spec against these before implementing.
8. `SPEC_CONTRACT_TESTS.md` — how to split the implementer and tester roles.

## The Lean gate

The acceptance gate for a spec is the `## Gate` section at the end of
the spec doc. The doc gate is `scripts/verify-specs.sh` (structure and
cross-reference checks on the docs). The code gate is the command list
in each Gate section: `cargo build`, `cargo test`, and the named e2e
scripts. A clean gate with zero open properties is the guarantee.

A real Lean 4 backstop lives in `lean/RushiSpec.lean`. It mirrors the
`rushi setup` resolver and is checked by `scripts/lean-gate.sh`
(wraps `nix develop .#lean` + `lake build RushiSpec`). The Lean
backstop is optional; the house gate remains the conformance and e2e
scripts. See `docs/lean-driven-development.md` §8.

- `scripts/verify-specs.sh` — run before pushing doc changes. Exit 0
  is clean; exit 1 names the failing doc and section.

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
