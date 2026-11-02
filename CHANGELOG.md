# Changelog

Each binary carries its own version in its `Cargo.toml`. The
version shows up on `--version`. This file records the
versioned change for each kernel binary, newest first.
Internal refactors without user-visible behavior do not get an
entry.

## compact 0.1.5 (2026-11-02)

The compact seed no longer crosses the kernel argv limit (FT-035).
The driver pre-seeds the seed transcript as the agent session's first
`user_message` in its log, before the fork. The fork is
`rushi run <c_id>` with no task: the loop's `claim` reports the
pending steer message, so the loop runs the compact agent. The seed
is a log line, not an argv slot, and scales past the 128 KiB cap
that broke a positional task (E2BIG). The log lives in the agent
dir, so the cleanup removes it. No new flag on `rushi run`.

## compact 0.1.4 (2026-11-02)

The write-handoff tool dir now resolves by layout, not by env var
(FT-034). The tool ships with the distribution, next to the rushi
bin. The driver probes `<exe_dir>/../tools/write-handoff` first,
using the canonicalized exe. It then scans `native_tool_paths`,
resolving relative entries against the config dir.

The `WRITE_HANDOFF_TOOL` env var is removed. A Nix rebuild ships the
tool. The driver finds it without any env var.

## write-handoff 0.1.0 (2026-11-02)

New native tool: the compact agent's only tool (FT-033). The tool
arg is `summary` only. The target dir, version, and mode come from
`handoff-target.json` in `$HARNESS_SESSION_DIR`, not the tool args,
which blocks path traversal. In `handoff` mode it writes
`handoff/v<N>.md` and `handoff.md`. In `branch` mode it writes
`branch-summary/v<N>.md`. All writes are atomic with no leftover
temp file.

## compact 0.1.3 (2026-11-02)

The compact mechanism is now a compact agent (FT-033). The driver
forks `rushi run <C>`, a full agent session with the single
`write-handoff` tool. This is the one compact mechanism in rushi.
There is no config option, and the one-shot path is removed.

Both the main compact and `--branch` fork the agent. In `branch`
mode the tool writes the branch summary file. In `handoff` mode it
writes the handoff files. The driver blocks on the agent, reads the
file back, appends the marker, and removes the agent session dir.

## assemble 0.1.4 (2026-11-02)

A new `--summary-text` flag. It renders the compacted region as a
readable markdown transcript for the compact agent seed. It reuses
the compact-form projection of the old events. The `--summary-input`
(one-shot request) path is unchanged.

## rushi 0.1.5 (2026-10-30)

`rushi run` prints the last assistant message on a clean idle
stop. The print is one write to stdout at the moment the loop
stops. The caller captures it (`result=$(rushi run ...)` or a
redirect) as the Unix form of the mission result.

- Only the idle stop prints. The exhausted, failed, and signal
  stops stay silent: the session log holds their record.
- An empty content (a tool-call-only final turn) prints nothing.
- A closed pipe keeps the exit code: the failed write is ignored,
  and the loop's exit code stands.
- Docs: `rushi docs headless-missions` gains the "Result on
  stdout" section. The ping-back and fan-out examples capture the
  stdout instead of the "prints nothing" note.

## rushi-log 0.2.0 (2026-10-26)

