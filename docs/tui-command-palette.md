# TUI `:` command palette

Status: Spec, not yet built (2026-09-11). The request lives in
`docs/tui_feature_requests_from_human.md` (the 2026-09-11 item).
The `edit-queue` entry is specified in
`docs/user-message-editing.md`. The extension command mechanism
follows the catalog intent in
`docs/skill-remapped-to-os-apps.md` section 5.

## 1. Request

The TUI is built on a vim mental model: normal mode for motion,
insert mode for text, operator-pending states for edits. The one
missing piece is a command surface. `pi` and other agent harnesses
open a slash-command window when the input is empty. This design
adds the vim-native equivalent: in normal mode, `:` opens a
floating command palette. The window lists commands, settings, and
session buffers. A right-hand preview pane shows help text, option
pickers, and secondary menus.

The goal is a single, discoverable entry point for everything that
is not a text edit:

- Run built-in commands (toggles, session control, quit, editor).
- Set configuration values (effort level, later model choice).
- Switch session buffers with fuzzy completion.
- Reach extension-provided commands and settings without the host
  hardcoding each one.

## 2. Today (verified in code)

- The `@` picker (`bin/tui/src/picker/`) is the reusable completion
  window. It owns a frizbee-backed ranker, a pure state machine,
  a two-pane floating layout, and a `Previewer` trait. The palette
  reuses this middle layer instead of forking it.
- `Action::CycleSessions(i32)` and `cycle_target` exist in
  `app.rs` but are unused. They are the seam this design reuses
  for `bn` / `bp`.
- `start_naming` and `Action::ConfirmNewSession` exist in
  `app.rs`. They only fire at startup when no session argument is
  given. There is no mid-session way to create a session.
- `cycle_reasoning_effort` in `main.rs` only cycles the effort
  value. It cannot set a specific level. The palette needs a set
  variant.
- No `:` palette, no session-list overlay, and no Alt-key mapping
  exist in `key_input` (`main.rs`).
- The browse overlay, the `@` picker, and the name input are the
  existing overlay precedents.

## 3. Design goals

- The palette is a floating window, not a vim Ex command line.
  It reuses the picker's two-pane layout and fuzzy ranker.
- Discovery first: the user can open the palette empty and browse
  every command with its help text and current state.
- Every command is a typed item, not a free-text string. This
  keeps execution safe and testable.
- The command set is extensible. Extensions register their own
  commands and settings through a new host op. The host renders
  them in the same window. No host recompile per extension.
- No new GUI dependency. The palette is `ratatui` plus the
  existing `picker/` layer.

## 4. Trigger and keys

- `:` in editor normal mode opens the palette. A non-empty draft
  does not block it: the overlay is separate from the draft and
  never writes into it.
- In normal mode, `:` first cancels a pending operator. `d:` drops
  the `d` and opens the palette, matching vim.
- The palette owns its key table while open. While it is open:
  - printable chars and Backspace edit the query
  - `j` / `k` or `Ctrl+J` / `Ctrl+K` move the cursor
  - `PgUp` / `PgDn` page, `Home` / `End` jump
  - `Enter` commits the highlighted item
  - `Esc` closes, dropping any sub-stage
  - `Ctrl+P` toggles the preview pane
  - `Ctrl+U` / `Ctrl+D` scroll the preview pane
- Host keys pass through unchanged: `Ctrl+C` (stop), `Ctrl+R`
  (run), `Tab` / `Shift+Tab`, and `q` when the quit gate is open.
- `Alt+Up` does not open the palette. It recalls the pending
  message queue, a separate feature in
  `docs/user-message-editing.md`.

## 5. Command model

One item type drives the whole window.

```rust
pub enum CmdKind { Run, Set, Goto, Ext }

pub struct PaletteItem {
    pub id: String,          // stable key, e.g. "effort"
    pub label: String,       // shown in the list, e.g. "effort"
    pub kind: CmdKind,
    pub hint: String,        // "Ctrl+O", current value, or ""
    pub help: String,        // preview-pane text
    pub options: Vec<CmdOption>, // non-empty only for Set
    pub ext: Option<String>, // extension name for Ext items
}
```

