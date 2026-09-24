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

**Corrigendum (2026-09-12, correction 64):** The drop-all design
above is reversed. In the `tool-display-external-extension`
session the model re-sent an unknown-parameter `bash` call
(`timeout` instead of `timeout_secs`) ~12 times in a row without
ever reacting to the callout — because correction 60 dropped the
callout pair from every request, the model never saw it. The
"self-priming" premise (the model re-emits the failed call *after*
seeing the failure) is self-contradictory when the failure is
dropped from every request. The operator decided that no tool
result, especially a failure, may be hidden from the model: the
model adjusts its behaviour until the call succeeds. `assemble`
no longer drops any pair; stale failures are managed by
compaction summarizing the old region. The A/B trade-off noted in
the correction-60 entry (failure history can prime the next call
on this NVFP4 model) is accepted knowingly; re-run the A/B if the
active model changes.

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

## FT-015 — All `ui_extensions/` extensions dead: PATH shadowing and unresolved relative command

**Symptom:** Every extension in the global `ui_extensions/` layer
shows "restart budget exhausted" in the TUI status row. No status
footer, no frame border coloring, no mermaid transform, no bell
notifications. The TUI itself still renders the transcript and input
box.

**Root cause:** Two compounding issues.

1. **Stale `target/release/bash` binary (FT-013 residual).**
   Before the FT-013 rename, the harness tool crate built a binary
   named `bash` into `target/release/`. The rename to
   `harness-bash` stopped future builds from producing `bash`, but
   the old binary was never cleaned up. The repo `.envrc` prepends
   `target/release` to `PATH` for direnv shells. `execvp("bash", …)`
   therefore found the stale harness tool instead of the system
   shell. The harness tool rejected the script argument
   (`statusline.sh`) and exited non-zero. All three bash-script
   extensions (statusline, frame, notify) hit the 3-restart budget
   and went dead.

2. **Bare-name command in the mermaid manifest.** The `mermaid`
   entry declared `command = "mermaid-ext"`, a bare name resolved on
   `PATH`. Without the ext `target/debug` dirs on `PATH` (the
   `.envrc` PATH entries are guarded by `[ -d … ]` and silently
   degrade when the build is absent), discovery refused the whole
   global layer with `command 'mermaid-ext' not found`, killing all
   four extensions at scan time.

**Fix:**

- `bin/tui/src/ext.rs`: Replaced `command_exists` (PATH-only check)
  with `resolve_command(dir, command)` that resolves relative paths
  against the entry directory. `Manifest` gains a
  `command_path: PathBuf` field (the resolved absolute path) that
  `build_argv` passes to `execv`. A bare name still resolves on
  `PATH`; a relative path resolves against the manifest directory;
  an absolute path runs as-is.
- `ui_extensions/mermaid/ext.toml`: Changed
  `command = "mermaid-ext"` to
  `command = "target/debug/mermaid-ext"`. The binary now resolves
  relative to its own entry directory. No `PATH` export is needed
  for the global layer.
- Deleted the stale `target/release/bash` binary. The current
  `tools/bash/Cargo.toml` builds `harness-bash`; the old `bash`
  artifact was a build-time leftover from before the FT-013 rename.
- `.envrc`: Removed the `ui_extensions/mermaid/target/debug` PATH
  export (no longer needed). Kept the `ext-rs` exports, which still
  use bare command names.

**Verification:** A PTY run of the release TUI on session
`port-goal-mode` with a clean `PATH` (no ext `target/debug` dirs)
spawns all four extensions and all four stay alive for the full
session. The `TUI_EXT_LOG` shows four `spawn` lines and zero
`death` or `respawn` lines. The `tui-pty-smoke.py` suite passes
all extension cases. 544 TUI unit tests pass.

**Residual risk:** The `ext-rs/` layer still uses bare command
names (`statusline-ext`, `tool_result-ext`, `notify-ext`) that
resolve on `PATH`. A user who activates that layer via
`[tui] ext_dirs` must put those dirs on `PATH` (via
`scripts/ext-env.sh` or the `.envrc`). A future pass could convert
those manifests to relative paths as well.

## FT-016 — Ext-host stderr trace pollutes TUI rendering