Renamed the log-appender stage binary from `log` to `rushi-log`
(issue #57). nixpkgs `buildEnv` drops any file named `log` when
it merges packages into a devshell, so a `bin/log` entry never
reached the devshell `PATH`. No `bin/log` alias: the builder
drops the alias too. The `LOG_BIN` env override and the
`compact --log` flag are unchanged.

## rushi 0.1.4 (2026-10-20)

`rushi run` accepts a hyphen-leading `task` (and `session`) value.
A message that starts with `-` (a bullet list, a dash) was rejected
by clap as an unknown flag, so the detached TV `send_message` path
lost the message silently (the detached call sends stderr to
/dev/null and still prints `message queued`). `task` and `session`
now carry `allow_hyphen_values`, and the `--` escape form stays
valid (issue #52).

## user 0.1.3 (2026-10-20)

The `content` positional accepts hyphen-leading values (the same
clap rejection as `rushi run` task). `content` now carries
`allow_hyphen_values` (issue #52).

## assemble 0.1.3 (2026-10-14)

Config resolution now uses the shared tier ladder
(docs/project-config-precedence.md, P-L12). Omitting `--config` no
longer falls back to a CWD-only default. On a Nix install the store
config is found side-by-side instead of erroring.

## model 0.1.2 (2026-10-14)

Adopts the shared tier ladder for config resolution (P-L12). Same
behavior change as `assemble` 0.1.3.

## parse 0.1.2 (2026-10-14)

Adopts the shared tier ladder for config resolution (P-L12). Same
behavior change as `assemble` 0.1.3.

## compact 0.1.2 (2026-10-14)

Adopts the shared tier ladder for config resolution (P-L12). The
loader stays lenient. The resolved path is what the `assemble` and
`model` children receive as `--config`.

## user 0.1.2 (2026-10-14)

Adopts the shared tier ladder for config resolution (P-L12). The
resolved top layer is what the loop child receives as `CONFIG`.

## hook-compact 0.1.1 (2026-10-14)

The compact hook resolves config through the shared tier ladder
(P-L12) instead of a `$CONFIG`-or-CWD default.

## rushi 0.1.3 (2026-10-14)

The CWD config tier now beats the Nix side-by-side tier
(docs/project-config-precedence.md, revision 2).

- A Nix project CWD with a `config.toml` now resolves that file as
  the top layer. The store config is the base layer under it.
- Before the fix the store tier always won on a Nix install, so the
  project config was dead and the top layer equalled the base layer.
- No-flag launches (TUI, `rushi run`) pick up the project config.
  The `--config` and `$CONFIG` tiers are unchanged.
- Proof: `scripts/config-layer-e2e.sh` scenario 3.

## rushi-common 0.1.7 (2026-10-14)

`resolve_config_path` reorders the ambient tiers: CWD `config.toml`
when present now outranks the Nix side-by-side config.

## rushi 0.1.2 (2026-10-14)

Layered config (docs/project-config-precedence.md). The Nix store
`config.toml` is the base layer. Every config file is the top layer
that deep-merges over it.

- Table keys recurse. A top value wins a conflict. Base keys absent
  from the top survive. A top array or scalar replaces.
- No base file (dev checkout, plain install): behavior unchanged.
- A legacy `[paths]` key in either layer is a load error.
- A project pipeline may reference machine hook defs. The dead-def
  warning is scoped to top-layer defs.
- `rushi config` dumps the top-layer file only. The merged value
  never prints, so store paths stay out of git-tracked files.
- The loop, `assemble`, `model`, `parse`, `compact`, and `user`
  all load through one shared `rushi_common::config_layer`
  function.

## assemble 0.1.2 (2026-10-14)

Adopts the layered config loader. A partial project `config.toml`
now inherits the machine model and tools on a Nix install.

## model 0.1.1 (2026-10-14)

Adopts the layered config loader.

## parse 0.1.1 (2026-10-14)

Adopts the layered config loader.

## compact 0.1.1 (2026-10-14)

Adopts the layered config loader. The loader stays lenient: any
error yields the empty table and the defaults apply.

## user 0.1.1 (2026-10-14)

Adopts the layered config loader for `[loop]` and `[paths]`
resolution.

## assemble 0.1.1 (2026-10-12)

The FT-030 fix. An image tool result never sends its payload as
text, and a compact boundary drops the image part.

- An image tool result projects to a short marker
  (`[image: <path>] <note>`) plus the `input_image` part. The
  full tool-log stdout no longer rides the `input_text` channel
  for image results. The marker replaces the payload in every
  wire form, including the non-vision "Image omitted" form and
  the summary-input compact form.
- The compact drops the image. An image event older than the
  last plain `compaction_summary` projects to the marker only.
  Pixel data does not re-cross a boundary (pi parity). A fresh
  `img` call after the compact still carries its part.
- The tool log keeps the full record. The TUI and the audit
  trail are unchanged.

## assemble 0.1.0

Baseline version of the kernel assemble binary.
