# TUI conversation browsing — spec review

Review of `docs/tui-conversation-browsing.md` (the 2026-09-05
spec). Scope: harmful shortcutting, YAGNI abuse, and whether the
spec is fully correct against the existing code and the machine's
neovim config.

## Verdict

- No YAGNI abuse. Every exclusion maps to the request scope, the
  machine config, or the "TUI is a view" rule. The one open
  feature (regex search) is staged, not cut.
- No harmful shortcutting in the scope design. The bar, the
  browse mode, and the gutter add the minimum surface the request
  names.
- The spec is **not fully correct against the existing code**.
  Three findings need fixes before the spec freezes (F1 the `s`
  disarm description, F2 the wrap-cache pointers, F3 the
  transcript-cap gap). Two minor wording fixes (F4, F5).

## Findings

### F1 — the `s` disarm description is wrong (major)

`docs/tui-conversation-browsing.md` section 4.2:

> any other key disarms. The armed `s` then delivers to the
> editor. On an empty draft the editor `s` is a no-op, so the
> disarm shows nothing.

Section 4.7 repeats it: "any other key disarms and delivers the
`s` to the input."

Evidence against the existing code:

- `bin/tui/src/vim_editor.rs`, normal-mode `s`: the handler calls
  `apply_operator_to_range('c', &range)`. For a non-linewise
  range, `apply_operator` returns `enter_insert = true`
  unconditionally (`fn apply_operator`, the `'c'` arm).
- On an empty draft the range deletes zero chars, but the mode
  still flips to `Insert`. The box title label shows
  `[INSERT]`. A temporary unit test confirmed it:
  `Editor::new()`, mode set to `Normal`, press `Key::Char('s')`,
  the mode is `Insert` (test ran green, then the test was
  reverted).
- The disarm also types the disarming key into the draft. The
  editor sits in insert mode at that point. The draft holds one
  char.

The description "a no-op" and "shows nothing" are both wrong.
The disarm shows the mode flip, and the draft gains the
disarming char.

The spec also says "Host routing mirrors the FT-012 quit arm."
The FT-012 arm does not deliver the armed key. In
`bin/tui/src/app.rs` the armed `q` is consumed by the host arm
logic. Any other key only disarms. The `q` never reaches the
editor. The spec adds a delivery step that FT-012 does not have,
then claims the mirror.

Note: the delivery design itself is not harmful. It ends in the
same state a plain `s` press does today (insert mode, then the
char types). The defect is the prose.

Fix, either form:

- mirror FT-012 exactly: the armed `s` drops on the disarm, not
  delivers. The disarming key keeps its normal role. Nothing
  types. No mode flip.
- or keep the delivery and describe the true effect: the editor
  `s` on an empty draft enters insert mode, the label flips to
  `[INSERT]`, the disarming key then types into the draft.

### F2 — the wrap-cache pointers are wrong (moderate)

Section 3: "The wrap cache is keyed by width
(`docs/tui.md` section 13)." Section 4.6: "the wrap cache
rebuilds (existing behavior, `docs/tui.md` section 13.4)."

`docs/tui.md` section 13 (the implementation record) and section
13.4 (tailer semantics: byte offset, carry buffer, no re-emit)
hold no wrap-cache statement. The cache lives in:

- `bin/tui/src/app.rs`, the `transcript_cache` field: keyed by
  `(events_version, width, ext reply version, palette level,
  palette)`.
