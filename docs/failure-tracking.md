# Failure tracking

Known failures and their status. One entry per defect. Each entry names
the symptom, the root cause, the fix, and the verification. New entries
take the next `FT-` number.

## FT-001 — `[malformed log line]` flash on a live loop

**Symptom:** While a loop runs, the TUI occasionally renders a red
`[malformed log line]` hint. The line it shows is not a real log line.
The user sees it as a corrupt log.

**Root cause:** A torn read. The TUI re-reads `events.jsonl` while the
loop appends to it. A read that races an in-flight append sees the last
line without its trailing newline. That partial segment fails
`Event::parse_line` and renders as malformed.

**Fix:** `read_events` (`bin/tui/src/port_file.rs`) now drops the final
segment when the read does not end in a newline. It treats that segment
as in progress and shows it on the next read. A persisted line always
ends in a newline, so a clean file never loses data.

**Verification:** Regression test
`read_events_drops_in_progress_tail_without_newline` passes. All 140
tui tests pass. The fix takes effect on the next TUI launch.

## FT-002 — Read/write race on `events.jsonl`

**Symptom:** The TUI reader and the loop writer share one file. A reader
can observe a writer's line mid-append.

**Root cause:** The writer appends with `O_APPEND` and one `write(2)`
per event (`port_file.rs` `append_event`). That is atomic only for lines
well under `PIPE_BUF`. A reader that races a large append can see a torn
line. `read_events` reads the whole file, so it can catch a torn tail.

**Mitigation:** The writer keeps the one-write append. The reader drops
an in-progress tail (FT-001 fix). The `MAX_LOG_READ_BYTES` cut already
drops a possibly partial first line.

**Residual risk:** Closed by FT-005. The writer now appends through
the locked single-write `LogLine` commit. The two-writer interleave
was the true residual hole. A size cap cannot fix it on a regular
file. The lock serializes it. Note: the one-write claim above held
only for the TUI writer. `bin/log` and `bin/user` needed the FT-005
fix.

## FT-003 — Restarted TUI cannot stop an orphan loop

**Symptom:** Double `q` now leaves the loop running as an orphan
(intended). A restarted TUI has no handle to that loop. `Ctrl+C` in the
new TUI does nothing to it.

**Root cause:** Loop handles live in the TUI process. When the TUI
exits, the running loop orphans. A new TUI has no way to signal a loop
it did not start.

**Fix:** Persist the loop process ID as a session artifact. `spawn_loop`
writes `<session_dir>/loop.pid` with the group leader PID. On `Ctrl+C`,
when no local handle exists, the TUI reads `loop.pid`, confirms the PID
is still this session's loop, and stops the process group. A recycled
PID that no longer names this session is left alone.

**Verification:** `stop_external_loop` probes the group and reads
`/proc/<pid>/cmdline`. It kills only a live PID whose command line names
the session. A live test
(`stop_external_loop_kills_a_live_orphan_group`) spawns a session-leader
loop, confirms it is a live group leader naming the session, stops it
through the reattach path, and confirms the group dies. The leader is
spotted as a zombie until reaped, so the death check treats a zombie as
dead.

## FT-004 — `edit` panics when a multi-byte character straddles byte 8192

