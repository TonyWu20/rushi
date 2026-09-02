# ext-fixture

Bash extension fixtures for the PTY smoke test
(`scripts/tui-pty-smoke.py`). Each subdirectory is one `[ext] dir`
layer holding a single extension entry (`ext.toml` plus the script
the manifest runs).

| Case              | Fixture            | What it proves                                   |
| ---------------- | ------------------ | ------------------------------------------------ |
| ext-stub-alive   | `stub/`          | a status extension owns the status row           |
| ext-frame-commandline | `frame/`    | the search prompt shows in the box title even when a frame extension labels the frame; the label returns after the search |
| ext-dying-hint   | `dying/`         | restart budget 1s/2s/4s, then the dead hint      |
| ext-badjsonl     | `badjsonl/`      | not-JSON lines and shape-invalid JSON payloads: per-op G5 fallbacks, no crash |
| ext-append-reject| `append-reject/` | whitelist reject flash, whitelisted append lands |

The smoke test points a temp config at one of these directories via
`[ext] dir` (an absolute path) and starts the TUI on a fresh
session. Every case also checks that no fixture process is left
running after the TUI quits (no orphans, docs/ui-extension.md
section 7).
