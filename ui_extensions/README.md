# ui_extensions

The global extension layer for the TUI extension host
(docs/ui-extension.md). Each subdirectory is one extension entry:
an `ext.toml` manifest plus the files its `command` runs. The TUI
loads this directory at start; a project `.pi/ui_extensions/` layer
overrides entries by name. An `[ext] dir` in `config.toml` replaces
the global layer for one run (used by the PTY smoke test and by
tests).

## References shipped here (ui-extension-plan stages 2 and 3)

| Entry | Language | Capability | What it proves |
| --- | --- | --- | --- |
| `statusline/` | bash | `status` | tick-driven powerline footer: rounded pills with Nerd Font glyphs (U+E0B6 caps, U+E0B4 arrows and end cap), Catppuccin Macchiato span colors; live dir, git (TTL 3 s), model, cumulative usage from the log (numbers shorten to k/M/B); shared-UI-state `ext_status` values are not listed (2026-09-02): host presentation, not footer content; line 1 is dir, git, model, line 2 is the stats pill alone, one line on wide terminals |
| `tool_result/` | bash | `render` | moved to `ui_extensions-demos/tool_result/` (the 2026-09-03 user report: the ext reply replaced the built-in box, and the fold key had no effect on the read and edit results). The demo: a styled header plus the body in one muted tone; a body that is a complete JSON document gets JSON syntax highlighting (2026-09-02: stop the gray abuse, docs/tui-color-tones.md section 4; the no-truncation rule rescopes to message content only, docs/tui-tool-result-truncation.md section 4 — the built-in render folds tool bodies, this reply protocol does not yet). Opt in with an `[ext] dir` pointing at a layer that carries it |
| `notify/` | bash | `notify` | bell and OSC for finished turns, tmux client-tty fallback, burst suppression on history resend |
| `mermaid/` | Rust | `transform` | `fence:mermaid` code blocks rendered as Unicode art by a Rust binary (stage 3) |

The bash references add no compiled binary on a user machine. The
`mermaid` entry is a standalone cargo package (not a member of the
root workspace). Put the reference binaries on `PATH` so the host
can resolve their commands:

```sh
# bash / sh / zsh
export PATH="$(bash scripts/ext-env.sh):$PATH"

# fish
set -gx PATH (bash scripts/ext-env.sh) $PATH
```

The script builds every reference package and prints their
`target/debug` dirs, colon-joined on one line. The host refuses
the start when a command is missing (fail-loud, docs/ui-extension.md
section 6). The PTY smoke test builds the binaries and sets `PATH`
on its own.

The Rust ports of the bash references live in the sibling
`ext-rs/` layer (ui-extension-plan stage 4). A user activates them
by pointing `[ext] dir` at `ext-rs/` or by copying an entry into the
project `.pi/ui_extensions/` layer, which overrides the global
entry by name.

The `scripts/ext-fixture/` layer holds the broken-behavior fixtures
(dying, badjsonl, append-reject, stub) for the PTY smoke test.
