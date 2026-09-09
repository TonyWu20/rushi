# System prompt generation

Status: Implemented (2026-09-13).
This doc replaces the mechanism in `../rushi-exts/docs/goal-rule-reinject.md`.
It keeps the decision items D1-D3 and the cache tradeoff analysis.

## How the prompt is built

The system prompt builds in two layers.
The kernel always starts from the user config `[system_prompt]` field.
That is the fixed starting point.
The kernel adds a generated tool list and the working directory line.
Extension hooks then add, edit, or remove named prompt fragments on demand.
The kernel joins the fragments into the final prompt.

The split of work:

- the user config owns the base text
- each extension owns one named fragment
- the kernel owns the join step

The kernel does not read the fragment text.
It treats a fragment as a named entry.
That keeps the kernel free of extension logic.

## Problem

Three issues surfaced after the TUI/extension repo split and the
config-driven `extra_tools_roots` mechanism.

### 1. The tool list is stale

`config.toml` hardcodes the `Available tools` list in
`[system_prompt]`. It names `read`, `write`, `edit`, and `bash`.
The real dispatch set is wider.
It is the union of the `tools_root` and `extra_tools_roots`
manifests. The current set holds 8 tools (4 kernel, 4 goal
extension).

The model already gets the full tool schemas.
`bin/assemble` builds the `tools` array of the model request.
So the model can call `goal_complete` and the other goal tools.
The prose list lags behind and drifts.

Fix: generate the list from the discovered manifests.
Stop maintaining it by hand.

### 2. The goal block rides as a user message

Today `hook-goal-arm` appends the goal block to the end of
`request.input`. It is the last user-role message.
The model re-acknowledges it every turn.

The cleaner target: put the goal block in the system prompt.
The model reads the goal context once.
This is already the target in `../rushi-exts/docs/goal-rule-reinject.md`.

### 3. Many extensions will want prompt text

Goal mode is the first consumer of added prompt text.
Future extensions add their own slices.
Examples: a "strict-mode" fragment, a "style" fragment.
If each extension edits one shared string, every add or remove
scans the whole prompt. That is O(n) over the full text.
A map keyed by extension gives O(1) add or remove.

## Design

### The final layout

The system prompt (`request.instructions`) is composed of:

```
instructions = base_prompt + generated_tool_list + cwd_line
             + join( fragments )
```

- **`base_prompt`**: the config `[system_prompt].text`, minus the
  old hardcoded tool list. The kernel owns it. Extensions never
  change it.
- **`generated_tool_list`**: built by `assemble` from the
  discovered manifests. Byte-stable per session.
- **`fragments`**: one entry per extension. Each extension owns
  exactly one key.

The fragment store is an ordered map.
Implementation: `IndexMap<FragmentId, String>`.
Equivalently: a `HashMap` plus a `Vec` that keeps insertion
order.

### The fragment key

A key is a `String`. Use `&'static str` for well-known keys.
Goal mode uses `"goal"`.
Future extensions register their own keys.
The key is the only place where the kernel and an extension meet.
The kernel never inspects the fragment text.

### Costs

- Add or replace a fragment: `map.insert(key, text)`. One step.
- Remove a fragment: `map.remove(key)`. One step.
- Build the prompt: `base + tool_list + cwd + join(fragments)`.
  This is O(n) where n is the number of active fragments.
  In practice n is 1-3.
- No extension scans the base prompt or other fragments.
  Each extension touches only its own key.

### Ordering and cache stability

The map orders by first insertion.
A fragment keeps its position once inserted.
The joined prompt stays byte-stable as long as the set and order
of active fragments does not change.
Adding a new fragment invalidates the prefix once.
Removing one invalidates it once.
Steady-state turns are byte-identical. The provider prefix cache
keeps hitting.

For goal mode, `"goal"` is the only fragment.
Ordering is trivial.
If a future extension inserts a fragment with a lower sort key,
it shifts the goal fragment position.
That happens on the set call, not on every turn.
The cost is one re-prefill, the same as today.

### The generated tool list

