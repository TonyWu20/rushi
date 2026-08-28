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
| `statusline/` | bash | `status` | tick-driven row: live dir, git (TTL 3 s), session, model, loop state, cumulative usage from the log, ext_status consumption; two-line layout under 100 cols |
| `tool_result/` | bash | `render` | the kind owner for `tool_result`; styled header plus the full body |
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
