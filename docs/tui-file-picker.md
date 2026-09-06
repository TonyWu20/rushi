# TUI file picker (`@`) and the completion window

Status: Implemented 2026-09-03 (day-0 scope). The request lives in
`docs/tui_feature_requests_from_human.md` (the 2026-09-08 item).
Library research lives in `docs/tui-file-picker-research.md`.
Sections 4 to 8 are the design and the build plan. Section 6 names
the open decision for the human. The `Ctrl+I` file-scope cycle
(section 4.1, P9) landed 2026-09-06: the picker starts with the
default scope (hidden and git-ignored files excluded) and each
`Ctrl+I` press cycles it wider — show git-ignored, then also show
hidden — until a third press returns to the default. In a standard
terminal `Ctrl+I` and `Tab` are the same key (both send byte 0x09,
which crossterm parses as `KeyCode::Tab`), so the picker binds `Tab`
to the cycle too; the `Ctrl+I` mapping stays for terminals that
report it distinctly (e.g. the kitty keyboard protocol). The
long-path abbreviation (section 4.4, P10) landed 2026-09-06 as
well: a result-list label wider than the list column — with the
preview pane on or off — collapses its leading directory levels
into a `...` prefix so the tail of the path stays visible.

## 1. Request

One feature has two parts. The first is the `@` file picker. The user
types `@` in the input box. A candidate list of files appears. The
list re-ranks as the user types. The user picks a path and the path
lands in the draft. This mirrors the `@` reference that every
agentic TUI offers.

The second is the completion window. The window that shows the
candidates is a reusable widget. It is not a one-off for files. Many
later features reuse the same window: symbols, buffers, git files,
command palette. The window is the public asset. The file picker is
its first consumer.

Fuzzy search is on from day 0. The match is not an exact prefix
filter. It ranks by relevance. The design borrows the look and feel
from `telescope.nvim` and `television` (Rust). It borrows the
engine choice: `frizbee` is the fuzzy matcher that powers
`television`, `skim`, and `fff` (research doc, section 1).

The display style is an open decision. Two options are designed in
section 6. The human picks one. The rest of this doc is written so
the decision stays cheap.

## 2. Today (verified in code)

- No completion window, picker, or fuzzy match exists in the TUI.
- The input box is the vim modal editor (`bin/tui/src/vim_editor.rs`).
  It is a `Vec` of lines plus a mode state machine.
- The editor renders as a rounded border block in
  `bin/tui/src/render.rs` (`draw`, `render.rs:2127`).
- The frame is one bordered panel with a vertical row stack
  (`render.rs` around line 2228). Rows: transcript, waiting
  messages, approval banner, working row, input box, status row.
- The `browse.rs` overlay is the closest existing pattern. It is a
  state machine with a cursor, a line list, and a search state.
- No `frizbee` or other fuzzy dependency in `Cargo.lock` yet.

## 3. Design goals

- Fuzzy, ranked, from day 0. No exact-prefix fallback as the main
  path.
- The window is a reusable widget. It owns its body. It does not own
  the container.
- Display is swappable. The same body renders inline or floating.
- The matching stays off the UI thread. The UI reads a snapshot.
- The window is a first-class asset. Later pickers plug in without
  touching the file picker.

## 4. The reusable middle layer

The layer is a new module under `bin/tui/src/picker/`. It has four
parts. Each part has one job.

### 4.1 Items (`picker/items.rs`)

- `PickerItem`: one candidate. It holds a display label, a stable
  value (the thing the user gets on select), and a payload for the
  preview.
- `ItemSource`: a trait that streams items into the picker. A file
  source lists files. A later symbol source lists symbols. The trait
  is the extension point for new sources.
- `FileItemSource`: the first `ItemSource`. It walks the working tree
  and honors `.gitignore` by default. It uses `git ls-files` when a
  repo is present, and a plain walk otherwise.
