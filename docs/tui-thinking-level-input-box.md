# TUI thinking-level input box border (docs/tui.md section 7.2)

Status: shipped (2026-09-02). The spec is `docs/tui.md`
section 7.2; the shared-UI-state channel and the id registry entry
are `docs/ui-extension.md` section 5. This doc is the plan,
contract, and record. Sections 2 and 4 are the contract.

Revision (2026-09-02, same day): user report — the reference
statusline's generic `ext_status` pill dump ran the
`model_thinking=4` and `loop_phase` values together with no
separator, and the bare number was not an intuitive
presentation. Both statusline implementations (the bash
reference and the Rust port) dropped the `ext_status` dump:
shared UI state is host presentation, not footer content. The
level now presents through the input-area border color only;
the number stays in the log and in `--describe` output, both
machine-facing. The `statuses` map stays on the tick payload
for consumers that want it.

## 1. Purpose and justification

Section 7.2 of `docs/tui.md` specified that the input-area border
color correlates with the active model's thinking level: the level
is published into the log as an `ext_status` event with id
`model_thinking`, and the TUI maps the last value to a border
color (0 gray, 1 blue, 2 cyan, 3 green, 4+ yellow).

The TUI's read side had shipped with the loop-phase work: the
`model_thinking` constant, the level read, the border color, the
frame-extension override, and the spinner tint. But nothing in the
repository published the level. No `model_thinking` event existed
in any session log, so the border rendered its default gray
forever. The item was specified, read side built, and the publish
side — the half that makes the color move — was missing.

The publish side is this change. The loop is the publisher:
"Published by the loop or a policy hook" (ui-extension.md
section 5). The loop owns the model config; it is the only
component that knows which effort the next model call carries.

## 2. Contract

The contract is additive. It reuses the `ext_status` event. No new
event type. No schema change. The schema is
`schemas/events/v1/ext_status.json`. Its `value` field allows any
JSON. This contract fixes one id and one value scale on that
field:

- id: `model_thinking`
- value: an integer, 0-4

| Level | Meaning | Border |
|---|---|---|
| 0 | no thinking (default) | gray |
| 1 | low | blue |
| 2 | medium | cyan |
| 3 | high | green |
| 4+ | highest | yellow |

The mapping is host presentation only: the TUI does not decide the
level. It renders whatever the loop or a policy hook published.
The `frame` extension may override the border color; without one,
the host's built-in palette above applies.

### Publish rules (loop side, `scripts/step.sh`)

- The step publishes the level at every step entry, before the
  claim. The level is a session attribute, not a phase attribute:
  the border reflects the active model even while the step routes
  tools or the session idles.
- The level is the active model's *configured* thinking. The step
  resolves it through `bin/model --describe` — the same config
  resolution as the API call itself: `[active].model` (or the
  `MODEL` env var) selects the model,
  `[model.<active>].reasoning_effort` overrides
  `[model].reasoning_effort`, missing keys default to `medium`,
  and `off` normalizes to `none`. A published level therefore
  matches what the request actually carries.
- The effort-to-level mapping is `bin/model::thinking_level_for`:
  `none` to 0; `minimal` and `low` to 1; `medium` to 2; `high`
  to 3; `xhigh` and `max` to 4; an effort outside the documented
  set to 0 (the level never claims a thinking the request does
  not carry).
- The publisher sends only on change (ui-extension.md section 5):
  the step reads the last `model_thinking` value in the log (a
  4096-line tail window) and appends only when it differs or was
  never published. The effort is frozen call config, so a
  session carries one event per value. A mid-session config edit
  publishes the new value on the next step.
- The marker appends through `bin/log` with schema validation. A
  failed append aborts the step, like every other append in the
  script. A failed `--describe` (no binary, broken config) skips
  the publish; the TUI falls back to its default level and the
  loop is unaffected.

### Render rules (TUI side, `bin/tui`)

The read side predates this doc. The contract is fixed here:

- `app.thinking_level()` reads the last `model_thinking` value of
  the active session's log. 0-4 pass through. A value outside
  the range clamps to 4 (the `4+` bucket). A missing event or a
  non-integer value falls back to 0. The state is a pure function
  of the log: it survives a TUI restart and switches with the
  session.
- `render::thinking_border(level)` maps the level to the host
  palette: 0 `DarkGray`, 1 `Blue`, 2 `Cyan`, 3 `Green`, 4+
  `Yellow`. The input-area border, the built-in title label's
  background, and the working-row spinner (the model-wait
  indicator) all render in this color.
- A `frame` extension's label style overrides the border color.
  Without a frame extension, the built-in palette applies.

## 3. Behavior

State transitions, all observable in the log:

1. The first step of a session appends
   `model_thinking=<level>` once. The TUI recolors the border
   from that event on.
2. The next step reads the last value, sees it unchanged, and
   appends nothing. The log stays quiet.
3. The user edits `reasoning_effort` in the config. The next step
   resolves the new effort and publishes the new level. The
   border recolors.
4. The TUI restarts. The per-id value map rebuilds from the log.
   The border restores without a new marker.