**Symptom:** The `edit` tool panics on a file larger than 8192 bytes
when byte 8192 falls inside a multi-byte character. Observed in the
better-ui session on 2026-08-28: two back-to-back panics at
`tools/edit/src/main.rs:93:12` ("end byte index 8192 is not a char
boundary; it is inside '─'"). The in-loop agent worked around it and
applied its edits through `python3` via the `bash` tool.

**Root cause:** The binary check sliced the decoded string by byte:
`text[..check_size]` with `check_size = min(text.len(), 8192)`. A Rust
string slice must start and end at a character boundary. Byte 8192
inside a 2-4 byte character (here '─', U+2500, in a box-drawing
comment line of `bin/tui/src/app.rs`) makes the slice panic.

**Fix:** The binary check now runs on the raw bytes before decoding:
`content_bytes[..check_size].contains(&0)`
(`tools/edit/src/main.rs`). A byte slice is index-safe. Behavior is
unchanged: a NUL byte in the first 8 KB still rejects the file.

**Verification:** Conformance test `edit: multibyte char straddling
byte 8192 (FT-004)` in `scripts/tool-conformance.sh`: 8191 ASCII
bytes plus '─' at bytes 8191-8194 accepts an edit without a panic.
The binary-rejection and BOM-preservation tests still pass. All 41
conformance tests pass. The runtime binary `tools/edit/bin/edit`
is rebuilt and updated.

## FT-005 — Two-writer interleave corrupts complete log lines

**Symptom:** Not observed directly. This is a structural hole behind
the FT-001 symptom. While a loop runs, the TUI process appends
events (`user_message`, `ext_status`) to the same log that the
loop's `bin/log` process appends to. Two concurrent writers can
interleave inside one byte range. The result is lines that end in a
newline and parse as garbage. The FT-001 tail-drop only drops a tail
without a newline. It cannot see this form. Related: `bin/log` and
`bin/user` appended each line in two `write(2)` calls (body, then
newline). The FT-002 one-write claim held only for the TUI writer.

**Root cause:** A regular file gets no `O_APPEND` write atomicity at
any size. The `PIPE_BUF` atomicity guarantee applies to pipes only.
An `O_APPEND` write resolves its end offset at write start. Two
concurrent writers can resolve the same offset and overwrite each
other's bytes.

**Fix:** The `LogLine` capability, copied to all three writers:
`bin/log/src/logline.rs`, `bin/user/src/logline.rs`, and the embedded
`mod logline` in `bin/tui/src/port_file.rs`. One type owns the
whole line bytes including the newline. Its `commit` is the only
write path: an exclusive `flock` plus one `write(2)` of the whole
buffer. The type has no `impl Write`, so a line cannot be appended
in pieces. Concurrent appends from any number of processes
serialize on the lock.

**Verification:** Regression test
`concurrent_commits_stay_line_granular` in all three crates: two
threads commit 50 lines of over 4 KB each. The log holds exactly 100
complete lines, each parseable, each payload intact. The full
`port_file` suite (28 tests) passes. The `bin/log` and `bin/user`
suites (2 tests each) pass.

**Residual risk:** `flock` needs local storage. The sessions root
is local in this deployment.

## FT-006 — Launch panic on an empty `assistant_message` content

**Symptom:** The TUI exits at startup, one second in, in a tmux
pane. The stderr holds a panic:
`thread 'main' panicked at bin/tui/src/render.rs:242:41: range
start index 1 out of range for slice of length 0`. Observed on the
better-ui session, 2026-08-28, after the user put the reference
extension binaries on `PATH`.

**Root cause:** The better-ui log holds an `assistant_message` with
an empty `content` string (log index 925: the model emitted only a
tool call, no text). The `AssistantMessage` arm of `event_lines`
(`bin/tui/src/render.rs`) builds `wrapped` as `Vec::new()` for an
empty content, then slices `&wrapped[1..]` without a guard. An
empty-slice start of 1 panics on a zero-length vec. The cap window
(`TRANSCRIPT_EVENT_CAP` 2000) covers the whole log (1302 events),
so the first draw of every launch renders that event and panics.
The `UserMessage` and `Error` arms carry the same unguarded slice;
their `wrapped` cannot be empty in practice (a non-empty default
string, a one-line wrap), so only the `AssistantMessage` arm
fires.

**Fix:** Guard all three `guttered(&wrapped[1..], ...)` call sites
with `if !wrapped.is_empty()`. An empty `wrapped` leaves the header
row alone, with no body row.

**Verification:** Regression test
`render::tests::empty_assistant_content_renders_header_without_panic`
(an empty content and a whitespace-only content, both with tool
calls) passes. All 173 tui tests pass. End-to-end in a tmux pane
under the devshell: `tui better-ui` renders the full log, accepts
key input, and quits cleanly.

## FT-007 — Launch refusal when the reference extension binary is
absent from `PATH`

**Symptom:** `tui better-ui` exits at once with `tui: ext manifest
.../mermaid/ext.toml: command mermaid-ext not found` (exit 1).
Observed 2026-08-28: the user's devshell `PATH` lacks the
reference extension binary dirs. The TUI is unstartable in that
shell until the dirs enter `PATH`.

**Root cause:** The `mermaid` manifest names `mermaid-ext`, a
binary built under `ui_extensions/mermaid/target/debug`. The
discovery-time refusal is documented design
(docs/ui-extension.md section 3: a broken command refuses the
start and names the file). The user's nix devshell
(`~/programming/flake.nix`) adds no extension dirs. `scripts/
ext-env.sh` prints the dirs, but nothing wires them into a
persistent shell.

**Fix:** A repo `.envrc` (direnv) puts the four built extension
binary dirs on `PATH` on entry into the directory. An absent
target dir degrades silently: the fail-loud message still names
the missing command.

**Verification:** `sh -c '. ./.envrc; command -v mermaid-ext'`
resolves `ui_extensions/mermaid/target/debug/mermaid-ext`. All
four `target/debug` dirs land on `PATH`. The tmux launch with the
`.envrc` `PATH` shows no discovery refusal and no panic (the
FT-006 fix).

**Residual risk:** Shells without direnv still need a manual
`PATH` export (`scripts/ext-env.sh`). The design keeps the
refusal: a missing command stays a start-time error, not a
runtime skip.
