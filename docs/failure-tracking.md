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
PID that no longer names this session is left alone. The TUI
reattaches at start and on every session switch. `external_loop_pid`
reads `loop.pid`, confirms the group is live and names the session, and
marks the session running without a local handle. The main loop
re-probes the active session once a second. The `[running]` bit shows
real loop state, not this process's memory. `Ctrl+R` blocks on the
probe. It refuses a second start for a live session. `Ctrl+C` emits
the stop intent regardless of local state. Main resolves it through
the probe when no local handle exists.

**Verification:** `stop_external_loop` probes the group and reads
`/proc/<pid>/cmdline`. It kills only a live PID whose command line names
the session. A live test
(`stop_external_loop_kills_a_live_orphan_group`) spawns a session-leader
loop, confirms it is a live group leader naming the session, stops it
through the reattach path, and confirms the group dies. The leader is
spotted as a zombie until reaped, so the death check treats a zombie as
dead. Unit tests `external_loop_pid_reports_a_live_orphan_group` and
`external_loop_pid_dead_pid_is_none` cover the probe.

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

**Follow-up (2026-09-05):** The PATH dependency of the global
layer is gone. The `mermaid` manifest now names its binary by a
path relative to its own entry (`target/debug/mermaid-ext`). The
host resolves a relative `command` against the entry directory,
not the TUI working dir, and not `PATH`
(docs/ui-extension.md section 3 and section 6). The global
`ui_extensions/` layer therefore starts without any `PATH`
export, as long as the binary is built. The `.envrc` and
`scripts/ext-env.sh` `PATH` setup stays for the opt-in `ext-rs/`
Rust ports, whose manifests keep the bare command names, and for
shells that want the harness stage binaries on `PATH`. A relative
command that does not name a file still refuses the start and
names the manifest (fail-loud unchanged).

## FT-008 — Empty tool-call arguments at long context

**Symptom:** `sessions/better-ui/events.jsonl` holds 55 tool
results with text "Tool arguments failed schema validation:
<field>." The field is `command` (49) or `file_path` (6). Every
`arguments` value is the empty object `{}`. No failure appears
below 50k input tokens. Failures cluster at 56k–73k and at
93k–112k input tokens.

**Root cause:** Context-length dependent model failure, not a
harness request defect. The local Qwen3.8-27B-NVFP4 model
(SGLang at `127.0.0.1:30000`) stochastically emits a tool call
whose arguments are the two-character string `"{}"`. The stream
completes with `status=completed`; SGLang passes the empty
arguments through verbatim. An A/B test on 2026-08-29 ruled out
the harness fields: at 110k input tokens, a pi-shaped request
(no `reasoning` field) glitched in 2 of 4 runs; `reasoning`
`medium` and `xhigh` glitched in 0 of 4 each. The same glitch
also fired once inside a pi session at 40k tokens. The pi
session context stays short, so the user sees it rarely there.
The NVFP4 4-bit quantization lowers the model's structured
output fidelity at long context.

**Fix:** No harness code defect. `route` rejects the empty
object with the field-level error from corrections entry 35
(`docs/loop-and-edit-implementation-corrections.md`). The model
re-issues the call with full arguments. All 55 recorded
failures recover within one turn: 3–6 log lines to the next
success, zero unrecovered, no stalled loop.

**Verification:** No code change to verify. The log confirms the
recovery pattern: no failure lacks a later success.

**Residual risk:** Two open items. First, context management:
the better-ui loop's own 2026-08-29 18:15 config rewrite set
`context_budget_chars = 660000` with `compact_result_chars =
80000`. The request then reached about 1.2M chars. No compact
stage fits the budget, and the loop exited with "Context budget
exceeded after compaction." Closed by FT-009: the compact floor
and the drop-oldest-steps pass now keep the session alive.
The same defect killed the session on 2026-08-29 at 10:13 UTC
under the current config. See FT-009. Correction 58 removes the
self-priming load on top of it: the compact pass drops the old
schema-error pairs out of the model request, so the history
stops teaching the model its own empty-argument failures.
The follow-up (correction 59) addresses the misattribution seen in
the 2026-08-30 revived turn: seven consecutive rejections with no
recovery, and the model's reasoning blaming the tool layer. The
rejection text now carries the resend instruction. Second,
a `bin/model`
observability gap: it substitutes `{}` when a terminal event
omits the `arguments` key, so a server-side drop would look
identical to a model-side empty call. SGLang always sends the key
in the captures, so the gap does not fire in this deployment.

