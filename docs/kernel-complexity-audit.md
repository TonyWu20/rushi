# Kernel Complexity Audit — 2026-09-16

Status: Audit. Findings are evidence-backed against commit `bb06e94`
(worktree `rushi-kernel-redundant-code-audit`). No code changes are
proposed in this doc. Sections 4 and 5 sequence the follow-up work.
They mark what needs a human decision.

Scope: audit the kernel tree's complexity against three docs.

- `docs/architecture.md` (phases, guardrails, properties)
- `docs/skill-remapped-to-os-apps.md` (base distribution, registration, self-documentation)
- `docs/refinement-policy.md` (evidence-first refinement, promotion thresholds, goals G1-G8)

The audit also weighs the kernel-side changes still owed by
`docs/subagent-design.md` (D1, D2, D3, D5, D7). Decisions D6 and D8
are already in the tree. Section 3 shows the evidence.

Method: run the gate commands in-tree. Grep `bin/`, `crates/`,
`scripts/`, `lib/` for the claims in the docs. Reproduce the failures
of stale test assets with minimal repros. Every file and line claim
below was checked against the tree, not copied from docs.

## 1. Gate baseline (run 2026-09-16)

| Command | Result |
|---|---|
| `cargo test --workspace` | pass, all 15 members, rc 0 |
| `scripts/tool-conformance.sh` | 43 passed, 0 failed. The lean-verify block SKIPs (no exts-built binary in this tree) |
| `scripts/cache-e2e.sh` | SKIP (no `DEEPSEEK_API_KEY`. Key-gated by design) |
| `scripts/lean-verify-e2e.sh` | not run. Ext-owned. The Lean toolchain is absent in this worktree |
| `scripts/steer-inflight-e2e.sh` | **FAIL, rc 1**. A legacy-key hard fail (finding F1) |

## 2. Doc-against-code cross-check

### 2.1 `architecture.md`

- **Phase state matches the tree.** Phase 2 is built. The loop is a
  Rust binary (`rushi run` / `step`). The `rushi-common` utility
  crate exists. Stage binaries spawn as subprocesses.

- **Phase 3 is not started.** There is no `crates/core`. Property P5
  (`core-io-free`) is open. The doc's blocked Gate is consistent.

- **Section 4.2 is stale by design, not drift.** It shows the
  hexagonal target layout (`crates/core`, adapters). The tree has
  the Phase-2 shape: stage binaries under `bin/`, plus the common
  crate and the tool crates. The doc names the loop binary
  `harness`, but the tree names it `rushi`. It also omits `user`,
  `hook-compact`, and the four tool crates.

- **Section 5 is marked "illustrative, not normative".** A refresh
  pass on 4.2 is low-priority bookkeeping.

- **The properties table still holds.** P2 (`non-json-wrap`) is
  open. `bin/route` has preview, slim, and legacy tests. None feeds
  non-JSON stdout and asserts the `{"text": "..."}` wrap. P4
  (`log-is-truth`) is open. No dedicated append-and-observe test
  exists.

- **`bin/log` carries two generic tests, not the P4 property.** The
  open row in the table is accurate.

### 2.2 `skill-remapped-to-os-apps.md`

- **The base tiers hold.**

  - Tier 1 kernel: loop core (`claim`, `assemble`, `model`, `parse`, `route`, `log`, `user`, `compact`, and the `rushi` loop).
  - Base tools: `tools/{read,write,edit,bash}`.
  - Growth machinery: the `tools/` dir, the `route` execution path, the extension host via `[paths] extension_tool_paths`, and PATH-style registration.

- **Tier 2 holds.** The TUI lives in the sibling `rushi-tui` repo.
  The kernel workspace has no TUI member. The "swappable, no
  build-time dependency" guardrail is satisfied.

- **Registration and self-documentation hold.** A tool registers by
  sitting on a tool path. `route` scans `native_tool_paths` first,
  then `extension_tool_paths`. Native wins a collision. The first
  entry wins.

- **No `SKILL.md` exists in the tree.** The tool list is
  prompt-resident. `assemble` generates it from the same manifests
  (`load_tool_schema` plus the D4 tool-list section).