5. A session log holds no `model_thinking` event (an old log).
   The default level 0 shows. The border is gray.

## 4. Failure modes

| Condition | Behavior |
|---|---|
| `--describe` fails (no binary, broken config) | The step skips the publish. The TUI shows the default level. The loop itself is unaffected. |
| Marker append fails | The step aborts with exit 1. Same as every other append. |
| Value is not a non-negative integer | The TUI falls back to 0 (the default). No crash, no fallback event. |
| A policy hook publishes a later value | Log order wins: the last `model_thinking` event is the level, whatever published it. |
| More than 128 distinct ext_status ids | The oldest-updated id drops from the per-id map. `model_thinking` drops only under that pressure. |
| The old marker ages out of the 4096-line tail window | The step republishes the same value once. The TUI takes the last event; the log quiets again. |

## 5. Conformance tests

Every row is a check.

| Test | Given | Expected |
|---|---|---|
| Mapped value | `bin/model --describe` on a config with `reasoning_effort = "xhigh"` | The output carries `"thinking_level": 4` and the effort string the request sends. |
| Per-model override | `[model.alpha]` sets `medium`, `[active].model = "alpha"`, the global is `high` | `--describe` reports `medium` and level 2. |
| `MODEL` env | `MODEL=beta` selects a model whose effort is `off` | `--describe` reports effort `none`, level 0. |
| Unknown effort | A model sets `reasoning_effort = "weird"` | The level is 0. The level never claims a thinking the request does not carry. |
| Marker in log | A session log holds `model_thinking=4` | The TUI renders the input-area border in yellow (`38;5;3` on a 256-color terminal). |
| No marker | A session log holds no `model_thinking` event | The border is gray (`38;5;8`), the default level. |
| On-change gate | Two consecutive steps, unchanged config | One `model_thinking` event in the log after the first step. The second appends nothing. |
| Config edit | The effort changes between steps | The next step publishes the new level. |
| Unit, loop side | The mapping table in section 2 | `bin/model` tests cover every documented effort, case-insensitivity, and the unknown-effort fallback. |
| Unit, TUI side | Last value wins, out-of-range clamp, non-integer fallback, watch-event update, session switch | `bin/tui` tests in `app.rs` (`thinking_level_*`) and `render.rs` (`thinking_border_maps_the_level_palette`). |
| PTY capture | `scripts/capture-thinking-border.py` runs the TUI under a pty against a marker log and a marker-free log | The marker session emits the yellow-family SGR; the plain session emits gray and no yellow. |
| Statusline dump | The same pty run with the reference statusline enabled (`scratch/ext-only` layer) | The footer renders its usage pill and holds no `model_thinking` or `loop_phase` text. The 2026-09-02 revision dropped the `ext_status` dump from both statusline implementations. |

## 6. What this does not do

- No capture of the model's *actual* thinking. The open feature
  request (capture the thinking block into the log, display
  behind a toggle) stays open in
  `docs/tui_feature_requests_from_human.md`. This is the
  configured level only: what the request asks for, not what the
  model returned.
- No new event type. No schema change. No protocol change.
  `ext_status` carries the value, as it carries `loop_phase`.
- No per-tick publish. The on-change gate keeps the log quiet.
- No TUI decision logic. The TUI reads one published value and
  maps it to a color. The mapping table is presentation.

## 7. Impact

Affected: `scripts/step.sh` (one helper, one call site),
`bin/model` (the `--describe` mode and the effort-to-level
mapping), `bin/tui` (tests only — the render path already
existed), and the docs named in section 11.

Unaffected: `bin/claim`, `bin/assemble`, `bin/parse`,
`bin/route`, `bin/log`, every schema, and the extension
protocol. The TUI adds no loop decision logic and no storage
literals. The guardrail tests stay green.

## 8. Migration

The change is additive. No `v` bump. Old logs hold no
`model_thinking` event. They replay into the default level:
the gray border, as they did before. Every consumer falls
back:

- The TUI shows the default level.
- The reference statusline shows no `model_thinking` pill. The
  2026-09-02 revision dropped its generic `ext_status` dump;
  before that revision the reference row listed it among up to
  two values.
- A `jq` reader finds no event. Nothing breaks.

The id already has a registry entry in `docs/ui-extension.md`
section 5. This ship makes the entry live.

## 9. Trade-offs, recorded

