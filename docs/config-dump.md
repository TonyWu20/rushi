# `rushi config` — Dump the Active Config

Decision record for a small new `rushi` subcommand. It prints the config
file the harness would actually use to stdout.

## 1. Motivation

The config the loop runs on may live anywhere in the resolution order
(`$CONFIG`, `--config`, the Nix side-by-side `<exe_dir>/../config.toml`,
or `./config.toml` in CWD). To tweak a key for local use, e.g.
`[paths] sessions_root`, the user first has to know which file is in
effect. Finding a Nix store path or a `$CONFIG` override is
undocumented friction.

## 2. Decision

- Add a `rushi config` subcommand. It resolves the config path with the
  same order as the loop subcommands, then prints the file's raw
  contents to stdout.
- stdout carries only the config text, so it is safe to redirect
  (`rushi config > my-config.toml`) or pipe. Diagnostics go to stderr.
- The dump guarantees a trailing newline, so the redirected file ends
  in a complete line that is safe to edit and re-save.
- The content is the file verbatim. No keys are added, removed, or
  reordered, and no defaults are materialized. The user tweaks the
  raw TOML and feeds it back in with `--config` or `$CONFIG`.
- Missing or unreadable file: error on stderr, exit code 1, empty
  stdout.

## 3. Change

- `bin/rushi/src/main.rs`: new `Config` subcommand variant and match
  arm. The module doc lists the subcommand.
- `bin/rushi/src/config.rs`: `config_dump(path)` helper — raw read plus
  the trailing-newline guarantee. Two unit tests: `verbatim and
  trailing newline` and `missing file errors`.
- `docs/reference/README.md` section 4: "Dumping the active config"
  (embedded in the binary via `include_str!`, so `rushi docs 4` covers
  it too).

## 4. Verification

- `cargo test -p rushi`: 25/25 pass, including the two new
  `config_dump` tests.
- End-to-end on the built binary, one scenario per resolution tier:
  - `$CONFIG` set: the dump equals that file byte for byte.
  - `--config <file>`: the dump equals that file.
  - Fake Nix layout (`bin/rushi` + `../config.toml`, run from an empty
    CWD): the side-by-side file is dumped.
  - CWD fallback (dev checkout): the dump equals `./config.toml`.
  - Nonexistent path: exit 1, empty stdout, stderr hint.

## 5. Out of scope

- Printing a resolved/normalized view with defaults filled in. That
  would change the semantics. Relative paths would become absolute,
  and computed budgets would appear. The result would not round-trip
  as a config the user can hand back to `--config`. The verbatim file
  is what gets edited.
- A flag to list the resolution order or print the winning source. The
  stderr error message already names the path that was tried.