**Symptom:** While the TUI runs, the extension host writes `eprintln!`
trace lines (`[ext-host] <pid> <msg>`) to stderr. Because the TUI owns
the terminal in alt-screen mode, those raw stderr writes bypass the
renderer and corrupt the displayed frame.

**Root cause:** When the user requested that TUI extensions log their
tracing, the agent added an unconditional `eprintln!` to `ext_log`
(`bin/tui/src/ext.rs`). The original comment assumed the alt-screen
does not capture stderr, so the lines would only appear in scrollback
after exit. In practice the terminal emulator merges both streams into
the visible viewport, so the trace interleaves with the TUI draw.

**Fix:** `ext_log` no longer writes to stderr. It now appends each
`pid ms msg` line to a log file. The path is `TUI_EXT_LOG` when set,
otherwise the default `<XDG_CACHE_HOME|~/.cache>/tui/ext-host.log`.
Write failures are dropped (the trace must not take the UI down).

**Verification:** Unit test `ext_log_writes_to_the_tui_ext_log_file`
in `bin/tui/src/ext.rs` passes. All 547 TUI unit tests pass. The
`TUI_EXT_LOG` override remains available for post-hoc inspection.


## FT-017 — Main-process diagnostic writes pollute the TUI frame

**Symptom:** At TUI startup the line
`[tui] startup: model=... effort=... level=...` printed on the
terminal and corrupted the alt-screen frame. Two more raw
`eprintln!` calls in the post-takeover path (palette-config error,
draw failure, editor failure) had the same defect.

**Root cause:** The startup trace from FT-014 was written with
`eprintln!`. After `TermGuard::init` the TUI owns the terminal.
A raw stderr write bypasses the renderer and lands inside the
alt-screen frame.

**Fix:** A new `tui_log` function in `bin/tui/src/main.rs`
appends one `pid ms msg` line to a log file, never to stderr.
The path is `TUI_LOG` when set, otherwise
`<XDG_CACHE_HOME|~/.cache>/tui/tui.log`. The startup trace, the
palette-config error, the draw failure, and the editor failure all
call `tui_log`. The two pre-takeover CLI errors (config load,
ext discovery) keep `eprintln!` because the terminal is not yet
owned. A guardrail test scans `main.rs` after the
`TermGuard::init` call and fails if any `eprintln!` reappears.

**Verification:** Unit tests `tui_log_writes_to_the_tui_log_file`
and `no_stderr_writes_after_the_terminal_takeover` in
`bin/tui/src/main.rs` pass. All 549 TUI unit tests pass.
Restart the TUI to load the rebuilt binary.

## FT-018 — TUI process leaks memory when the loop is not running

**Symptom:** The TUI process RSS hit 47 GB within minutes.
The session had no running loop.
The tailer thread holds 99.9% CPU. The inotify thread holds
78.4% CPU. The pmap output shows about 350 uniform 128 MB
anonymous regions.

**Root cause:** Two defects compound in the tailer wake path
(`bin/tui/src/port_file.rs`).

1. **Raw inotify causes a busy loop.** The tailer watches the log
   file and its parent directory via raw `notify`. The `notify`
   inotify backend pushes each event into an unbounded
   `std::sync::mpsc` channel. The TUI writes `tui-trace.jsonl`
   and `goal.json` into the same session directory. Every write
   fires a directory-level inotify event. The tailer's
   `recv_timeout` returns at once because a new event already sits
   in the queue. The loop never sleeps. The tailer spins at 99.9%
   CPU. Each call runs `read_tail` on a file with no new data.

2. **The carry buffer triggers wasted work.** When `want == 0`
   but `carry` holds a partial line, `read_tail` falls through.
   It clones `carry`, builds `candidate`, scans for newlines,
   finds none, and reassigns `carry`. Each iteration allocates two
   copies of the carry buffer. The busy loop multiplies this cost.
   glibc fragments the heap into 128 MB arena regions.

**Fix:**