- `Run`: fire one `Action`. No arguments. `Enter` executes and
  closes.
- `Set`: a value with a choice. The preview pane lists
  `options`; the user selects one and `Enter` applies it. The
  current value is marked.
- `Goto`: open a sub-list. The query keeps feeding the sub-list
  filter. Used by the session buffer (`b`).
- `Ext`: an extension-owned command. `Enter` sends an `invoke`
  op to that extension.

The fuzzy ranker scores items by `label` plus a keyword field.
The keyword field carries the shortcut and synonyms so `togg`
finds the toggles and `think` finds both thinking items.

## 6. v1 command set

Built-in items, registered when the palette opens:

| item          | kind | behavior                                    |
| ------------- | ---- | ------------------------------------------- |
| `toggle-tools`     | Run  | the `Ctrl+O` fold/expand toggle             |
| `toggle-thinking`  | Run  | the `Ctrl+T` show/hide toggle               |
| `expand-thinking`  | Run  | the `Ctrl+X` collapse/expand toggle         |
| `effort`           | Set  | pick `none..max`; main writes the config    |
| `b`                | Goto | fuzzy session list, `:b <name>` switches    |
| `bn`               | Run  | `Action::CycleSessions(+1)`                |
| `bp`               | Run  | `Action::CycleSessions(-1)`                |
| `new-session`      | Run  | open the name input, then `ConfirmNewSession` |
| `edit-queue`       | Run  | bulk recall, see `docs/user-message-editing.md` |
| `e`                | Run  | `Action::OpenEditor` on the draft           |
| `q`                | Run  | `Action::Quit`                             |

Loop start and stop stay out of v1. The `Ctrl+R` / `Ctrl+C`
bindings already cover them and are more direct. A loop section
can be added later.

## 7. The session buffer (`b`, `bn`, `bp`)

The vim buffer concept maps onto the harness session:

- `bn` is `Action::CycleSessions(+1)`. `bp` is
  `Action::CycleSessions(-1)`. Main already owns that path: list
  sessions, reload the log, restart the watch, clear extension
  replies, resend history, resync the loop probe.
- `b` is a `Goto` item. Committing it swaps the left pane to a
  fuzzy list of session names from `port.list_sessions()`.
- The typed text after `b` filters the list. `Enter` on a name
  emits `Action::SwitchSession(name)`.
- The preview pane shows the recency rank, the active-session
  marker, and the loop-running bit for the highlighted session.

This is the lightweight session-list overlay the 2026-09-08
request described: one keypress reaches the list, `j` / `k`
navigate, `Enter` switches, `Esc` closes, and no editing keys are
hijacked. It resolves the pending "Redesign session navigation"
item without repurposing `Tab`.

## 8. Effort setting

- Extract a `set_effort(config_path, active_model, value)` helper
  from `cycle_reasoning_effort` in `main.rs`.
- `Ctrl+L` still cycles: it calls `set_effort` with the next
  value in the effort order.
- The palette `effort` item calls `set_effort` with the chosen
  value. The option list is the effort order
  (`none, minimal, low, medium, high, xhigh, max`). The current
  value is read from the active model's config entry and marked in
  the preview pane.
- On apply, main writes the config, bumps `events_version` so the
  transcript rebuilds, and flashes the new value. The loop picks
  up the new level on its next step, like the cycle path.

## 9. The `edit-queue` entry

`:edit-queue` is a `Run` item. Its commit runs the bulk recall
defined in `docs/user-message-editing.md`: retract every pending
user message, load the combined text into the editor, and wait
for the user to edit and send. The palette doc only owns the
entry and its key routing. The retract event, the loop skip
rules, and the idle-on-retract behavior live in the other doc.

## 10. Extension-provided commands

The palette is extensible through the existing extension host.
No command list is baked into the host binary.

- Add a `commands` cap to the `CAPS` set in `ext.rs`. An
  extension opts in by listing `commands` in `caps`.
- Host to extension, on palette open:
  `{"v":1,"op":"commands","session":...,"loop_running":...}`
