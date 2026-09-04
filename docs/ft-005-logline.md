# FT-005: `LogLine` — motivation, analysis, and implementation

Record for the FT-005 entry in `docs/failure-tracking.md`.
Commit `60b88e2` (the capability and its tests).
This doc keeps the reasoning that led to the design, so the next
session can re-derive it instead of re-discovering it.

## 1. Motivation: reading the ruxe post

Source: "A data race that doesn't compile" by Corentin Corgié
(`https://corentin-core.github.io/posts/ruxe-type-level-disjointness/`,
June 2026). The post builds this property into `ruxe`, a Redux-flavored
Rust learning library (PR #26 in that repo; the HList/Sculptor
vocabulary comes from the `frunk` crate).

The problem there: a parallel reducer pipeline. Several slice reducers
run concurrently over disjoint slices of one state. Sequential gives
N× latency. Parallel gives max(latency_i). The catch: parallel plus
shared state equals a data race if two reducers ever touch one slice.
In most languages the type system gives up at that point. The post
shows Rust can refuse to build the racing pipeline.

The core idea is a reformulation:

- Negative properties do not compile. "No two reducers share a slice"
  needs type inequality (`H != T`) or negative reasoning
  ("this trait is not implemented"). Stable Rust has no such syntax.
  Trait resolution reasons monotonically: adding an impl never
  disables code that compiled before. `negative_impls` has sat
  unstable for years over exactly this coherence concern.
- The positive reformulation compiles. "Each slice has exactly one
  reducer" is a bijection: an existence property. Walk the state's
  slice HList. For each slice, look up the reducer whose `Slice`
  associated type matches it (the frunk `Sculptor` pattern, matched on
  an associated type instead of a concrete type). The trait resolver
  enforces the property on every lookup it performs:
  - exactly one match: resolves, done
  - two or more: ambiguous-impl error, build fails
  - zero: unsatisfied bound, build fails
- HList is the walking structure: `HCons<H, T>` / `HNil`. One base and
  one recursive impl cover any arity. Tuples cannot do this. Each
  arity is a distinct type. `Here` / `There<I>` Peano witnesses
  disambiguate the two recursive impls that would otherwise overlap.

The transferable takeaway, in the post's words: wherever you reach
for "check there are no duplicates at runtime", phrase it as "check
there is exactly one match at compile time". The payoff: the
disjointness guarantee is not enforced at runtime because it does
not need to be. The compiler refuses to assemble the racing program.

## 2. Why that does not prevent our race (and what does transfer)

Our known failures of this kind: FT-001 (torn read, red
`[malformed log line]` flash) and FT-002 (read/write race on
`events.jsonl`).

- Our race is cross-process and temporal. The loop's `bin/log`
  process appends events. The TUI process reads the log and appends
  its own events. A trait bound in one process cannot stop bytes
  written by another process.
- Type-level disjointness is an intra-program structural property.
  There is no "two reducers on one slice" configuration to reject
  here. Both roles touching one file is the design.
- So the ruxe trick cannot make FT-002 impossible on its own.

What transfers is the reformulation discipline:

- "No torn lines" is a negative property. Nothing in stable Rust
  proves it.
- "Every line is committed by exactly one locked write" is a
  positive, structural property. A capability type can encode it.
  That is the design in section 4.

## 3. Analysis: two findings in this repo

### Finding 1: the one-write claim held for the TUI only

FT-002 states the writer appends with "one `write(2)` per event".
That is true for the TUI's `append_event` (one `write` of the whole
line). `bin/log` and `bin/user` used `writeln!(file, ...)` instead.
That is two syscalls per line: the JSON body, then the newline.
A reader racing the second write sees a body without its newline,
the in-progress tail state. The FT-001 tail-drop survives that
state, but the window is doubled for every line the loop or
`user` appends.

### Finding 2: two-writer interleave yields complete garbage lines

While a loop runs, the TUI process appends `user_message` and
`ext_status` events to the same log the loop's `bin/log` process
appends to. Two live writers, by design of the current phase.

- A regular file gets no `O_APPEND` write atomicity at any size.
  The `PIPE_BUF` atomicity guarantee applies to pipes only.
- An `O_APPEND` write resolves its end offset at write start.
  Two concurrent writers can resolve the same offset. Their byte
  ranges then interleave.
- The result is lines that end in a newline and contain mixed
  bytes from two events. The reader parses them as complete events.
  They fail the schema check or `Event::parse_line` and render as
  the red `[malformed log line]` hint. That is the FT-001 symptom
  with a different root cause.
- The FT-001 mitigation (drop the unterminated tail) cannot see
  this form. Every interleaved line ends in a newline.