- `bin/tui/Cargo.toml`: Add `notify-debouncer-mini = "0.7"`.
- `bin/tui/src/port_file.rs`:
  - Replace `notify::recommended_watcher` with
    `notify_debouncer_mini::new_debouncer`. The debouncer collapses
    all inotify events in a 50 ms window into one delivery.
    The tailer no longer sees raw individual events.
  - `register_watches` now takes `&mut dyn Watcher` instead of
    `&mut RecommendedWatcher`.
  - The tailer loop drops the manual drain and sleep hack. One
    `recv_timeout` on the debounced channel is enough.
  - Tighten the `read_tail` early return. A partial tail alone
    cannot make progress, so the function returns `Ok(())` without
    cloning `carry` when no complete line is held.

**Verification:** Build the release binary. Run
`tui file-picker-ignored-files` against a session with no running loop. RSS
holds at 22 MB with 62 threads and 0.0% CPU. No 128 MB
anonymous region appears in pmap. All 576 TUI tests pass.

## FT-019 — `goal_complete` / `goal_blocked` fail to spawn: missing
binary path in the error and a resolver blind to in-tree builds

**Symptom:** The `rushi-tui/sessions/select-and-yank` log holds
repeated `goal_complete` and `goal_blocked` tool results with the text
"Failed to spawn tool: No such file or directory (os error 2)". The
`[paths] extra_tools_roots = ["../rushi-exts/goal-tools/"]` line in
`config.toml` looked wrong. The model called the tools and they never
ran.

**Root cause:** Two separate issues. First, the resolver. The goal
tools are standalone cargo packages under `rushi-exts/goal-tools/`.
Each `tool.toml` names its binary by a bare command name. The old
`scan_tool_root` resolver probed only the route binary's own directory
and the tool's `bin/` copy. The goal-tools have no `bin/` copy. Their
binaries live in `target/release/<name>` and `target/debug/<name>`.
So the resolver fell back to a bare `PATH` lookup. Second, the PATH
precondition. Those build dirs reach `PATH` only through the
`rushi-exts` `.envrc` and `ext-env.sh` export, guarded by a `[ -d … ]`
test. The select-and-yank shell had no such export. The `route`
`Command::new` PATH lookup failed. Both facts combined: the config
discovered the manifest and registered the tool name, but the spawn
still failed. The error text held only the OS error. It named no
binary, so the user could not tell what path was missing.

**Fix:** Two changes in `bin/route/src/main.rs`.

1. **Resolver:** `scan_tool_root` now probes the tool package's own
   cargo output after the two old candidates:
   `<tool>/target/release/<name>` and
   `<tool>/target/debug/<name>`. It canonicalizes the winning path to
   an absolute form, so the tool subprocess `CWD` cannot break it.
   The bare `PATH` fallback stays last.

2. **Error text:** the spawn-failure branch now names the tool and the
   expected binary path. The new text is
   `Failed to spawn tool '<name>': expected binary '<path>' (<io
   error>)`.

**Verification:** `cargo build` and `cargo build --release` pass for
`route`. `cargo test -p route` passes all 16 tests, including the two
new ones: `scan_tool_root_resolves_binary_from_cargo_target_release`
and `scan_tool_root_falls_back_to_bare_command_when_no_candidate_exists`.
An end-to-end probe of the rebuilt release `route` against the real
`rushi-exts/goal-tools` root now runs `goal_complete` in-tree. It
returns the domain error "HARNESS_SESSION_DIR is not set", not the
`No such file or directory` text. A synthetic missing-binary probe
prints the new error with the expected path: `Failed to spawn tool
'fake_tool': expected binary 'definitely-missing-bin' (No such file or
directory (os error 2))`.

**Residual risk:** A tool with no `bin/` copy, no in-tree build, and
no `PATH` entry still fails to spawn. The improved error text now
names the tool and the expected binary path, so the user can add the
dir to `PATH` (the `rushi-exts` `.envrc` / `ext-env.sh` export) or
build the package in place.

## FT-020 — Hard-trim backstop invalidates prompt cache on long contexts

**Symptom:** The `hard_trim` backstop in `bin/assemble` drops the oldest
step groups whenever the request estimate exceeds the compact trigger
level. In the `select-and-yank-impl` session, 52 trim events fired on a
~240k-token context. Each trim rewrites the request prefix, invalidates
the server prompt cache, and forces a full re-prefill.