**Corrigendum (2026-08-30, correction 60):** The root-cause
line above is wrong. The empty-argument calls are not a
stochastic model glitch. The same model makes no such calls in
pi, where the context holds no failure history. A controlled A/B
on the better-ui request: all 10 schema-error pairs in the input
glitch in 3 of 3 runs. Nine out, the newest kept, glitch in 2 of
3. No pairs at all, clean in 3 of 3. The failure pairs in the
request prime the next call. The harness controls the trigger
through what it sends. Correction 60 drops every schema-error
pair from the model request, keep window included.

## FT-009 — Session dies and stays dead after a failed compact

**Symptom:** `sessions/better-ui` logged three terminal errors
(2026-08-29 10:13:53, 10:13:55, 10:15:16 UTC):
"Context budget exceeded after compaction. Start a new session
or reduce scope." The last one followed the user's `Continue`
message. No turn made progress after the errors. The session was
unrecoverable without a config change or a new session.

**Root cause:** The auto-compact of corrections entry 52 halved
the caps in three fixed stages: `8000, 4000, 2000` for results,
`2000, 1000, 500` for text, with keep windows `24, 12, 6, 3, 2`.
The session log held 604 tool results, 586 assistant messages,
and 16 user messages: about 1.47M projected chars against the
440k budget. The tightest stage still projected near 740k chars,
so all 15 stage combinations failed and `assemble` emitted the
terminal error. Each turn re-ran `assemble` on the same log,
re-emitted the error, and `claim` returned `idle`. Per-event
caps cannot express a total-size constraint: the compacted size
of old events grows with the session length, and the fixed floor
sat above the budget for this log. The 18:15 rewrite incident
recorded in FT-008 is the same defect under wider caps.

**Fix:** Corrections entry 55. `compact_search` halves the caps
from the config base down to the floor
(`compact_min_result_chars = 128`, `compact_min_text_chars = 64`).
When the floor still does not fit, it drops the oldest step
groups, one at a time, until the request fits. User messages and
the keep window are never dropped. The terminal error now fires
only when the task statement plus the keep window alone outgrow
the budget. The config base caps (`8000`/`2000`) stay. They
choose the first stage's fidelity. The `context_budget_chars`
value of 440000 stays. It pins the request under the FT-008
degradation zone of this model (about 55k input tokens). It is
not a tuning error.

**Verification:** The fixed `assemble` on `sessions/better-ui`
prints a 380628-char model request under the 440000-char budget.
It keeps all 16 user messages, including the `Continue` message.
Older items carry the `[compacted:]` markers at the working
stage. Four new tests cover the floor halving, step grouping,
the drop-oldest fit, and the still-unsatisfiable case. All 13
`assemble` tests pass. The log stays intact: dropping affects
the model request only, never the event log.

**Residual risk:** Closed by corrections entry 57. A session whose
keep window plus user messages alone outgrow the budget no longer
dies. `assemble` emits a `context_exhausted` event instead of the
terminal error. The loop summarizes the compacted log, seeds a new
session with the summary, and records the marker with the seeded
name. `claim` reports the `exhausted` state. The TUI resumes in the
seeded session with one key. See FT-010 for the token-budget half
of the fix.

## FT-010 — The char-based context budget ran 2x off

**Symptom:** The harness derived its context budget as
`context_tokens * chars_per_token` with `chars_per_token = 4`.
Measured on the better-ui session, the model counted about 8 chars
per input token. The harness believed its budget was about 229k
tokens. It was about 114k. The request reached the degradation zone
without the compact engaging.

**Root cause:** The char heuristic was the budget driver. The owner
flagged the char-based idea as naive and kept it for lack of
harness experience (handoff document, coupled factor 3). No token
measurement existed anywhere in the budget path. The user knob was
in chars, not tokens.

**Fix:** Corrections entry 57. The budget is in tokens now. The
user knob is `context_budget_tokens`. The full-log decision runs on
the last measured `usage.input_tokens` plus the projected growth of
the appended events. The char heuristic survives only as the
pre-measurement fallback: a fresh session, a server that reports no
usage, and the compact candidates. The default budget is the model
window minus the output reservation. `config.toml` sets the knob to
55000 tokens, the FT-008 degradation-zone bound, and the
chars-per-token rate to the measured 8.

