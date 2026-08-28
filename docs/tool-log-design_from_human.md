# Tool log and TUI trace design (from human)

A design note recorded from the human. It fills a gap in the harness:
`events.jsonl` is the source of truth for agent events, but the TUI has no
log of its own errors, and tool result bodies flood the event log.

## The gap

- `events.jsonl` is the single source of truth. It records user and
  assistant messages, tool calls, and tool results. Agent and harness
  errors are logged explicitly and loudly there.
- The TUI has no log of its own errors and warnings. When the TUI showed a
  `[malformed log line]` hint, there was no trace to read. The finding had to
  be recorded by hand in `docs/failure-tracking.md` (FT-001, FT-002).
- Tool result bodies are inlined in `events.jsonl` as large text. One tool
  call can add 16 KB of raw lines. The agent is flooded with tool output.
  The event log grows fast and the context budget (FT auto-compact) works
  harder.

## Proposal

Two changes. Both keep `events.jsonl` the source of truth. Everything stays
trackable and replayable from the log.

### 1. A TUI trace log

- The TUI binary writes its own errors and warnings to a trace log.
- The log records, with a timestamp: render failures, port errors,
  malformed-line hints, key handling faults, and loop spawn/stop events.
- This makes TUI-side problems readable on their own. No human report is
  needed to learn that a malformed line appeared.

### 2. A tool activity log, split from `events.jsonl`

- Move tool result bodies out of `events.jsonl` into a per-session tool
  log.
- `events.jsonl` keeps: `user_message`, `assistant_message` (rich
  content), `tool_call`, and a slim `tool_result` index. The index carries
  the call id, status, byte length, a pointer into the tool log, and a short
  head/tail preview.
- The tool log holds the full stdout, stderr, and exit status of each tool
  call, in order, keyed by the same call id as the `tool_call` event.
- The model still receives tool results. `assemble` reads them from the
  tool log (or slices from it). The event log stays the source of truth for
  the conversation. The tool log is the source of truth for tool activity.
- Replay: given `events.jsonl` plus the tool log, the whole session
  replays.

## Benefits

- The agent sees a slim index instead of 16 KB of raw tool text. Less
  context flood. The auto-compact work gets easier.
- The TUI and the tool layer have a trace to read when something fails.
  Fewer hand-written failure entries.
- `events.jsonl` stays the source of truth and stays replayable.

## Open questions

- Where does the tool log live: the session dir, or a global trace tree?
- How does `assemble` feed tool results to the model now: the full body, a
  summary, or on-demand fetch by call id?
- Does the slim `tool_result` event point into the tool log by byte
  offset, or by call id lookup?
- Does the TUI trace log go to the session dir, or a fixed global path?
- How does the split interact with the context budget and auto-compact?

## Related

- `docs/failure-tracking.md` FT-001, FT-002: the malformed-line flash and
  the read/write race this design helps make traceable.
- `docs/failure-tracking.md` FT-003: the loop.pid reattach (a TUI trace
  entry would record loop spawn/stop).
- `docs/failure-tracking.md` FT-005 (`docs/ft-005-logline.md`): the
torn-write hole behind FT-002, closed by the locked single-write
`LogLine` commit. The tool log and the TUI trace log must write
through the same commit. Their readers inherit the FT-001 tail-drop
rule.