- **The header "Applied so far" block is stale.** It claims the
  application scripts sit on `scripts/` and names four aligned apps
  (`band_match`, `timestamp_compare`, `capture-thinking-border`,
  `verify-reattach`).

- **The first application left the repo.** `scripts/tui-capture.py`
  left this repo in `fad9fd2` (the repo split). It now lives at
  `../rushi-tui/scripts/tui-capture.py`.

- **The four remaining scripts are one-off analysis scripts.**
  Section 11 of the same doc records them as "pending the human's
  call". They are still in the tree.

- **Properties status holds.** P1 path-registration is proven via
  route discovery in `tool-conformance.sh`. See F1 for a SKIP-gated
  nuance. P2 self-doc is still blocked. No test asserts the
  `--help` output of the four named scripts.

- **P2 code side is ready.** Each script carries a `--help` path.
  P3 catalog is open. `tools --list` is not built, consistent with
  the doc's Gate. P4 prefix-stability is proven via `cache-e2e.sh`
  (key-gated SKIP here). P5 fenced-failure is proven by the route
  spawn-failure rows.

### 2.3 `refinement-policy.md`

- **G1 (idempotent step replay): acceptance not automated.** The
  stated acceptance is: run `rushi step` twice on the same log. The
  second run emits no new events. `scripts/crash-e2e.sh` covers the
  G2a kill-recovery cases. No script or test runs a step twice and
  asserts zero new events.

- **This is an open acceptance gap (F7).** It belongs with the crash
  e2e or a small dedicated replay e2e.

- **G2a done, G2b parked: consistent.** `crash-e2e.sh` kills
  `log`, `route`, and `claim` mid-flight. The G2b parking entry is
  in `docs/itches.md` (2026-09-15).

- **G3 (typed vocabulary): consistent.** `crates/rushi/src/event.rs`
  has 14 `Event` variants. `EVENT_TYPES` holds 14 tags. The
  subagent-design D8 inventory is exact. `schemas/` is gone. The
  retired `event_validation` module survives only as a doc comment.

- **G4 (tool conformance): consistent, with one caveat.** The doc
  says the runner hard-codes the four native tools. The script now
  also carries ext-gated `lean-verify` rows (SKIP-guarded). Its P1
  route-discovery row is stale (F1).

- **G7 (producer-side validation): consistent for the kernel.**
  `log` rejects lines that fail to parse into a known `Event`.
  `parse_event` is the single validation step. The known gap (TUI
  producer paths) belongs to the sibling repo.

- **P7 trigger table: both "Done" rows verify.** Model retry:
  `bin/rushi/src/step.rs` carries the 2 × 3 s retry loop.
  `model_timeout_s` resolves in `crates/rushi/src/model_settings.rs`
  line 135.

- **Daemon plus attachable TUI: verified.** `loop.pid` reattach
  lives in `bin/rushi/src/run_loop.rs`.

- **P8 not-yet list: one entry is partially stale.** "Multi-agent
  scheduling … approved but not built" is true only for D1, D2, D3,
  D5, D7. Decisions D6 (tool-path lists) and D8 (typed events) are
  built, as independent changes.

### 2.4 `subagent-design.md` — kernel-side deltas

**Landed. The doc's Gate section is stale on these:**

- **D6, tool-path lists: built.** `[paths] native_tool_paths` and
  `extension_tool_paths` exist in `bin/rushi/src/config.rs`,
  `crates/rushi/src/stage.rs` (`StageEnv`), `bin/assemble`,
  `bin/parse`, and the `route` CLI. Native scans before extension.
  An earlier entry wins a collision.

- **Legacy keys are hard fails now.** `tools_root` and
  `extra_tools_roots` are rejected by
  `crates/rushi/src/config_check.rs`. The `RUSHI_EXTRA_TOOLS_ROOT`
  env fallback is gone from all Rust. Only doc mentions remain.

