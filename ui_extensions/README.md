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
| `goal/` | Rust | `commands`, `append`, `row` | Registers `goal`, `goal edit`, `goal pause`, `goal clear`, and `goal resume` in the TUI command palette. `goal` and `goal edit` arm an in-memory flag; the next `user_message` event (forwarded via `kinds = ["user_message"]`) triggers a direct write of `goal.json` in the session dir — no agent round-trip. `goal pause` sets `active = false`; `goal clear` deletes `goal.json`; `goal resume` re-activates a blocked/completed goal. Owns the host-reserved row slot above the input box (docs/ui-extension.md section 4, `row` capability): the goal status line while a goal is open, the armed hint while a write/edit is pending; without this extension installed, the bare TUI shows no goal row. The `model.before` hook (`harness-hook-goal-arm`) appends a cache-stable goal block (objective + goal-mode rules + trust-boundary framing) to every model request while a goal is active. `goal_complete` and `goal_blocked` are agent-side tools, not user commands |

The bash references add no compiled binary on a user machine. The
`mermaid` and `goal` entries are standalone cargo packages (not
members of the root workspace). Their manifests name the binary by a
path relative to their own entry (`target/debug/mermaid-ext`,
`target/debug/goal-ext`), so the host resolves them against the entry
directory without a `PATH` export. Build each once with `cargo build`
in its directory:

```sh
cd ui_extensions/mermaid && cargo build
cd ui_extensions/goal && cargo build
```

The global layer then loads with no `PATH` setup: the host resolves
the relative command path against the entry dir (docs/ui-extension.md
section 6). The `ext-rs/` Rust ports still resolve their binaries on
`PATH`; use the script to add their build dirs:

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