`bin/assemble` already finds the tool manifests.
It reads `tools_root` and `extra_tools_roots` from the config.
It emits them as JSON schemas into the `tools` array.

It should also render a readable tool list.
It injects that list into `instructions`.
That replaces the hand-maintained block in
`config.toml [system_prompt]`.

Concretely: after `tool_schemas` is built (currently around line
1199 of `bin/assemble/src/main.rs`), render:

```
Available tools:
- read: …
- write: …
- edit: …
- bash: …
- goal: …
- goal_blocked: …
- goal_complete: …
- lean-verify: …
```

Each `…` is the `description` field from `tool.toml`.
When that field is absent, use the tool name.
Order: primary `tools_root` first, alphabetical.
Then each `extra_tools_roots` entry in config order,
alphabetical within each.
This matches dispatch precedence: the primary root wins on a
name collision.

`config.toml [system_prompt].text` drops the `Available tools:`
block. It keeps only the generic guidance paragraph.
`assemble` appends the generated list before the cwd line.

**Why `assemble`, not a hook?**
The tool list is a pure function of the config and the on-disk
manifests. It is identical on every model call in a session.
Manifests do not change mid-session.
`assemble` already owns the config read and the schema discovery.
So no hook is needed. No per-call transform. No cache cost.

### The hook contract

The `model.before` window already lets a hook replace the whole
request JSON. We extend that contract:

- The `request` object gains an optional `prompt_fragments` field.
- The wire form is an ordered array of `[id, text]` pairs:
  `[[id, text], …]`. Not a plain JSON object. JSON object key
  order is unspecified. The join must be deterministic for cache
  stability. The ordered array makes the order explicit.
- A hook that adds a fragment reads the current
  `prompt_fragments`. It sets or removes its key. It returns the
  modified request.

The kernel joins the final `prompt_fragments` into
`request.instructions` after the hook chain. The kernel code is
`apply_model_before_transform` in `bin/rushi/src/step.rs`.

The kernel is a generic joiner.
It does not know which fragments exist.
It just joins them.
The join is O(k), where k is the number of active fragments.
It is not O(n) over the prompt text.

This keeps the kernel at the starting point: the config base
prompt, the generated tool list, and the cwd line.
All prompt mutation belongs to the hooks.

For the goal case, the hook is `hook-goal-arm`:

| Goal state                 | Hook action                              |
| -------------------------- | ---------------------------------------- |
| No goal                    | `prompt_fragments["goal"]` is absent     |
| Goal open or blocked       | `prompt_fragments["goal"]` = fenced goal block |
| Goal done (`goal_complete`) | hook sees `is_open() == false`, removes the `"goal"` key |

`goal_blocked` keeps the goal open. Its state is blocked, not
completed. So the fragment stays.
Only `goal_complete` sets the state to completed.
Only that triggers the removal.
This matches the requirement: **only `goal_complete` clears the
goal fragment**.

### The goal block content

Unchanged from `../rushi-exts/docs/goal-rule-reinject.md`.
The block is the fenced
`<goal_instructions>…</goal_instructions>` region.
It holds the objective, the 10 rules, `<goal_id>`, the
goal-tools hint, and the "no token budget" line.
It is a byte-stable pure function of `(goal_text, goal_id)`
(P17).

The `goal` tool schema is filtered from `request.tools` in every
state (D1).
The `goal_complete` and `goal_blocked` schemas are present only
while a goal is open.

### Config changes

In `config.toml` and `config-low.toml` `[system_prompt]`:

- remove the `Available tools:` bullet list from `text`.
- add a comment: `# The tool list is generated by assemble from tools_root + extra_tools_roots.`

No new config keys are needed.
The fragment store lives in memory per model call.
The hooks set its contents. The config does not.

## Concrete changes

### 1. `bin/assemble/src/main.rs` (kernel)

- After `tool_schemas` is built, render the tool list from the
  same manifests. Use the name and the `description` from each
  `tool.toml`.
- Insert the list into `system_prompt` after the config base text
  and before the cwd line.
- Accept an optional `--fragments <json>` CLI argument. Or read
  the fragments from the session dir. This carries the current
  `prompt_fragments`.