- Extension to host:
  `{"v":1,"op":"commands_list","commands":[
    {"id":"myext.reload","label":"Reload myext","kind":"run",
     "hint":"", "help":"..."}]}`
- Execution, host to extension:
  `{"v":1,"op":"invoke","req":N,"id":"myext.reload","value":"..."}`
- Extension to host:
  `{"v":1,"op":"invoke_reply","req":N,"ok":true,"message":"done"}`

The request/response pair follows the `transform` / `transformed`
pattern exactly: a request id, a two-second timeout, and a G5
fallback (`docs/refinement-policy.md` G5, `docs/ui-extension.md`
section 4). A dead or stale extension is dropped from the list
and the host flashes the reason. Its commands simply do not
appear. No extension crash takes the TUI down.

An extension that exposes settings declares its options in
`commands_list`. The host renders them as `Set` items and sends
the chosen value in `invoke`. The host does not interpret the
values. It only carries them to the owning process.

`protocol_v` stays at 1. All of this is additive. Existing
extensions that do not declare the cap are untouched
(`docs/refinement-policy.md` P1b).

## 11. Module layout

New module `bin/tui/src/palette/`, mirroring `picker/`:

- `palette/state.rs` — `PaletteState` and a `PaletteAction`
  enum. Pure, crossterm-free. It holds the open flag, the query,
  the sub-stage (`Root` or `SessionList`), the cursor, and the
  preview scroll. Tests drive it directly, like `picker/state.rs`.
- `palette/items.rs` — `PaletteItem`, `CmdKind`, `CmdOption`, and
  the built-in registration. Also the `commands_list` merge that
  appends extension items under the built-ins.
- `palette/preview.rs` — a `CommandPreviewer`. It renders help
  text, option pickers, and session metadata into the preview
  pane.
- `palette/render.rs` — the two-pane float body. It takes a
  `Rect` and never decides where that `Rect` is, matching
  `picker/render.rs`.
- `palette/fuzzy.rs` — reuse `picker/fuzzy.rs`. No new ranker.

The shared `compute_float_layout` moves out of
`picker/render.rs` into a small `bin/tui/src/float.rs` so the
picker and the palette share the wide/narrow orientation logic.
Both callers switch to the shared helper.

## 12. Rendering

- The palette float draws last, over the transcript and input
  rows, exactly like the `@` picker float in `render.rs`.
- Left pane: the filtered list. One row per item. The row shows
  the label, a kind marker, and the hint. `Set` items show the
  current value in the hint. The active row is highlighted.
- Right pane: the preview. `Run` items show help text. `Set`
  items list the options with the current one marked. `Goto`
  shows the sub-list in the left pane instead. `Ext` items show
  the extension's help text.
- The pane follows the picker's `preview_cutoff` auto-hide rule
  and the `Ctrl+P` toggle.
- One overlay at a time is the invariant. The palette, the
  picker, and the browse overlay are mutually exclusive. While
  the palette is open, the picker and browse stay closed.

## 13. Key routing

`press()` in `app.rs` gains a new block, placed between the
`@` picker block and the `pending_name` block:

- While the palette is open, its state machine owns typing,
  motion, `Enter`, and `Esc`. Host keys pass through.
- The `pending_name` bar still wins over the palette. If the
  name input is active, the palette is not entered.
- In the normal-mode char branch, `:` opens the palette before
  the editor sees the char. This keeps the char out of the draft.
- `Alt+Up` is mapped to `Key::AltUp` in `key_input` and handled
  in the same branch. It never opens the palette.

## 14. New `Action` variants

- `SwitchSession(String)` — load one session by name, reusing the
  `CycleSessions` main-loop path without the delta.
- `SetEffort(String)` — write one effort value to the active
  model's config entry and flash the result.
- `InvokeExtCommand { ext: String, id: String, value: Option<String> }`
  — send an `invoke` op through the host and flash the reply.
- `RecallQueue` — run the bulk queue recall
  (`docs/user-message-editing.md`). Main appends the retract
  events and loads the combined text into the draft.

`CycleSessions(i32)` stays as is for `bn` / `bp`.

