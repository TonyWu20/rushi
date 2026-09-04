# Vim modal input — design plan

The input area is a multi-line draft editor with vim modal input
(`bin/tui/src/vim_editor.rs`). This document is the full plan for that
feature. It maps the feature 1:1 onto the reference implementation the
user pinned, and it lists every divergence and every fix.

## 1. The reference

The reference is the `pi-vim` extension, pinned in
`~/programming/pi-config/flake.nix`:

- `burneikis/pi-vim` at rev `b53ce8f` (v1.7.0) — the editor engine
  (modes, motions, operators, registers, text objects, dot-repeat,
  search, command-line mode).
- `extensions/vim-modal.ts` (local) — the statusline bridge. It only
  publishes the mode label (`vim-modal.ts` `MODE_LABEL` set:
  NORMAL / INSERT / REPLACE / VISUAL / V-LINE / COMMAND and the
  `[d-PENDING]` operator-pending form). The label set in
  `vim_editor.rs` must match it.
- Two upstream fix commits after the pin close the bugs reported for
  this TUI. The target behavior is the pinned rev plus these fixes:
  - `8b99ecc` "line movement compat fixes": `h`/`l` clamp inside the
    line (no wrap to the next line); `x`/`X` delete a counted range
    clamped to the line (no line joining); `w`/`W` skip indentation
    when they cross to a line; counted `O` inserts one copy per count
    after `Esc`.
  - `7f19ea2` "fix paste handling": host-side bracketed-paste
    plumbing. Not needed here; our editor owns its text directly.

The engine files and their Rust counterparts:

| reference file     | Rust counterpart                                  |
| ------------------ | ------------------------------------------------- |
| `state.ts`         | `Editor` fields + `Mode`                          |
| `motions.ts`       | `mod motions` (word/WORD/line/find/para/bracket)  |
| `operators.ts`     | `mod operators` (range extract/delete/indent)     |
| `registers.ts`     | `Register` map with a `linewise` flag             |
| `text-objects.ts`  | `mod text_objects` (word, quotes, brackets)       |
| `repeat.ts`        | `RecordedChange` + replay (`.`)                   |
| `search.ts`        | search state + `/` `?` `n` `N` `*` `#`            |
| `modes/insert.ts`  | `Mode::Insert` handler                            |
| `modes/replace.ts` | `Mode::Replace` handler + `replaced_chars` stack  |
| `modes/visual.ts`  | `Mode::Visual` / `Mode::VisualLine` handler       |
| `modes/normal.ts`  | `Mode::Normal` handler + paste + dot replay       |
| `vim-editor.ts`    | rendering of the mode indicator (our `render.rs`)  |

## 2. State

The editor owns one buffer (`Vec<String>`), a cursor `(row, col)`,
and the vim state. The state fields mirror `state.ts`:

- `mode`: Normal / Insert / Replace / Visual / VisualLine / CommandLine
  (idle state is Insert: the composer starts in typing mode).
- `count` / `count_started`: the numeric prefix (capped at 99999).
  `0` extends an open count; a bare `0` is not a count start.
- `pending_operator` / `pending_operator_count`: `d c y > <` await a
  motion or a text object. A second copy (`dd`) is linewise.
  Operator and motion counts multiply (`2d3w` = six words).
- `register`: the active register char (`"` default, `"r` selection).
- `visual_anchor`: the other end of the selection.
- `pending_char_motion`: `f F t T r` await their character.
- `pending_g`: the first `g` of `gg`.
- `pending_text_object_prefix`: `i` / `a` after an operator.
- `pending_register`: the `"` selection awaiting a register name.
- `open_line_repeat_count`: counted `O` repeats the inserted line.
- `last_char_search`: `;` / `,` repeat the last find motion.
- search state: last pattern, direction, command-line buffer.
- `last_change` + `current_recording`: the dot-repeat system.
- `replaced_chars`: replace-mode backspace restores originals.
- undo / redo stacks of (lines, row, col) snapshots (`u`, and
  `Ctrl+R` when the editor holds redo state; otherwise `Ctrl+R`
  keeps its host role of starting the loop).

## 3. Modes and key map

### Normal

