# TUI Syntax Highlighting

Decision record for the shared syntax-highlight component
(`bin/tui/src/highlight.rs`): what shipped, where it is used, and the
engine tradeoff (hand-rolled vs `syntect` vs tree-sitter). Related:
`tui-color-pi-alignment.md` §2 (how pi colors syntax),
`tui-file-picker.md` §9 (the follow-up that triggered this), and
`coding-conventions.md` (the `bon` builder rule the `read_body`
change follows).

## 1. What shipped

A general, reusable highlight engine in `bin/tui/src/highlight.rs`:

- `language_from_path` — detect a language from a file path
  (~30 languages by extension, plus `Makefile`/`Dockerfile`
  special-cases, `generic` fallback).
- `CodeHighlighter` — a stateful per-line tokenizer. It carries
  `in_block_comment` and `md_fence` across lines so `/* ... */`
  and markdown fences resume on the next line. It runs in char
  space so a multibyte line cannot desync the indexes.
- `highlight_text_lines` — the one-shot entry point. It takes
  `text` + an optional `lang` + the `Palette` and returns
  `Vec<Vec<(Style, String)>>`, one styled row per hard line.

Both consumers drive the same engine through the existing
`Palette`/`Role` system. No new style mechanism.

## 2. Where it is used

- Picker preview pane (`picker/preview.rs`): `FilePreviewer::content`
  detects the language from the item path and calls
  `highlight_text_lines`. The renderer paints the plain
  (default-style) runs with `Role::PlainText` and keeps the token
  colors.
- Transcript tool-result body (`tool_display.rs` `read_body`): the
  `Read` result text is detected from the result `path` field and run
  through the shared `CodeHighlighter`. The read tool's `N: `
  line-number prefix is split off first so the highlighter sees only
  file content; the prefix renders in the code tone. `read_body` is
  now a `bon` builder (`coding-conventions.md`).

Both paths truncate to the preview cap **before** highlighting, so a
truncated view still has full syntax color. This matches pi.

## 3. How pi actually does it

pi colors language syntax with **highlight.js**, a regex lexer, not
a parser (see `tui-color-pi-alignment.md` §2). It supports many
languages because highlight.js bundles ~190 of them; the "rich
color" is a 9-role theme (`syntaxKeyword`...`syntaxPunctuation`)
mapped onto the active pi theme. `pi-tool-display` reuses the same
`highlightCode` entry point for diff lines: token colors from the
theme roles, the row background tinted with `toolSuccessBg`
(a 12% green or red mix). Truncation is orthogonal: pi truncates to
the read/operated lines first, then highlights the survivors.

Our engine already reproduces that color model: the `Palette`/`Role`
table carries the 9 `syntax*` roles with the pi `catppuccin
macchiato` values, and the highlighter colors through the same
roles. So for the ~30 languages we ship, the color experience
already matches pi.

## 4. The two gaps vs pi

1. **Language breadth.** We hand-roll ~30 languages; highlight.js
   bundles ~190.
2. **Per-token colors inside the `Edit` diff.** pi highlights each
   diff line's tokens (language from extension) and tints the row
   background. Our `edit_body` still colors whole lines
   `DiffAdded`/`DiffRemoved` with no per-token color.

## 5. Engine tradeoff

|            | tree-sitter          | syntect             | hand-rolled (now)  |
|------------|----------------------|---------------------|--------------------|
| Native     | no (C FFI + C build) | yes (pure Rust)     | yes                |
| Languages  | per-language C grammar| ~100+ Sublime grammars | ~30          |
| Diff tokens| yes                  | yes                 | not yet            |
| TUI fit    | overkill             | good                | good, limited      |

- tree-sitter is editor-oriented: per-language C builds, an FFI
  layer, and an incremental parser meant for a live editor. For a
  preview pane and an inline tool-result body, that build/FFI cost is
  disproportionate. The "many languages" problem is already solved
  more cheaply by a highlight.js-style tokenizer.
- `syntect` is the Rust-native equivalent of the highlight.js pi
  uses. Real grammar-based highlighting, ~100+ bundled languages,
  no C toolchain. It slots into the existing seam by mapping
  `syntect` scopes to the 9 `syntax*` roles to the palette.
- The seam is already in place: the engine emits `(Style, String)`
  through `Palette`/`Role`. Swapping the engine later does not
  touch `picker/preview.rs` or `tool_display.rs`.

## 6. Decision

Keep the hand-rolled engine for now. It already matches pi's color
model for the shipped languages, adds no dependency, and is fast on
the re-render hot path. The extension architecture makes adopting a
real engine later cheap.

Adopt `syntect` (not tree-sitter) when more languages or per-token
diff colors are wanted:

- replace the body of `code_line` with a `syntect` one-shot
  highlight per line, mapping its scopes to the 9 `syntax*` roles;
- extend `edit_body` to run each surviving diff line through the
  shared highlighter (language from `path`) so diffs get per-token
  colors + the diff background, matching pi.

Revisit when the hand-rolled version falls short (missing languages,
wrong tokenization for complex code).

## 7. Status

- Shipped: the reusable engine + both consumers. No new
  dependencies. 494 tests pass, clippy clean.
- Open: per-token colors inside the `Edit` diff.
- Deferred: `syntect`. Adopt only when breadth or diff-token color
  demand it.

## Properties

Lean-style invariants for this spec (see `lean-driven-development.md`).
One property per non-trivial invariant. Each property is observable:
given an input, an output guarantee.

P1. language-detect: given a file path, observe the language detected
    from the extension (the shipped extension set plus the
    `Makefile`/`Dockerfile` special cases), and an unknown extension
    falls back to `generic`.
P2. stateful-tokenizer: given a multi-line input where a block
    comment or a markdown fence spans lines, observe the tokenizer
    carries the state across lines and resumes it on the next line.
P3. multibyte-safe: given a line containing multibyte characters,
    observe the tokenizer runs in char space and the token boundaries
    do not desync.
P4. one-shot-entry: given `text`, an optional language, and a
    `Palette`, observe `highlight_text_lines` returns one styled row
    per hard line.
P5. truncate-then-highlight: given content that exceeds the preview
    cap, observe truncation happens before highlighting and the
    truncated view still carries full syntax color.

## Verification

Each property maps to its proof. `proven` means the cited test or
script exists and passes. `open` names the blocker and what unblocks
it.

| P# | Property | Proof | Status |
|----|----------|-------|--------|
| P1 | language-detect | `language_from_path_maps_known_extensions`, `code_highlighter_unknown_lang_is_plain` in `bin/tui/src/highlight.rs` | proven |
| P2 | stateful-tokenizer | `code_highlighter_block_comment_spans_lines`, `fence_state_toggles_across_lines` in `bin/tui/src/highlight.rs` | proven |
| P3 | multibyte-safe | `code_highlighter_survives_multibyte_lines` in `bin/tui/src/highlight.rs` | proven |
| P4 | one-shot-entry | `highlight_text_lines_shared_entry_point` in `bin/tui/src/highlight.rs` | proven |
| P5 | truncate-then-highlight | `read_preview_folds_to_the_preview_lines`, `read_preview_highlights_known_language` in `bin/tui/src/tool_display.rs` | proven |

## Gate

The acceptance commands. All must exit 0 for this spec to be proven.

```
cargo build
cargo test
```