- `FileScope` (section P9): how much of the tree the source shows.
  Three values, cycled by `Ctrl+I` while the picker is open — in a
  standard terminal `Tab` is the same key (both send byte 0x09, which
  crossterm parses as `KeyCode::Tab`), so `Tab` cycles the scope too:
  `Standard` (the default — in a git repo: tracked plus untracked,
  not-ignored files; in a plain walk: everything except dot entries
  and build/dependency directories), `IncludeIgnored` (also the
  git-ignored set — in a plain walk, the build/dependency
  directories), and `IncludeHidden` (also the dot entries). In a git
  repo the git listings already report hidden tracked and untracked
  files, so `IncludeHidden` only adds what the ignored step did not
  already cover. The scope lives on the picker state, resets to
  `Standard` on open and close, and a cycled press makes the app
  re-enumerate the current search root and re-rank the live query.

### 4.2 Match (`picker/match.rs`)

- `PickerMatcher`: a frizbee-backed ranker. It holds one
  `frizbee::Matcher` per query. It calls `match_list` on the item
  list and returns ranked `PickerItem`s.
- A background worker thread owns the matcher. The UI pushes the
  query through an `mpsc` channel. The worker publishes an
  `Arc<Snapshot>`. The UI reads the latest snapshot without blocking.
  This is the `television` model (research doc, section 2).
- Sort order: score, then an optional index bias. A frecency sort
  is a later add (section 4.4).

### 4.3 State (`picker/state.rs`)

- `PickerState`: a pure state machine. It holds the query string,
  the open flag, the cursor index, the visible window, and the file
  scope (P9).
- It is crossterm-free. Tests drive it directly, like `app.rs` and
  `browse.rs` do today.
- Keys: type to edit the query, `Ctrl+J` / `Ctrl+K` or arrows to
  move, `PgUp` / `PgDn` to page, `Home` / `End` to jump, `Enter` to
  commit, `Esc` to close, `Ctrl+U` / `Ctrl+D` to scroll the preview
  pane, `Ctrl+P` to toggle the preview pane, and `Ctrl+I` / `Tab` to
  cycle the file scope (P9; in a standard terminal the two are the
  same key — byte 0x09).

### 4.4 Body render and preview (`picker/render.rs`,
`picker/preview.rs`)

- `render_picker(f, state, snapshot, rect, previewer, hints)`: draws
  the body into a given `Rect`. It never decides where the `Rect` is.
- `Previewer`: a trait. The file previewer shows file content. The
  null previewer shows nothing. `preview_cutoff` hides the pane when
  the result count drops below a threshold (a `telescope` behavior).
- The file preview is a first-class, day-0 part of the body. It is
  the main reason the picker uses the floating container
  (section 6). It shows the selected file's text and follows the
  cursor on every move.
- The preview scroll reuses the visual mode scroll primitives when
  the visual mode lands (the open select-and-yank, section 11 of
  `docs/tui-conversation-browsing.md`). Until then the pane scrolls
  on `Ctrl+J`/`Ctrl+K` and `Ctrl+U`/`Ctrl+D`.
- The result list abbreviates long path labels (P10): when a label
  is wider than the list column — with the preview pane on or off —
  the leading directory levels collapse into a `...` prefix and the
  largest suffix of the path that fits the column stays visible
  (`.../a/b/src/app.rs`). The budget follows the list column width,
  which differs with the preview pane on or off. A label without
  directory levels, or still too wide with only `.../` plus the
  file name, falls back to the plain head truncation with a
  trailing `…`.
- If a render or source function grows past eight parameters, use the
  `bon` builder. This follows `docs/coding-conventions.md`.

### 4.5 Extensibility

The window is reusable because three seams are open:

- `ItemSource` swaps the data. Files now. Symbols and git files later.
- The sort order swaps. Fuzzy score now. Frecency later.
- `Previewer` swaps the pane. File text now. Code, diff, or none later.

The display container is a fourth seam (section 6). A file picker and
a symbol picker share all four.

## 5. The `@` trigger

- In the editor insert mode, a `@` preceded only by whitespace (or at
  the start of the line) opens the picker. The text after `@` seeds
  the query. An `@` preceded by a non-whitespace character (e.g.
  `user@domain`) does not trigger the picker.
- The picker filters the item list live as the user types.
- `Enter` replaces the `@query` token with the chosen path (prefixed
  with `@`) and closes the picker. The draft keeps the caret at the
  path end.
- `Esc` closes the picker and leaves the draft as typed (the `@` and
  any query text remain).