- Append the fragment values to `instructions` after the cwd
  line. When the argument is absent, the field is simply absent
  (a no-op).
- The joined `instructions` is byte-stable while the fragment set
  does not change.

### 2. `bin/rushi/src/step.rs` (kernel)

- In `apply_model_before_transform`, after the hook chain: if the
  transformed request carries `prompt_fragments`, join them into
  `request.instructions` in stable order.
- Delete the `prompt_fragments` field before the request goes to
  the model. This is the generic kernel join step. One pass, with
  no knowledge of fragment meaning.
- Log a `hook.model.before.transform` event with the fragment key
  list. Do not log the values.

### 3. `rushi-exts/goal-hooks/hook-goal-arm/src/main.rs` (exts)

- Replace the `input` append with `prompt_fragments`.
- Goal open: `request.prompt_fragments["goal"] = fenced_block`.
- Goal closed or absent: delete
  `request.prompt_fragments["goal"]`.
- Also manage the `request.tools` filtering per D1. The `goal`
  schema is filtered always. The `goal_complete` and
  `goal_blocked` schemas are added or removed with the open
  state.
- Drop the trailing `input` item and the `instructions` fallback
  from the current code.

### 4. `rushi-exts/goal-hooks/hook-goal-idle` (exts)

- No logic change. The continuation nudge ("continuation #N")
  still goes into `request.input` as a user message. That is
  conversation content, not a prompt fragment.
- The goal rules no longer duplicate here. They live in the
  system prompt fragment.

### 5. `rushi-exts/goal-state/src/lib.rs` (exts)

- Rename `build_goal_block()` or add `build_goal_fragment()`. It
  returns the fenced block string. Same content, with a new doc
  comment: "system prompt fragment, not a trailing input item".
- Add `fn without_goal_fragment(instructions: &str) ->
  Option<String>` only if a code path must strip the old-style
  block during migration. Likely not needed: switch all call
  sites at once.

### 6. `docs/` updates

- `../rushi-exts/docs/goal-rule-reinject.md`: point its mechanism section at this
  doc. Mark the `input`-append path as superseded.
- `goal-ux.md` (rushi-tui): reword P6/P15/P16/P17. The block
  sits in `instructions`, not in `input`.
- `INDEX.md`: add this doc.

### 7. `bin/rushi/src/config.rs` (kernel)

- No change to `HarnessConfig`. The fragment store is a runtime
  concept carried in the request payload. It is not a config key.

## Cache tradeoff

Same as `../rushi-exts/docs/goal-rule-reinject.md` §Cache tradeoff:

- Goal set: one re-prefill. The fragment appears, so the prefix
  changes.
- Goal complete: one re-prefill. The fragment leaves, so the
  prefix reverts.
- Steady-state turns in between: byte-identical, so the cache
  stays warm.
- Generated tool list: byte-stable per session, so no extra cache
  cost.

For a goal session of N turns: two re-prefills spread over N
turns.
The generated tool list is part of the stable prefix. It adds no
churn.

## Decisions

- **D1** (inherited): the `goal` tool schema is invisible to the
  agent in every state. The agent's goal tools are
  `goal_complete` and `goal_blocked` only.
- **D2** (inherited): `goal resume` applies only to a blocked
  goal.
- **D3** (inherited): the `goal-state` crate lives in the exts
  repo.
- **D4** (new): the kernel `assemble` generates the tool list
  from the discovered manifests. `config.toml [system_prompt].text`
  carries no tool list. The generated list is part of the stable
  prefix: it is deterministic from the config and the filesystem,
  both constant per session.
- **D5** (new): prompt fragments are an ordered map
  (`IndexMap<String, String>`) carried in the model request
  payload (`request.prompt_fragments`). The kernel joins them
  generically after the hook chain. Extensions own their keys.
  The kernel never inspects the fragment content. Add or remove
  is one step per extension.
- **D6** (new): only `goal_complete` clears the goal fragment.
  `goal_blocked` keeps it, since the goal is still open. A manual
  `goal clear` (TUI palette) also clears it, since it marks the
  goal closed.