- The effort-to-level mapping lives in `bin/model`, next to the
  resolution it must match. The six accepted thinking levels
  (`minimal`, `low`, `medium`, `high`, `xhigh`, `max` — `none`
  turns thinking off) collapse into the host palette's five colors:
  `minimal` shares the low bucket with `low`, and `max` shares the
  top bucket with `xhigh` (the `4+` row of the spec's table).
- The on-change gate scans a 4096-line tail. A marker older than
  the window republishes the same value once. It is a no-op for
  the TUI, which takes the last event.
- `--describe` starts the model binary once per step. It reads
  the config and exits; it makes no network call and touches no
  log. The cost is a process start per step.
- The level is config-derived. A model that thinks more or less
  than its configured effort does not move the color. That is
  the spec: "the TUI does not decide the level, it renders
  whatever the loop or a policy hook published."

## 10. Rejected alternative

An awk TOML scrape in `step.sh` would have resolved the effort
independently of `bin/model`. The resolution is a precedence
chain (the env var, the per-model table, the global table, the
default) that the model binary already owns. A second copy would
drift, and a drifting level would make the border lie about
what the request carries. The publish side therefore asks the
model binary for the answer: `--describe` is a read-only mode of
the existing resolution. A policy-hook extension as publisher was
also considered: no hook knows the model config. The loop owns
it.

## 11. Implementation notes

This section is non-normative. The contract is sections 2 and 4.

Loop side:

- `scripts/step.sh` gains `publish_model_thinking`. It runs the
  helper at every step entry, before the claim. The helper
  resolves the level through `bin/model --describe --config
  $CONFIG`, gates on the last value in a 4096-line tail of the
  log, and appends the event with `jq -cn` through `bin/log`
  with the schema dir. The `ts` comes from
  `date -u +%Y-%m-%dT%H:%M:%SZ`, the same form every other
  event in the script uses.
- `bin/model` gains the `--describe` flag. It resolves the
  active model and the effort exactly as the call path does,
  normalizes `off` to `none`, and prints one JSON object
  (`active`, `reasoning_effort`, `thinking_level`). It makes no
  API call and reads no stdin.

TUI side:

- No new code. `app.rs` holds `THINKING_STATUS_ID`,
  `THINKING_LEVELS`, `DEFAULT_THINKING_LEVEL`, and
  `thinking_level()`. `render.rs` holds `thinking_border` and
  applies it to the input-area border, the built-in label
  background, and the working-row spinner. This ship adds the
  tests: `thinking_level_*` in `app.rs` and
  `thinking_border_maps_the_level_palette` in `render.rs`.

Docs on ship: `docs/ui-extension.md` section 5 names the loop
as the publisher. `docs/loop-and-edit-implementation.md` notes
the publish in the config section. This doc lands in
`docs/INDEX.md`.

## 12. Acceptance

- `cargo test --workspace` passes.
- `bin/model --describe` reports the active model, the effort as
  sent, and the 0-4 level.
- One loop step appends exactly one `model_thinking` event. A
  second step appends none.
- `scripts/capture-thinking-border.py` passes: the marker log
  renders a yellow input-area border, the marker-free log
  renders gray.
- With the reference statusline enabled, the footer shows no
  `ext_status` values: no `model_thinking` or `loop_phase` text,
  no `st:` section. The level presents through the border color
  only; the number stays in the log and in `--describe` output.

## Properties

Lean-style invariants for this spec (see `lean-driven-development.md`).
One property per non-trivial invariant. Each property is observable:
given an input, an output guarantee.

P1. effort-level-map: given a configured reasoning effort, observe
    `bin/model --describe` reports the mapped level: `none` to 0,
    `minimal`/`low` to 1, `medium` to 2, `high` to 3, `xhigh`/`max`
    to 4, and an unknown effort to 0.
P2. level-read: given the last `model_thinking` value in the log,
    observe the TUI level tracks it, a later event wins, an
    out-of-range value clamps to 4, and a missing or non-integer
    value falls back to 0.
P3. border-color-map: given a thinking level, observe the
    input-area border, the title-label background, and the spinner
    render in the level's color (0 gray, 1 blue, 2 cyan, 3 green,
    4+ yellow).
P4. restart-rebuild: given a TUI restart, observe the per-id value
    map rebuilds from the log and the border restores without a new
    marker.
P5. pty-capture: given the TUI run under a pty against a marker log
    and a marker-free log, observe the marker session emits
    yellow-family SGR codes and the plain session emits gray with no
    yellow.
P6. publish-on-change: given two consecutive steps with an
    unchanged config, observe the first step appends one
    `model_thinking` event and the second appends nothing.

## Verification

Each property maps to its proof. `proven` means the cited test or
script exists and passes. `open` names the blocker and what unblocks
it.

| P# | Property | Proof | Status |
|----|----------|-------|--------|
| P1 | effort-level-map | `thinking_level_maps_the_documented_efforts`, `thinking_level_is_case_insensitive_and_unknown_is_zero` in `bin/model/src/main.rs` | proven |
| P2 | level-read | `thinking_level_tracks_the_last_published_value`, `thinking_level_later_event_wins`, `thinking_level_clamps_an_out_of_range_value`, `thinking_level_falls_back_to_the_default` in `bin/tui/src/app.rs` | proven |
| P3 | border-color-map | `palette_thinking_border_tracks_the_levels` in `bin/tui/src/color.rs` | proven |
| P4 | restart-rebuild | `thinking_level_session_switch_rebuilds` in `bin/tui/src/app.rs` | proven |
| P5 | pty-capture | `scripts/capture-thinking-border.py` | proven |
| P6 | publish-on-change | Blocked: no test drives the `scripts/step.sh` on-change gate. Unblock with a step-level check that a second step appends no `model_thinking` event when the config is unchanged | open |

## Gate

The acceptance commands. All must exit 0 for this spec to be proven.

```
cargo build
cargo test
scripts/capture-thinking-border.py
```