- With zero results, `Enter` keeps the raw `@query` text in the
  draft.
- List navigation uses `Ctrl+J` / `Ctrl+K` (or arrow keys), leaving
  plain `j`/`k` free for typing into the query.
- The picker only opens on a freshly typed `@`; a stale `@` left in
  the draft after a previous pick or dismiss does not re-trigger it.
- `Ctrl+I` cycles the file scope while the picker is open
  (P9): default (hidden and git-ignored files excluded) → show
  git-ignored → also show hidden → back to default. Each press
  re-enumerates the current search root at the new scope and
  re-ranks the live query. A flash line names the new mode, and the
  float title carries a scope tag while a widened scope is active.
  The scope resets to the default every time the picker opens or
  closes. In a standard terminal `Ctrl+I` and `Tab` are the same
  key (both send byte 0x09, which crossterm parses as
  `KeyCode::Tab`), so the binding is `Tab` — pressing the physical
  `Tab` key (i.e. `Ctrl+I`) cycles the scope. The `Ctrl+I` mapping
  (`KeyCode::Char('i')` + `CONTROL`) stays for terminals that report
  it distinctly (e.g. the kitty keyboard protocol).

## 6. Display decision (settled: floating)

Both options render the same body (section 4.4). Only the container
differs. The middle layer works with either.

Decision: the floating spawned window (Option B). Its strength is
room for the file content preview. The user feeds the agent
documents and files heavily. The preview confirms the target without
opening the file in a second tmux pane or shell session. The inline
option stays designed but is not built first.

### Option A: inline, under the input box

A new row in the vertical layout. It sits above the input box, like
the working row and the waiting-message block. It shows when the
picker is open and its height is zero when closed.

Pros:

- It fits the existing render model. No new window system.
- No z-order, focus, or close manager. One new `Constraint`.
- It matches agent TUIs that list candidates under the prompt.
- It is the smallest change and the easiest to test.

Cons:

- It takes rows from the transcript while open.
- A wide preview pane fights the result list in a short strip.
- It reads as attached to the prompt, not as a modal surface.

### Option B: floating spawned window

A `Rect` drawn on top of the frame, after the main panel. The body
renders into the float. It can be centered or bottom-anchored.

Pros:

- Clear visual separation. It reads as a modal, like `telescope`.
- A large preview pane fits without squeezing the log.
- It seeds a floating-window layer for later overlays (diffs, help).

Cons:

- It needs a window stack. Z-order, focus, and close are new code.
- It must coexist with the `browse` overlay and the `frame`
  extension. The "one overlay at a time" rule is a new invariant.
- The cursor is placed in overlay coordinates, so crossterm math
  changes.
- It is more code and more visual invariants to test first.

### The preview pane

The preview pane is the point of the floating choice. Its position
follows the float width: right on a wide float, bottom on a narrow
one. The header line
holds the path, size, and line count. It follows the cursor on every
move. `preview_cutoff` hides the pane when the result count is small,
so a short list keeps the full width. A key toggles the pane. Scroll
keys move within the pane.

The scroll reuses the visual mode primitives when the visual mode
lands (`docs/tui-conversation-browsing.md` section 11, the open
select-and-yank). Until then the pane scrolls on `Ctrl+J`/`Ctrl+K` and
`Ctrl+U`/`Ctrl+D`.

### Orientation: wide and narrow

The float recomputes its region on every terminal resize. A
`Layout::build` function takes the terminal size and the picker
state and returns the region rects. The orientation is a function of
the float width. This mirrors the orientation model in `television`
(`landscape` and `portrait`), section 2 of the research doc.

- Wide: the float holds at least `WIDE_MIN` columns. The result list
  takes the left columns. The preview pane takes the right columns.
  The input bar spans the bottom.
- Narrow: the float is below `WIDE_MIN` columns. The result list
  takes the top rows. The preview pane takes the bottom rows. The
  input bar spans the bottom.
- Too narrow: the float is below the `MIN` floor. The preview drops
  out. The float shows the list and the input bar in one column,
  like a plain `fzf` list.

The orientation flips at the threshold with no key press. A terminal
drag moves the preview from the right to the bottom.