**Root cause:** The hard-trim target equals the compact trigger level
(e.g. 212,992 or 245,760 tokens), which sits below the 262,144-token
model window. The overflow compact in the agent loop already recovers
true overflow unconditionally. The trim therefore fires preemptively
and rewrites the prefix without preventing a failure that would not
otherwise occur.

**Fix:** Removed the group-drop branch in `bin/assemble/src/main.rs`.
The `context_exhausted` fallback still fires when the framing alone
exceeds the target. The e2e scenario `last-resort` became `no-trim`
and now runs with `compact_enabled=true`. The design doc section 9.8
and property P10 are marked disabled.

**Verification:** `cargo build` clean. `cargo test --workspace` passes.
`scripts/compact-e2e.sh` passes 70 assertions including the new
`no-trim` scenario.

**Follow-up:** The `compact_trigger_base` configuration interface was
removed. The trigger now always uses the `context_budget` base
(`context_budget_tokens - compact_reserve_tokens`), with no user-
configurable knob. The `TriggerBase` enum was deleted from
`crates/rushi/src/compact_math.rs`. The `pi-parity-cold` e2e
scenario (which tested the removed `input_budget` base) was
removed. All e2e seed sizes were re-calibrated to the new trigger
level of 7500 tokens in the test config.

## FT-021 — Boundary-path estimate misses the kept region

**Symptom:** After a `compaction_summary` marker is written, the
next `estimate_context` call returns a value that is too low by the
size of the kept region. The proactive threshold check and the
post-failure rescue check both undercount, so compact never fires
when the true context is above the trigger.

**Root cause:** `estimate_from_events` (in
`crates/rushi/src/compact_math.rs`) identified the boundary by log
position. It then sliced `active_events[b+1..]`, which is only the
events *after* the marker in log order. The kept events
(`seq >= first_kept_seq`) sit *before* the marker in log order
because the marker is appended at the end of the log. The kept
region was therefore excluded from the estimate.

**Fix:** `estimate_from_events` now carries the 1-based positional
index alongside each active event. The boundary path filters by
`first_kept_seq` instead of log-order position: it keeps every
active event whose position is `>= first_kept_seq`, covering both
the original kept events and any events appended after the marker.

**Verification:** Unit tests `boundary_path_applies_margin_no_boundary_does_not`
and `estimate_excludes_masked_branch_events` in
`crates/rushi/src/compact_math.rs`. The `select-and-yank-impl`
session now estimates 303 563 tokens (above the 245 760 trigger)
instead of the old ~281 000 that missed the kept region.

## FT-022 — Compact binary trigger mismatch causes silent noop

**Symptom:** The loop\'s proactive threshold check fires
(uncapped estimate > trigger), but the compact binary noops with
"the trigger is cold" and writes no `compaction_summary`. The model
call then proceeds with an oversized context and the provider
rejects it. The session log shows `hook.compact.before = proceed`
but no `compaction_summary` marker.

**Root cause:** The compact binary computed its own trigger reading
with a capped full-form estimate (`text: Some(200)` chars cap on
assistant text and tool-call args, `chars_per_token = 4`). For
code-heavy sessions the capped estimate is far lower than the
uncapped one. The loop used `estimate_from_events` (uncapped, with
25% margin), the binary used a capped local estimate. The two
disagreed: the loop saw 303 563 > 245 760, the binary saw
216 493 < 245 760.

**Fix:** The compact binary now calls `estimate_from_events` (the
same function the loop uses) for its trigger reading. Both paths
share one estimator: uncapped, calibrated `chars_per_token`,
rewind-aware, with the 25% margin on the boundary path. The
capped estimate survives only inside the `find_cut` walk and the
summary-input drop search, where capping assistant text is
intentional.

**Post-failure rescue.** A second safety net was added to
`step.rs`: when model API retries are exhausted and the error
detail does not match `is_overflow`, the loop re-runs
`estimate_context`. If the estimate exceeds the trigger, it fires
a compact (Overflow, then LastResort) and resets the retry counter
instead of writing a terminal error. This catches the case where a
provider rejects an oversized request with an opaque error that
the overflow classifier does not recognise.