- **D8, typed events: built and verified.** The `Event` enum has 14
  variants matching the doc's inventory. `parse_event` is the
  single validation step. `schemas/` is removed, and no `Value`-
  based unknown-type path remains in the kernel. The doc's P8
  claim holds. Both changes predate the spec draft, landing in
  `4c9b1e7` and `faab1bd`.

**Not built. The doc's Gate is accurate on these:**

- **D1/D5:** no `spawn_agent` tool dir, no `config_gen` module, no
  `rushi config-gen` subcommand. Zero `rg` hits for
  `config_gen|spawn_agent` in the kernel Rust.

- **Decision 2026-09-16 (human).** `spawn_agent` is not a kernel
  citizen. It does not meet the criteria for a kernel workspace
  member. The tool is an exts-owned application, like `goal` and
  `lean-verify`. The kernel ships `config_gen` and `rushi
  config-gen` only.

- **D2/D7:** no sub-session nesting, no blocking child runner, no
  `scripts/subagent-e2e.sh`.

**Spec/code gap to resolve at build time (decision needed):**

- **The D6 path-resolution fallback is not implemented as
  specified.** The spec says resolution tries the config dir first,
  then falls back to `<exe_dir>/../<entry>`. The code resolves
  relative entries against the config dir only
  (`bin/rushi/src/config.rs:150-189`).

- **The `route` candidate list covers binaries, not paths.**
  `register_tool_dir` tries sibling and in-tree binary locations.
  Consequence: a child config written into a session dir (D3) with
  install-relative tool entries would miss the install dir.

- **`config_gen` must choose a remedy.** Stamp absolute paths into
  the child config, or the resolver gains the exe-dir fallback.
  This is a design decision, not a detail.

- **The spec's env-var claim is not true today.** Spec section 4
  says route sets `$HARNESS_SESSION_DIR`, `$CONFIG`, and
  `$HARNESS_BIN`. Verified 2026-09-16: route sets only
  `HARNESS_SESSION_DIR` (`bin/route/src/main.rs:376`). `$CONFIG`
  reaches tools by process-environment inheritance only.
  `$HARNESS_BIN` exists nowhere in the kernel. Both must join the
  tool env.

- **Stale paragraph.** D6 says "the `tools-bash-only/` symlink dir
  in this repo is a workaround for that blindness". The symlink is
  gone. The scanner now understands tool dirs directly (`route`
  `scan_tool_path` handles both shapes).

## 3. Redundancy and complexity findings

Severity: **R** = regression or stale. Fix regardless of policy.
**P** = policy-mapped promotion or itch candidate
(`refinement-policy.md` P0, P3, P9). All findings cite the code that
proves them.

### F1 (R) — the D6 rename left four test assets stale

1. `scripts/steer-inflight-e2e.sh:52`,
   `scripts/model-before-transform-e2e.sh:47`, and
   `scripts/tool-after-transform-e2e.sh:43` generate
   `[paths] tools_root = "$ROOT/tools"` into the config they hand
   to `rushi run` or `rushi step`. Both entry points call
   `HarnessConfig::load` (`bin/rushi/src/main.rs:125,132`). That
   path hard-fails on the legacy key (`config.rs:122-130`).

2. **Reproduced.** `steer-inflight-e2e.sh` exits 1 with "config
   [paths] contains legacy key `tools_root` … 0 passed, 4 failed".
   The other two fail through the same config-load path.
   `tool-after-transform-e2e.sh` is the e2e of record for
   `docs/image-read-kiss.md`.

3. `scripts/tool-conformance.sh:511` calls
   `route --tools "$TOOLS_DIR" --extra-tools "$EXTS_ROOT/goal-tools"`.
   Those flags were removed in `4c9b1e7`. `route` now rejects the
   call. **Reproduced:** "unexpected argument '--tools' found". The
   line's `|| true` swallows the CLI error.

4. **In a full exts environment the row would fail.** With the
   exts `lean-verify` binary and a `goal-tools/` dir present, the
   row would report `FAIL: lean-verify: route discovery`.

5. **The exts layout moved too.** `$EXTS_ROOT/goal-tools` no longer
   exists. Ext moved it to `goal-app/goal-tools` (see
   `config-exts.example.toml:58`). The guard
   `[ -d "$EXTS_ROOT/goal-tools" ]` is false today. The row SKIPs
   everywhere.

