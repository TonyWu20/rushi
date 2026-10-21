# Session Log Compression — Zstd Archive at Rest

Status: Spec approved. The human selected the model on
2026-10-21: per-commit frames while active, one -19
consolidation when idle. All decisions in section 5 closed the
same day. The build runs on branch `feat/log-zstd`.
The measurements are reproducible with the commands in section 7.

## 1. Problem

The session tree holds 137 session directories.
`events.jsonl` totals 1.1 GB. `tools.jsonl` totals 671 MB.
The largest single event log is 15 MB.
The largest tool log is 13.7 MB.
Every model call re-reads the full raw log.
`bin/assemble` reads all of `events.jsonl` and `tools.jsonl`.
`bin/compact` reads the full event log.
`bin/claim` reads the full log for the state machine.
The tail scans read the whole file and walk it in reverse.
They look for the last `assistant_message` id and the last
`model_thinking` value.

## 2. Measurements

Measured on `sessions/better-ui-colors_h1`, the largest event log.
Raw sizes: `events.jsonl` 15 MB, `tools.jsonl` 13.7 MB.

| Format           | events | tools |
| ---------------- | -----: | ----: |
| raw jsonl        |  100%  | 100%  |
| gzip -6          |   17%  |  15%  |
| zstd -9 -T0      |   12%  |   4%  |
| zstd -19 -T0     |   10%  |   2%  |
| lumpi .lmp (v9)  |   13%  |   5%  |

Frame size sets the ratio. zstd sees only one frame at a time.
Per-line frames give 3.2x on the event log.
A whole-file zstd -19 stream gives 9.3x.
Block frames sit between the two:

| Frame size (events log) | Ratio |
| ----------------------- | ----: |
| 1 line                  |  3.2x |
| 4 lines                 |  4.0x |
| 16 lines                |  5.1x |
| 64 lines                |  6.0x |
| 256 lines               |  6.9x |
| whole file (-19)        |  9.3x |

The level is not the lever. The window is.
Per-line ratio across levels: -1 gives 3.0x, -9 gives 3.2x,
-22 gives 3.3x. The maximum level adds nothing.
Per-line frames cap at 3.3x, at any level.
The whole file at -22 reaches 9.3x, the same as -19.
A single line holds each key once. The compressor finds no
repetition to match. Sixty-four lines hold the same keys
hundreds of times. The matches carry the gain.

The tool log is more repetitive. It reaches 50x at zstd -19.
The curve flattens fast. Sixteen lines capture most of the gain.
One-line tool-log frames reach 4.6x. A single big record compresses well.
Decompression cost is small. One 13.7 MB tool log decompresses
in 20 ms. That is 0.7 GB/s.
lumpi grep runs on the packed archive without unpacking.
It found 839 `ext_status` matches in 199 ms on the 1.9 MB event
archive. It found 19 `exit=1` records in 32 ms on the tool archive.

## 3. Design Constraints

The live log is append-only.
`LogLine::commit` takes an exclusive `flock`, then writes one
whole line (FT-005, docs/ft-005-logline.md).
Four processes write the log: `user`, the loop, `log`, `route`.
All readers read the whole file into one string.
`rewind` and fork keep every old line replayable.
`compact` appends markers only. It never rewrites the log.
The TUI reads the live log from the rushi-tui repo.
Humans grep and jq the raw log when something fails.
The failure-tracking entries (docs/failure-tracking.md) depend on
the raw text.

## 4. Options

### A. Always-compressed live log (viable)

Gzip and zstd both allow concatenated stream members.
The append path can emit one compressed frame per commit.
`LogLine::commit` keeps its one-lock, one-write contract.
It compresses the line, then writes one frame.
Every reader sits behind a streaming zstd decompressor.
No raw-or-zst state, no hydration, no archive trigger.
One code path everywhere. Sessions compress from line one.

Two frame sizes:

- A1: one frame per commit. Measured 3.2x on events.
  No buffering, no index. Each commit shows up as soon as its
  frame lands, so the TUI stays live.
- A2: block frames. Retired. The idle -19 pass delivers the
  finished-file ratio. The block buffer adds per-process state
  and a visibility delay for less. A1 stays the live model.

Costs of the always-compressed model:

- The ratio tops out at 3-7x. It never reaches the 10-50x of a
  finished file. The window is the limit.
- 1.8 GB lands near 490 MB under A1, near 330 MB under A2.
  Option B lands near 125 MB.