**Verification:** All 197 unit tests, 86 compact e2e assertions
(including the new `silent-failure-rescue` scenario), and 19
rewind e2e assertions pass. The live `select-and-yank-impl`
session now compacts on the next step: `compaction_summary` is
written with `tokens_before = 303563`, `tokens_after = 21795`,
`version = 2`, `parent_version = 1`, `diverge_seq = 771`.

## FT-023 — Estimator undercount and silent SSE overflow prevent
compaction

**Symptom:** The `select-and-yank-impl` session logged two terminal
errors (`model API call failed after retries: `, empty detail)
at 13:34 and 13:35 UTC. The SGLang server returned HTTP 200 with
an SSE `response.failed` event whose `response.error` field was
`null`. The harness never compacted; the session died.

**Root cause:** Two compounding defects.

1. **Estimator undercount (Bug A).** The boundary path of
   `estimate_from_events` (`crates/rushi/src/compact_math.rs`)
   computed a pure chars/cpts estimate over the kept region and
   multiplied by 5/4. For code-heavy sessions the real
   chars-per-token ratio is ~2.6, not 4, so the estimate
   undercounted by ~18 %. On the affected session the estimate was
   213 260 vs. a trigger of 212 992 — only 268 tokens above. The
   post-failure rescue check (`est > trigger`) sat on the knife's
   edge and could easily miss. The proactive threshold check
   likewise fired too late, after the context had already grown
   past the model's 262 144-token window.

2. **Silent SSE overflow (Bug B).** SGLang's `/v1/responses`
   endpoint (see sglang#12081) reports a context overflow as an
   HTTP-200 SSE stream ending in `response.failed` with
   `response.error = null`. The actual ValueError text appears
   only in the server's stderr, not in the client-visible payload.
   The model binary's `SseParser` fell through to the `_ => {}`
   catch-all and dropped the event silently. The harness received
   `stop_reason = "error"` with an empty `detail`, so
   `is_overflow("")` returned false and the reactive overflow
   recovery never fired.

**Fix:** Three changes.

1. **`crates/rushi/src/compact_math.rs`** — the boundary path of
   `estimate_from_events` now anchors on the last measured
   `assistant_message` usage (input + output tokens) within the
   kept region, plus a chars/cpts estimate of events appended after
   that reading. The 25 %-margin chars/cpts fallback is used only
   when no measured reading exists in the kept region. This makes
   the estimate track the provider's own count (261 477 on the
   affected session, well above the 212 992 trigger).

2. **`bin/model/src/main.rs`** — `SseParser` now captures the
   `error` field from a bare SSE `data: {"error":{...}}` event
   (no `type` field) and from `response.failed` events whose
   `response.error` is a non-null string or object. The captured
   text is surfaced in `finalize` as the `detail` field so the
   overflow classifier in `step.rs` can match it.

3. **`bin/rushi/src/step.rs`** — a new *silent-overflow early
   detection* block runs before the transport-retry loop. When the
   model error detail is empty, the context estimate exceeds the
   trigger, and no prior overflow recovery has occurred, the loop
   fires an immediate compact (Overflow, falling back to
   LastResort) instead of burning three futile retries.

**Verification:** `cargo test` passes all 204 tests (including new
`boundary_path_anchors_on_measured_reading_in_kept_region` and
`estimate_excludes_masked_branch_events`). `est_probe` on the
`select-and-yank-impl` log reports 261 477 tokens (was 213 260).
`compact --reason overflow` on a sandbox copy of the session
compacts successfully: `tokens_before = 261477`,
`tokens_after = 22793`, `version = 5`.

## FT-024 — The length-stop e2e assertion survived correction 64

**Symptom:** The `length-stop` scenario in `scripts/compact-e2e.sh`
expected the re-included request to drop the truncated pair. It
kept the pair.

**Root cause:** The assertion dates to correction 60, when
`bin/assemble` dropped stale pairs. Correction 64 removed that
drop. The truncation notice now reaches the model like any other
result. The e2e line was never updated to match.

**Fix:** Flipped the `length-stop` assertion. The request keeps the
truncation notice, so the expected count is 1, not 0. The label
now cites correction 64.