6. **Net effect.** `architecture.md` P1's "proven via
   `tool-conformance.sh`" rests on a row that no longer executes its
   route-discovery proof.

### F2 (P) — `[paths]` tool-path extraction is a three-way copy that diverged

Identical-shaped blocks in three files:

- `bin/rushi/src/config.rs:150-189`. `config_dir` via `canonicalize()`.
- `bin/assemble/src/main.rs:895-924`. `config_dir` via `parent()`.
- `bin/parse/src/main.rs:75-105`. `config_dir` via `parent()`,
  computed twice (once per list).

The P3 trigger is met on both counts. The same structure appears in
three binaries. The copies diverged on the same field
(canonicalize vs parent). `subagent-design.md` D6 demands one
resolution function for both lists. The tree currently violates
that. `config_gen` (D3) would be a fourth consumer.

### F3 (P) — three manifest-walk shapes and two `toml_to_json` copies

- `route`: `scan_tool_path` plus `register_tool_dir` plus
  `toml_to_json`. The full manifest: command, args, timeout, schema,
  binary candidate resolution.

- `assemble`: `tool_dirs_in` plus `load_tool_schema` plus its own
  `toml_to_json`. Model-facing schema and description for the D4
  tool list.

- `parse`: `collect_tool_names`. Names only, for call validation.

`toml_to_json` is a byte-level duplicate in
`bin/route/src/main.rs:712` and `bin/assemble/src/main.rs:1569`.
The P3 threshold (three copies of the manifest walk) is met. The
`toml_to_json` pair is two copies with identical semantics.

Natural home: one `tools` module in `rushi-common`. Consumers:
`route`, `assemble`, `parse`, and (once built) `config_gen` and the
`spawn_agent` tool.

### F4 (P) — two divergent token-estimator pairs

- `bin/assemble/src/main.rs` defines its own `Ev` projection (line
  76). It keeps verbatim `reasoning` items, `usage_input` /
  `usage_output`, and `image`.
  `estimate_ev_tokens` (line 703) uses a flat 4800-token image term
  and no caps. `estimate_request_tokens` (line 745) carries the
  measurement-anchor logic.

- `crates/rushi/src/compact_math.rs` owns `Ev` (`reasoning_chars`,
  `Result { chars }`). Image chars fold in via `project_event`.
  `est_tokens` (line 67) is cap-aware via `Caps`.
  `estimate_from_events` (line 156) is used by `compact`, the
  `rushi` step, and `setup` probes.

The `itches.md` entry "compact trigger math is a second copy"
closed the trigger math, cut walk, and estimator into
`compact_math`. But `assemble` kept a live, separate estimator.
Its semantics differ: caps vs no caps. Reasoning as serialized JSON
chars vs `reasoning_chars`. Different image handling.

The same log can estimate differently depending on which binary's
estimator runs the gate. This is P3's "diverged on the same field"
trigger.

Decision needed: unify in `compact_math` (preferred, so `compact`'s
post-compact sanity check and `assemble`'s hard-trim agree on
numbers), or document the divergence as intentional.

### F5 (R) — `HarnessConfig` hides dead surface

`#[allow(dead_code)]` sits at struct level
(`bin/rushi/src/config.rs:16`), with a field-level allow on
`last_measured_input` (line 49). It papers over two dead fields:
`log_bin` is never read outside the config module. `last_measured_input` initializes to 0 and is never read.
Remove the allows. Drop the fields, or wire them up.

### F6 (R) — doc drift in the audit-target docs themselves

- `skill-remapped-to-os-apps.md` header "Applied so far" block:
  stale as documented in 2.2. The four named "aligned apps" are
  one-off scripts. `tui-capture.py` moved to `rushi-tui`.

- `docs/INDEX.md:78` (subagent-design row): says "tools_roots list"
  and "D9 typed event vocabulary". The actual keys are
  `native_tool_paths` / `extension_tool_paths` (already landed).
  The typed-events decision is D8. The row implies the whole spec
  is unbuilt, when D6 and D8 are in.

