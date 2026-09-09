# Handoff versioning and the handoff DAG

Status: Implemented (2026-07-17). This document extends
`docs/handoff-strategy.md` and `docs/rewind-fork-design.md`.

## 1. The request

Two concerns from the owner:

1. **Preserve more truth.** After a long multi-compact run the user
   wants to proof-read earlier handoff versions to verify that the
   latest compact did not lose critical information. A single
   `handoff.md` that gets overwritten on every compact destroys that
   history.
2. **Multi-branch handoff identity.** Even without a dedicated
   branch-summary feature, the DAG of rewind markers already creates
   multiple branches. When compaction happens on different branches,
   each summary must be linked to its divergence point so that
   `assemble` can pick the correct handoff for the active path.

## 2. Pi tree and branch-summary study

Pi models a session as a tree of entries. Every entry carries an
`id` and a `parentId`, forming a DAG. Two entry types are relevant
here:

- **`compaction`** — replaces the compacted prefix with a `summary`
  plus a `retainedTail`. It sits on one branch of the tree.
- **`branch_summary`** — when the user navigates from branch A to
  branch B, pi summarises the abandoned entries on A and stores the
  result as a `branch_summary` entry. The `fromId` field points to
  the last entry on the abandoned branch, giving the summary a
  concrete divergence point in the tree.

Key insight: pi does not store a flat, overwriting "latest
handoff". Each summary is an immutable node in the tree, linked to
its parent by `parentId`. The active path (determined by the
current branch tip) determines which summary is in play.

## 3. Mapping to the append-only log

This harness uses a flat, append-only `events.jsonl` with 1-based
sequences. The equivalent of pi's tree is the combination of:

- **Rewind markers** (`rewind` events) that mask abandoned spans
  via `active_ranges`.
- **Compaction boundaries** (`compaction_summary` events) that
  shadow the pre-boundary region.

Both mechanisms are "markers + shadowing": the log retains every
event, and projection selectively excludes masked or compacted
spans. The handoff DAG is implicit in the log already; what was
missing was explicit identity and versioning on each summary node.

## 4. The versioning model

### 4.1 New fields on `compaction_summary`

| Field            | Type    | Meaning                                                                 |
|------------------|---------|-------------------------------------------------------------------------|
| `version`        | u64     | 1-based session-global ordinal. Uniquely identifies this handoff.      |
| `parent_version` | u64     | `version` of the previous handoff on this branch. 0 = first.          |
| `diverge_seq`    | u64     | The log seq where this version diverges from its parent. 0 = start.  |

`diverge_seq` is the `first_kept_seq` of the parent boundary (0 for
the first handoff, which summarizes from seq 1). This is the DAG
edge: node N diverges from node N-1 at log position `diverge_seq`.

### 4.2 File layout

```
sessions/<n>/
  handoff/
    v1.md        ← first compaction
    v2.md        ← second compaction (builds on v1)
    v3.md        ← …
  handoff.md     ← copy of the latest version (backward compat)
```

`handoff.md` continues to exist as a convenience for tools that read
a single file. The versioned files under `handoff/` are the
authoritative history.

### 4.3 Version computation

At compact time the harness scans the log for all
`compaction_summary` events:

- `version` = (count of existing boundaries) + 1
- `parent_version` = version of the most recent boundary (0 if none)
- `diverge_seq` = `first_kept_seq` of that parent boundary (0 if none)

This is a global ordinal: two branches that both compact after a
shared ancestor produce versions 2 and 3 with the same
`parent_version = 1` but different `diverge_seq` values. The DAG is
not a simple chain.

## 5. Changes by component

| Component            | Change                                                                  |
|----------------------|------------------------------------------------------------------------|
| `schemas/…/compaction_summary.json` | Add `version`, `parent_version`, `diverge_seq` (required) |
| `crates/rushi/src/compact_math.rs`  | New `handoff_version_meta(events) → (version, parent_version, diverge_seq)` |
| `crates/rushi/src/stage.rs`         | `CompactStatus` gains `version`, `parent_version`, `diverge_seq`    |
| `bin/rushi/src/stage_runner.rs`     | Extract new fields from compact stdout                                 |
| `bin/rushi/src/step.rs`             | `write_handoff` writes `handoff/v<N>.md` + `handoff.md`; `append_compaction_summary` includes new fields |
| `bin/compact/src/main.rs`           | Compute and emit new fields in marker and status JSON                   |
| `bin/assemble/src/main.rs`          | `Boundary` carries `version`; framing reads `handoff/v<N>.md` when available |

## Properties

P1. **Version uniqueness.** Given N compaction events in a session
    log, the N-th has `version = N`. No two events share a version.

P2. **Parent link.** For version N > 1, `parent_version = N-1` and
    `diverge_seq` equals the `first_kept_seq` of version N-1.

P3. **File persistence.** After K successful compactions,
    `handoff/v1.md` through `handoff/vK.md` all exist with distinct
    content. `handoff.md` equals `handoff/vK.md`.

P4. **Assemble selects the correct version.** Given a log with
    multiple boundaries on different branches, `assemble` reads the
    handoff file whose version matches the active boundary, not the
    globally-latest file.

P5. **Backward compatibility.** A log with pre-versioning
    `compaction_summary` events (no `version` field) still assembles
    correctly: `parse_boundary` defaults `version` to 0, and
    `assemble` falls back to `handoff.md` or the embedded summary.

P6. **Append-only invariant.** Adding versioning adds fields to new
    events. It never rewrites or removes earlier log lines.

## Verification

| P# | Property | Proof | Status |
|----|----------|-------|--------|
| P1 | version uniqueness | `handoff_version_meta` unit tests in `compact_math.rs` | proven |
| P2 | parent link | `handoff_version_meta_one_boundary`, `_two_boundaries` | proven |
| P3 | file persistence | e2e `compact-e2e.sh` scenario-1 checks `handoff/v1.md`; scenario-iterative checks `v2.md` | proven |
| P4 | correct version | e2e `compact-e2e.sh` threshold scenario checks `handoff/v1.md` matches `handoff.md` | proven |
| P5 | backward compat | `handoff_version_meta_legacy_events_without_version` test | proven |
| P6 | append-only | Inherited from FT-005; no new write path | proven |

## Gate

```
cargo build
cargo test
scripts/compact-e2e.sh
scripts/e2e-rewind.sh
```
