# Session fork: `rushi fork PARENT NEW --fork-at N`

Status: Implemented. Lands on branch `issue/67` (issue #67).

The `rushi fork` subcommand copies one session's two logs into a new
session, cut at a log entry. The new session starts idle. The caller
starts it through the plain `rushi run` flow.

## 1. Proposal note (P6 format, docs/refinement-policy.md)

Observed: the `rushi-sessions-events` channel in `TonyWu20/tv-rushi`
plans a `fork_here` action on the picked entry (issue #67, Consumer
section). The channel has no kernel primitive to cut a session at a
seq. The pi `/tree` workflow (docs/rewind-fork-design.md) forks in
the log with `rewind` markers. A separate session fork is the shape
the channel wants: one name, one seq, one new session dir.

Reproduce: today the cut is manual log surgery. The operator slices
both logs with `jq`, hand-builds the `.idx` sidecar, copies the
`cwd` file, and checks the cut point by eye. No command exists.

Pre-test: a script can cut plain-text logs. It cannot rebuild the
zstd sidecar with the open-time validation the readers run. It
cannot enforce the settled-boundary rule. The rule is the P8 write
guard (docs/rewind-fork-design.md P8) in cut form. A prefix that
keeps a tool call without its result must refuse the cut. P4
question 4 wants that invariant in code, not in discipline.

Proposal: one subcommand on the existing binary. No new event
type, no binary split, no endpoint. The cut reuses the existing
`logstream` primitives (`consolidate` pattern, `encode_index`,
`index_path`) and the P8 guard (`rushi_common::rewind::guard_rewind`)
over the prefix. The writer builds the target in a temp dir and
renames it into place, the `consolidate` pattern.

Impact: no consumer breaks. The typed event vocabulary stays as-is.
No `v` bump. The TUI fallback rules stay as-is. The new dir is a
plain session dir: every reader takes it as-is.

Migration: additive only. Old sessions serve as the parent inputs.
They stay untouched. A refused fork writes nothing.

Tests: unit tests in `bin/rushi/src/fork.rs`, the
`logstream::write_consolidated` tests in
`crates/rushi/src/logstream.rs`, and `scripts/e2e-fork.sh` over the
real binaries.

## 2. The cut

```
rushi fork PARENT NEW --fork-at N [--sessions-root R] [--cwd C]
```

- `PARENT` — session name or directory. Resolution is the `rushi
  run` session rule (`resolve_session`, `bin/rushi/src/config.rs`):
  a bare name joins the sessions root, a path or an existing dir
  is used as-is.
- `NEW` — the new session name. A bare name: no path separator.
  The target dir is `<sessions-root>/NEW`.
- `--fork-at N` — the 1-based log seq. The content holds every
  event with seq at or below N.
- `--sessions-root R` — where the command finds the parent and
  places the new session. Relative values resolve like the `rushi
  run` flag (`resolve_dir_override`, issue #16).
- `--cwd C` — the new session's working dir. The flag wins.
  Without the flag, the fork copies the parent's recorded `cwd`
  file.

### The sessions-root rule (the open question, closed)

The effective root follows this order:

1. The `--sessions-root` flag, when given.
2. Else, for a dir-form parent (a path or an existing dir), the
   parent dir's parent: the parent's own sessions root. A bare
   dir-form name (an existing dir in the CWD, no separator) has
   an empty parent: the root is the CWD itself.
3. Else, the config `[paths] sessions_root` (the `rushi run`
   rule, resolved against CWD).

The command never reverts to the caller tree (the issue #49
sessions-root rule). A missing parent is a named error. The
command creates no directory.

### The event cut

The fork copies lines 1..=N of the parent's `events.jsonl`. The
parent's layout decides the fork's layout (P2, P3 of
docs/session-log-compression.md):

- A plain-text parent yields a plain-text fork. No sidecar.
- A zstd parent yields one consolidated level-19 frame plus a
  rebuilt `<log>.idx` sidecar: one record, seq from 1, the whole
  stream, the whole text. `rushi archive`'s idle pass writes the
  same pair in place.

The cut point must be settled (P8 of docs/rewind-fork-design.md,
cut form). The guard checks the projected prefix: the active path
at N over the rewind markers inside the prefix. Every kept tool
call must keep its `tool_result`, and every kept `tool_result`
must keep its call. A violation refuses the fork with a named
error that names the stranded pair. The caller snaps the pick to a
settled point first, as the events-channel `rewind` action does.

### The tool-log cut

`route` writes one tool line before each `tool_result` event, and
both share the call `id` (`emit_result`, `bin/route/src/main.rs`).
The tool-log boundary is the last kept line keyed by the last
`tool_result` at or before N. A prefix with no `tool_result` at or
before N keeps no tool log. A parent without a tool log keeps no
tool log. A key that sits in no tool line keeps every tool line.
The fork never drops a record a kept event can reference.

### The working dir

The precedence is: an explicit `--cwd` (resolved against CWD, the
`rushi run` rule), else the parent's recorded `cwd` file, else no
`cwd` file. `rushi run` rewrites the runtime files at start
(`write_loop_identity`, `write_session_cwd`).

### The atomics

The target dir is a hard error when it exists. No overwrite. The
writer builds the session in a temp dir under the root and renames
it into place, the `consolidate` pattern: one rename makes the dir
complete. A failure removes the temp dir. No partial write
survives.

The fork copies no runtime files: no `loop.pid`, no `loop.meta`,
no `.loop.lock`, no `tui-trace.jsonl`, no handoff artifacts, no
hook state files. The forked session is idle after the fork. It
holds no lock and no pid.

On success the command prints nothing and exits 0. The target dir
is `<root>/NEW`.

## Properties

P1. fork-prefix: given M parent lines and N in [1, M], the forked
log decodes to exactly lines 1..=N, in order.

P2. format-preserved: given a plain parent log, the forked log stays
plain. Given a zstd parent, the forked log stays a zstd stream with
a validating sidecar.

P3. resume-append: given a forked zstd session, the next append
lands as one frame after the forked frame. The sidecar record
continues the seq numbering.

P4. tool-boundary: given a parent tool log and boundary N, the last
kept tool line keys the last `tool_result` at or before N. Every
`tool_result` in the forked context holds its tool line.

P5. no-tool-skip: given no parent tool log, or no `tool_result` at
or before N, the fork holds no tool log.

P6. settled-boundary: given a prefix that keeps a tool pair
split, the fork refuses with a named error. The error names the
stranded pair.

P7. range-guard: given N outside [1, the line count], the fork
refuses with a named error and writes nothing.

P8. target-exists: given an existing target dir, the fork refuses
with a named error. The existing session stays untouched.

P9. cwd-precedence: given an explicit `--cwd`, it wins. Given the
parent's recorded `cwd`, the fork copies it. Given neither, the
fork holds no `cwd` file.

P10. idle-fork: given a successful fork, the forked session holds
no lock file, no pid record, no identity record. A later plain run
rewrites the runtime files.

P11. root-placement: given the root rule, the forked session sits
in the effective root. A missing parent raises a named error. The
command creates no directory in the caller tree.

P12. no-partial: given a fork failure after the write starts, no
partial target dir survives.

## Verification

| P# | Property | Proof | Status |
|----|----------|-------|--------|
| P1 | fork-prefix | `fork_copies_the_event_prefix` in `bin/rushi/src/fork.rs`; e2e scenario 1 in `scripts/e2e-fork.sh` | proven |
| P2 | format-preserved | `fork_keeps_a_plain_log_plain` and `fork_writes_a_consolidated_zstd_log_with_a_valid_sidecar` in `bin/rushi/src/fork.rs`; e2e scenarios 1 and 2 in `scripts/e2e-fork.sh` | proven |
| P3 | resume-append | `a_commit_after_the_forked_stream_continues_the_seq` in `bin/rushi/src/fork.rs`; e2e scenario 1b in `scripts/e2e-fork.sh` | proven |
| P4 | tool-boundary | `the_tool_boundary_keys_the_last_kept_line`, `a_missing_key_keeps_every_tool_line`, `a_malformed_line_never_skews_the_tool_cut`, and `a_malformed_tool_line_keeps_the_keyed_record` in `bin/rushi/src/fork.rs`; e2e scenario 3 in `scripts/e2e-fork.sh` | proven |
| P5 | no-tool-skip | `a_prefix_without_results_keeps_no_tool_log` and `a_parent_without_a_tool_log_keeps_none` in `bin/rushi/src/fork.rs`; e2e scenario 2 in `scripts/e2e-fork.sh` | proven |
| P6 | settled-boundary | `a_stranded_prefix_is_refused_by_name` in `bin/rushi/src/fork.rs`; e2e scenario 4 in `scripts/e2e-fork.sh` | proven |
| P7 | range-guard | `an_out_of_range_boundary_is_refused` in `bin/rushi/src/fork.rs`; e2e scenario 5 in `scripts/e2e-fork.sh` | proven |
| P8 | target-exists | `an_existing_target_is_refused_and_untouched` in `bin/rushi/src/fork.rs`; e2e scenario 5 in `scripts/e2e-fork.sh` | proven |
| P9 | cwd-precedence | `the_cwd_precedence_is_flag_then_record` in `bin/rushi/src/fork.rs`; e2e scenario 6 in `scripts/e2e-fork.sh` | proven |
| P10 | idle-fork | `the_fork_holds_no_loop_files` in `bin/rushi/src/fork.rs`; e2e scenario 1 in `scripts/e2e-fork.sh` | proven |
| P11 | root-placement | `the_root_rule_is_flag_then_parent_parent_then_config`, `a_root_dir_parent_raises_no_root`, and `a_missing_parent_raises_a_named_error` in `bin/rushi/src/fork.rs`; e2e scenarios 6, 7, and 8 in `scripts/e2e-fork.sh` | proven |
| P12 | no-partial | `a_failed_write_leaves_no_partial_target` in `bin/rushi/src/fork.rs`; e2e scenario 5 in `scripts/e2e-fork.sh` | proven |

## Gate

```
cargo build
cargo test
scripts/e2e-fork.sh
scripts/verify-specs.sh
```

## Related

- `docs/rewind-fork-design.md` — the rewind event, the active-path
  mask, and the P8 write-boundary guard the cut reuses.
- `docs/session-log-compression.md` — the zstd stream layer, the
  sidecar records (P8), and the idle consolidation the fork writer
  shares.
- `docs/ft-005-logline.md` — the locked single-write append the
  forked session resumes through.
