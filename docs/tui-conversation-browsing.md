# TUI conversation browsing

Status: shipped in the 2026-09-05 pass. The requests live in
`docs/tui_feature_requests_from_human.md` (the 2026-09-05
items). Sections 3 to 5 are the contract. Section 6 records
the reference machine. Sections 7 and 8 cover the search
decision and the build stages.

## 1. Requests

Two requests share one pain. The user scrolls back in the
session log (the wheel or `Ctrl+U/D`). The view leaves the
tail. The position sense is lost. The walk back to the
latest conversation position is manual, confusing, and
tiring. The user names the scroll bar and the `gg`/`G`
jump as the highest-priority UX fixes.

**Request 1 — the position bar.** A one-column bar at the
right edge of the transcript pane. It shows the view
position over the whole log. It appears when scrolling
begins. It stays up in browse mode.

**Request 2 — the conversation browse mode.** A cursor over
the rendered transcript. A double `s` enters it, under the
same two conditions as the `q q` exit path: the input area
is empty and the editor is in normal mode. The motions are
vim's. A line-number gutter shows the absolute number at
the cursor and relative numbers elsewhere. The detail
behavior matches the neovim configured on this machine
(section 6). A regex search on the log is discussed in
section 7.

## 2. Today (verified in code)

- `App.scroll` (`bin/tui/src/app.rs`): the visual lines
  scrolled up from the log end. `0` means "follow the
  tail."
- `Ctrl+U` / `Ctrl+D`: half a page up / down. `half_page()`
  is `(viewport - 1) / 2`, fallback `10`. The wheel moves
  three lines. `PgUp` / `PgDn` move ten. `SCROLL_CAP` is
  `100_000`.
- `render.rs` computes `start = total - scroll - h`. The
  window is the last `h` lines of the wrapped transcript.
- Nothing indicates the position. No bar, no line numbers,
  no distance-to-tail readout. No key jumps to the top or
  the end. The transcript holds no cursor.
- The quit gate (FT-012, `docs/failure-tracking.md`): `q`
  and `Ctrl+Q` arm and fire only in normal mode with an
  empty draft. Any other key disarms the arm. The armed
  `q` types nothing in that state.
- The `tui` crate holds no regex dependency (checked in
  `bin/tui/Cargo.toml`).

## 3. Contract 1 — the position bar

The bar is one column at the right edge of the transcript
pane. It is a pure function of the state: the total line
count, the visible height, the scroll offset, the cursor
line, and the mode. No new persisted state. A restart or a
session switch resets it, as it resets the scroll today.

Display rules:

| Mode | Condition | Bar |
|---|---|---|
| normal | `scroll > 0` (the view is not at the tail) | shown |
| normal | `scroll == 0` (the view follows the tail) | hidden |
| browse | always | shown |

The request words: the bar shows "when the user begins
scrolling." `scroll > 0` is that moment. At the tail the
bar is hidden. The user is at the latest position, and no
aid is needed.

Geometry (the track maps the loaded transcript, top =
oldest):

- track height: the pane visible height `h`. One track cell
  maps to `total / h` log lines, rounded up, at least one.
- window thumb: a contiguous run of cells. The thumb
  height is `max(1, h * h / total)`, capped at the track.
  The thumb bottom sits `scroll` log lines above the track
  bottom.
- tail marker: one accent cell at the track bottom. This is
  the "latest conversation position" anchor the request
  names.
- cursor marker (browse mode only): one accent cell at the
  cursor line.
- the loaded window: the transcript holds the last `2000`
  events (`TRANSCRIPT_EVENT_CAP`, `bin/tui/src/render.rs`),
  and the log read keeps the last `50 MB`
  (`MAX_LOG_READ_BYTES`, `bin/tui/src/port_file.rs`). The
  track maps that window, not the full log.

Column ownership: while the bar shows, it owns the
rightmost transcript column. The text width drops one
column and the transcript rewraps. The rewrap happens once
at the bar transition. The wrap cache is keyed by width
(the `transcript_cache` field of `bin/tui/src/app.rs`,
`docs/ui-extension-plan.md` stage 1), so a returning width
is cheap.
While the gutter shows (section 4.3), the gutter owns the
leftmost columns. The bar and the gutter never share a
column.

Colors: the track draws in the dim tone. The thumb draws in
the normal transcript tone. The tail and cursor markers
draw in the accent tone (the `Border4` role of
`docs/tui-color-scheme.md` section 6). Capability lowering
applies as to every other role.

## 4. Contract 2 — the browse mode

### 4.1 The line and cursor model

A "line" in this spec is one wrapped visual line of the
transcript at the current pane width (the `transcript_lines`
output). Lines number from `1` at the oldest. The machine's
neovim runs `wrap = false`, so this is a recorded deviation
D1 (section 6.4): the TUI line is the rendered unit, not
the source unit.

The cursor is `(line, col)`. `line` runs `1..=total`. `col`
is the zero-based character position within the visual
line. Every move clamps `col` to the line length, like
neovim.

### 4.2 Entry and exit

