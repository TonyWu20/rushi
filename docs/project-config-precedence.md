# Project config precedence — the layered `config.toml`

Decision record (refinement-policy P6 + P10). User decision 2026-10-14:
option 2b, layered merge, uniform rule. The Nix store `config.toml`
is the base layer; any other config file is a top layer that
deep-merges over it.

## Observed

Headless missions launch as `cd <worktree> && exec rushi run <s> <task>`
(`rushi docs headless-missions`). The launch carries no `--config`.
On a Nix install the store config wins the resolution order, so a
project `config.toml` is dead unless someone remembers the flag.
The remembered flag is a discipline step in every mission script,
TUI launch, and `user` poke.

A partial top config also downgrades silently: a file holding only
`[system_prompt]` drops the machine model section, and the active
model falls to the kernel default (`deepseek` on `127.0.0.1:8080`).
Proven this session via `assemble` on a scratch session
(`/tmp/rushi-tiny-test`): request JSON shows `model: deepseek`,
`tools: []`.

## Reproduce

```
# before: partial config, Nix-style store base present, no layering
target/release/assemble --session <scratch> --config /tmp/tiny.toml
# request JSON: "model": "deepseek", "tools": []
# after: the store base layers under the partial file
# request JSON: machine model, machine tools, the partial prompt
```

## Pre-test

Config/script workaround: `export CONFIG=$PWD/config.toml` in the
mission script. Rejected (user, 2026-10-14): it leaves the discipline
hole open. TUI launches and `user` pokes do not run mission scripts.

## Proposal

New `rushi_common::config_layer` (P3: the read-parse-load copy sits
in six binaries, above the three-copy threshold).

- `find_base_layer()` — the Nix side-by-side `<exe_dir>/../config.toml`
  (the `resolve_config_path` tier 4 location after the 2026-10-14
  fix; tier 3 before it). None in dev checkouts and plain
  installs.
- `deep_merge(base, top)` — RFC 7386 style. Table keys recurse.
  The top value replaces on every other combination (scalar, array,
  table-vs-other). Base keys absent from the top survive.
- `load_layered(top) -> ConfigLayers { value, top, base }` — the
  merged value, the unmerged top layer, and the base path.
  `load_layered_with_base(top, base)` takes an injectable base for
  tests. `top == base` (string or canonical path) parses alone.
- One uniform rule (user decision): every source layers over the
  store base. `--config` and `$CONFIG` included. The loop forwards
  its resolved top path to the stages; each stage re-derives the same
  base from its own exe. No stage-protocol change.

Consumers switched to `load_layered` (six): the loop
(`HarnessConfig::load`), `assemble`, `model`, `parse`, `compact`,
`user`. `route` is not a consumer: it takes tool paths as CLI args
and parses `tool.toml` manifests, not the harness config.

Validation on the merged value:

- `legacy_key_report` and the pipeline step-without-def error run on
  the merged value. A project pipeline may reference machine defs.
- The dead-def warning is scoped to top-layer defs
  (`dead_def_warnings` in `bin/rushi/src/config.rs`). A machine def
  displaced by a top pipeline rewrite never warns: the user cannot
  edit a store-path file.

`rushi config` stays a top-layer dump: it prints the file the user
edits and git-tracks. A merged view would carry machine-specific
store paths into the tracked file.

## Impact

- Six consumer binaries plus `rushi-common`. No new binary, no new
  flag, no event change, no `v` bump.
- Front-ends (rushi-tui) import `resolve_config_path`; adoption of
  `load_layered` is a one-liner when they read the config.
- Nix installs: behavior changes by design (the store config becomes
  a base layer). Dev checkouts and plain installs: no base file
  exists, behavior is unchanged.
- Relative tool paths in the base resolve against the top layer's
  config dir. The Nix generator emits absolute store paths, so the
  base carries no relative entries in practice.

## Migration

Additive. Old sessions replay untouched (config is not an event).
Store-alone and no-base runs behave exactly as today.

## Properties (frozen before implementation)