- Consequence: FT-002's proposed durable fix, a size cap on a
  single appended event, does not close this hole on a regular
  file. Two writers each under the cap can still interleave at
  byte granularity. Serialization closes it.

## 4. Design: the `LogLine` capability

- One type owns the whole line bytes, including the trailing
  newline. It is the only value that reaches the log.
- `commit(path)` is the only write path: open with `O_APPEND`,
  take an exclusive `flock`, do one `write(2)` of the whole
  buffer, unlock.
- The type has no `impl Write`. A line cannot be appended in
  pieces. The compiler enforces that part of the guarantee.
- `flock` serializes appends across any number of processes. The
  lock dies with the writer. A dead writer cannot wedge the log.
  `flock` needs local storage. The sessions root is local here.
- The ruxe parallel: the refusal surfaces at first use, not at
  construction. `LogLine::from_json` always succeeds. The
  guarantee bites at `commit`, the first point where the line
  meets the file. The post notes the same nuance for ruxe: the
  bijection check surfaces at `.reduce()`, not at `new()`.
- Placement follows the phase-1 policy (no shared crate): three
  copies, `bin/log/src/logline.rs`, `bin/user/src/logline.rs`,
  and an embedded `mod logline` in `bin/tui/src/port_file.rs`.
  Guardrail 3 (`docs/tui.md` section 10) makes `port_file.rs` the
  only TUI module that may know the storage layout, so the TUI
  copy lives there. `notes/itches.md` records the duplication as
  a promotion candidate alongside the schema validator.

## 5. Implementation (commit `60b88e2`)

- `bin/log`: new `logline.rs`. The append loop calls
  `LogLine::from_json(line).commit(&log_path)`. The old open plus
  `writeln!` pair is gone. `libc` and `tempfile` deps added.
- `bin/user`: same. The entry-point append is one locked commit.
- `bin/tui`: `append_event` keeps schema validation and cwd
  recording, then builds one `LogLine` and commits it inside the
  existing `spawn_blocking`. The short-write check disappears.
  `write_all` reports it.
- Reader side is unchanged. FT-001 tail-drop still covers the
  single-writer in-progress tail. The tailer holds a partial
  line until its newline.
- `docs/tui.md` spec line updated. `notes/itches.md` gained the
  three-way-copy entry.

## 6. Verification

- `concurrent_commits_stay_line_granular`, present in all three
  crates: two threads commit 50 lines of 4 KB each to one file.
  The file holds exactly 100 complete lines. Each parses as JSON.
  Each payload is intact. Without the lock, this invariant can
  break under concurrent writers. The test pins it.
- `commit_appends_one_complete_line_per_event`, all three crates:
  sequential commits stay whole lines.
- Suites at the commit: `port_file` 22 tests, `bin/log` 2,
  `bin/user` 2. Workspace build clean. Verified in a worktree
  checked out at `60b88e2`, not just the working tree.

## 7. Residual and follow-ups

- `flock` needs local storage. True for this deployment. A remote
  sessions root would need a different serialization (the daemon
  split in `docs/tui.md` section 12 is the likely home for that).
- The FT-005 entry in `docs/failure-tracking.md` rides that file's
  own commit. The file was untracked at `60b88e2` and carries
  earlier entries (FT-001 through FT-004) from other work.
- The three-way `LogLine` copy is a promotion candidate for a
  shared crate. Keep the copies in sync. Do not grow them in
  parallel.

## Properties

Lean-style invariants for this spec (see `lean-driven-development.md`).
One property per non-trivial invariant. Each property is observable:
given an input, an output guarantee.

P1. complete-unit: given a `LogLine` value committed to a path,
    observe the whole line plus its trailing newline in one locked
    write.
    
P2. line-granular-concurrency: given two threads that commit 50
    lines of 4 KB each to one file, observe 100 complete lines,
    each parseable as JSON with intact payloads.
    
P3. no-wedge: given a writer that dies while holding the commit
    lock, observe the lock free on the next commit.

## Verification

Each property maps to its proof. `proven` means the cited test exists
and passes. `open` names the blocker and what unblocks it.

| P# | Property | Proof | Status |
|----|----------|-------|--------|
| P1 | complete-unit | `commit_appends_one_complete_line_per_event` in `crates/common/src/logline.rs` | proven |
| P2 | line-granular-concurrency | `concurrent_commits_stay_line_granular` in `crates/common/src/logline.rs` | proven |
| P3 | no-wedge | Blocked: no test kills the holder and re-commits. Unblocked by a flock-release test that kills the holder, then asserts the next commit succeeds | open |

## Gate

The acceptance commands. All must exit 0 for this spec to be proven.

```
cargo build
cargo test
```