Entry: a double `s`, under the same two conditions as the
`q q` exit path:

1. the input area (draft) is empty;
2. the editor is in normal mode.

Host routing mirrors the FT-012 quit arm:

- in the gated state (normal, empty draft), the first `s`
  arms. A second `s` inside `3 s` enters browse mode.
- any other key disarms. The armed `s` drops, like the
  armed `q` of FT-012. The disarming key then acts in the
  editor alone, with no `s` effect.
- the arm expires after `3 s`. A fresh `s` re-arms.
- in every other state (`insert`, `replace`, `visual`, the
  command line, the name input, a non-empty draft), `s`
  goes to that input untouched. In the editor's normal
  mode with a draft, `s` keeps its editor role (change one
  char, `docs/vim-editor-design.md` section 3). In that
  state the status row hints `ss browses — clear the
  draft first`, like the FT-012 `q` hint.

Entry places the cursor on the first visible line, col `0`.
The view does not move on entry. The user's eyes sit on the
current view, and `gg` / `G` reach the edges in one
stroke.

Exit:

- a double `s` in browse mode leaves it. The same `3 s` arm
  window. The view stays where browse left it (`scroll`
  unchanged): the user is still on the same spot of the
  conversation. The cursor, the gutter, and the bar drop
  away. The editor is in normal mode with the still-empty
  draft.
- `q` and `Ctrl+Q` in browse mode keep the quit gate. The
  gate still holds (empty draft, editor in normal mode
  under the overlay), so a double `q` quits the TUI as
  today. Loops keep running as orphans. Only `Ctrl+C` stops
  a loop (FT-012, unchanged).
- `Tab` in browse mode exits it, then cycles sessions. The
  browse state never survives a session switch. The scroll
  resets today, and the browse state resets with it.

While browse mode holds, the editor is frozen. The draft
cannot change. The empty-draft invariant of the gate holds
for the whole browse session.

### 4.3 The line-number gutter

Shown only in browse mode. Outside browse mode the pane
draws as today: no gutter, and no bar at the tail.

Side: the gutter goes on the **left**, the bar on the
**right** (the request asks for a suggestion). Rationale:

- the machine's neovim draws the number gutter on the left
  (`number = true`, `relativenumber = true`,
  `signcolumn = "yes"` in `lua/core/options.lua`).
- the left edge anchors the text. The eye reads the number,
  then the content, in one left-to-right pass.
- the right edge already carries the bar (section 3). Two
  aids on one edge crowd each other. Left numbers, right
  position keeps each aid readable alone.

Content (hybrid numbering, exactly the machine's `number`
plus `relativenumber`):

- the cursor line shows the **absolute** line number;
- every other visible line shows the **relative** distance
  to the cursor line;
- the cursor number draws in the accent tone; the rest
  draw in the dim tone (the machine's `CursorLineNr`
  highlight, lowered to the active capability level);
- the cursor line itself gets the `cursorline` highlight
  (the machine sets `cursorline = true`): a low-contrast
  background across the line, under the cursor block.

Width: the gutter is as wide as the digit count of `total`
plus one trailing space, like neovim. Numbers right-align
in it. The text shifts right by the gutter width in browse
mode only. The transcript rewraps at the mode transition,
as with the bar.

### 4.4 The key table

Counts prefix the motions, like the editor (`docs/tui.md`
section 7.1): a count, then the key, capped at `99999`. A
count of `0` is a no-op motion. Unbound characters are a
no-op. The status row hints one line: `browse: gg top, G
end, :N line, ss leave`.

| Key | Action | neovim match |
|---|---|---|
| `j` | cursor down one line, col clamped | `j` |
| `k` | cursor up one line, col clamped | `k` |
| `h` | cursor left one column, floor `0` | `h` clamped (the compat fix, `docs/vim-editor-design.md` section 1) |
| `l` | cursor right one column, ceiling the line end | `l` clamped |
| `<count>j` / `<count>k` | n lines down / up | `5j` |
| `<count>h` / `<count>l` | n columns left / right | `5h` |
| `gg` | cursor to line `1`, view to the top | `gg` |
| `G` | cursor to the last line, view to the tail (`scroll = 0`) | `G` |
| `<count>gg` | cursor to line n | `5gg` |
| `Ctrl+U` | view half a page up; the cursor jumps to the new top line if the scroll strands it | `Ctrl-U` (the view scroll, cursor pulled when stranded) |
| `Ctrl+D` | view half a page down; the cursor jumps to the new bottom line if stranded | `Ctrl-D` |
| `:N` | the command line: cursor to line N, clamped `1..=total` | the ex-form goto-line |
| `:Nj` / `:Nk` | accepted, the `j` / `k` ignored: the ex-form already names the line | the request's form, mapped to `:N` |
| `Esc` | cancel a pending count; close the open search line; clear the active search highlight (section 7); never leaves browse mode | the machine's `Esc` = `noh` (section 6.2) |
| `s` ×2 | leave browse mode (section 4.2) | — |
| `q` ×2 / `Ctrl+Q` | quit the TUI (the gate holds, section 4.2) | — |
| `Ctrl+C` / `Ctrl+R` | host roles: stop / start the loop | — |
| `Ctrl+O` / `Ctrl+T` / `Ctrl+X` | host roles: the tool fold, the thinking toggles (the cursor clamps to the new line count) | — |
| `Tab` | leave browse mode, then cycle sessions | — |