| Property | Proof (named test / scenario) |
|---|---|
| P-L1 a partial top over a base: base keys survive, top keys win | `load_layered_with_base_merges_and_records` (config_layer) |
| P-L2 nested table keys recurse; base keys absent from top survive | `deep_merge_table_recurse` (config_layer) |
| P-L3 a top scalar or array replaces the base value | `deep_merge_scalar_and_array_replace` (config_layer) |
| P-L4 no base: value equals the top file verbatim, `base == None` | `load_layered_no_base_is_verbatim` (config_layer) |
| P-L5 top is the base file itself: parses alone, no self-merge | `load_layered_same_file_parses_alone` (config_layer) plus `scripts/config-layer-e2e.sh` scenario 5 (the CWD tier path, the canonicalize branch of the same-file guard) |
| P-L6 fake Nix layout + partial project config: request JSON carries the base model, the top prompt, the base tools | `scripts/config-layer-e2e.sh` scenario 1 |
| P-L7 store config alone: request JSON equals today's behavior | `scripts/config-layer-e2e.sh` scenario 2 (the flag path) and scenario 4 (the no-flag path: no CWD config, the side-by-side tier resolves the store config alone) |
| P-L8 a base-layer def referenced by no pipeline never warns; an unreferenced top-layer def warns | `dead_def_warnings_scoped_to_top_layer` (bin/rushi config) |
| P-L9 a legacy key in either layer is a load error on the merged value | `legacy_key_in_base_layer_is_a_load_error` (config_layer) |
| P-L10 a top file read/parse error propagates as `Err` | `load_layered_top_errors_are_err` (config_layer) |

## Consequence table (acceptance)

The user's acceptance criteria for the layered config. Every row
holds in `rushi-common` 0.1.7.

| Situation | Behavior | Proof |
|---|---|---|
| Nix install, CWD has a project config | the project config layers over the store base | P-L11, scenario 3 |
| Nix install, no CWD config | the store base alone, today's behavior | P-L7, scenario 4 |
| `--config partial.toml` anywhere on a Nix machine | the partial top layers over the store base. No silent model downgrade, ever | P-L6, scenario 1 (asserts the base model and the base tools survive) |
| Dev checkout / plain install | no base file exists, the merge is a no-op, today's behavior unchanged | P-L4, `load_layered_no_base_is_verbatim` plus `find_base_layer_is_none_in_dev_layout` |
| Top equals base (CWD is the store dir) | parses alone, never merges a file over itself | P-L5, unit test plus scenario 5 |

The tier ladder runs in the `rushi` loop binary only
(`rushi_common::paths::resolve_config_path`). The stage binaries
(`assemble`, `model`, `parse`, `compact`) and the `user` binary keep
the CWD-relative `--config` default: no flag means `config.toml` in
the CWD, and a missing file is a load error. Layering still applies
to them at load (P-L6, scenario 1): an explicit `--config` top
layers over the side-by-side store base when one exists. Extending
the tier ladder to the stage binaries is an open decision, not a
silent change.

## Gate

```
cargo test -p rushi-common
cargo test -p rushi -p assemble -p model -p parse -p compact -p user
cargo build --release
bash scripts/config-layer-e2e.sh          # scenarios 1-5 (P-L6, P-L7, P-L11, P-L7 no-flag, P-L5 CWD-is-store)
```

A clean gate with zero open properties is the guarantee.

## Revision 2 (2026-10-14): the CWD tier beats the Nix tier

The first revision kept the pre-existing resolution order (Nix
side-by-side tier above the CWD tier). That order made the design
case impossible on a Nix install: the store config always exists at
the Nix tier, so the CWD tier never ran. The resolved top layer was
the store file itself, equal to the base layer, and a project
`config.toml` was dead.

The defect was caught in the field. A Nix devShell running
`rushi 0.1.2` (the first revision) launched from a project CWD
with a partial `config.toml`. `rushi config` dumped the store
file, not the project file. The session log at
`/export/castep-improve-dm/sessions/check-system-prompt/` shows
the agent running on the machine system prompt, not the project
one.

Fix: the CWD `config.toml` tier now sits above the Nix
side-by-side tier (revision-2 resolution order, reference section 4).
When the CWD file wins, the store config is the base layer under it.
When the CWD has no config, the store file is the top layer and
parses alone (top equals base, the same-file guard applies). Dev
checkouts and plain installs are unchanged: no Nix side-by-side
file exists.

New property:

| Property | Proof (named test / scenario) |
|---|---|
| P-L11 no-flag launch from a CWD that holds `config.toml` resolves that file as the top layer, ahead of the Nix side-by-side file | `scripts/config-layer-e2e.sh` scenario 3: a real `rushi` binary in a fake Nix layout, run from a project CWD, dumps the project file via `rushi config`. A process-level chdir unit test is deliberately not added: CWD is process-global and would race the parallel test harness. |

Gate addition: scenario 3 of `scripts/config-layer-e2e.sh` (needs
the `rushi` loop binary built: `RUSHI_BIN=target/release/rushi`).

## Out of scope

- A merged-view dump in `rushi config` (`docs/config-dump.md` keeps
  its ruling: no resolved view).
- Array concat (union) for tool-path lists. Replace is the rule.
  Omit the key to inherit.
- CWD directory-tree search for the top layer. The top layer is the
  launch CWD only.