- `INDEX.md` is now 169 lines. Its own maintenance rule says keep
  it under 150.

### F7 (P) — G1 acceptance is not automated

No test runs `rushi step` twice on one log and asserts the second
run is a no-op (refinement-policy G1). `crash-e2e.sh` covers the
G2a kills only. Adding a short double-step scenario to
`crash-e2e.sh` (or a dedicated `scripts/replay-e2e.sh`) closes the
G1 acceptance.

### F8 (note) — complexity concentration

`bin/rushi` is 4,705 lines. `step.rs` alone is 1,967 (loop
orchestration, retry, compact and handoff, tool-result handling).
This is the Phase-2 shape by design, not a violation. It is the
natural first extract target if P3's core-crate trigger fires
(typed vocabulary stable for 20 sessions, replay-tested loop state
machine).

`setup.rs` (973) is distribution bootstrap. Keep it out of any
loop-core extraction. No action now. The complexity map should be
explicit.

**Resolution (decision 2026-09-16, human):** `step.rs` was split
in-binary, not into a crate. The P3/P8 core-crate gate stands. No
new workspace member, no architecture commitment. The 1,967-line
file now holds only the entry points and the state dispatch. The
concerns moved to submodules of `crate::step`:

| module | lines | holds |
|---|---|---|
| `step/model.rs` | ~930 | describe/estimate, `awaiting_model` pipeline, retry loop, overflow/length recovery, `exhausted.handle` |
| `step/tool.rs` | ~340 | `awaiting_tool_result`, `tool.before`/`tool.after` routing, `route_batch` |
| `step/compact.rs` | ~235 | compact with hooks, post-compact sanity, handoff writers |
| `step/approval.rs` | ~195 | approval round-trip, `awaiting_approval` resume |
| `step/hook.rs` | ~120 | hook windows, `HARNESS_*` env, result logging |
| `step/logio.rs` | ~105 | event append/scan, `ext_status` markers |
| `step.rs` | ~135 | `do_step`, `make_runner`, `StepMode`, dispatch |

Blast radius per change is now one submodule. The public surface is
unchanged. `crate::step::` still exposes `append_event`, `hook_env`,
`make_runner`, `StepMode`, and `do_step` to `run_loop` and `main`.
If P3's trigger later fires, `model.rs` + `tool.rs` are the
ready-made `core` crate seed.

Gate after the split (run 2026-09-16): `cargo build` clean with 0
warnings. `cargo test --workspace` 18/18 suites ok.
`scripts/tool-conformance.sh` 43/0. `scripts/compact-e2e.sh` 98/0
(exercises the step pipeline end to end: threshold, pi-parity,
overflow, length-stop, compact-failure, kill-switch scenarios).

## 4. Kernel-side work owed by `subagent-design.md` (sequenced)

Ordered so each step stands alone. Items 1 and 2 are shared with
the redundancy cleanup above.

1. **Shared `tools` module in `rushi-common`**. It closes F2 and
   F3. It also serves the D6 "one resolution function" need and
   the `config_gen` need.
   The module holds one resolver (config dir plus the D6 exe-dir
   fallback, the decision point in section 2.4), one manifest
   walk, and one `toml_to_json`. The consumers are `route`,
   `assemble`, and `parse`.

2. **`config_gen` module plus `rushi config-gen` subcommand**
   (D3, D5). Pure `project(parent, args) -> child config`.
   Deterministic (property P3 of the spec). No `[tui]` section.
   `sessions_root` stamped to `<parent_session_dir>/sub-<uuid8>`
   (D2. `resolve_session` already passes `/` through,
   `config.rs:331-338`).

3. **Route tool-env exports.** Add `HARNESS_BIN` and `CONFIG` to
   the env route sets for tool subprocesses. Today it sets only
   `HARNESS_SESSION_DIR` (`bin/route/src/main.rs:376`). Without
   them the exts tool cannot exec the child run or find the
   parent config.

