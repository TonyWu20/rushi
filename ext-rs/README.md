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
| `statusline-rs/` | `ui_extensions/statusline/` | `status` | the same row on Rust: git TTL 3 s, cumulative usage from the log, ext_status consumption, two-line layout under 100 cols |
| `tool_result-rs/` | `ui_extensions/tool_result/` | `render` | the same kind owner on Rust: header plus the full body, body precedence of docs/tui.md 13.1 |
| `notify-rs/` | `ui_extensions/notify/` | `notify` | the same bell and OSC on Rust: finished turns, burst suppression at start, tmux client-tty fallback |

Each entry builds its own binary. Build with `cargo build` inside
the entry dir; the binary lands in `./target/debug`. Put those
directories on `PATH` so the host can resolve the commands
(`statusline-ext`, `tool_result-ext`). The PTY smoke test builds
both and sets `PATH` on start.

The `mermaid` transform reference (ui-extension-plan stage 3) is a
Rust binary in `ui_extensions/mermaid/`: a bash mermaid renderer is
out of scope for the ground rules (no JS, no heavy tooling). Every
other surface has a bash and a Rust reference, which is the stage 4
exit criterion.