- digits: open the count (`0` extends).
- `d c y > <`: open the operator. `dd cc yy >> <<`: linewise,
  counted (`3dd`). `D C Y`: `d$ c$ yy` shortcuts.
- `"`: register selection. `.`: dot-repeat. The pinned reference
  replays the recorded key sequence (any recorded count digits are
  baked into it); a bare `N.` override does not multiply the
  replayed change.
- `p P`: paste (count = number of copies for linewise; char-wise
  pastes ignore the count, like the reference).
- motions: `h j k l 0 $ ^ w b e W B E g g G { } % ; , f F t T`
  (with their character), `n N * # / ?` (search).
- `h` and `l` clamp inside the line (the compat fix; no wrap).
  `j k` move lines and clamp the column. `w b e` are word motions
  with vim's class rule (word char / punctuation / blank); `W B E`
  are the whitespace-delimited variants.
- `w` on the last word of the buffer lands on the last character of
  that word (the reported bug: a single-word draft must move, not
  stay). Deviation from the pinned reference, aligned with neovim:
  with a pending operator, `dw` / `dW` / `yw` / `cw` / `cW` on the
  last word of a line consume to the end of the line (the last
  char dies, not the last char minus one). A no-op `w` at the last
  char deletes the single char under the cursor. A landing on the
  start of a real next word (a blank before it) stays exclusive.
  Cross-line `dw` keeps the compat rule: it does not consume the
  newline of the current line with count 1; bigger counts cross it.
- `cw` / `cW` behave as `ce` / `cE` when the cursor is not on a
  blank (the reference rule).
- `i a I A o O R s x X r ~ J` as in the reference; `o` and `O`
  insert a blank line below / above (counted `O` repeats after
  `Esc`). `s` deletes `count` chars at the cursor and enters insert
  (change without motion; the reference has no `s` — a documented
  extension). `S` replaces the line (linewise change; also a
  documented extension). `J` joins the next `count` lines. `~`
  toggles case for `count` chars. `x` deletes `count` chars under
  the cursor, clamped (no line join); `X` deletes `count` chars
  before, clamped. Both set the delete registers.
- `v V`: enter visual / visual-line (anchor = cursor).
- `u`: undo. `Ctrl+R`: redo (host routes it here only while the
  editor holds redo state).
- `Esc` cancels the pending operator, count, and char-motion.
  Deviation from the pinned reference: the reference leaves a
  typed count open after a plain `Esc`, so a stale digit can make
  a later `dw` delete N words or `db` eat the whole line (the
  reported intermittent delete). This port cancels the count and
  the `g` prefix on a plain normal-mode `Esc`, like vim.
- `j k` with a pending operator are linewise ranges (the reference
  rule); `h l` are char-wise.

### Insert

- chars insert at the caret; `Shift+A` types `A` at the caret, like
  the reference base editor;
  `Ctrl-J` (host-normalized to `Enter`
  for the editor) splits the line; `Backspace` removes the char left
  of the caret, and joins lines at column 0.
- `Esc`: to normal, the cursor steps back one char; at column 0 it
  climbs to the previous line. Counted `O` copies the inserted line
  below before returning.
- `Ctrl+C`: to normal without stepping back.
- typing is recorded for dot-repeat (backspace pops the record).

### Replace (`R`)

- a typed char overwrites the char under the cursor (last char of a
  line is overwritten, not appended); at end of line it appends.
  `Shift+A` types `A` at the caret, like the reference base editor.
- `Backspace` restores the original char (the replace stack) and
  steps back. `Enter` splits the line. `Esc`: to normal, step back.

### Visual / Visual-line

- the anchor and the cursor bound the selection; line-wise `V`
  covers whole lines. Motions move the cursor against the anchor
  (same set as normal). `o O` swap the ends. `v` / `V` toggle the
  kind; `v` inside visual (or `V` inside visual-line) exits to
  normal.
- `d x D` delete the selection (sets the delete registers);
  `c s C` change it (delete + insert at its start); `y Y` yank it;
  `> <` indent / dedent; `J` joins the selected lines; `~` toggles
  case in the selection; `p P` replace the selection with the
  register and leave normal mode.
- `i a` + object key moves the selection onto the text object.
- `f F t T ; , g G 0 $ ^ w b e W B E { } % n N * # / ?` move or
  extend.
- `Esc` exits to normal.