4. **`spawn_agent`: an exts-owned application.** Decision
   2026-09-16 (human): it is not a kernel citizen and not a kernel
   workspace member. The tool dir and binary live in the exts
   repo. They register via `extension_tool_paths`. The kernel
   never names `spawn_agent`, which strengthens P1.

   The exts tool shells out to `rushi config-gen` (item 2). It runs
   a blocking exec of `$HARNESS_BIN run --session-dir <child>
   --config <child_cfg> <task>`. It reads the terminal `ext_status`.
   It exits 0 on `idle`, non-zero on `error` or timeout. No
   spawn-tool dir in the child's `native_tool_paths`. Depth 1 by
   construction (P5).

5. **`scripts/subagent-e2e.sh`, exts-gated.** Kernel-side: an
   exts-gated row in the e2e scripts (SKIP when the tool is
   absent), mirroring the `lean-verify` rows in
   `tool-conformance.sh`. The full P1-P7 suite lives in the exts
   repo. Close the spec's Gate from both sides.

Policy basis (P0): items 1-3 and 5 are the approved spec's
kernel-side work. Item 4 is exts-owned application work. The
approved spec names the tool as an extension, and the human
confirmed the tiering on 2026-09-16. Item 1 has its own P3
evidence (three-way copy with divergence, plus a fourth consumer
inbound). Nothing here is speculative.

## 5. Recommendations (decisions requested)

**Fix now, no policy discretion (regressions):**

- F1: re-key the three e2e scripts from `tools_root` to
  `native_tool_paths`.

- F1: rewrite the `tool-conformance.sh` P1 row to
  `route --native-tool-path … --extension-tool-path …`, against the
  current exts layout (`goal-app/goal-tools` plus `lean-verify`).

- F5: drop `log_bin`, `last_measured_input`, and the struct-level
  `#[allow(dead_code)]`.

**Fold into the subagent build (section 4):**

- F2/F3: the shared `tools` module.

- The D6 exe-dir-fallback decision (absolute-path stamping vs
  resolver fallback).

- The exts-owned `spawn_agent` tool (section 4, item 4). Its
  kernel dependencies are items 2-3 only.

**Evaluate:**

- F4: unify the two estimators in `compact_math`, or record the
  divergence as intentional.

- F7: add the G1 double-step e2e.

**Doc bookkeeping:**

- Refresh the `skill-remapped-to-os-apps.md` header block.

- Fix the `INDEX.md` subagent-design row. Keep INDEX under 150
  lines.

- Refresh `subagent-design.md`'s Gate (D6/D8 landed, fallback gap
  noted, `tools-bash-only` paragraph removed).

- The four one-off scripts in `scripts/` stay pending the human's
  call, per `skill-remapped` section 11. No unilateral deletion.

**Do not build (P0):** the Phase-3 `core` crate extraction stays on
the not-yet list (`refinement-policy` P8 / P3 triggers unmet). No
in-process tool execution, no streaming, no plugin system. Their
triggers are unmet. Note: F8 was resolved 2026-09-16 as an in-binary
module split only — the `core` crate question is untouched and stays
gated on P3.

## Properties

P1. Gate green: `cargo build`, `cargo test`, and
    `scripts/tool-conformance.sh` all exit 0 on the audited tree.
P2. Stale-asset claims: the assets named in F1 fail or SKIP against
    the current binaries, exactly as described.
P3. Duplication claims: each of F2-F4 names 2+ concrete copies with
    file and line, and at least one divergent field.

## Verification

| P# | Property | Proof | Status |
|----|----------|-------|--------|
| P1 | Gate green | `cargo test --workspace` rc 0. `scripts/tool-conformance.sh` 43/0 (run 2026-09-16, section 1) | proven |
| P2 | Stale assets | `scripts/steer-inflight-e2e.sh` rc 1 (legacy-key hard fail). `route --tools` rejected by clap (both reproduced, section 3 F1) | proven |
| P3 | Duplication map | `rg` listings in F2/F3/F4 (config.rs:150, assemble:895, parse:75. route:712 plus assemble:1569. assemble:703 vs compact_math:67) | proven |

## Gate

Gate: clean. All audit properties are proven above.

```
cargo build
cargo test
scripts/tool-conformance.sh
```