- `docs/ui-extension-plan.md` stage 1 and
  `docs/ui-extension.md` ("That cache folds into the transcript
  cache key").

The behavior claims are correct against `app.rs` (width in the
key; a new event bumps `events_version`, the cache rebuilds).
Only the pointers are wrong. Fix: point at
`docs/ui-extension-plan.md` and the `app.rs` field.

### F3 — the transcript cap is never named (moderate)

The spec speaks of the whole log three times:

- section 3: "the track maps the whole log, top = oldest";
- section 4.6: "The transcript grows: `total` rises";
- section 4.7: "The cursor pins to its line number: existing
  lines keep their numbers, and new lines append."

The existing code bounds the transcript:

- `bin/tui/src/render.rs` `TRANSCRIPT_EVENT_CAP = 2000`: the
  oldest events drop out of the rendered lines.
- `bin/tui/src/port_file.rs` `MAX_LOG_READ_BYTES` = 50 MB:
  `read_events` keeps only the log tail.

Past the cap, `total` stops rising. Each new event shifts every
line number down by one. A cursor pinned to a line number
rebinds to the line behind it. The track maps the loaded tail,
not the whole log.

The request wording is "the view position over the whole log."
The feature can only show position over the loaded tail. Fix:
state the cap in section 3 (the track maps the loaded
transcript) and add the at-cap behavior to section 4.7 (numbers
shift on each new event; the pinned line follows the shift).

### F4 — the insert-mode `s` label (minor)

Section 9, the "gate: insert" row: "the first `s` types into the
editor (the change motion)". In insert mode the `s` types a
char. The "change motion" role is the normal-mode one
(`docs/vim-editor-design.md` section 3). The expected outcome
(types into the editor, no arm) is right; drop the parenthetical
or rename it.

### F5 — the neoscroll spec key (minor)

Section 6.3 names `karb94/neoscroll`. The lazy spec key on the
machine is `karb94/neoscroll.nvim`
(`~/.config/nvim/lua/modules/plugins/ui.lua`). Use the full key.

## Verified claims

Every other load-bearing claim checks out against the code and
the machine config.

### Section 2 (today)

| claim | evidence | status |
|---|---|---|
| `App.scroll`: lines up from the log end, `0` = follow the tail | `bin/tui/src/app.rs`, the `scroll` field doc | ok |
| `Ctrl+U` / `Ctrl+D` half a page; `half_page()` = `(viewport - 1) / 2`, fallback `10` | `app.rs` `half_page`: viewport >= 4 gives `(viewport - 1) / 2`, else `10` | ok |
| the wheel moves three lines; `PgUp` / `PgDn` ten; `SCROLL_CAP` `100_000` | `app.rs` `Key::Wheel` and `Key::PgUp` / `Key::PgDn` arms; the `SCROLL_CAP` const | ok |
| `render.rs` computes `start = total - scroll - h`; the window is the last `h` wrapped lines | `render.rs` line 1982: `total.saturating_sub(scroll + h)` | ok |
| no position indicator today | `render.rs` `help_line`: key hints only, no position | ok |
| no key jumps to the top or the end | `main.rs` `key_input`: `Home` / `End` map to the editor's `0` / `$` (draft); no log jump | ok |
| the quit gate (FT-012): `q` / `Ctrl+Q` arm and fire only in normal mode with an empty draft; any other key disarms; the armed `q` types nothing | `app.rs` `Key::Quit` arm and `quit_gate_open`; `main.rs` maps plain `q` and `Ctrl+Q` to `Key::Quit`; `docs/failure-tracking.md` FT-012 | ok |
| no regex dependency in the `tui` crate | `bin/tui/Cargo.toml` `[dependencies]` | ok |

### Sections 3 to 5 (the contract)

- the thumb math is self-consistent: `total = 1000`, view `24`
  gives `max(1, 24 * 24 / 1000) = 1`, as section 9 states;
- the gutter width (digits of `total` plus one space) and the
  hybrid numbering match `number` + `relativenumber` on the
  machine;
- the `s` arm window (`3 s`) matches the FT-012 `QUIT_ARM_TTL`
  (`3 s`, `app.rs`);
- the exit path resets `scroll = 0`: consistent with
  `App::set_active` resetting scroll on a session switch;
- `q` / `Ctrl+Q` in browse mode keep the gate: the gate checks
  only the editor mode and the draft, both frozen in browse
  mode;
- the `:N` box-title prompt reuse matches `docs/tui.md`
  section 7.1 (the search prompt renders `/pat█` in the box
  title);
- the `Ctrl+U` / `Ctrl+D` reroute statement matches
  `docs/tui.md` section 7 (the command-line clear and the
  idle-composer kill roles);
- the `h` / `l` clamp cite matches
  `docs/vim-editor-design.md` section 1 (the `8b99ecc` compat
  fix);
- the normal-mode `s` role (change one char) matches
  `docs/vim-editor-design.md` section 3;
- the border colors (`Border4` as the marker tone, dim track,
  normal thumb) exist in `docs/tui-color-scheme.md` section 6.

### Section 6 (the machine reference)

Verified against `~/.config/nvim`:

| claim | evidence | status |
|---|---|---|
| `number` / `relativenumber` / `signcolumn` / `cursorline` / `scrolloff` / `wrap` / `ignorecase` / `smartcase` / `incsearch` / `wrapscan` / `magic` / `jumpoptions` | `lua/core/options.lua` | ok |
| `n` maps to `nzzzv`; `Esc` runs `flash_esc_or_noh` (hide the flash session, else `noh`) | `lua/keymap/editor.lua` lines 49 and 54; `lua/keymap/helpers.lua` `flash_esc_or_noh` | ok |
| scrollview: virtual mode, `winblend = 0`, startup signs for folds, marks, search | `lua/modules/configs/ui/scrollview.lua` | ok |
| neoscroll animates `<C-u> <C-d> <C-b> <C-f> <C-y> <C-e>` and `zt zz zb`; the redraw is event-driven at about `100 ms` | `lua/modules/configs/ui/neoscroll.lua`; `bin/tui/src/main.rs` polls at `100 ms` | ok (F5: the spec key is `karb94/neoscroll.nvim`) |
| `*` / `#` keep the machine defaults, no override found | no `*` or `#` normal-mode map in `lua/keymap/` | ok |

### Sections 7 to 9

- the stage split holds: stage 1 adds no dependency, and the
  `regex` crate is absent from `bin/tui/Cargo.toml` today;
- the search case rule (case-blind without an uppercase,
  sensitive with one) matches the machine `ignorecase` +
  `smartcase` pair;
- the view-restore rule matches `jumpoptions = "stack,view"`;
- the `incsearch` live jump matches the machine option;
- the conformance table arithmetic is self-consistent (the bar
  geometry row, the gutter rows, the counted-motion rows);
- the mutation-gate rows fit the repo test convention
  (`cargo test -p tui`).

## YAGNI assessment

No abuse. The scope cuts in section 5 and section 7.4 are
each justified:

| cut | basis |
|---|---|
| no marks and jumps | the request names none |
| no search outside browse mode | the request asks for search inside browse mode only |
| no smooth-scroll animation | the machine's `neoscroll` plugin has no TUI analog; the redraw is a `100 ms` event loop (deviation D2, recorded) |
| no scroll or cursor persistence | `docs/tui.md` section 1: the TUI is a view, not the source of truth |
| no replace | the log is append-only |
| no telescope / fzf | a tool-level backend, outside the TUI |
| regex search as stage 2 | the request leaves it open; the spec answers it and defers the dependency to stage 2 |

The request's two items and their priority ("the bar plus
`gg`/`G`" first) are fully scoped in stage 1. Nothing the
request names is dropped.

## Shortcutting assessment

No harmful shortcutting in the scope design:

- the bar is a pure function of existing state. No new
  persisted state. The restart and session-switch reset
  matches today's scroll reset;
- the gutter width and the thumb math are the minimum formulas
  the request implies;
- the browse key table adds no motion the request does not
  name;
- the editor freeze in browse mode keeps the FT-012 gate
  invariant (empty draft, normal mode) for the whole session.

The only shortcut that harms is the one F1 names: the
disarm path is justified by a false claim about the editor
`s`. An implementer who trusts the prose ships a disarm that
behaves as the prose says (invisible) when it in fact shows
the `[INSERT]` flip and types the char.

## Required changes

All five applied to the spec on 2026-09-05 (commit `8c7cb6a`):

1. F1: the `s` disarm rewritten. The FT-012 mirror (drop, no
   delivery) won. The `gate: disarm` conformance row was added.
2. F2: the two wrap-cache citations repointed to the
   `transcript_cache` field of `bin/tui/src/app.rs` and
   `docs/ui-extension-plan.md` stage 1.
3. F3: the `TRANSCRIPT_EVENT_CAP` / `MAX_LOG_READ_BYTES`
   bounds named in section 3; the at-cap number shift added
   to sections 4.6 and 4.7.
4. F4: the insert row of `s` now reads "the typed char".
5. F5: the plugin name is now `karb94/neoscroll.nvim`.