`WIDE_MIN` is the sum of a minimum list width and a minimum preview
width. The list needs room for the path and the score. The preview
needs room for code. `MIN` is the floor below which the preview
stops. Both start around 80 columns and are config knobs, not hard
constants.

### Reusability read

Option A reuses the current single-panel model and is cheaper to
build. Option B adds a layer that other features can reuse. The body
is the same in both. Choosing B first pays a window-stack cost.
Choosing A first keeps the picker small and lets a window layer come
later. The middle layer does not block either choice.

## 7. Dependency choice

- Add `frizbee` to `bin/tui/Cargo.toml`. `television` uses
  `frizbee = "0.13"` (research doc, section 2). Pin to the same
  major version.
- No new GUI or event dependency. The picker reuses `ratatui` and
  `crossterm`, which are already present.
- The file source uses `std::fs` and, when in a repo, shells out to
  `git ls-files` through the existing tool surface. No new walker
  crate is required.

## 8. Build plan

The plan delivers the reusable layer first, then the target module.
Each step is small and testable on its own.

- Step 0: add `frizbee` and the `picker/` module. Build `items.rs`
  and `match.rs` with unit tests. No UI.
- Step 1: build `state.rs`, `render.rs`, and `preview.rs` with
  geometry tests. The widget body is ready. Still no app wiring.
- Step 2: wire the `@` trigger in `vim_editor.rs` and `app.rs`.
  Add the floating window layer and route the body into the float
  (Option B). The picker with the preview pane is usable.
- Step 3: add the extension seams. Frecency sort, multi-select,
  which-key help, and the symbol and git-file sources.

Each step ends with a passing test suite and one real session that
exercises the new surface.

## 9. Open items

- Frecency store: where it lives and how it persists.
- Preview depth: plain text on day 0. Code highlight shipped as a
  shared component 2026-09-08: `bin/tui/src/highlight.rs`
  (`language_from_path`, `CodeHighlighter`, `highlight_text_lines`).
  The picker preview pane (`picker/preview.rs`) and the transcript
  `Read` tool-result body (`tool_display.rs` `read_body`) both drive
  it. The language is detected from the file path; unknown types and
  binary files stay plain. No new dependency: a hand-rolled
  per-language tokenizer over the existing `Palette`/`Role` system.
  Tree-sitter was considered and deferred: grammar build weight is
  disproportionate for a preview pane and an inline tool-result
  body. Revisit if highlight quality demands it.
- The preview scroll reuses the visual mode when it lands. Until
  then the pane scrolls on `Ctrl+J`/`Ctrl+K` and `Ctrl+U`/`Ctrl+D`.
- The multi-select and quickfix behavior from `telescope` is a later
  add, not day 0.

## Properties

Lean-style invariants for this spec (see `lean-driven-development.md`).
Each property is observable: given an input, an output guarantee.

P1. at-trigger: given an `@` typed in insert mode at the start of a
    line or preceded only by whitespace, observe the picker open and
    the text after `@` seed the query; given `@` preceded by a
    non-whitespace character, observe no trigger.
P2. enter-replaces: given a highlighted candidate and `Enter`, observe
    the `@query` token in the draft replaced by the chosen path
    prefixed with `@`, the picker close, and the caret placed at the
    path end.
P3. esc-closes: given `Esc` while the picker is open, observe the
    picker close and the draft retain the typed `@` and query text.
P4. zero-results: given a query that matches no candidate, observe
    `Enter` keep the raw `@query` text in the draft unchanged.
P5. fuzzy-ranking: given a partial query, observe candidates returned
    in relevance order (fuzzy match), not filtered by exact prefix.
P6. preview-cutoff: given the result count dropping below the cutoff
    threshold, observe the preview pane hide and the list use full
    width.
P7. orientation: given a wide float (at least WIDE_MIN columns),
    observe the list on the left and the preview on the right; given a
    narrow float, observe the list on top and the preview below; given
    a very narrow float (below MIN), observe the preview drop out.
P8. git-source: given the working directory is inside a git repository,
    observe the file list come from `git ls-files`; given a non-repo
    directory, observe a plain directory walk.