**Verification:** `length-stop` passes 7 of 7. The full compact-e2e
suite passes 96 of 96.

## FT-025 — Noop silent-overflow compact escalated to last-resort

**Symptom:** The `silent-failure-rescue` scenario failed after the
proactive compact. Three empty-detail errors should have burned the
retry budget and let the fourth call recover. Instead a terminal
error fired on the second error. The counts wanted 0 errors and 4
assistant messages, got 1 and 3.

**Root cause:** The FT-023 early-detection block treats any
non-`Compacted` compact as failed. It escalates to `last_resort`
and runs a forced last-resort compact.

Here the kept region already fits the keep budget, so the compact
returns `Noop`. The escalated `last_resort` flag then stopped the
loop one call early. The FT-022 retry-exhaustion rescue had the
same Noop/Failed conflation.

**Fix:** `step.rs` now branches on `CompactOutcome` in the
early-detection and retry-exhaustion blocks. A `Noop` compact
resets the retry counter and continues, leaving `last_resort`
untouched. A genuine unresolvable overflow still stops when
retries exhaust. Only a `Failed` summary call escalates to the
forced last-resort compact.

**Verification:** `silent-failure-rescue` passes 5 of 5. The full
compact-e2e suite passes 96 of 96. `cargo test` and clippy are
clean, and the other e2e suites pass.


## FT-026 — `Model called unknown tool .` aborts the loop

**Symptom:** The loop stopped with the terminal error event
`Model called unknown tool .` The tool name after "tool" is
empty. `bin/parse` produced the event, and the loop never
routed the call.

**Investigation (2026-09-18):** `bin/parse` validates every
tool call name against the native and extension tool
manifests. An unknown or empty name emits the
`Model called unknown tool {name}.` error event and exits 2.
`bin/rushi` appends that event and skips `route`. The loop
ends. `route` already has a recoverable `not_run` path for
unknown tools (`bin/route/src/main.rs:317`). Parse blocks it
first, so that path never fires for unknown names.

The empty name likely comes from the local model emitting a
`function_call` with a blank `name`. The responses-API
conversion in `bin/model` keeps the first name it sees per
call id (`fc_names` `or_insert`, `bin/model/src/main.rs`).
A name missing at `output_item.added` stays blank. No session
log from the failing run was found on this machine.

**Investigation verified against current code:**

- `bin/parse/src/main.rs:250-254` — the unknown-name check
  pushes the `error` event and returns exit 2. The
  `assistant_message` and `tool_call` lines are never
  emitted. The recorded symptom is the empty case.
- The name is read with `.unwrap_or("unknown")`. A missing
  key prints `unknown`. A present empty string prints
  nothing.
- `bin/model/src/main.rs:678-683, 757, 908` — a
  `function_call` item event without `name` stores `""`.
  `fc_names` inserts it via `or_insert`. Later reads fall
  back to `unwrap_or_default()`. So the emitted `tool_call`
  carries `name: ""`.
- `bin/rushi/src/step.rs:369-400` — `route` runs only when
  parse exits 1. On exit 2 the `error` event is appended and
  routing is skipped.
- `bin/claim/src/main.rs:228-235` — an `error` event
  settles the session to `idle`. The `run` loop then exits
  (`bin/rushi/src/run_loop.rs:80`). This is the "loop
  ended" half of the symptom.
- `bin/route/src/main.rs:314-321` — the recoverable path
  already exists. An unknown name yields
  `Outcome::not_run("Unknown tool {tc_name}.")`. It is
  emitted as a `tool_result` with `is_error: true`.
- A `tool_result` maps to `awaiting_model`
  (`bin/claim/src/main.rs:171-172`). The pending call
  resolves by id (`:305-321`). The next step re-invokes the
  model with the failure in context. This is the
  self-correction loop that correction 64 needs.

**Proposed solution (implemented as correction 65):**

Design: `route` is the single source of truth for tool-name
validity. Parse stops hard-failing on unknown or blank
names. It forwards the call instead. `route`'s existing
`not_run` path reports the failure to the model. The model
corrects the call on the next turn.