`Ctrl+U` / `Ctrl+D` in browse mode belong to the browse
handler. They do not reach the editor command-line or
idle-composer roles (`docs/tui.md` section 7): the browse
handler owns the keys while the mode holds. The host roles
of `Ctrl+U` / `Ctrl+D` in normal mode stay unchanged.

The `:N` command line reuses the editor's box-title prompt
render (`docs/tui.md` section 7.1): `:42█`. `Enter`
commits, `Esc` cancels. Anything but a line number hints
`line: 1..total` and moves nothing.

### 4.5 View following (the `scrolloff` rule)

The machine's neovim sets `scrolloff = 3`. After any cursor
motion, the view keeps at least three lines above and
three lines below the cursor line. The view scrolls the
minimum lines that satisfy both margins. When the log
edge sits closer, the view pins to that edge. `gg` pins
the top. `G` pins the tail. The wheel and `Ctrl+U` /
`Ctrl+D` move the view first. The cursor follows the edge
only when the scroll would strand it (the neovim `Ctrl-U`
/ `Ctrl-D` edge rule, section 4.4).

### 4.6 New events while browsing

The tailer keeps appending (`docs/tui.md` section 13.4).
The transcript grows: `total` rises and the wrap cache
rebuilds (existing behavior; the `transcript_cache` field
of `bin/tui/src/app.rs`). The cursor pins to its line
number: existing lines keep their numbers, and new lines
append. Under the `2000`-event cap, `total` holds: each
new event drops the oldest line, and every number shifts
down one. The view does not auto-follow new events in browse
mode. The bar's tail marker shows the live position moving
past the view. Leaving browse mode lands the view on the
grown tail.

The fold and thinking toggles keep their host roles in
browse mode. They change the line count. The cursor clamps
to the new total, the numbers recompute, and the bar
redraws.

### 4.7 Failure modes

| Condition | Behavior |
|---|---|
| `total` fits the view | `gg` and `G` land on the same line; the gutter shows `1..total`; the bar thumb spans the full track |
| an empty transcript (no events) | entry is allowed; the cursor is absent; every motion is a no-op; `gg` / `G` hint |
| `:N` with `N > total` | the cursor lands on the last line (the neovim clamp) |
| `:N` with `N = 0`, an empty input, or a non-number | no move; the command line hints `line: 1..total` |
| pane resize while browsing | the transcript rewraps, `total` changes, the cursor col clamps, the view re-centers on the cursor with the scrolloff margins |
| a fold or thinking toggle changes `total` under the cursor | the cursor clamps to the new total |
| the event cap | `total` holds; each new event drops the oldest line; every number shifts down one; the cursor keeps its number and rebinds to the shifted line |
| a session switch | the browse state resets with the scroll (section 4.2) |
| a TUI restart | no browse state, no scroll position: both reset to the tail (the TUI is a view, `docs/tui.md` section 1) |
| the arm window | a second `s` after `3 s` re-arms; any other key disarms; the armed `s` drops (the FT-012 disarm) |

## 5. What this does not do

- No transcript editing. Browse mode is read-only. The log
  stays the source of truth (`docs/tui.md` section 1).
- No marks and jumps (`m` / backtick) on the log.
- No search outside browse mode (section 7 is the decision
  space).
- No smooth-scroll animation. The machine's `neoscroll`
  animates the half-page keys; the TUI jumps instantly
  (deviation D2, section 6.3).
- No scroll or cursor persistence across restarts or
  session switches.
- No change to the `q q` quit gate, the FT-012 behavior, or
  the editor's `s` motion outside the gated state.
- No bar or gutter in the input box, the statusline, or the
  extension panes.
- No transcript editing or buffer-mutating operators
  (`d` / `c` / `x` / `>` / `<`). Stage 3 adds the read-only
  `y` operator and visual selection only (section 11).

## 6. Reference: the neovim on this machine

The `~/.config/nvim` tree (lazy.nix layout: `lua/core/`,
`lua/keymap/`, `lua/modules/`).

### 6.1 The options the spec matches

| machine option | value | TUI mapping |
|---|---|---|
| `number` / `relativenumber` | `true` / `true` | the hybrid gutter of section 4.3 |
| `cursorline` | `true` | the cursor-line highlight of section 4.3 |
| `scrolloff` | `3` | the view following of section 4.5 |
| `signcolumn` | `"yes"` | deviation D4 |
| `wrap` | `false` | deviation D1 |
| `ignorecase` / `smartcase` | `true` / `true` | the search case rules, section 7 |
| `incsearch` | `true` | the live first-match jump, section 7 |
| `wrapscan` | `true` | `n` / `N` wrap the ends, section 7 |
| `magic` | `true` | the patterns are regex, section 7 |
| `jumpoptions` | `"stack,view"` | the `n` / `N` view restore, section 7 |