- Raw grep and jq need a `zstd -dc` pipe. The FT-005 debug
  path changes shape.
- Every reader in this repo and the rushi-tui repo takes the
  zstd layer.
- A torn tail frame is a new failure class. A crash mid-frame
  leaves a truncated last frame. Readers stop at the last
  complete frame and drop the rest.
- The commit moves to flush time. A crash loses the unflushed
  buffer. The stage-boundary flushes bound the loss to one
  step, the same window the raw log already loses.
- The tool log holds one record per call. Its frames stay
  single-line at 4.6x. The tool-log ratio does not reach the
  50x of a finished file.
- The cost of a full re-read is one stream decompression pass.
  It stays under 100 ms even at a 100 MB session (raw).

### B. Idle -19 consolidation (pairs with A1, selected)

The idle pass rewrites the frame file into one zstd -19 stream.
It reads the stream, writes a temporary file, verifies the
round-trip by hash, then renames over the original.
The whole pass runs under the log-line lock.
No writer can interleave. The reader never sees a change.
Both layouts are zstd streams. Only the frame layout differs.

Resume after consolidation costs nothing. The writer appends
per-commit frames to the consolidated stream.
Zstd streams accept concatenated frames of any size.

The archiver needs no hydration step. No raw file ever deletes.
Legacy raw sessions keep their plain text.
The reader sniffs the zstd magic bytes.
Plain text stays plain text.

At rest, the file reaches the finished-file ratio.
It lands near 10x for events, near 50x for the tool log.
The 1.8 GB tree lands near 130 MB.
While active it sits near 550 MB at 3.3x.

The archiver runs at the loop's session end, after the lock
releases. A manual `rushi archive` command covers the rest.
A config gate keeps the automatic pass switchable.

### D. lumpi .lmp (offline tool only)

lumpi packs the whole file. It is a CLI app, not a library.
On this data, whole-file zstd matches or beats lumpi in ratio.
lumpi unpack is not byte-exact. It drops null fields.
It normalizes integers and reformats the JSON.
The harness replay needs the bytes back exact.
The `.lmp` format cannot serve as the storage of record.
Use lumpi as an offline grep tool for cross-session analysis.
It ships no harness code. It is documented only.

### E. Model wire compression (deferred)

`bin/model` could gzip the request body to the provider.
Most OpenAI-compatible servers accept a gzip body.
SSE responses return uncompressed on most servers.
The gain is network bytes, not log storage.
Defer this option. Re-open it when model latency is the cost
driver.

## 5. Decisions

- D1: resolved 2026-10-21. A1 live (per-commit frames, 3.3x)
  plus B idle consolidation (-19, 10-50x at rest).
- D2: resolved with D1. No raw file, no deletion, no hydration.
  The consolidation is an in-place frame re-layout.
- D3: resolved 2026-10-21. The `zstd` crate joins the kernel
  binary. The flake gains `pkgs.zstd`. A1 forces it, because
  the writer compresses.
- D4: closed 2026-10-21. The human does not want lumpi
  recorded. It stays outside the harness. No doc entry.
- D5: deferred. The model wire gzip stays parked.
- D6: resolved 2026-10-21. The rushi-tui repo takes the zstd
  stream layer plus the raw-text fallback. The issue files
  after the kernel work lands. Cross-repo work.
- D7: retired. No block frames, no flush trigger values.
- D8: resolved 2026-10-21. The archiver runs at the loop's
  session end and through a manual `rushi archive` command.
  One config flag gates the automatic pass.
- D9: resolved 2026-10-21. The file name stays `events.jsonl`
  with zstd content. The reader sniffs the magic bytes.
  Legacy raw text keeps working under the same name.

## 6. Read Sites Changed (A1 + B)

| Site                              | Today     | With Option B          |
| --------------------------------- | --------- | ---------------------- |
| `assemble` event + tool log read  | raw only  | zstd stream reader    |
| `compact` `read_events`           | raw only  | zstd stream reader    |
| `claim` `derive_state` input      | raw only  | zstd stream reader    |
| loop tail scans (id, thinking)    | raw only  | zstd stream reader    |
| `approval` log scans              | raw only  | zstd stream reader    |
| `user`, `log`, `route` appends    | raw only  | compress in `LogLine` (A1) |
| TUI live log read                 | raw only  | zstd layer (A1), raw fallback |
| `est_probe` example               | raw only  | zstd stream reader    |