**Verification:** `assemble` on the frozen `sessions/better-ui`
log now compacts against the token budget. The printed request is
366283 chars, under the 440k-char candidate check. It carries the
measured estimate (257454 input tokens on the last measured turn)
instead of the 2x-off char math. The assemble test
`full_log_fits_is_driven_by_measured_tokens` pins the decision:
the char fallback fits a 131k-token budget where the measured
110k-plus-growth estimate fails a 55k-token budget.

**Residual risk:** The compact candidates check against the token
budget times the chars-per-token rate. The rate is a single global
estimate. A content mix that deviates from the measured ratio
shifts the check. Keep the configured rate at or under the
measured ratio: a low rate over-estimates and compacts earlier. A
high rate risks sending a request over the window.

## FT-011 — Shift+A hides the capital A in the TUI composer

**Symptom:** In the TUI composer, Shift+A types nothing. The caret
jumps to the end of the current line instead. In insert mode the
jump keeps insert mode. In replace mode overtyping continues at
the line end. The user cannot type a capital A at the caret in
either typing mode.

**Root cause:** A host extension in `bin/tui/src/vim_editor.rs`.
The commit that shipped the vim-modal input (90e3bb9) added a
`Key::Char('A')` arm to `insert_press` and `replace_press`. It
moved the caret to the line end instead of the char path. The
reference pi-vim base editor passes every char through. The port
made the capital A unreachable in the two typing modes. The port
commit 7b1367f named the jump a documented extension, not a
defect.

**Fix:** Removed the `Key::Char('A')` arms from `insert_press` and
`replace_press` (`bin/tui/src/vim_editor.rs`, commit ef55518).
Shift+A now falls into the generic `Key::Char(c)` arm and types
`A` at the caret, like the reference base editor. The insert and
replace doc notes dropped the extension text.

**Verification:** Regression tests `shift_a_types_uppercase_a_in_insert_mode`,
`shift_a_at_line_end_appends_the_char`,
`shift_a_types_uppercase_a_in_replace_mode`, and
`shift_a_types_uppercase_a_in_the_composer` pin the behavior.
All 269 tui tests pass.

## FT-012 — `q` in a typing mode blocks the letter and quits the
TUI mid-draft

**Symptom:** In the composer's insert mode, `q` types nothing. The
status row flashes `press q again to quit`. A second `q` inside
3 s quits the TUI and drops the half-typed draft. The same block
hits the search command line (`/q...` cannot be typed) and the
new-session name input.

**Root cause:** `key_input` (`bin/tui/src/main.rs`) maps
`Char('q')` to `Key::Quit` before any character path. `App::press`
(`bin/tui/src/app.rs`) handled `Key::Quit` at the app level, before
the editor saw the key, in every mode. The two-step quit arm
preempted the editor's character input. The Shift+A defect (FT-011)
was the same class: a key intercepted above the editor that a typed
char never reaches.

**Fix:** The quit gate (the pi Ctrl-d rule, docs/tui.md section 7):
`q` and `Ctrl+Q` arm and fire only in normal mode with an empty
draft. In every other state the key is plain text: it types into
the composer (insert/replace), the search box (command line), or
the name input. Normal mode with a non-empty draft shows the hint
`clear the draft, then q q quits` and leaves the text intact.

**Verification:** Regression tests in `bin/tui/src/app.rs`:
`q_types_a_char_in_insert_mode`, `q_types_a_char_in_replace_mode`,
`q_types_into_the_search_command_line`,
`q_types_into_the_name_input`, and
`q_hints_the_gate_with_a_nonempty_draft`. The existing
`quit_needs_two_q_within_the_window` and
`expired_arm_requires_a_fresh_q` now Esc to normal mode first. All
314 tui tests pass.

**Open item:** Two other keys preempt the editor under a condition,
by documented design (docs/tui.md section 7): `y` / `n` / `e`
answer the oldest pending `approval_request`, and `h` resumes the
pending handoff. While those conditions hold, the three letters
(and `h`) do not type in a typing mode. Left as is: the block is
the feature, not a defect.

## FT-013 — The `bash` tool binary shadows the system shell and
deads every bash-script extension

**Symptom:** The `statusline`, `frame`, and `notify` extensions die
within seconds of TUI start. The extension log (the `TUI_EXT_LOG`
seam) records `spawn`, `exit=2`, three respawns at the 1 s/2 s/4 s
backoff, then `dead (budget spent)` for each. The `mermaid`
extension (a native binary) stays alive. Observed 2026-09-04 in a
shell where the direnv `.envrc` was active.