### 6.2 The machine's search keys (`lua/keymap/editor.lua`)

- `n` maps to `nzzzv`: the next match, then the view
  centers on it (`zz`) and folds open (`zv`). The TUI
  mapping: `n` / `N` jump and center the view on the
  match. The `zv` part is a no-op: the log has no folds.
- `Esc` in normal mode runs `flash_esc_or_noh`: hide an
  active flash session, else `noh` (clear the search
  highlight). The TUI mapping is the `Esc` row of section
  4.4.

### 6.3 The plugins

- `dstein64/nvim-scrollview` (`lua/modules/configs/ui/
  scrollview.lua`): a right-edge position bar in virtual
  mode, `winblend = 0`, with startup signs for folds,
  marks, and search. The section 3 bar is the TUI analog:
  one right-edge column, position plus markers. The signs
  map to bar markers only (the cursor, the tail). That is
  deviation D3.
- `karb94/neoscroll.nvim` (`lua/modules/configs/ui/neoscroll.lua`):
  smooth animations on `<C-u> <C-d> <C-b> <C-f> <C-y>
  <C-e>` and `zt zz zb`. Deviation D2: the TUI has no
  animation engine (the redraw is the event-driven loop,
  about `100 ms`). The TUI half-page keys jump instantly.
  The distances stay the neovim ones.

### 6.4 Deviations, recorded

- D1 — the line unit: the machine's nvim never wraps
  (`wrap = false`). The TUI transcript wraps at the pane
  width. The spec line is the wrapped visual line.
- D2 — no smooth-scroll animation (the `neoscroll` gap
  above).
- D3 — the bar carries no fold or mark signs. The log has
  neither.