## Open questions

- Does the `--fragments` CLI argument to `assemble` (or a
  session-dir file) cost more than a kernel join after the hook
  chain? For 1-3 fragments, likely negligible. The CLI-arg path
  is simpler. The kernel-join path keeps fragments out of the
  serialized request.
  **Recommendation**: kernel join (`step.rs`). It keeps the
  fragments out of the model request JSON. It avoids a second
  serialization pass.
- If a future extension must modify (not just add or remove)
  another extension's fragment, the map still handles it:
  `map.get_mut(other_key)`. Discourage it. Each extension owns
  its own key. Cross-extension modification is a contract
  violation. Make it a no-op or an error.

## References

- `../rushi-exts/docs/goal-rule-reinject.md` — decision items D1-D3, cache
  tradeoff analysis, goal-block content.
- `bin/assemble/src/main.rs` — current `instructions` build
  (line ~904) and `tool_schemas` build (line ~1123).
- `bin/rushi/src/step.rs` — `apply_model_before_transform` (line
  ~624), `model_retry_loop`.
- `rushi-exts/goal-hooks/hook-goal-arm/src/main.rs` — current
  `model.before` injection (input append, to be replaced).
- `rushi-exts/goal-tools/goal_complete/src/main.rs` — the tool
  that triggers fragment removal (via state, `is_open() ==
  false`).
- `rushi-tui/docs/goal-ux.md` — properties P1-P17 (P6/P15/P16/P17
  need rewording for the new injection point).

## Properties

P1. generated-tool-list: given a `tools_root` with tool manifests
    and optional `extra_tools_roots`, observe that `assemble`
    renders a tool list in `instructions` with one `- name: desc`
    line per tool, in manifest discovery order (primary root first,
    then each extra root, alphabetical within each), inserted
    between the config base text and the cwd line.

P2. fragment-join: given a `model.before` transform that carries
    `prompt_fragments` (an ordered array of `[id, text]` pairs),
    observe that the kernel joins the text values in order and
    appends them to `request.instructions`, removes the
    `prompt_fragments` field from the request before the model
    call, and logs the fragment key list (not the values) in a
    `hook.model.before.transform` event.

P3. fragment-stability: given the same fragment set and goal state,
    observe that consecutive model calls produce byte-identical
    `instructions`.

P4. goal-fragment-lifecycle: observe that the `"goal"` fragment is
    present when the goal is open or blocked, and absent when the
    goal is completed or the goal file is missing. Only
    `goal_complete` (or a manual `goal clear`) removes it;
    `goal_blocked` keeps it.

P5. tool-filter-d1: observe that the `goal` tool schema is filtered
    from `request.tools` in every state. The `goal_complete` and
    `goal_blocked` schemas are present only while a goal is open.

## Verification

| P# | Property | Proof | Status |
|----|----------|-------|--------|
| P1 | generated-tool-list | Code inspection of `bin/assemble/src/main.rs` (tool list rendering + insertion order) | open |
| P2 | fragment-join | Code inspection of `bin/rushi/src/step.rs` `apply_model_before_transform` (join, strip, log) | open |
| P3 | fragment-stability | `test_transform_is_idempotent_byte_stable` in `hook-goal-arm` + `test_block_byte_stable_across_turns` in `goal-state` | proven |
| P4 | goal-fragment-lifecycle | `test_open_goal_sets_fragment_and_filters_tools`, `test_closed_goal_strips_fragment_and_close_tools`, `test_blocked_goal_keeps_fragment` in `hook-goal-arm` | proven |
| P5 | tool-filter-d1 | Same tests as P4 (open: close tools present, `goal` absent; closed: close tools removed) | proven |

## Gate

```
cargo build
cargo test
cd ../rushi-exts/goal-state && cargo test
cd ../rushi-exts/goal-hooks/hook-goal-arm && cargo test
cd ../rushi-exts/goal-hooks/hook-goal-idle && cargo test
cd ../rushi-exts/goal-hooks/hook-goal-tools && cargo test
```
