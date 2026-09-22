# Itches

Problems to solve.

## LogLine is a four-way copy (2026-08-30) → resolved 2026-09-08

At the Phase 2 split, `LogLine` moved to the shared
`rushi-common` crate (`crates/rushi/src/logline.rs`). All kernel
producers (`bin/log`, `bin/user`, `bin/route`, `bin/rushi`) import
it. The TUI copy moved to the `rushi-tui` repo. This itch is closed.

**Resolution details (2026-09-08).** The single `LogLine`
definition now lives at `crates/rushi/src/logline.rs` (module
`rushi_common::logline`). The four old copies are gone:

- `bin/log`, `bin/user`, `bin/route`, `bin/rushi` — each now
  imports `use rushi_common::logline::LogLine;` (one import line,
  no local `logline.rs`).
- `bin/tui` — the TUI moved to the `rushi-tui` repo in commit
  `9fb0a5e` ("Trim the TUI + ui-extension layers out of the
  kernel"). The TUI's `port_file.rs` imports the same shared type
  via a path dep: `rushi-common = { path =
  "../../../rust-unix-harness/crates/rushi" }`. The old embedded
  `mod logline` in `port_file.rs` was removed when the TUI
  joined the shared-crate import. No second `LogLine` definition
  exists in `rushi-tui`.

`LogLine`, the only type that may write a session log (FT-005),
lives in four places:

1. `bin/log/src/logline.rs`
2. `bin/user/src/logline.rs`
3. `bin/tui/src/port_file.rs` (embedded `mod logline`)
4. `bin/route/src/logline.rs` (the per-session tool log,
   correction 58)

The duplication follows the phase-1 policy (no shared crate). It
joins the validator as a promotion candidate for a shared
`core`/`bin/common` crate. Keep the four copies in sync. Do not
grow them in parallel.

## Event schema validator is now a third copy (2026-08-27) → resolved 2026-09-08

At the Phase 2 split the validator moved to the shared `rushi-common`
crate (`crates/rushi/src/event_validation.rs`). All kernel producers
(`bin/user`, `bin/log`, `bin/rushi`) call it. The old TUI copy moved
with the TUI to the `rushi-tui` repo. This itch is closed.

Producer-side G3 validation (check the event against
`schemas/events/v1/<type>.json` before append) exists in three
places:

1. `bin/user/src/main.rs` — `validate_event` (user_message producer).
2. `bin/log/src/main.rs` — loads all schemas, validates every line.
3. `bin/tui/src/port_file.rs` — minimal local validator used by
   `append_event`.

The duplication is intentional for Phase 1 (no shared `core` crate,
per `architecture.md`), but the third copy crosses the promotion
threshold in `refinement-policy.md`. Candidates for one shared
validator:

- a small `schemars`-free JSON-Schema subset crate (`core` or
  `bin/common`), or
- one binary that owns validation and the producers call it.

Blocker to watch: the validator subset must stay small enough to be
portable. The TUI copy (`port_file.rs`) supports only `const`,
`required`, `properties`, `items`, and primitive `type` checks
(string/integer/number/boolean/array/object). Do not grow the three
copies in parallel.

## The compact trigger math is a second copy (2026-09-03) → resolved 2026-09-09

`bin/compact` duplicated the trigger math and the cut walk of
`bin/assemble` (phase-1 policy: no shared crate). The one-step
predicted reading (`last + rate`), the `est_tokens` estimator, and
the backward cut walk each lived in both binaries. The estimator
copy is noted in the `find_cut` doc comment. Keep the two copies in
sync. Do not grow them in parallel.

Resolved: the math now lives once in `crates/rushi/src/compact_math.rs`
(the shared `rushi-common` crate, docs/phase-2-plan.md section 6).
`bin/compact` imports `rushi_common::compact_math`; the local
copies are removed.

## The compact strategy is not a port (2026-09-07) → resolved 2026-09-08

The Phase 2 plan (`docs/phase-2-plan.md`) baked the in-place compact
strategy into the `harness` loop. The `StageRunner::compact` payload
named no handoff outcome. The `awaiting_model` branch ended every
recovery path with a re-projection in the same session. The `run`
skeleton held one session, one flock, one `loop.pid` for the
process life. A handoff strategy touched four modules.

**Resolution (2026-09-08).** The design in
`docs/loop-lifecycle-hooks.md` resolves this by decomposing the
monolithic strategy into fine-grained lifecycle windows. Each window
is an independent, swappable extension point. The
`exhausted.handle` and `overflow.resolve` windows carry the decision
vocabulary (`stay_compact`, `handoff`, `stop`). The `SessionStore`
port survives as the fs boundary behind the `exhausted.handle` hook.
The loop no longer branches on a strategy name; it fires the window
and applies the decision. The swap is now a hook registration plus
config, not a loop rewrite.

## The marker schemas join the validator list (2026-09-03)

The three auto-compact marker schemas (`compaction_started`,
`compaction_failed`, `compaction_summary`) join the `bin/log`
hardcoded schema list and the `bin/claim` no-op list. The
`bin/compact` binary does not validate: it pipes every marker
through `bin/log`, the owner of the validator list. The
`bin/tui` semantic parser gains the three `EventKind`s.

## `lean-verify` `drt` op has no progress, checkpoint, or smoke mode (2026-09-12, episode 1) → resolved 2026-09-12

**Observed.** Running `lean-verify op=drt` with `n=100000`
takes ~3 h with zero output until completion. No progress
counter, no checkpoint/resume, no `--smoke` alias. When the
goal was re-posted (context overflow) mid-run, the only way to
know whether the gate was still alive was `ps aux` — the tool
gave no feedback.

**Reproduce.** Pipe `{"op":"drt","n":100000,...}` to
`target/release/lean-verify`; wait 3 h with no stdout until
the final JSON blob.

**P4 checklist.**
1. Unblock a current task? Marginally — a `--smoke` (n=2000,
   ~15 s) would make the "quick check" a one-liner instead of a
   judgment call.
2. Correctness bug? No.
3. Recurring manual step? Yes — every DRT run requires manually
   picking `n` and deciding whether to background the process.
4. Invariant → code? Partially — a `--smoke` flag moves "how
   many inputs for a quick check" from human discipline to a
   named preset.

**Pre-test.** A `--smoke` alias or auto-tier (`<10k` = fast,
`≥10k` = full) is a one-line config, not a protocol change.
But per P2 rule-of-three, one episode is not enough to add a
new flag to the tool. **Parked as an itch.**

**Resolution (2026-09-12).** The itch was confirmed real and solved
in the exts-owned `lean-verify` tool (`rushi-exts/goal-tools/lean-verify/`):
- `"smoke":true` is the named quick tier (n=2000, ~15 s): the
  quick check is a one-liner, no judgment call on `n` (an
  explicit `n` still wins; the full tier gates the release).
- A progress/heartbeat file `<dir>/.drt-progress.json` is written
  every ~10 s or 100 inputs (pid, next index, rate, eta,
  `updated_at`): a re-posted goal polls it instead of `ps aux`;
  a clean run deletes the file.
- A stop on mismatch/timeout keeps the file as a checkpoint, and
  `"resume":true` continues the same call from the first failed
  index (parameters must match the checkpoint; `input_gen` must
  be deterministic). The result JSON reports `progress_file`,
  `checkpoint`, and `resumed_from`; a live run is protected by a
  pid liveness guard.
Covered by `scripts/lean-verify-drt-e2e.sh` (kill+resume,
fix+resume, live-run guard) and the drt steps of
`scripts/lean-verify-e2e.sh`.

## `lean-verify` `drt` op has no input-validation mode (2026-09-12, episode 1) → resolved 2026-09-12

**Observed.** The `input_gen` parameter is a shell command that
must emit well-formed scenario lines. There is no way to verify
that the generator's output is parseable without running the
full DRT comparison. When a test list contained a line that was
supposed to be malformed but was actually well-formed, the only
detection path was a full DRT run (~3 h for 100 K inputs).

**Reproduce.** Write a generator that emits one unparseable line;
run `lean-verify op=drt` with `n=100000`. The mismatch is only
visible after the full comparison completes.

**P4 checklist.**
1. Unblock a current task? Yes — a `--check-inputs` mode (parse
   each generated line, report the first unparseable one, exit)
   would save a 3 h run on a broken generator.
2. Correctness bug? No, but a *wasted-compute* bug: 3 h × 100 K
   inputs to discover the generator was off by one.
3. Recurring manual step? Yes — every time the generator or the
   protocol changes, re-validation requires a full gate run.
4. Invariant → code? Yes — "the generator emits well-formed
   lines" is currently human discipline; a `--check-inputs`
   mode makes it a 10-second automated check.

**Pre-test.** The parser logic lives inside the Lean model
executable and the Rust production binary. There is no standalone
"parse one line" subcommand, so a pure script cannot do the job
without duplicating the parser. A tool-level `--check-inputs`
flag (or a separate `op=check-inputs`) is the right shape.
**Parked as an itch** (one episode).

**Resolution (2026-09-12).** Solved by a new `op=check-inputs` in the
exts-owned `lean-verify` tool (`rushi-exts/goal-tools/lean-verify/`): it
runs the generator's lines through the
model executable (and the production executable when given) and
reports the first line either side rejects — a non-zero exit or a
timeout; accepted inputs exit 0. Default n=2000 makes the
preflight ~10 s instead of a 3 h full drt; `"stop_on_reject"`
stops at the first rejection, `"max_rejections"` bounds
collection. It shares the progress/checkpoint/resume machinery
with drt. The invariant "the generator emits well-formed lines"
moved from human discipline to an automated check. Covered by the
check-inputs steps of `scripts/lean-verify-drt-e2e.sh` and
`scripts/lean-verify-e2e.sh`.

## Model-settings defaults drift across binaries (2026-09-12) → resolved 2026-09-12

**Observed.** Model-settings resolution (per-model section lookup,
`max_output_tokens` / `context_tokens` fallbacks) lived in four
binaries with independent defaults. The stage binaries (`model`,
`assemble`, `compact`) used `max_output_tokens = 4096` and
`context_tokens = 131072`. The kernel and the `rushi setup`
template use 32768 and 262144. The 4096 cap truncated large
tool-call arguments mid-JSON. The parse stage logged
"Model emitted malformed tool arguments" on every retry. The
goal-continuation hook re-injects the goal each cycle, and the
loop stalls in a tight failure cycle.

**Reproduce.** Run a session against a local model with the
4096-token output cap. Ask the model to emit a tool call whose
arguments exceed 4096 tokens (a large `edit` or `write`). The
response cuts off mid-JSON. The parse stage hard-fails and the
loop retries indefinitely.

**Resolution (2026-09-12).** Two changes:

1. **Truncation detection in the model stage.**
   `bin/model/src/main.rs` now detects incomplete JSON in
   tool-call arguments. When `stop_reason` is `"stop"` and an
   argument is incomplete JSON, the model stage reclassifies to
   `"length"`. The parse stage's length-stop path logs the
   truncated group and emits a "re-issue with shorter arguments"
   tool result. The loop recovers instead of looping on a hard
   error.

2. **Shared model-settings resolution.**
   `crates/rushi/src/model_settings.rs` now owns the
   `ModelSettings` struct, the resolver functions, and the TOML
   helpers. All four binaries import from this module instead of
   keeping local copies. Defaults are `max_output_tokens = 32768`
   and `context_tokens = 262144`, matching the `rushi setup`
   template. A default change is now a single edit in one file.

## Nix-configured package: external hooks in `$out/hooks/` are unreachable (2026-09-15)

**Observed.** The `rushi-config` flake builds a configured rushi
package. Its generated `config.toml` names the external hooks by
bare binary name. `lib.mkRushi` bundles those hook binaries into
`$out/hooks/`. At runtime, every external hook firing fails to
spawn.

The session log records an error marker per hook:

```
{"id":"hook.run.idle.error","type":"ext_status",
 "value":"spawn harness-hook-goal-idle: No such file or directory (os error 2)"}
```

The failure was reproduced against the store package. The PATH had
only `$out/bin` plus the system dirs. Each external hook logged a
`hook.<window>.error` marker. The window then fell back to its
default decision.

The kernel-bundled `harness-hook-compact` resolves fine. It ships
in `$out/bin/`. Only the external hooks are unreachable.

**Root cause.** The kernel fires hooks with `Command::new(command)`.
That is a bare `PATH` lookup. No code searches the package
`hooks/` directory.

There is no resolver like the one tools have
(`resolve_kernel_tools_dir`). Nor one like the stage-binary
sibling resolution (`resolve_bin`). `lib.mkRushi` copies external
hook binaries into `$out/hooks/`. That dir is not on `PATH`.

Only `$out/bin/` is, via `mkShell` or `nix run`. The consumer
flake's comment says the kernel resolves commands from there. The
kernel never implemented that.

**Behavioral impact.** In the Nix build the external hooks are
dead. Goal continuation on `run.idle` dies. Goal compact on
`compact.before` dies. The tool guards on `tool.before` die.

Those guards are goal-tools, no-find-grep, and simple-english. The
model transforms on `model.before` die. Those are goal-arm and
simple-english. Every failed window applies its default decision.

The compact hook still works. Its binary ships in `$out/bin/`.

**Chosen fix: option 1, the kernel resolver.** The other two
options stay open as alternatives but were not needed.

**Resolution (2026-09-15).** Added `resolve_hook_command` in
`bin/rushi/src/config.rs`. `HarnessConfig::load` now resolves a
bare hook command against the sibling `bin/` dir first. Then it
tries the package `hooks/` dir. Then it falls back to the raw
name.

The raw name keeps the old `PATH` behavior. A name with a path
separator is used as-is, so dev configs are unchanged.

The Nix packaging needed no change. `lib.mkRushi` already ships
the external hooks in `hooks/` next to `bin/`. The generated
config already uses bare names. The kernel now resolves them.

Proof: unit tests in `config.rs` cover the sibling `bin/` hit, the
`hooks/` hit, the missing-everywhere fallback, and the
path-verbatim case. An end-to-end run of a fake Nix layout (PATH
holding only `bin/`) shows the `hooks/` hooks get invoked. No
`hook.<window>.error` markers appear.

**Note.** The kernel change sits in this repo's git working tree.
The `rushi-config` flake pulls the kernel by `git+file://`. So the
fix reaches a Nix build only after the kernel change is committed
and the flake lock is updated.

## G2b: tool side effects repeat after a mid-tool crash (2026-09-15)

**Observed.** The loop writes the `tool_call` event, then `route`
spawns the tool and writes the `tool_result` only after it finishes.
A kill -9 between those two writes leaves a `tool_call` with no
result. On restart, `claim` owes the result and the loop re-runs
the tool. Any side effect of the first run happens twice.

**Reproduce.** `scripts/crash-e2e.sh` test B: start `route` with a
slow bash call, kill -9 it while the tool runs, then run the
recovery. The log stays consistent (one `tool_result` per call),
but the tool's side effects ran twice.

**Status: parked (G2b of docs/refinement-policy.md).** The
log-completeness and no-dangling-state halves (G2a) are proven by
`scripts/crash-e2e.sh`. Closing this itch needs a design decision:
a `tool_started` marker event plus a recovery rule for
started-but-unresolved calls, or an explicit idempotent-tools-only
scope. Per P0, build it only when a real episode of a
non-idempotent tool losing work appears.

## Core-crate extraction stays parked (2026-09-16)

**Observed.** Question: any actual benefit to entering Phase 3
and building `crates/core`?

Reasons it is not worth it now:

- No recorded episode demands a core crate (P0).
- Utility-level duplication is already solved by `rushi-common`.
- The concrete benefits map to parked triggers: G8 overhead
  measurement is not built. No second concurrent session is in
  use. No plugin requirement exists.
- The typed vocabulary is one day old (2026-09-15), far short
  of the 20-session stability bar.
- The subagent kernel surface (`config_gen`, route env exports,
  shared tools module) fits in `rushi-common`. Phase 3 is not
  forced by it.

**Decision.** Stay on the not-yet list (`refinement-policy` P8).
Re-evaluate only when a trigger fires, as recorded in the
`architecture.md` Phase 3 re-evaluation gate: G8 fork/exec over
budget. Real multi-agent use. Replay/DRT as the main verification
vehicle. Or the P3 gate formally opening.

**Seed (do anyway).** The G1 double-step replay test (audit
finding F7) is the missing replay half of the P3 gate. It pays
off before Phase 3.

## Lean backstop fully retired (2026-09-17, user decision)

**Observed.** `lean-verify op=drt n=100000` waited 3 h with no
stdout (2026-09-12, the `lean-verify` drt episode above). The Lean
backstop was already optional ("the house gate remains the
conformance and e2e scripts", `docs/INDEX.md`). Actual use showed
the method was overkill for what it bought.

**Decision (2026-09-17, user).** Full retirement. Removed:
`lean/` (specs, lakefile, toolchain pin), `verification/rewind-drt/`
(+ its cargo workspace member), `scripts/lean-gate.sh`,
`scripts/lean-verify-e2e.sh`, `scripts/lean-verify-drt-e2e.sh`,
`scripts/rewind-drt-e2e.sh`, `scripts/rewind-drt-inputs.sh`, the
flake `aeneas` input, `devShells.lean`, `devShells.aeneas`, and the
Lean toolchain entries in `devShells.default`. The invariants keep
their Rust proofs (`rewind_*` tests, `scripts/e2e-rewind.sh`). The
exts-owned `lean-verify` tool is unaffected in this repo but no
longer gets a toolchain from the kernel devShell. See
`docs/lean-driven-development.md` §8.

## aarch64-darwin first-class + CI added (2026-09-17, user decision)

**Observed.** `docs/harness-distribution.md` (P5) already named
aarch64-darwin as a flake-managed author platform, but the flake's
`supportedSystems` listed only `[x86_64-linux aarch64-linux]`
(nixpkgs 26.11 dropped x86_64-darwin, so the darwin attrset was
skipped). The repo had no CI despite ~10 commits/day velocity and a
fully scripted house gate.

**Decision (2026-09-17, user).**
- `aarch64-darwin` added to `supportedSystems`: Macs build and
  develop rushi natively through the flake.
- GitHub Actions CI added (`.github/workflows/ci.yml`) at push/PR.
  The Lean gate is out of CI (retired above). Scope note: the e2e
  bash suites are Linux-only and run on the two Linux systems. The
  `aarch64-darwin` job carries the hermetic flake build, `cargo
  test`, and the doc gate. See the scoping entry below.
- `scripts/cache-e2e.sh` stays a local key-gated test: it needs a
  machine-local `config.toml` plus a live `DEEPSEEK_API_KEY`.
- `run-idle-continue-e2e.sh` remains a rushi-exts gate, not part of
  this repo's CI.

**Follow-up (consistency).** `lib/mk-rushi.nix` still declared the
configured package's `meta.platforms = platforms.linux` (nixpkgs' full
Linux arch list, no darwin), contradicting the flake's
`supportedSystems`. Aligned `lib/mk-rushi.nix` to declare exactly the
flake's three systems (`x86_64-linux aarch64-linux aarch64-darwin`),
with a comment pointing back to this decision.

## e2e suites scoped to Linux runners (2026-09-17, user decision)

**Observed.** CI run 35145471528 ran the full gate on all three
flake systems. The two Linux jobs passed. The `aarch64-darwin`
job failed at `e2e: compact` (46 of 98) after its `nix build`,
`cargo test`, and doc gate all passed. Every macOS failure was a
GNU-vs-BSD userland mismatch, not a rushi defect:

- `wc -l` pads the count on macOS, breaking the suites'
  `[ "$n" -eq 1 ]` numeric comparisons.
- `sed -i 's/…/' file` uses GNU in-place syntax. BSD `sed`
  rejects it ("sed: -I or -i may not be used with stdin").

The e2e bash suites target the GNU/Linux dev platform. Porting the
affected suites to BSD userland (wc padding, `sed -i`, `timeout`,
`xxd`, `sha256sum`) is a large slow-feedback effort, separate from
the CI-landing work.

**Decision (2026-09-17, user).** Scope the e2e suites to the two
Linux runners. Keep the hermetic build, `cargo test`, and doc gate
on all three.

- `.github/workflows/ci.yml` gains a matrix `e2e` flag: `true` on
  `x86_64-linux` and `aarch64-linux`, `false` on
  `aarch64-darwin`. The nine e2e steps are guarded by
  `if: matrix.e2e`.
- The macOS job still proves aarch64-darwin: `nix build .#rushi`
  packages darwin-arm64, and the full Rust test suite runs
  natively on the Mac runner.
- Follow-up, not in this PR: port the e2e bash suites to BSD
  userland so they run on macOS too. That is a separate effort,
  if macOS e2e coverage is wanted.

Corrects the `aarch64-darwin first-class + CI added` entry above:
the e2e suites do not run on `aarch64-darwin`.

## Flake packages gain standard `meta`; `maintainers`/`teams` left unset (2026-09-18, user decision)

**Observed.** `flake.nix`'s `packages.<system>` derivations
(`rushi`/`default`, `docs-md`, `docs-html`) carried no
nixpkgs-standard `meta`. `buildRustPackage` auto-fills some fields,
but `description`, `homepage`, `license`, and `mainProgram` were
absent.

**Decision (2026-09-18, user).**
- Add a nixpkgs-standard `meta` to every package in the flake's
  `packages` output:
  - `rushi` / `default` — `description` ("rushi — Unix-philosophy
    agent harness (kernel + distribution)"), `homepage`
    (`https://github.com/TonyWu20/rushi`, matches the Cargo
    manifests), `license = licenses.mit` (matches `LICENSE`),
    `mainProgram = "rushi"`.
  - `docs-md` / `docs-html` — `description`, `homepage`,
    `license = licenses.mit`. `docs-md` is a `nixosOptionsDoc`
    result, so `meta` is merged via `// { meta = …; }`.
- `meta.maintainers` / `meta.teams` stay **unset** (user picked
  option 1). Verified: `tonywu20` / `TonyWu20` is not a registered
  nixpkgs maintainer in the pinned rev (`lib.maintainers`,
  5211 entries, rev `801bef6a`). The "maintainerless" entry in
  nixpkgs' meta lint is informational only — no effect on build or
  distribution. Revisit only if rushi ever lands in nixpkgs proper
  (which would need a nixpkgs `maintainers/maintainer.json` PR
  first).
- No `teams` entry: no nixpkgs team owns rushi; claiming one
  (e.g. `teams.rust`) would misattribute maintenance.

**Follow-ups.**
- License mismatch, resolved 2026-09-18 (user decision: single MIT).
  `lib/mk-rushi.nix` now declares `meta.license = licenses.mit`,
  matching the repo `LICENSE`. Verified via
  `nix eval './tests/nix#packages.x86_64-linux.case-meta.meta.license.shortName'`
  → `"mit"`.
- `devShells`' `rushi` derivation carries no `meta`. User decided
  2026-09-18 this is fine: dev shells are not distribution
  packages. Closed, no follow-up.

## No front-end launcher subcommands in the kernel, and `rushi tui` to be retired (2026-09-20, user decision)

**Observed.** PR #23 (`rushi serve`, opened 2026-09-20) adds a
second front-end launcher subcommand to `bin/rushi`.
It resolves a `rushi-web` binary (`[web].binary`, then side-by-side,
then PATH), forwards `--sessions-root`, `[web].host`, `[web].port`,
and `--loop-cmd "<exe> run"`, and exits with the child's code.
It mirrors the existing `rushi tui` arm.
The webui repo carried a private kernel patch so it could pin the
kernel commit that ships the launcher.

**Decision (2026-09-20, user).** PR #23 is not supported.
The kernel does not host per-front-end launcher subcommands.

- `serve` is a thin wrapper. `rushi-web` can do its whole job
  itself. It reads the config TOML and spawns `rushi run` on PATH.
  This passes the P4 pre-test (config or script, no protocol change).
- P2 rule of three. `rushi tui` was the first need, inlined as early
  coupling of the then-sole UI into the kernel CLI. `serve` would be
  the second, and the pattern grows one subcommand per front-end.
  The kernel stays front-end-agnostic per
  `docs/skill-remapped-to-os-apps.md` §2. Tier 2 is swappable.
  The base includes a default front-end but does not mandate one.
- `rushi tui` is superseded by invoking the front-end binary
  directly from the rushi-tui repo, a `rushi-tui` command.
  Removal is a separate, user-visible change tracked here.
  It is not part of PR #23.
- P3 note (2026-09-20): promoting the config resolver into
  `rushi-common` waives P3's "three copies or diverged bug"
  trigger on purpose.
  The recorded self-wiring decision creates the second and third
  consumers (rushi-tui, rushi-web) before any copies exist.
  Sharing now is the cheaper path.

**Follow-ups (open, P0-gated. Build only on a recorded episode):**
- [ ] Remove `Command::Tui`, `resolve_tui_binary`, and
      `config_tui_binary` from `bin/rushi/src/main.rs`.
      Decide the bare-`rushi` default.
      Today `rushi` with no args launches the TUI (main.rs:123).
- [ ] Update the README subcommand table and the `[tui]` section of
      `docs/reference/README.md`.
- [x] flake.nix devShell comment that documents the TUI-on-PATH
      lookup.
      Done 2026-09-22 in issue #31.
      The comment now documents the `rushi-tui` lookup
      (side-by-side, then PATH).
- [x] Config-path discovery for self-wired front-ends.
      Done 2026-09-20.
      The 4-step resolver ($CONFIG > CLI > Nix side-by-side > CWD)
      and `resolved_exe()` (issue #25 symlink recovery) now live in
      `rushi-common` (`crates/rushi/src/paths.rs`).
      `bin/rushi` calls the shared function.
      Front-ends get identical resolution by linking the crate they
      already path-depend on.
      No kernel endpoint is needed, so the
      `rushi config --print-path` idea is retired.
- [ ] rushi-tui repo: ship the entry point as one `rushi-tui`
      command. It resolves its own config (`$CONFIG`, `--config`,
      or CWD) and owns session/loop wiring.
- [ ] rushi-web repo: self-wire. Read the config TOML and spawn
      `rushi run`. Drop the private kernel patch. Pin only the
      Tier-1 CLI contract, not a launcher commit.

**Remote record:** PR #23 review comment at
`github.com/TonyWu20/rushi/pull/23#issuecomment-5759736599`.