- D4 — the gutter shows only in browse mode. The machine's
  `signcolumn = "yes"` always draws a sign column. The TUI
  pane holds no signs outside browse mode, so the gutter is
  browse-only (the request says "when the browsing mode is
  active").

## 7. Discussion: a regex search in browse mode

The request asks whether the browse mode takes a regex
search on the conversation log. Decision: yes, as stage 2
of this feature. Section 7.3 gives the scoped contract.

### 7.1 What the machine's neovim already assumes

`magic = true` (regex is the default pattern language),
`ignorecase` plus `smartcase` (case-blind unless the
pattern holds an uppercase), `incsearch` (the first match
jumps live while the pattern types), `wrapscan` (`n` /
`N` wrap), the `n` / `N` remaps of section 6.2, and `Esc`
= clear the highlight. A literal-only search would not
match this setup.

### 7.2 The options

- A. No search. The bar, `gg` / `G`, and `:N` fix the
  position sense. Reading a long log without a find stays
  the exact pain the request names.
- B. Literal search, reusing the editor's command-line
  engine over the transcript lines. Cheap: no new
  dependency. But the machine's setup assumes regex, and a
  literal find over command output, code, and paths is the
  weak tool.
- C. Regex search, scoped to browse mode (below).

### 7.3 The scoped contract (stage 2)

Keys (the command line reuses the editor's box-title
prompt, `docs/tui.md` section 7.1):

- `/` opens a forward search, `?` a backward one. The
  pattern types into the box title. `Enter` commits,
  `Esc` cancels and clears the highlight (section 6.2),
  and a backspace on the empty input cancels.
- patterns are Rust `regex` syntax (the `magic` match).
  The `tui` crate gains the `regex` dependency (stage 2
  only; `bin/tui/Cargo.toml` holds none today).
- case: the machine's `ignorecase` plus `smartcase`. A
  pattern without an uppercase compiles case-blind; with
  one, it compiles case-sensitive.
- `incsearch`: each keystroke re-matches, and the cursor
  jumps to the first match in the search direction. The
  cost is one pass over the cached wrapped transcript
  lines per keystroke. This is what the machine's nvim
  does too.
- `n` / `N` step match to match, wrapping the ends
  (`wrapscan`), then center the view on the match
  (section 6.2, the `zz` remap).
- view restore (`jumpoptions = "stack,view"`): a backward
  `N` after a forward jump restores the view state held
  before that forward jump. One remembered
  `(scroll, line, col)` pair is enough.
- `*` / `#`: the word under the cursor (the longest
  alphanumeric plus underscore run), escaped into a
  pattern, searched forward / backward. The machine's
  defaults (no override found).
- highlight: while a search is active, every match line in
  the visible transcript draws in the highlight tone. The
  current match line draws in the accent tone. `Esc`
  clears it (section 6.2). Leaving browse mode clears it.
- an invalid pattern keeps the last valid one. The command
  line hints the regex error.
- a pattern matches inside one visual line only (the
  neovim single-line search; no line-spanning patterns).

### 7.4 What stays out

- No host-level, browse-free find over the pane. That is a
  different feature with its own key question.
- No replace (no `:s`): the log is append-only. The TUI
  never edits it (section 5).
- No multi-line patterns, no sub-replace, no search
  history beyond the last pattern.
- No telescope / fzf integration (the machine's tool-level
  search backend, out of TUI scope).

## 8. Stages

Stage 1 — the priority set (the request's "highest UX
priority"): the position bar (section 3), the browse mode
entry / exit (section 4.2), the key table (section 4.4,
without the search rows), the gutter (section 4.3), the
scrolloff view following (section 4.5), and `gg` / `G`. No
`regex` dependency yet.

Stage 2 — the regex search (section 7.3): the `regex`
dependency, the `/` `?` command line, `n` `N` `*` `#`,
the match highlight, and the view restore.

Stage 3 — select-and-yank (section 11): the visual
selection, the `y` operator with the word / line-end /
last-line / counted-line motions, the inside and around
text objects, and the shared register store. No new
dependency: the `vim_editor.rs` primitives are reused.

## 9. Conformance tests

Unit tests live in `bin/tui/src/` (the repo convention:
`cargo test -p tui`). Every row is a check.

| Test | Given | Expected |
|---|---|---|
| bar hidden at the tail | normal mode, `scroll = 0` | no bar column; the text width is the full pane |
| bar on scroll-back | normal mode, `scroll > 0` | one right-edge column; the thumb bottom is `scroll` lines above the track bottom |
| bar geometry | `total = 1000`, view `24`, `scroll = 500` | thumb height `max(1, 24 * 24 / 1000)` = `1`; the thumb bottom is `500` lines above the bottom |
| bar in browse | browse mode, `scroll = 0` | the bar shows; the cursor marker sits on the last line |
| gutter numbering | the cursor is line `50` of `100` | line `50` shows `50` (the accent tone); lines `40` and `60` show `10` (the dim tone); line `1` shows `49` |
| gutter width | `total = 100` | three digits plus one space, right-aligned |
| gate: insert | insert mode, empty draft | the first `s` types into the editor (the typed char); no arm |
| gate: disarm | normal, empty; `s` then `i` inside `3 s` | no browse; the armed `s` drops (the FT-012 disarm); `i` enters insert alone |
| gate: non-empty | normal mode, the draft held | the `s` goes to the editor; no arm; the hint `ss browses — clear the draft first` |
| gate: enter | normal, empty; `s s` inside `3 s` | browse; the cursor is the first visible line, col `0`; the view does not move; the gutter and the bar draw |
| gate: expired arm | the arm expires; a fresh `s` | a new arm |
| gate: exit | in browse; `s s` inside `3 s` | normal mode; the view stays where browse left it; the gutter and the bar drop |
| quit in browse | in browse; `q q` | the TUI quits (the gate holds) |
| `j` / `k` clamp | the cursor is line `1`; `k` | no move (the floor) |
| `h` / `l` clamp | the cursor is col `0`; `h` | no move |
| counted motion | `5j` from line `3` | the cursor is line `8`; the col clamps |
| `0j` | — | no move |
| `gg` | cursor line `40` of `100`, view mid | cursor line `1`, view at the top |
| `5gg` | — | cursor line `5`, view at the top with the scrolloff margins |
| `G` | — | cursor the last line, view at the tail (`scroll = 0`) |
| `:42` | `total = 100` | cursor line `42` |
| `:999` | `total = 100` | cursor line `100` (the clamp) |
| `:42j` / `:42k` | `total = 100` | cursor line `42` (the suffixes ignored) |
| `Ctrl+U` strand | the cursor sits on the view top; `Ctrl+U` | the view up half a page; the cursor lands on the new top line |
| scrolloff margin | a `j` puts the cursor within three lines of the view bottom | the view scrolls the minimum lines to clear the margin |
| grow while browsing | browse; a new event lands | `total` rises; the cursor pins; the tail marker moves; the view does not follow |
| exit after grow | the above, then `s s` | the view stays where browse left it |
| resize while browsing | the pane narrows; the wrap reflows | the cursor col clamps; the view re-centers |
| unbound key | a letter not in the table | the status hint; no state change |
| stage 2: `/err` | the log holds `error` lines | the cursor jumps to the first match live (`incsearch`); the match lines highlight |
| stage 2: smartcase | the pattern `ERR` | case-sensitive: only `ERR` matches |
| stage 2: `n` / `N` wrap | the cursor on the last match; `n` | the cursor wraps to the first match; the view centers |
| stage 2: bad pattern | `(` | the hint shows the regex error; the last valid pattern stays active |
| stage 2: `*` | the cursor on the word `foo_bar` | the search for the escaped `foo_bar`, forward |
| mutation gate | the bar draw is deleted | the bar tests fail |
| mutation gate | the arm logic is deleted | the gate tests fail |

## 10. Impact

Affected:

- `bin/tui/src/app.rs` — the browse state, the `s` arm,
  the scroll clamp, the gate check. Stage 3 adds the shared
  register store lifted from `Editor` to `App`, referenced by
  both `Browse` and `Editor` (section 11.3).
- `bin/tui/src/main.rs` — the key routing: the `s` arm in
  the gated state, the browse key table, the `Ctrl+U/D`
  reroute while browse holds.
- `bin/tui/src/render.rs` — the bar column, the gutter
  column, the cursor-line highlight, the width math. Stage 3
  adds the visual-selection highlight.
- `bin/tui/src/browse.rs` (new) — the browse state machine
  and the key table. The stage 2 search state. The stage 3
  visual selection, the `y` operator, and the yank wiring
  (section 11).
- `bin/tui/src/vim_editor.rs` — the motion and text-object
  primitives (`word_forward`, `line_end`, `go_to_last_line`,
  `resolve_text_object`, `extract_text`, `yank_to_register`,
  etc.) become `pub(crate)` so the browse module can reuse
  them (section 11.5). The `registers` store moves from
  `Editor` to `App` (section 11.3).
- `bin/tui/Cargo.toml` — the `regex` dependency (stage 2).
  No new dependency in stage 3.

Unaffected: the loop, the schemas, the extension protocol,
every binary but `tui`, the editor's mode handlers (the arm
intercepts only the gated state), the `q q` gate, the
tailer, `SessionPort`.

Acceptance: `cargo test -p tui` passes, including the
conformance rows above. A live session shows the bar on
scroll-back, the browse cursor and gutter, and `G` back to
the live tail. The request items close with Shipped notes
in `docs/tui_feature_requests_from_human.md`.

## 11. Contract 3 — select-and-yank in browse mode

Stage 3 of this feature (section 8). The request lives in
`docs/tui_feature_requests_from_human.md` (the 2026-09-05
follow-up item).

### 11.1 The request

The browse mode (section 4) already holds a cursor over the
rendered transcript. The request adds vim's select and yank
on top of it, with three points:

- the browse mode is the natural fit for vim's `Visual`
  mode: select text on the log, then yank it to a register.
- the select-and-yank path is the quoting tool. Anything in
  the conversation can be quoted into the draft to ask the
  agent about it.
- the yank operator cooperates with the existing browse
  motions: `yw` (a word), `y$` (to the line end), `yG`
  (to the last line), `<n>yy` (n lines), and the `i` /
  `a` text objects: inside double quotes, single quotes,
  parentheses, square brackets, and braces.

### 11.2 The operator set is read-only

- The transcript is read-only (section 5). Browse mode
  never edits it.
- `y` is the only operator that makes sense on a read-only
  buffer. `d` / `c` / `x` / `>` / `<` mutate the buffer and
  stay out of scope (section 11.7).
- A yank writes to the register store. It changes no
  transcript line.
- The yanked text is the rendered transcript text (the
  `transcript_lines` output of `bin/tui/src/render.rs`),
  not the source event JSON. The deviation D1 of section
  6.4 holds: the line unit is the wrapped visual line.

### 11.3 The shared register store

- The editor already owns the register set (the `registers`
  field of `bin/tui/src/vim_editor.rs`, the pi-vim
  `registers.ts` port). It stores a `RegContent` per
  register: the text plus the linewise flag.
- Stage 3 lifts that store to `App` and shares one store
  between the `Editor` and the `Browse` state machine. The
  editor's `paste` reads the store as today. The browse
  yank writes into it.
- The yank writes through the editor's `yank_to_register`
  function. The merge semantics stay the vim ones: a yank
  sets the unnamed `"` register and the `0` register. A
  named `A-Z` prefix appends to the lowercase register.
  `+` / `*` alias the system clipboard. `_` discards.
- The register prefix works in browse mode, like the
  editor: `"a y ...` yanks to register `a`. A bare `y`
  uses the unnamed register.
- The quoting path: enter browse, yank the span, `ss`
  leaves browse, `p` in the editor pastes the yank into
  the draft. The user then edits and sends it to the
  agent.
- No new persisted state. A session switch or a TUI restart
  resets the registers with the rest of the browse state
  (section 4.7), like the scroll reset today.

### 11.4 The key table (the section 4.4 additions)

Counts prefix the motions, like section 4.4: a count, then
the key, capped at `99999`. A count of `0` is a no-op.
`Esc`, the host keys, and the `q` / `Tab` rows keep their
section 4.4 roles. Stage 3 adds the visual state and the
`y` operator:

| Key | Action |
|---|---|
| `v` | char-visual: the anchor sits at the cursor |
| `V` | linewise visual: the anchor holds the cursor line |
| a motion in visual (`j` `k` `h` `l` `w` `0` `^` `$` `G`) | extend the selection from the anchor to the motion target |
| `y` in visual | yank the selection; leave visual; the cursor moves to the selection end |
| `Esc` in visual | cancel the selection; the cursor returns to the anchor |
| `y` | open the yank operator: a motion or a text object follows |
| `yy` / `Y` / `<n>yy` | yank n whole lines (the linewise rule, like the editor) |
| `yw` | yank to the end of the word under the cursor (the operator `w` rule: the `extend_w_eol` extension) |
| `y$` | yank to the line end, inclusive |
| `yG` | yank from the cursor line to the last line, linewise |
| `y0` | yank from the cursor to the line start |
| `y^` | yank from the cursor to the first non-blank char |
| `yi"` / `ya"` | yank inside / around the double quotes |
| `yi'` / `ya'` | yank inside / around the single quotes |
| `yi(` / `ya(` | yank inside / around the parentheses |
| `yi[` / `ya[` | yank inside / around the square brackets |
| `yi{` / `ya{` | yank inside / around the braces |
| `yi<` / `ya<` | yank inside / around the angle brackets |

Notes:

- the `a` (around) forms take the same object with the
  enclosing delimiters included.
- an operator-pending key is a motion: while `y` waits, the
  `G` of `yG` and the `w` of `yw` act as motions, like the
  editor's `pending_operator` rule. A bare `G` without a
  pending operator keeps the section 4.4 role.
- an unmatched text object is a no-op with a hint
  (section 11.8). The operator clears.
- the yank operator is not a change: no recording, no undo
  stack (the browse mode has neither; the editor keeps its
  own undo for the draft).
- the status hint grows one clause: `browse: v select, y
  yank, yy lines, yw word, ss leave`.

### 11.5 The reuse plan

The motion, text-object, and register logic already exists in
`bin/tui/src/vim_editor.rs` (the pi-vim port). Every
primitive is a pure function over `&[String]` lines and a
`(usize, usize)` cursor. That is the browse `View` shape:
the `texts` slice is `&[String]`, the cursor is `(line, col)`
over the same lines.

Reuse (the functions stay in `vim_editor.rs`, made
`pub(crate)` for `browse.rs`):

- motions: `word_forward`, `word_end`, `line_end`,
  `line_start`, `first_nonblank_motion`, `char_left`,
  `char_right`, `go_to_last_line`, `extend_w_eol`;
- the range types and builders: `MotionResult`, `OpRange`,
  `motion_to_range`, `text_object_to_range`, `extract_text`;
- the text objects: `resolve_text_object` (the `i` / `a`
  prefixes over `"`, `'`, `` ` ``, `(`/`)`, `[`/`]`,
  `{`/`}`, `<`/`>`) and the `TextObjectFn` type;
- the registers: `RegContent`, `yank_to_register`,
  `get_register`, `is_valid_register`.

Hand-roll in `browse.rs` (the browse-specific glue only):

- the visual selection state: the anchor `(line, col)`, the
  char / linewise flag, the extension on a motion, the
  cancel on `Esc`;
- the key rows of section 11.4: the `y` operator state, the
  text-object pending prefix, the `v` / `V` entry;
- the yank wiring: build the `OpRange`, run `extract_text`
  over the view `texts`, write the shared register;
- the store lift: the `registers` map moves to `App`; the
  `Editor` and the `Browse` both operate on that one store.

The complexity is bounded by the editor's operator
machinery, which already carries it. Stage 3 adds no second
copy of that logic. Porting more vim operators beyond this
set is not justified: the request names exactly these
motions and objects, and they are already built.

### 11.6 The crate question

The request asks whether an external Rust crate should carry
the operator logic if this part grows. It does not:

- the primitives exist in-tree (`bin/tui/src/vim_editor.rs`)
  and are the source of truth for the editor's motions.
  A crate copy would drift from that source of truth;
- the surveyed external options do not fit. `hjkl-engine`
  (the vim FSM and motion grammar, pre-1.0) targets an
  interactive editing grammar. It would duplicate the
  operator dispatch and add a heavy dependency for logic
  that is already written. `vii` binds a live vim instance.
  The full editors (OxideEdit, edtui, and the rest) are
  applications, not reusable operator libraries;
- decision: no new crate. `bin/tui/Cargo.toml` gains no
  dependency in stage 3.

### 11.7 What stage 3 does not do

- no buffer-mutating operators (`d` / `c` / `x` / `>` /
  `<`): the transcript is read-only (section 11.2);
- no block visual (`Ctrl+V`), no multi-cursor, no `.`
  repeat in the browse mode;
- no `:g` / `:global` over the log;
- no text object beyond the editor's set (`"` `'` `` ` ``
  `()` `[]` `{}` `<>`);
- no register persistence across a restart or a session
  switch (section 5).

### 11.8 Failure modes

| Condition | Behavior |
|---|---|
| a text object with no closing delimiter | no range; the yank is a no-op; the hint names the object |
| an empty visual selection (`v` then `Esc`) | no register write; the cursor returns to the anchor |
| a linewise yank across lines | `extract_text` joins the lines with a newline (the editor rule) |
| a wrapped visual line mid-word | the yank is over the rendered line; the object respects the rendered chars (deviation D1, section 6.4) |
| the cursor on the last line, `yG` | the range is that line alone; one line is yanked |
| a counted `yG` | the target clamps to the last line (the `G` rule, section 4.4) |
| a linewise then a char yank to one register | the merge rule of `yank_to_register`: the join inserts a newline between linewise pieces |
| an empty transcript | every yank is a no-op (the section 4.7 motion rows hold) |
| a session switch or a restart | the registers reset with the browse state (section 4.7) |

### 11.9 Conformance tests (the stage 3 rows)

Unit tests live in `bin/tui/src/browse.rs` (the repo
convention: `cargo test -p tui`). Every row is a check.

| Test | Given | Expected |
|---|---|---|
| `yw` | the cursor mid-word on a line | the word text lands in the `"` register; the cursor moves to the word end |
| `y$` | the cursor at col 2 | the text from col 2 to the line end lands in the register (inclusive) |
| `yG` | the cursor on line 3 of 10 | the lines 3..10 land in the register, linewise |
| `3yy` | the cursor on line 2 of 10 | the lines 2..4 land in the register, linewise |
| `yi"` | the cursor inside a quoted span | the inside text lands in the register, without the quotes |
| `ya(` | the cursor inside a paren span | the span plus the two parens lands in the register |
| the visual yank | `v` `j` `j` `y` | the three lines land in the register (the linewise join) |
| the visual cancel | `v` `j` `Esc` | no register write; the cursor is the anchor |
| the register handoff | a browse yank; `ss`; `p` in the editor | the draft gains the yanked text |
| the named register | `"a yw` | the word lands in register `a`, not the unnamed one |
| the unmatched object | `yi[` with no `]` on the line | a no-op; the hint names the object; the operator clears |
| the read-only gate | any yank in browse | the transcript lines are unchanged |
| the mutation gate | the yank wiring is deleted | the handoff and the register rows fail |

## Properties

Lean-style invariants for this spec (see `lean-driven-development.md`).
Each property is observable: given an input, an output guarantee.

P1. bar-visibility: given the view is not at the tail (scroll > 0) or the
    user is in browse mode, observe the position bar shown at the right
    edge of the transcript pane; given the view at the tail in normal
    mode, observe the bar hidden.
P2. bar-geometry: given total line count T, viewport height H, and
    scroll offset S, observe the thumb height equal max(1, H*H/T) with
    its bottom S lines above the track bottom.
P3. gutter-numbering: given browse mode with the cursor at line N of
    total T, observe line N show its absolute number in the accent tone
    and every other visible line show its relative distance in the dim
    tone.
P4. browse-entry-exit: given normal mode with an empty draft, observe
    a double `s` within 3 s enter browse mode and a second double `s`
    exit it; given a non-empty draft or insert mode, observe `s` not
    trigger browse entry.
P5. jump-motions: given `gg`, observe the cursor on line 1 and the view
    at the top; given `G`, observe the cursor on the last line and the
    view at the tail (scroll = 0).
P6. goto-line: given `:N` where N is within 1..=total, observe the
    cursor move to line N; given N greater than total, observe the
    cursor clamped to the last line; given a non-number, observe no
    move and a hint.
P7. regex-search: given a forward regex pattern typed after `/`,
    observe the cursor jump to the first match on each keystroke and
    all match lines highlight; given `n` or `N`, observe step to the
    next or previous match with wrap-around and the view center on the
    match.
P8. yank-read-only: given a yank operator or text object in browse
    mode, observe the yanked text land in a register and the
    transcript lines remain unchanged.

## Verification

Each property maps to its proof. `proven` means the cited test exists
and passes. `open` names the blocker and what unblocks it.

| P# | Property | Proof | Status |
|----|----------|-------|--------|
| P1 | bar-visibility | `bar_hides_at_the_tail`, `bar_shows_on_scroll_back`, `bar_shows_in_browse_with_the_cursor_marker` in `bin/tui/src/render.rs` | proven |
| P2 | bar-geometry | `bar_geometry_row` in `bin/tui/src/browse.rs` | proven |
| P3 | gutter-numbering | `gutter_numbering`, `gutter_width_is_digits_plus_one` in `bin/tui/src/browse.rs` | proven |
| P4 | browse-entry-exit | `browse_gate_enter_s_s`, `browse_gate_exit_s_s`, `browse_gate_non_empty_draft_hints` in `bin/tui/src/app.rs` | proven |
| P5 | jump-motions | `gg_lands_on_line_one_at_the_top`, `g_lands_on_the_last_line_at_the_tail` in `bin/tui/src/browse.rs` | proven |
| P6 | goto-line | `goto_line_42_of_100`, `goto_clamps_to_the_last_line`, `goto_non_number_hints_and_moves_nothing` in `bin/tui/src/browse.rs` | proven |
| P7 | regex-search | `forward_search_jumps_live_and_highlights`, `n_wraps_to_the_first_match_and_centers`, `n_after_a_forward_jump_restores_the_saved_view` in `bin/tui/src/browse.rs` | proven |
| P8 | yank-read-only | open: stage 3 select-and-yank not yet implemented in `bin/tui/src/browse.rs`; unblocked when the `y` operator, visual selection, and shared register store are wired into browse mode. | open |

## Gate

The acceptance commands. All must exit 0 for this spec to be proven.

```
cargo build
cargo test -p tui
```