### Command-line (search input)

- `/` and `?` from normal or visual open the command line (the
  prompt renders in the input box title; `Esc` or a backspace on an
  empty buffer cancels). The prompt wins over a `frame` extension
  label: the host keeps its modal-state render while a search is
  open, and the frame label returns when the search ends
  (docs/ui-extension.md section 10).
- `Enter` runs the search (literal, case-insensitive, per line,
  wrap-around) and returns to the mode that opened it.
- `Ctrl+U`: in command-line mode it clears the buffer; in the idle
  composer's insert mode it kills the current line (the base editor's
  line kill); otherwise it is the half-page log scroll. The host
  routes `Enter`, `Esc`, `Backspace`, and `Ctrl+U` here instead of
  their host roles (send / cancel / editor / scroll).

## 4. Operators and registers

An operator combines with a motion result into a range
`{start, end, linewise, inclusive}`. `inclusive` marks `e`, `$`,
`f`, `t`, and `l` (the reference rule: `e` and `$` extend to the
end of the word / line).

- `d`: delete the range into the delete registers (the numbered
  registers shift on every unnamed delete), cursor to the range
  start (last non-blank char of the line when the delete leaves the
  line empty).
- `c`: same delete, then insert mode at the range start. A linewise
  `c` leaves one empty line.
- `y`: yank into the yank registers (unnamed + `0`); cursor to the
  range start.
- `> <`: indent / dedent the lines in the range (2-space shift).
- paste `p / P`: a linewise register splices its lines after
  (`p`) or before (`P`) the cursor line, with a copy count; a
  char-wise register splices inline at `col+1` (`p`) / `col`
  (`P`), and multi-line char-wise content merges with the current
  line. The cursor lands on the last pasted char (`p`) or the pasted
  start (`P`). This is the fix for `dw` + `p` opening new lines:
  a word delete is char-wise, so it pastes inline.

Register set (the reference `registers.ts`): `"` unnamed, `0` last
yank, `1..9` delete history, `a z` named, `A Z` append, `_` black
hole, `+ *` clipboard alias. Content is `{text, linewise}`.

## 5. Text objects