1. `bin/parse/src/main.rs:250-254` — the
   `if !valid_tools.contains(tc_name)` check and the set it
   runs against stay (see the operator decision below). Only
   the exit-2 hard-fail is dropped. For a non-`length`
   stop, unknown or blank names reach
   `emit_assistant_and_tool_calls`.
2. One `assistant_message` is emitted. One `tool_call` is
   emitted per call. The exit code is 1.
3. The malformed-arguments hard-fail is unchanged
   (`:233-248`, contract
   `docs/loop-and-edit-implementation.md:247`). It is
   tracked separately as FT-027.
4. `bin/route/src/main.rs:314-321` — make the not-run
   message actionable. Use the style of `ArgsIssue::Unknown`
   (`:341-351`).
5. Name the tool. State that no tool ran. List the available
   tool names from the loaded manifests. Tell the model to
   re-issue the call.
6. Distinguish the blank case. Say "the model emitted a tool
   call with an empty name".
7. `docs/loop-and-edit-implementation.md:248` — update the
   parse contract line. Unknown names are not a parse
   failure.
8. `route` rejects them with a recoverable `tool_result`
   (`is_error: true`). Parse-level hard failures remain.
   They are malformed JSON input, non-object arguments, and
   `length`-stop truncation.
9. Add a corrections entry 65 to
   `docs/loop-and-edit-implementation-corrections.md` when
   implemented. Mark the applied deviation from entry 35.
   Flip this entry to Fixed with its verification.

Tests to add:

- parse: unknown name on a non-length stop exits 1. The
  `tool_call` is emitted. No `error` event. No "unknown
  tool" text.
- A blank name emits the `tool_call` with `name: ""`. The
  existing `length_stop_unknown_tool_is_not_validated` and
  malformed-args tests still hold.
- route: the unknown-tool `not_run` message names the tool.
  It lists the available tools. The blank-name message is
  explicit.
- e2e: use the stub-model pattern from
  `scripts/compact-e2e.sh`. The session log gains a
  `tool_result` with `is_error`. The text says "Unknown
  tool". No terminal `error` event appears.
- `claim` returns `awaiting_model`. The loop continues to
  the next model call. No script pins the old exit-2
  behavior.

Operator decisions (2026-09-15):

- Keep `valid_tools`. The set guarantees that no tool other
  than the ones named in the passed-in config can be called.
  `route`'s manifest scan enforces the dispatch side. The
  check and the set stay in parse. Only the exit-2
  consequence is dropped.
- Scope stays unknown names. The malformed-arguments
  hard-fail is a separate structural hole. It is tracked as
  FT-027 and stays out of this fix.
- `bin/model` is unchanged. The blank-name pass-through
  (`fc_names` `or_insert`) stays. The recovery mechanism is
  `route`'s `not_run` message, not a model-stage guard.

**Fix:** `parse` no longer hard-fails on an unknown or blank
tool name. It forwards the call. One `assistant_message` and
one `tool_call` are emitted, exit 1. The `valid_tools` set
stays in `parse`. Only the exit-2 branch was dropped.

`route` now owns the reject. Its not-run `tool_result` names
the tool, says no tool ran, lists the available tool names,
and tells the model to re-issue the call. The blank case is
called out explicitly. The malformed-arguments hard-fail is
unchanged (FT-027).

**Status:** Fixed. Implemented as correction 65 on 2026-09-18.

**Verification:** The `parse` test
`unknown_tool_name_forwards_to_route` pins exit 1 with the
`tool_call` emitted, no `error` event, and no unknown-tool
text. `blank_tool_name_forwards_with_empty_name` pins the
blank forward. The `route` tests pin the actionable not-run
message.

The compact e2e `unknown-tool` scenario drives a stub model
that emits an unknown tool call. The log gains a not-run
`tool_result` with `Unknown tool` text. No terminal `error`
event appears. `claim` returns `awaiting_model`. The loop
continues to the next model call. All workspace unit tests
pass and the full compact-e2e suite passes.

## FT-027 — The parse hard-fail on malformed arguments blocks
route's recoverable hints

**Symptom:** Not directly observed in a tracked session. This
is a structural hole in the FT-008 / FT-026 family. A call
whose `arguments` are not a JSON object kills the loop
instead of giving the model a correction. Parse emits a
terminal `error` event and exits 2. No `tool_result` is
written. The loop ends before the model can see a resend
hint.

