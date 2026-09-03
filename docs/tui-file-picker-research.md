# Fuzzy file picker - library research

Technical research for the `@` file-picker and the reusable
auto-completion window widget. Three references: `frizbee` (matcher),
`television` (Rust TUI), `telescope.nvim` (Lua Neovim plugin).

## 1. frizbee (saghen/frizbee)

Repo: https://github.com/saghen/frizbee

Rust SIMD fuzzy string matcher. Core algorithm is Smith-Waterman
local alignment with affine gaps, run row-wise over SIMD lanes.
Same family as FZF and Nucleo, but faster. Also ships C, C++,
Python, and WASM bindings. Supports `no_std` + `alloc`.

### Performance
- ~4x faster than Nucleo, ~5x faster than FZF (typo off).
- ~20x faster than both on unicode.
- Multithreaded via work-stealing; near-linear scaling.
- Chromium tree (~1.4M paths, median 67 chars): 22ms single
  thread, ~3.5ms with 8 threads, for one needle.
- Fast enough to re-match a full file list on every keystroke.

### API
```rust
use frizbee::{Config, Matcher, Pattern};

// one matcher, reused for the whole list
let mut m = Matcher::new("fBr", &Config::default());
let matches = m.match_list(&haystacks);        // -> Vec<Match>
let matches = m.match_list_parallel(&haystacks, 8); // multi-thread

// multi-atom query with mode syntax:
//   fuzzy  'foo  ^foo  foo$  ^foo$  !foo
//   fuzzy  sub   prefix suffix exact neg
let mut m = Matcher::from_query("foo !^bar", &Config::default());

// iterator form (lazy, a bit slower than match_list)
use frizbee::{iter::FuzzyMatchExt, radix_sort_matches};
let mut v: Vec<_> = haystacks.iter()
    .fuzzy_match("fBr", &Config::default())
    .collect();
radix_sort_matches(&mut v);
```
- `Match { score: u16, index: u32, exact: bool }`, sorted by score
  descending then index ascending.
- `Pattern::parse_query` + per-pattern `PatternConfig` overrides
  (set `max_typos` per atom, etc.).

### Scoring algorithm
Not a plain subsequence test. It is a scored local alignment that
supports insertion (fuzzy gaps in the haystack), deletion and
substitution (typo tolerance), with affine gap penalties and a set
of bonuses. This is why it ranks like FZF but is faster.

Default score constants (`src/const.rs`):
- `match_score = 12`, `mismatch_penalty = 6`
- `gap_open = 5`, `gap_extend = 1` (affine)
- `prefix_bonus = 12` (first char of haystack)
- `delimiter_bonus = 4` (match after a non-alnum char)
- `capitalization_bonus = 4` (camelCase bump, e.g. B in fooBar)
- `matching_case_bonus = 4`, `exact_match_bonus = 8`

Matching modes: `Fuzzy` (default, Smith-Waterman), `Exact`, `Prefix`,
`Suffix`, `Substring` (literal, no typos). `Config` knobs:
`max_typos` (default `Some(0)`, i.e. typo-free but still fuzzy),
`casing` (`Smart` default), `unicode` (`Smart` default), `sort`,
`scoring`.

### Who uses it
Consumers listed by the project: `blink.cmp`, `atuin`,
`television` (alexpasmantier/television), `skim` (skim-rs/skim),
`fff` (dmtrKovalenko/fff). Confirms the "frizbee powers these
pickers" premise.

### Design takeaway
Drop-in Rust dependency. Build one `Matcher`, feed the file list,
re-match per keystroke, use `match_list_parallel` for big trees.
Turn `max_typos` on for typo tolerance. This is the fuzzy engine for
the picker from day 0.

## 2. television (alexpasmantier/television, "tv")

Repo: https://github.com/alexpasmantier/television

Fast, portable fuzzy finder (an fzf-class tool). Built on
`ratatui` 0.30 + `crossterm` 0.28. Uses `frizbee = "0.13"` as its
matcher. 6.2k stars, very active.

### UI model
- Not a floating popup. It renders a full-screen or inline
  terminal viewport (ratatui `Viewport::Fullscreen`, `Inline`, or
  `Fixed`). `TuiMode::Inline` pins to the current cursor row and
  takes the remaining terminal height (min 10 rows).
- Layout regions (`screen/layout.rs`): results list, input bar
  (top or bottom), preview pane, status bar, help panel.
- Orientation: `landscape` (preview on the right) or `portrait`
  (preview above/below). `InputPosition` is `Top` or `Bottom`.
  `Layout::build` computes all `Rect`s from the config.

### frizbee integration
- `Matcher<I>` (`television/matcher/mod.rs`) wraps a
  `frizbee::Matcher`. Matching runs on a dedicated background
  worker thread fed by an `mpsc` channel.