## 15. Build plan

- Step 0: the `palette/` module. `state.rs` and `items.rs` with
  unit tests. No UI. The built-in command list is data.
- Step 1: `preview.rs` and `render.rs`. The two-pane float with
  geometry tests. Move `compute_float_layout` to the shared
  `float.rs`.
- Step 2: wire `:` into `press()`. Register the built-in items.
  The palette is usable with the v1 command set.
- Step 3: the session sub-list (`b`), `SwitchSession`, and the
  effort `Set` item with `set_effort`.
- Step 4: the `commands` cap and the `invoke` op. A demo
  extension registers two commands and one setting.

Each step ends with a passing test suite and one real session
that exercises the new surface.

## 16. Open items

- Frecency for the session sub-list. The list ranks by mtime now.
  A frecency store is a later add.
- The agent tool catalog (`tools --list`,
  `docs/skill-remapped-to-os-apps.md` section 5) is a separate
  window. It is not part of the `:` palette v1.
- Free-form command arguments and multi-line input are later
  adds. v1 items take no free-form arguments.
- Per-message queue editing is in
  `docs/user-message-editing.md`.

## Properties

Lean-style invariants for this spec (see `lean-driven-development.md`).
Each property is observable: given an input, an output guarantee.

P1. colon-trigger: given the `:` keypress in normal mode with an empty
    draft, observe the floating command palette open over the
    transcript; a non-empty draft does not block the open.
P2. key-table: given the palette is open, observe printable characters
    and Backspace edit the query, `j`/`k` and `Ctrl+J`/`Ctrl+K` move
    the cursor, `Enter` commits, `Esc` closes dropping any sub-stage,
    `Ctrl+P` toggles the preview pane, and host keys (`Ctrl+C`,
    `Ctrl+R`, `Tab`, `Shift+Tab`, `q` when quit gate open) pass
    through unchanged.
P3. command-kinds: given a `Run` item, observe `Enter` fires the
    action and closes; given a `Set` item, observe `Enter` applies the
    selected option; given a `Goto` item, observe a sub-list opens and
    the query feeds the sub-list; given an `Ext` item, observe an
    `invoke` op is sent to the owning extension.
P4. session-buffer: given a `b` `Goto` item with typed text, observe
    the session list filter and `Enter` emit `SwitchSession(name)`;
    given `bn`/`bp`, observe `CycleSessions(+1)` / `CycleSessions(-1)`.
P5. effort-set: given the `effort` `Set` item with a chosen value,
    observe main write the config, bump `events_version`, and flash the
    new value.
P6. extension-commands: given an extension declaring the `commands`
    cap and returning a `commands_list` reply, observe its items appear
    in the palette; given a dead or stale extension, observe its items
    are absent and the TUI remains responsive.

## Verification

Each property maps to its proof. `proven` means the cited test exists
and passes. `open` names the blocker and what unblocks it.

| P# | Property | Proof | Status |
|----|----------|-------|--------|
| P1 | colon-trigger | open: `:` key routing not yet wired in `press()`; unblocked when build plan step 2 lands. State machine unit tests exist in `bin/tui/src/palette/state.rs`. | open |
| P2 | key-table | open: key routing and preview-toggle wiring not yet in `main.rs`; unblocked at build plan step 2. State-machine transitions tested in `bin/tui/src/palette/state.rs`. | open |
| P3 | command-kinds | open: item dispatch and `invoke` op not yet wired; unblocked at build plan steps 2–4. Item data structures tested in `bin/tui/src/palette/items.rs`. | open |
| P4 | session-buffer | open: `SwitchSession` action and `b` sub-list not yet wired; unblocked at build plan step 3. | open |
| P5 | effort-set | open: `set_effort` extraction and config write not yet implemented; unblocked at build plan step 3. | open |
| P6 | extension-commands | open: `commands` cap and `invoke` op protocol not yet built; unblocked at build plan step 4. | open |

## Gate

Gate: blocked — the command palette is spec-only; build plan steps 2 to
4 must land before the gate can pass.

```
cargo build
cargo test -p tui
```