The archiver holds the log-line lock for its whole pass.
No writer interleaves. No second lock is needed.

## 7. Reproduce

    S=sessions/better-ui-colors_h1
    zstd -19 -T0 -c $S/events.jsonl | wc -c
    zstd -19 -T0 -c $S/tools.jsonl  | wc -c
    zstd -dc $S/events.jsonl.zst | wc -c   # round-trip byte check

lumpi:

    cargo install lumpi
    lumpi pack $S/events.jsonl
    lumpi grep $S/events.jsonl.lmp 'type=ext_status'
    lumpi unpack $S/events.jsonl.lmp | diff - $S/events.jsonl
    # the diff is not empty: JSON reformatting, null drops

Per-line frame ratio (sampled 400 lines, zstd -19 each):

    python3 - <<'EOF'
    import subprocess, random
    random.seed(1)
    p = "sessions/better-ui-colors_h1/events.jsonl"
    lines = open(p, "rb").read().splitlines()
    s = random.sample(lines, 400)
    raw = sum(len(l) + 1 for l in s)
    c = 0
    for l in s:
        c += len(subprocess.run(["zstd", "-19", "-q", "-c"],
                                input=l, capture_output=True).stdout)
    print(f"per-line {raw/c:.2f}x vs raw {raw}")
    EOF

The scratch work area held the bench outputs (2026-10-21):
`scratch/compress-bench/`.

## 8. Spec (properties and build order)

P1: one writer. `LogLine::commit` is the only append path. It
compresses the line into one zstd frame, then writes the frame
in one locked write. The lock serializes writers. The write is
one frame. The lock holds on a gate descriptor. The commit
reopens the current inode under the lock and writes there. The
format peek reads four bytes from its own plain descriptor.
Kernels 6.16 and later refuse to seek an `O_APPEND` descriptor,
so the peek never seeks.

P2: one stream. `events.jsonl` and `tools.jsonl` are one zstd
stream, a concatenation of frames. Legacy logs stay plain text.
The reader sniffs the four zstd magic bytes. The format is
decided at creation and held for the file's life: a new file
becomes a zstd stream. A legacy plain text file stays plain
until an explicit archive migrates it. No file mixes both.

P3: transparent reader. `logstream::read_log_text` returns JSONL
text from either layout. Legacy sessions need no migration.

P4: torn tail. A truncated final frame stops the read at the
last complete frame. The partial line drops at parse time. A
reader never fails on a torn tail.

P5: consolidation is safe. The idle pass reads the stream,
writes a temp file, verifies the round-trip by byte compare,
then renames over the original. The whole pass runs under the
log-line lock. No writer interleaves.

P6: resume is free. After consolidation, the writer appends
frames to the consolidated stream. No hydration step exists.

P7: the trigger. The auto pass runs at the loop's session end,
when the reason is `idle`. The `[log] consolidate_on_idle` flag
gates it, default on. `rushi archive` runs the pass by hand.

Build order:

1. `rushi-common` gains the `zstd` dependency and the `logstream`
   module (frame compress, `read_log_text`, `consolidate`).
2. `LogLine::commit` writes frames instead of raw bytes.
3. Every session-log read site moves to `logstream::read_log_text`.
4. `rushi archive` subcommand, the `[log]` config flag, the
   session-end auto pass.
5. The flake adds `pkgs.zstd`.
6. Unit tests for P1 to P7. The workspace `cargo test` stays
   green. One e2e session proves the loop on compressed logs.
7. Every e2e script that reads a session log decodes through
   a magic-byte sniff. A plain legacy log passes through as
   text. A zstd stream decodes with the `zstd` binary.

## Consumers

Downstream repos read session logs through the shared reader.
`rushi-tui` and `tv-rushi` call
`rushi_common::logstream::read_log_text`. That one implementation
serves every consumer. A consumer does not copy the layout logic.

The next release after this merge carries `logstream`. Main
carries 0.1.7 at merge time. Consumers pin that release. They
do not pin a version number in their docs.

## Related

- `docs/reasoning-log-size.md` — the earlier log-size decision
  (strip the reasoning `summary`).
- `docs/tool-log-design_from_human.md` — the `tools.jsonl` split.
- `docs/ft-005-logline.md` — the locked single-write append.
- `docs/rewind-fork-design.md` — why old lines stay replayable.