P9. scope-cycle: given the picker is open at the default scope,
    observe the first scope-cycle press advance the scope to "show
    git-ignored" and the item list re-enumerated with the ignored
    files included; given a second press, observe the scope advance
    to "also show hidden"; given a third press, observe the scope
    return to the default (hidden and git-ignored excluded). The
    cycle is driven by `Ctrl+I` — in a standard terminal the same
    press arrives as `Tab` (byte 0x09, parsed by crossterm as
    `KeyCode::Tab`), so both `Ctrl+I` and `Tab` drive it. Given the
    picker closed, observe a scope-cycle press do nothing. Observe
    the scope reset to the default on every picker open.
P10. path-abbrev: given a result-list path label wider than the
    result-list column (with the preview pane on or off), observe
    the leading directory levels collapsed into a `...` prefix and
    the largest suffix of the path that fits the column kept
    visible (e.g. `.../a/b/src/app.rs`); given a label that fits
    the column, observe it shown in full; given a label without
    directory levels, or still too wide with only `.../` plus the
    file name, observe a head truncation with a trailing `…`
    instead.

## Verification

Each property maps to its proof. `proven` means the cited test exists
and passes. `open` names the blocker and what unblocks it.

| P# | Property | Proof | Status |
|----|----------|-------|--------|
| P1 | at-trigger | `at_token_preceded_by_only_whitespace_is_detected`, `at_token_at_line_start_is_detected`, `at_token_glued_to_word_is_not_detected` in `bin/tui/src/vim_editor.rs` | proven |
| P2 | enter-replaces | `replace_at_token_swaps_in_the_value`, `replace_at_token_keeps_text_after_caret` in `bin/tui/src/vim_editor.rs`; `commit_returns_index_when_results` in `bin/tui/src/picker/state.rs` | proven |
| P3 | esc-closes | `esc_closes_without_commit` in `bin/tui/src/picker/state.rs` | proven |
| P4 | zero-results | `commit_with_zero_results_returns_none` in `bin/tui/src/picker/state.rs` | proven |
| P5 | fuzzy-ranking | `query_filters_and_ranks`, `fuzzy_match_finds_partial` in `bin/tui/src/picker/fuzzy.rs` | proven |
| P6 | preview-cutoff | `preview_cutoff_hides_pane` in `bin/tui/src/picker/render.rs`, `toggle_preview_flips_when_above_cutoff` in `bin/tui/src/picker/state.rs` | proven |
| P7 | orientation | `wide_layout_splits_side_by_side`, `narrow_layout_stacks_vertically`, `too_narrow_drops_preview`, `orientation_flips_on_resize` in `bin/tui/src/float.rs` | proven |
| P8 | git-source | `file_item_source_is_a_git_repo`, `file_item_source_non_git_walks` in `bin/tui/src/picker/items.rs` | proven |
| P9 | scope-cycle | `ctrl_i_cycles_the_scope_and_returns_recollect`, `tab_cycles_the_scope_like_ctrl_i`, `scope_starts_standard_and_resets_on_open_and_close`, `ctrl_i_does_nothing_when_closed`, `tab_does_nothing_when_closed` in `bin/tui/src/picker/state.rs`; `file_scope_cycles_standard_to_ignored_to_hidden`, `walk_scope_controls_hidden_and_build_dirs`, `git_scope_includes_ignored_and_hidden_files` in `bin/tui/src/picker/items.rs`; `ctrl_i_recollects_picker_items_under_new_scope`, `tab_recollects_picker_items_under_new_scope`, `ctrl_i_surfaces_git_ignored_session_files` in `bin/tui/src/app.rs`; `tab_maps_to_key_tab`, `ctrl_i_maps_to_key_ctrl_i` in `bin/tui/src/main.rs` | proven |
| P10 | path-abbrev | `abbrev_keeps_fitting_labels_unchanged`, `abbrev_collapses_leading_parent_levels`, `abbrev_handles_absolute_labels`, `abbrev_falls_back_to_head_truncation`, `render_picker_abbreviates_long_paths_in_narrow_list_column` in `bin/tui/src/picker/render.rs` | proven |

## Gate

The acceptance commands. All must exit 0 for this spec to be proven.

```
cargo build
cargo test -p tui
```
