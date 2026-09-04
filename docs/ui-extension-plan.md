# UI extension plan, staged

Status: work breakdown for `ui-extension.md`.
Stage order is build order. Each stage ships a working TUI.
A stage does not need a later stage.
A stage that fails acceptance does not advance.

## Ground rules

- The mechanism completes at stage 1. Later stages add
  extensions and one host feature (span extraction, stage 3)
- No stage adds hot reload or in-process mounting
  (`ui-extension.md` sections 9 and 12)
- Acceptance per stage: conformance tests plus the PTY smoke
  (`scripts/tui-pty-smoke.py`)
- The `docs/tui.md` section 10 guardrail scans run every stage

## Stage 0 — vocabulary: `ext_status`

Goal: the shared-state event type exists, is schema'd, and
renders safely.

- add `ext_status` to `EventKind` and the wire registry
  (`bin/tui/src/event.rs`)
- add `schemas/events/v1/ext_status.json` and G3 producer coverage
- transcript suppresses `ext_status` by default. The log keeps it

Acceptance:

- a parse test: `ext_status` lines are semantic, not fallback
- schema test: a missing `value` field is rejected
- transcript test: an `ext_status` event adds no lines

Exit: `cargo test -p tui` green.

## Stage 1 — the extension host (the mechanism)

Goal: the full protocol, with no extension content yet.

- `bin/tui/src/ext.rs`: manifest parse, host, channels, fallbacks
- discovery: global `ui_extensions/`, then project
  `.pi/ui_extensions/` (override by name). Layer order is
  host-fixed
- fail-loud validation: a bad manifest or broken command
  refuses the start and names the file
- process groups (`setsid`), stdout pump, restart budget
  1 s / 2 s / 4 s, dead hint
- ops: `event` (kind filter), `tick`, `transform`, `lines`,
  `status`, `transformed`, `append` (whitelist), `notify`
- layout: the statusline row. Kind ownership in
  `build_transcript_lines`. The transcript cache folds in
  extension replies
- `config.toml`: an optional `[ext] dir` override

Acceptance, with fake extensions (bash scripts):

- a stub extension alive: its row renders
- kill the extension: hint after the restart budget
- malformed JSONL: per-op G5 fallback. The TUI stays up
- an `append` outside the whitelist: rejected with a flash
- quit: no orphan processes (check in the smoke script)

Exit: `cargo test` plus PTY smoke green. Guardrail scans green.

## Stage 2 — reference extensions, bash first

Goal: prove the point with no new compiled code.

- `ui_extensions/statusline/`: `ext.toml` plus `statusline.sh`
  - tick-driven, git TTL-cached at 3 s
  - session, model, and loop state from `tick`
  - cumulative stats from `assistant_message.usage`
  - two-line layout when the terminal is narrow
- `ui_extensions-demos/tool_result/`: `ext.toml` plus a script.
  The `render` kind owner for `tool_result`
- `ui_extensions/notify/`: `ext.toml` plus a script. Bell and
  OSC through the `notify` op, tmux tty resolution

Acceptance:

- a real session: the statusline shows live dir, git, model,
  and usage stats
- restart the TUI mid-session: the stats survive from the log
- `tool_result` renders through the script. Kill the script:
  the built-in render returns
- a finished turn rings the terminal

Exit: all three references run in a real session.

## Stage 3 — transformers: mermaid, then LaTeX

Goal: inline diagram rendering, like pi's.

- host span extraction: `fence:mermaid` code blocks
- `ui_extensions/mermaid/`: a Rust binary on the `mermaid-text`
  crate (a pure-Rust mermaid-to-Unicode renderer; `grok-mermaid`
  is the JS package the ground rules rule out). Its own cargo
  package. `ext.toml` declares the transform
- a 2 s timeout. Stale `req` replies drop. A resize re-requests
- `inline:latex` span detection lands with the mechanism.
  The LaTeX renderer binary is follow-up work (open item)

Acceptance:

- a mermaid block in an assistant message renders as Unicode art
- the extension is missing or times out: the raw block shows
- a resize triggers one re-transform per block

Exit: `cargo test` plus smoke green.

## Stage 4 — Rust ports and open items

- `statusline-rs` and `tool_result-rs`: Rust ports of the
  stage 2 references
- settle the `ui-extension.md` section 11 open items:
  pre-approval semantics, the `ext_status` growth bound,
  per-op timeout values, the `order.toml` decision

Exit: every surface has a bash and a Rust reference.

## What no stage includes

- hot reload (stop, edit, restart stays the update path)
- in-process mounting of extension code
- audio output

## Properties

Lean-style invariants for this plan (see `lean-driven-development.md`).

P1. mechanism-complete: given the stage-1 host, observe the full protocol, manifest, discovery, and fallback behavior with no extension content required.
P2. reference-renders: given the bash reference extensions, observe each render in a real session and survive a kill.
P3. no-hot-reload: given an edited extension, observe the update path stay stop, edit, restart. The host never hot-reloads or mounts in-process.
P4. guardrail-scan: given any stage, observe the `docs/tui.md` section-10 forbidden-string scan pass.

## Verification

| P# | Property | Proof | Status |
|----|----------|-------|--------|
| P1 | mechanism-complete | `manifest_bad_toml_refuses_with_the_file`, `restart_budget_ends_in_dead` in `bin/tui/src/ext.rs`; `scripts/tui-pty-smoke.py` | proven |
| P2 | reference-renders | `ui_extensions/statusline`, `ui_extensions/notify`, `ui_extensions-demos/tool_result` plus `scripts/tui-pty-smoke.py` | proven |
| P3 | no-hot-reload | Blocked: no test forbids a hot-reload path. Unblock with a test that the host exposes only stop and restart. | open |
| P4 | guardrail-scan | `storage_and_loop_strings_stay_behind_the_port`, `stage_names_are_not_strings_in_the_tui` in `bin/tui/src/main.rs` | proven |

## Gate

The acceptance commands. All must exit 0 for this spec to be proven.

```
cargo build
cargo test
scripts/tui-pty-smoke.py
```