- Results are published as an immutable `Arc<Snapshot>`; the UI
  thread reads the latest snapshot non-blockingly.
- Items stream in through an `Injector`; `find(pattern)` sets the
  needle; `results()` returns the last snapshot.
- `SortStrategy`: `Score`, `Index`, or `Hoisted` (frecency table
  hoists frequent items to the top).
- Optional typo resistance behind a config flag / `--typos`.

### Keyboard / channel model
- The input box edits the pattern; results update live in the
  background.
- Navigation: up/down, page up/down, home/end
  (`Picker::select_next`/`select_prev`, `move_cursor` with step and
  pane height). `Picker<T>` keeps `ListState` plus a `relative_state`
  for the visible window.
- Data sources are TOML "cable" files (files, text, git, docker,
  env, ...). Each channel can bind keys to actions and define its
  own previewer.
- Shell integration: Ctrl+T smart autocomplete, Ctrl+R history.

### Design takeaway
Proves frizbee drives a real-time, multi-source TUI picker on a
background thread with a clean snapshot handoff. The three-way split
`Picker<T>` + `Matcher<I>` + background worker is the reusable
middle layer to copy. Note television is inline/fullscreen, not a
floating overlay.

## 3. telescope.nvim (nvim-telescope/telescope.nvim)

Repo: https://github.com/nvim-telescope/telescope.nvim

Lua fuzzy finder over lists. Modular: pickers + sorters +
previewers. ~19.8k stars.

### Display
- Rendered as a Neovim floating window (`nvim_open_win`), with
  optional `winblend` transparency. Not inline.
- `layout_strategy` chooses the geometry:
  - `horizontal` (default): results + preview side by side, prompt
    at the bottom.
  - `vertical`, `center` (dropdown), `cursor` (ivy /
    cursor-relative), `bottom_pane`.
- `layout_config` tunes `height`, `width`, `prompt_position`,
  `preview_cutoff`.
- `preview_cutoff`: when the result count drops below it, the
  preview pane closes and its space goes to results.
- A separate preview window shows the file content; a key toggles it.

### Fuzzy search / scoring
- Default `fuzzy` sorter (`lua/telescope/sorters.lua`): a pure-Lua
  2-gram heuristic. Score blends n-gram overlap, consecutive-match
  bonus, whole-substring bonus, a "tail" bonus, and an
  uppercase-match bonus; `score = 1 / denominator`; results are
  filtered when the denominator is under 0.5 for queries longer than
  2 chars. Fast but approximate, not a full subsequence ranker.
- Recommended native sorters: `telescope-fzf-native` (fzf C lib) or
  `telescope-fzy-native` (fzy C lib) for true subsequence ranking
  and better speed.
- `fuzzy_with_index_bias` blends the fuzzy score with a small
  index penalty. A `frecency` sorter orders files by recent /
  frequent access.
- File lists come from `fd` / `rg` / `git ls-files`, respecting
  `.gitignore`.

### UX patterns to copy
- Named layout modes, plus cycling between them
  (`cycle_layout_list` defaults to `horizontal` then `vertical`).
- Preview window decoupled from the results list.
- Multi-select (`<Tab>` / `<S-Tab>`), send to quickfix
  (`<C-q>` / `<M-q>`).
- which-key help overlay (`<C-/>` in insert, `?` in normal).
- `prompt_position` configurable (top or bottom).
- Core keys: `<C-n>/<C-p>` next/prev, `<CR>` confirm, `<Esc>`
  close, preview scroll with `<C-d>/<C-u>/<C-f>/<C-k>`.

## Design takeaways for the `@` picker + reusable window widget

- Fuzzy from day 0: depend on `frizbee`. One `Matcher` per picker
  session, feed the file list, re-match per keystroke, use
  `match_list_parallel` for large trees, enable `max_typos` for
  typo tolerance.
- Reusable middle layer: copy television's architecture - a
  background matching thread plus an immutable snapshot plus an
  item injector. The UI reads the latest snapshot non-blockingly.
- Reusable window widget: build the picker body as a self-contained
  widget with three pluggable parts - a data source (file list /
  "channel"), a sorter (frizbee-based), and actions. Decoupling
  these makes the widget reusable for buffers, symbols, git files,
  and commands, not just files.
- Display choice (deferred to the user):
  - Inline under the input box (most agent TUIs, television
    inline): simplest, no overlay stack, natural fit for an
    `@`-triggered picker in the input bar.
  - Floating spawned window (television fullscreen, telescope
    popup): clearer visual separation, but needs a window stack
    manager for z-order, focus, and close.
  - For reusability, make the container swappable: the widget owns
    its own layout math and renders the same body into either an
    inline region below the input box or a floating overlay.
- UX to carry over: preview pane with `preview_cutoff`, keyboard
  navigation (`j/k`, page up/down, home/end), prompt top or bottom,
  optional multi-select, which-key help, frecency sort, index bias.