`i a` + `w W " ' ` ( ) { [ <` (with their closing pair). Same
ranges as the reference: inner word / a word (trailing, else
leading, blank), quotes, and nesting-aware brackets.

## 6. Dot-repeat

A change records its keys, its inserted text, and whether it
entered insert mode. `.` replays the keys through the normal handler
and then types the recorded text once (overtyping in replace
mode). Any count of the original change is baked into the recorded
keys (`2d` records `2 d`). The pinned reference ignores a bare `N.`
override in the replay loop, so `2.` after `dw` still deletes one
word; `2.` after `2dw` deletes two (the recorded `2` is replayed).
Replay suppresses new recording.

## 7. Rendering (`render.rs`)

- The cursor line renders one block cell:
  - char-wise modes (normal / replace / visual / visual-line): the
    block covers the char under the cursor; the line continues from
    the *next* char. The old code appended the covered char a second
    time (`[t]this` for a cursor on `t`). The fix drops it.
  - insert: the block is a blank cell at the caret; the line keeps
    the char under it.
  - command-line: no block in the text area; the prompt renders in
    the box title (`/pat█`) and the hardware cursor sits on it. The
    prompt beats a `frame` extension label in that title: the
    extension keeps its border, color, and height, and its label
    returns when the search ends.
- The mode label in the border title mirrors `vim-modal.ts`
  (`[NORMAL]`, `[d-PENDING]`, `[COMMAND]`).
- the visual selection keeps its highlight behavior.

## 8. Host routing (`app.rs`)

- `Enter`: the editor owns it in command-line mode (run the
  search); otherwise it sends the draft.
- `Ctrl+C`: with no active session it goes to the editor (insert /
  replace `Ctrl+C` = plain exit to normal); with an active session
  it stops the loop, as before.
- `Ctrl+R`: redo when the editor holds redo state; otherwise the
  loop start.
- `Ctrl+U`: clear the command-line buffer when in command-line
  mode; otherwise the half-page scroll.
- `Backspace Delete Left Right Up Down Home End Esc` and every char
  reach the editor, which owns their per-mode meaning.
- `Tab`, `PgUp`, `PgDn`, wheel, `q`, `Ctrl+E` keep their host
  roles.

## 9. Tests

The suite in `vim_editor.rs` ports the reference behavior:

- the three reported bugs, each with its exact case:
  1. `w` on the only word of a draft moves to its last char; `w`
     at the last char of the buffer stays.
  2. `dw` on `b` of `a b c d`, then `p`, pastes inline: `a cb  d`
     (the register holds `b ` and `p` inserts it after the cursor
     char); repeated `p` repeats the inline paste without opening
     lines.
  3. render-level check: the cursor line spans never repeat the
     covered char (the `[t]this` case) — covered by a focused test
     on `cursor_line_spans`, the span builder `render.rs` uses.
- the reference compat cases: `w`/`W` skip indentation after
  crossing a line; unicode letters are word chars; `cw` = `ce`;
  single `dw` at end of line preserves the newline; operator and
  motion counts multiply; `O` auto-indents; counted `O` repeats.
- motion edges: `h l` clamps; `w b e W B E` on blanks, line ends,
  and line crossing; `gg G` counts; `f t ; ,` misses stay put;
  `{ } %` paragraphs and brackets; `n N * #` search wrap.
- operator edges: `d c y > <` × each motion; `dd cc yy`; `D C Y`;
  counted forms; inclusive ranges (`de` includes the word end,
  `d$` includes the line end); linewise `j k` under operators.
- register rules: delete history shift, yank registers, `A`
  append, `_` discard, named reads; paste `p P` with counts and
  multi-line char-wise content.
- visual: char / line selection, `o O` swap, `v V` toggles,
  operators on the selection, `p` replacing the selection,
  text-object selection.
- insert / replace: caret back on `Esc`, line join at column 0,
  replace restore on backspace, counted `O`.
- dot-repeat: `.` repeats an insert change, an operator change, and
  `2.` with a count; replace-mode replay overtypes.
- undo / redo: `u` and `Ctrl+R` snapshots.
- host routing (`app.rs` tests): command-line `Enter` runs the
  search; `Ctrl+C` without a session exits insert; `Ctrl+R` redoes
  before starting the loop; `Ctrl+U` kills the line in the idle
  composer and scrolls the log otherwise.

## Properties

Lean-style invariants for this spec (see `lean-driven-development.md`).

P1. last-word-motion: given a single-word draft, observe `w` move to the word last character and stay at the buffer end.
P2. inline-paste: given `dw` on an inner word then `p`, observe the deleted word paste inline after the cursor without opening a new line.
P3. count-cancel: given a typed count then a plain `Esc`, observe the count and `g` prefix cancel so a later `dw` or `db` stays in range.
P4. dot-repeat: given a recorded change, observe `.` replay the recorded keys and type the recorded insert text once.
P5. arrow-map: given arrow, home, end, and delete in normal mode, observe they map to the vim motions `j`, `k`, `h`, `l`, `0`, `$`, and `x`.
P6. no-cursor-dup: given the cursor on a character in a char-wise mode, observe the covered character render exactly once, with no duplicate to the right.

## Verification

| P# | Property | Proof | Status |
|----|----------|-------|--------|
| P1 | last-word-motion | `w_on_last_word_of_single_word_draft`, `w_on_last_char_of_last_word_stays_put` in `bin/tui/src/vim_editor.rs` | proven |
| P2 | inline-paste | `dw_then_p_pastes_inline_after_cursor` in `bin/tui/src/vim_editor.rs` | proven |
| P3 | count-cancel | `esc_in_normal_cancels_a_stale_count`, `stale_count_cannot_make_db_eat_a_line` in `bin/tui/src/vim_editor.rs` | proven |
| P4 | dot-repeat | `dot_replays_the_recorded_keys`, `dot_repeats_insert_change` in `bin/tui/src/vim_editor.rs` | proven |
| P5 | arrow-map | `arrows_map_to_vim_motions` in `bin/tui/src/vim_editor.rs` | proven |
| P6 | no-cursor-dup | `on_char_cursor_does_not_duplicate_the_covered_char` in `bin/tui/src/render.rs` | proven |

## Gate

The acceptance commands. All must exit 0 for this spec to be proven.

```
cargo build
cargo test
```
