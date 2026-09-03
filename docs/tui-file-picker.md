# TUI file picker (`@`) and the completion window

Status: Implemented 2026-09-03 (day-0 scope). The request lives in
`docs/tui_feature_requests_from_human.md` (the 2026-09-08 item).
Library research lives in `docs/tui-file-picker-research.md`.
Sections 4 to 8 are the design and the build plan. Section 6 names
the open decision for the human.

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
  and honors `.gitignore`. It uses `git ls-files` when a repo is
  present, and a plain walk otherwise.

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
  the open flag, the cursor index, and the visible window.
- It is crossterm-free. Tests drive it directly, like `app.rs` and
  `browse.rs` do today.
- Keys: type to edit the query, `j`/`k` or arrows to move, `PgUp` /
  `PgDn` to page, `Home` / `End` to jump, `Tab` to toggle a
  multi-select (later), `Enter` to commit, `Esc` to close.

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
  on `j`/`k` and `Ctrl+U`/`Ctrl+D`.
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

- In the editor insert mode, a `@` at a word start opens the
  picker. The text after `@` seeds the query.
- The picker filters the item list live as the user types.
- `Enter` replaces the `@query` token with the chosen path and
  closes the picker. The draft keeps the caret at the path end.
- `Esc` closes the picker and leaves the draft as typed.
- With zero results, `Enter` commits the raw `@query` text. This
  matches how agent TUIs treat a failed reference.
- The token rules keep the existing editor motion. The picker only
  owns the candidate list and the final insert.

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
select-and-yank). Until then the pane scrolls on `j`/`k` and
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
  then the pane scrolls on `j`/`k` and `Ctrl+U`/`Ctrl+D`.
- The multi-select and quickfix behavior from `telescope` is a later
  add, not day 0.