**Root cause:** FT-007's `.envrc` puts `$PWD/target/debug` on
`PATH` so `tui` and `harness` resolve. The same directory holds the
harness tool binary `bash` (built by `tools/bash` into
target/debug). In that shell, `bash` resolves to the Rust tool,
not the system shell. The extension host spawns each
bash-script extension via `execvp` of the manifest command with
the manifest args (`bash statusline.sh`). The Rust tool rejects
`statusline.sh` as an unexpected argument and exits 2. The host
respawns with the same result and marks the extension dead after
the budget. The `.envrc` itself is sound; it exposed a latent
name collision.

**Fix:** Renamed the tool binary from `bash` to `harness-bash`.
The agent-facing tool name stays `bash` (the `tools/bash`
directory and its dispatch name are unchanged).
`tools/bash/Cargo.toml` builds `[[bin]] name = "harness-bash"`;
`tools/bash/tool.toml` sets `command = "harness-bash"`. `route`
(`bin/route/src/main.rs`) resolves the tool binary by the manifest
`command` name: it probes the build output next to the route
binary, then the tool's `bin/` copy, then a bare `PATH` lookup,
regardless of whether the binary name equals the tool directory
name. `scripts/tool-conformance.sh` and `docs/bash-tool.md`
reference the new name. `target/debug` no longer contains a file
named `bash`, so the `.envrc` `PATH` entry no longer shadows the
system shell and the extension host's `execvp("bash")` reaches the
real shell.

**Verification:** With `target/debug` prepended to `PATH` (the
`.envrc` condition), `bash` resolves to the system shell and
`harness-bash` resolves to the tool binary. `cargo test --workspace`
passes (497 tests). `scripts/tui-pty-smoke.py` under the same
prepended `PATH` passes all 15 cases, including
`ext-statusline-real`, `ext-statusline-repo`, and
`ext-statusline-slowgit`.

**Residual risk:** Any future tool binary named after a system
utility shadows that utility for every process that inherits the
`.envrc` `PATH`. The current tool set (`edit`, `list`, `read`,
`write`, `log`, `model`, `parse`, `route`, `user`, `assemble`,
`claim`, `compact`) collides with none of the utilities the
reference extensions invoke (`bash`, `jq`, `git`, `awk`, `sed`,
`mktemp`). Re-check at the next tool addition.

## FT-014 — Startup thinking-level restore: repeated false "fixed" claims

**Symptom:** During the TUI command-palette work the startup
thinking-level restore was reported as fixed multiple times. After
each report the user re-ran the TUI with `config-low.toml` and
`config.toml` and saw the same wrong result: the palette showed
`* medium` and the input border kept the stale level. The user
flagged the pattern directly: "things like 'you claimed fix, I
found not yet' have repeated multiple times."

**Root cause:** Each fix was verified by source inspection and unit
tests, not by running the real binary against the real config files.
The first fix reordered the config-read block to run after
`set_active`. That ordering was correct but not sufficient:
`resolve_reasoning_effort` still used
`text.parse::<toml::Value>()`. On the pinned toml crate (`toml =
"1.1.4"`) that call fails on the real config text and the
`unwrap_or_else` fallback returns an empty table. The function then
returns the default `"medium"` for both config files. The unit tests
passed because they tested the reordering, not the parse path. The
defect stayed live until the parse switched to `toml::from_str`.

**Fix:** `resolve_reasoning_effort` (`bin/tui/src/main.rs`) now
parses with `toml::from_str`. The startup block sets `effort_current`
and the thinking level after `set_active`. A one-line stderr trace
(`[tui] startup: model=... effort=... level=...`) prints before the
TUI takes over, so the resolved value is visible without opening the
palette. The `SetEffort` and `CycleEffort` handlers keep the border
in sync through `set_thinking_level`.

**Verification:** A PTY test against the release binary: `config-low.toml`
resolves `effort=low level=1` (Border1) and `config.toml` resolves
`effort=xhigh level=4` (Border4). A session whose log already holds
a stale `model_thinking=4` still shows the config value after
startup. The config wins. All 544 tui tests pass.

**Discipline:** A fix is not done until the real binary, run against
the real input the user runs, shows the user-visible result. Source
order and unit tests are necessary but not sufficient. For a bug
where a helper computes a value and the UI displays it, the
verification is the displayed value in the running TUI, not the
shape of the code.