The one observed incident (a 4096-token output cap cut
tool-call arguments mid-JSON, recorded in `docs/itches.md`
under "Model-settings defaults drift") no longer reaches
this gate. The model stage now marks truncated arguments as
a `length` stop. Parse skips validation on `length` stops.
The residual hole is a non-truncated, non-object
`arguments` value. That is broken or missing JSON at a
normal stop.

**Root cause:** The parse stage hard-fails with exit 2 on a
bad `arguments` value. The check sits in the `process`
validation loop of `bin/parse/src/main.rs`. It fires when a
non-`length` stop leaves the `arguments` key missing. It
fires for an `arguments` string that does not parse as
JSON. It fires for a value that is not a JSON object. The
error message names the call id.

`bin/rushi` (`bin/rushi/src/step.rs:369-400`) skips
`route` when parse exits 2. `bin/claim` settles the
session to `idle` on the `error` event. The `run` loop
then exits. `route` already owns recoverable paths for
every shape. The invalid-JSON branch
(`bin/route/src/main.rs:296-306`) tells the model to resend
with a JSON object. The `validate_args` block turns a
non-object value into an `ArgsIssue` hint with the
correction 59 resend hint.

Missing `arguments` become an empty object. Tools that need
fields then get a `MissingRequired` hint. The parse gate
fires first, so none of these hints reach the model. The
contract is `docs/loop-and-edit-implementation.md:247`.

**Fix (proposed, not implemented):** Parse stops
hard-failing on missing or non-object `arguments`. It
forwards the call like any other. It emits one
`assistant_message` and one `tool_call` per call. The exit
code is 1. `route`'s existing `not_run` messages carry the
recovery.

Correction 64 keeps failure pairs in the model context.
The model fixes the call on the next turn. The `length`-
stop truncation path is unchanged. The `valid_tools` name
machinery stays in parse per the FT-026 operator
decision. `bin/model` is unchanged.

**Status:** Fixed. Implemented as correction 66 on
2026-09-24. `parse` now collects the malformed calls of a
turn instead of exiting 2 on the first. It logs the
`assistant_message` with the bad arguments defaulted. It
routes the healthy calls. Each malformed call settles to a
not-run error `tool_result` with a re-issue hint. Only a
turn where every call is malformed still exits 2.

**Deviation from the proposal:** The proposal forwarded
the bad calls to `route`. The implementation settles them
in `parse` itself. Each bad call gets its own errored
`tool_result` with the re-issue hint. The shape matches
the existing `length`-stop recovery.

**Verification:** Pinned by correction 66. The `parse`
test `malformed_args_single_call_recover_not_terminal`
pins the not-run result and the clean step end.
`malformed_args_mixed_calls_route_the_healthy_ones` pins
the mixed routing. The old `non_length_malformed_args_
still_fail` test was replaced. All 25 parse tests pass.

## FT-028 — The PR #26 regression test fails on the macOS CI

**Symptom:** PR #26 (issue #25) merged with green Linux jobs. The
macOS gate failed exactly on
`symlinked_exe_recovers_store_layout`. Both Linux jobs passed.

**Root cause:** The regression test compared two path strings.
The resolved side passed through `fs::canonicalize`. The
expected side was built from the raw `tempfile::tempdir()`
base. On macOS the temp dir lives under `/var`, which links
to `/private/var`. So the two prefixes differed and the
string comparison failed. On Linux `/tmp` is a real
directory. Both sides matched, so the test passed. The
production `resolved_exe` fix from PR #26 was correct. Only
the test expectation was platform-dependent.

**Fix:** PR #28 canonicalizes the fake package `bin/` base
up front. Both sides of the comparison now share one
canonical form. The regression semantics are unchanged. The
two-level symlink chain must still resolve to the package
root for sibling hook discovery.

**Verification:**
- Reproduced on Linux by pointing `TMPDIR` at a symlinked
  path. The test failed before the fix and passed after it.
- `cargo test --workspace` is green on the new base.
- All three CI jobs passed on PR #28, including
  `macos-14`. The PR merged as `f901b8d`.
