# ext-rs

The Rust reference layer (ui-extension-plan stage 4). Each
subdirectory is one extension entry: an `ext.toml` manifest plus a
standalone cargo package. It is a sibling of `ui_extensions/` (the
bash reference layer); a user activates it by pointing `[ext] dir`
in `config.toml` at this directory, or by copying an entry into the
project `.pi/ui_extensions/` layer, which overrides a same-named
global entry.

| Entry | Port of | Capability | What it proves |
| --- | --- | --- | --- |
| `statusline-rs/` | `ui_extensions/statusline/` | `status` | the same powerline footer on Rust: Nerd Font glyph pills with per-span hex colors, git TTL 3 s, cumulative usage from the log (numbers shorten to k/M/B), ext_status consumption, line 1 is dir, git, model, line 2 is the stats pill alone |
| `tool_result-rs/` | `ui_extensions/tool_result/` | `render` | the same kind owner on Rust: header plus the full body, body precedence of docs/tui.md 13.1 |
| `notify-rs/` | `ui_extensions/notify/` | `notify` | the same bell and OSC on Rust: finished turns, burst suppression at start, tmux client-tty fallback |

Each entry builds its own binary. Put the binaries on `PATH` so
the host can resolve the commands (`statusline-ext`, `tool_result-ext`,
`notify-ext`):

```sh
# bash / sh / zsh
export PATH="$(bash scripts/ext-env.sh):$PATH"

# fish
set -gx PATH (bash scripts/ext-env.sh) $PATH
```

The script builds every reference package (this layer plus
`ui_extensions/mermaid`) and prints their `target/debug` dirs,
colon-joined on one line. The PTY smoke test builds the binaries
and sets `PATH` on its own.

The `mermaid` transform reference (ui-extension-plan stage 3) is a
Rust binary in `ui_extensions/mermaid/`: a bash mermaid renderer is
out of scope for the ground rules (no JS, no heavy tooling). Every
other surface has a bash and a Rust reference, which is the stage 4
exit criterion.
