# TUI Proposal — First Stateful Rust Binary

## 1. Why the TUI first

The TUI is the one part of the system that is genuinely **stateful, long-lived, and interactive** — the kind of component bash glue is bad at. The pipeline stages can stay one-shot stdin→stdout filters; the TUI is a different animal: it is a **window onto the session log and a supervisor for the loop**.

Crucially, the TUI is **stateful as a view, not as the source of truth**. It may keep cursor position, scrollback, a draft buffer, and child-process handles in memory. Session state stays in the log. If the TUI crashes, nothing is lost — restart it and it re-renders from the log.

## 2. Coupling contract — what the TUI actually depends on

The TUI must not know how the loop is implemented or how a session is stored. It depends on exactly **one internal abstraction and two external data contracts**:

### 2.1 One internal port: `SessionPort`

All TUI access to sessions goes through a trait. Phase 1 implements it over files; a later phase implements it over a Unix socket to a daemon. The rest of the TUI never sees a path, a script name, or a file.

```rust
#[async_trait]
pub trait SessionPort {
    /// List known session ids.
    async fn list_sessions(&self) -> Vec<SessionId>;

    /// Return all events of a session, oldest first (or a cursor-based page).
    async fn read_events(&self, session: SessionId) -> Vec<Event>;

    /// Append one event to a session's log. Must be atomic.
    async fn append_event(&self, session: SessionId, event: Event) -> Result<(), BusError>;

    /// Start the loop for a session and return a child handle.
    /// The TUI does NOT know what command this runs — it is opaque.
    async fn spawn_loop(&self, session: SessionId) -> Result<Box<dyn LoopHandle>, BusError>;
}
```

Every file path, `turn.sh`, `events.jsonl`, lockfile, and daemon socket lives behind this port. The TUI is broken by a protocol change only if the port is changed; implementation changes below the port are invisible to it.

### 2.2 The event vocabulary (versioned)

The TUI renders **semantic event categories**, not raw protocol details. Every event carries a version marker; the TUI renders known categories and falls back to raw JSON for anything it does not recognize. New event types therefore do not break it.

```jsonl
{"v":1,"type":"user_message","ts":"...","content":"rename the file"}
{"v":1,"type":"assistant_message","ts":"...","content":"I'll use the bash tool."}
{"v":1,"type":"tool_call","ts":"...","id":"call-1","name":"bash","arguments":{"command":"mv a b"}}
{"v":1,"type":"tool_result","ts":"...","id":"call-1","value":{"exit":0},"is_error":false}
{"v":1,"type":"approval_request","ts":"...","id":"appr-1","call_id":"call-1","prompt":"Allow mv a b?"}
{"v":1,"type":"approval","ts":"...","id":"appr-1","decision":"allow"}
{"v":1,"type":"cancel","ts":"...","target":"turn"}
{"v":1,"type":"error","ts":"...","message":"model call failed"}
```

Rendering rules:

- known category → semantic pretty-print
- unknown `type` → render `type` plus pretty-printed JSON, still in order
- unsupported `v` → render raw and show a "log version newer than this TUI" hint

This is the whole trick: **the TUI does not model every event; it models rendering fallbacks.**

### 2.3 The loop command (config, opaque)

The TUI does not contain the string `turn.sh` or `step.sh`. Config supplies the command that runs a session loop:

```toml
[loop]
command = "bash"
args = ["turn.sh"]          # phase 1; later: ["harness", "run"]
arg_style = "append_session" # how the session id is passed
```

`spawn_loop` runs exactly that. The TUI treats it as a black box: start it, stream its stderr to a log pane, stop it on request. What the command does internally — claim/assemble/model/parse/route/tool/log, a Rust binary, a shell pipeline — is irrelevant to the TUI.

## 3. Role

1. **Render the session log** — read events through `SessionPort`, pretty-print known categories, fall back for unknown ones.
2. **Compose and append user events** — `user_message`, `approval`, `cancel`.
3. **Supervise the loop** — start/stop the opaque `loop.command` for the active session.
4. **Handle approvals** — when an `approval_request` event appears, prompt the human and append an `approval` event. No knowledge of how the loop waits for the answer.
5. **Session switcher** — list sessions via `SessionPort`.

## 4. Non-goals

The TUI **never**:

- validates tools
- assembles prompts
- decides policy
- resolves what the agent owes next
- knows what process/script implements the loop
- knows how sessions are stored

It only **renders and appends**. That keeps it an adapter, not the core. If reducer-like logic starts to appear in the TUI, that code belongs in `claim`/`assemble` or the future core.

## 5. Event flow (implementation-agnostic)

```
TUI starts
  └─> SessionPort.list_sessions() -> shows session list
  └─> SessionPort.read_events(s1) -> renders history

User types "rename the file"
  └─> SessionPort.append_event(s1, user_message)
  └─> SessionPort.spawn_loop(s1)        # opaque command from config
  └─> events stream in; TUI renders them as they appear

An approval_request event appears
  └─> TUI renders [y] allow / [n] deny / [e] edit
  └─> User chooses -> SessionPort.append_event(s1, approval)

User presses stop
  └─> TUI signals the loop handle; appends cancel if appropriate
  └─> log remains intact; next start resumes from it
```

No `turn.sh`, no `pending/approval.json`, no `state.json` appears in this flow. Those are phase-1 implementation details below the port.

## 6. UI layout

```
┌ Session: s1 ──────────────────────────── [running] ─┐
│                                                     │
│  user      rename the file                           │
│  assistant I'll use the bash tool.                   │
│  tool:bash mv a b                         [done 0]   │
│  tool:bash → exit 0                                  │
│  assistant Done. Renamed `a` to `b`.                 │
│                                                     │
├─────────────────────────────────────────────────────┤
│ > rename the file                                   │
│ [Ctrl+R run] [Ctrl+E edit] [Ctrl+C stop] [Tab next] │
└─────────────────────────────────────────────────────┘
```

When unconsumed `user_message` events sit in the log, two waiting-
message blocks render between the transcript and the input box
(docs/tui-pending-user-messages.md). The `steer` queue delivers at
the next step of the running loop; the `follow` queue runs as new
turns after the loop would stop. Each block holds one header row
with the count and the delivery hint, then up to three message
preview rows, then a `+N more` row for the rest. A message is
consumed when an `assistant_message` event follows it in the log,
and the `follow` queue clears at every turn boundary:

- steer, loop running: `N message(s) waiting — steering —
  injected at the next step`
- steer, loop stopped: `N message(s) waiting — no loop running —
  waiting for Ctrl+R run`
- follow: `N message(s) waiting — follow-up — run after the loop
  stops`

`Ctrl+F` toggles the composer between the steer and the follow
queue. A sent message lands in the toggled queue: steer messages
wait for the next step, follow messages run as new turns after the
loop stops (the loop side, `scripts/turn.sh` and `scripts/step.sh`,
injects the follow turn at the idle boundary, commit of the
2026-09-03 follow-up stage). The block is computed at draw time
from the active session's events.

The session title holds the loop-phase bit. The loop publishes the
phase as an `ext_status` event with id `loop_phase` (values `wait`
and `tools`; docs/tui-model-wait-indicator.md). The bit is a pure
function of the last marker value and the loop-running bit, so it
survives a TUI restart:

- loop stopped: `[idle]`
- loop running, no marker or an unknown value: `[running]`
- loop running, last value `wait`: `[wait]`
- loop running, last value `tools`: `[tools]`

A working row sits above the input box, below the transcript. It
shows while the loop runs, spinner plus text. When the loop stops,
no row draws and the transcript absorbs the place. The input box
does not move. A statusline extension does not own the row, so
it shows under any statusline:

- `wait` state: `waiting for model · Ns`
- `tools` state: `tools running · Ns`
- `running` state, no marker or unknown value: `Working...`
- `idle`: no row

`N` is whole seconds since the marker, `Mm SSs` at 60 s and up.
The spinner shows one braille frame per redraw, about 100 ms.

## 7. Key bindings

| Key | Action |
|---|---|
| `Enter` | In the search command line: run the search. Otherwise: append the typed text as `user_message`: sends the whole multi-line draft, in any modal state |
| `Ctrl-J` | Insert mode: a hard newline (multi-line draft). Normal mode: the `j` motion |
| `Ctrl+E` | Open `$EDITOR` for long input, then append |
| `Ctrl+R` | In normal mode with pending redo state: redo. Otherwise: `SessionPort::spawn_loop(active_session)`. The persistent `loop.pid` probe blocks the start when a live loop holds the session (FT-003) |
| `Ctrl+C` | With an active session: stop the loop and append a `cancel` event. A local handle stops its group. Without one, the `loop.pid` probe stops the external group (FT-003). Without an active session: the editor's insert-exit key (insert/replace to normal) |
| `Ctrl+U` | In the search command line: clear the input. In the idle composer's insert mode: kill the current line. Otherwise: half-page up in the log |
| `Ctrl+O` | Fold/expand tool result bodies. Collapsed: the preview cap per tool (docs/tui-tool-display-port.md). Expanded: the full body up to the expanded cap. The toggle is global for the whole transcript (the pi `app.tools.expand` keymap) |
| `Ctrl+T` | Collapse/expand the model's thinking block (the pi `app.thinking.toggle` keymap). Collapsed: one label row; expanded: the full reasoning text. Expanded is the default |
| `Ctrl+X` | Show/hide the thinking blocks entirely (the secondary toggle; the pi keymap keeps `Ctrl+X` for message copy, which the harness TUI does not have) |
| `Ctrl+F` | Toggle the composer between the steer queue and the follow queue (the pending message blocks above) |
| `Ctrl+L` | Cycle the active model's `reasoning_effort` through `none, minimal, low, medium, high, xhigh, max`. Writes `[model.<active>]` in `config.toml` in place, comments kept; the input-border color follows the new level |
| `y` / `n` / `e` | Answer the oldest pending `approval_request`: allow / deny / edit-then-allow |
| `h` | One-key handoff resume (correction 57). Only when the log holds a `context_exhausted` marker that seeded a session and no loop runs. Switches to the seeded session and starts its loop. The old session's local loop stops. Without those conditions, `h` stays the editor key |
| `Tab` | Switch session |
| `q` ×2 | Quit the TUI, only in normal mode with an empty draft (the pi Ctrl-d rule, FT-012). In every other state `q` is plain text: it types into the composer, the search box, or the name input. `Ctrl+Q` follows the same gate. Loops keep running as orphans. Only `Ctrl+C` stops a loop |

The input area is a multi-line textarea with native vim modal input
(section 7.1), in a rounded-corner border whose color correlates with
the model thinking level (section 7.2). The frame is customizable by
a `frame` extension (docs/ui-extension.md: the `frame` capability
owns the border, label, and height; the TUI renders the draft and
cursor).

### 7.1 Vim modal input

The draft is a `Vec` of lines plus a modal key state machine
(`vim_editor.rs`). The port follows the pinned `@burneikis/pi-vim`
reference (the flake-pinned rev, plus its `dw`/paste compat fixes):
motions, operators, text objects, registers, search, dot-repeat,
and the mode handlers. The design record is `docs/vim-editor-design.md`.

- **normal**: `h j k l`, `w b e W B E`, `0 $ ^`, `gg G`, `f F t T`
  (with `;` / `,`), `{ }`, `%`, `x X`, `d c y` + motion or text
  object (`dd cc yy >> <<` are linewise; `d$` is `D`, `c$` is `C`,
  `yy` is `Y`), `s` (change one char), `S` (change the line), `p P`
  (with a count; `"reg` selects the register, `A-Z` append), `i a I
  A o O` (counted `O` repeats the line, like vim), `r` (replace one
  character), `R` (overwrite mode), `J`, `~`, `.` (dot repeat), `u`
  (undo), `Ctrl-R` (redo), `v V` (char-wise / line-wise visual).
  `Esc` cancels pending state: the operator, the count, the `g`
  prefix (like vim; the pinned reference leaks the count).
  `dw` / `dW` / `yw` / `cw` on the last word of a line consume to
  the end of the line (the neovim rule; the pinned reference stops
  one char short)
- **insert**: chars insert (`Shift+A` types `A` at the caret, like
  the reference base editor), `Ctrl-J` inserts a hard newline
  (`Enter` sends the draft, so it never reaches the editor),
  `Backspace` joins lines at column 0, `Ctrl+C` / `Esc` return to
  normal (`Esc` steps the caret back one char, the vim rule).
- **replace** (`R` in normal): each typed char overwrites the one
  under the cursor (`Shift+A` types `A` at the caret, like the
  reference base editor); `Backspace` restores the original; `Esc`
  returns to normal
- **visual / visual-line** (`v` / `V`): `d c y p P > < ~ J` act on
  the mark-to-cursor span; after an operator the caret returns to the
  mark; `Esc` leaves visual
- **command line** (`/` or `?` in normal or visual): the pattern
  types into the box title (`/pat█`); `Enter` runs the search and
  returns to the opening mode, `Esc` or a backspace on the empty
  buffer cancels, `Ctrl-U` clears the input. `n` / `N` repeat the
  last search; `*` / `#` search the word under the cursor. The
  prompt wins over a `frame` extension label in that title (the
  host keeps its modal-state render; docs/ui-extension.md section
  10), and the frame label returns when the search ends

Counts prefix operators and motions: `3dd`, `2w`, `3c`. Operator and
motion counts multiply (`2d3w` is six words). The register set holds
named registers (`a-z`), the unnamed `"`, the black hole `_`, and
numbered registers; a delete yanks into them, like vim. `p` inserts
the char-wise register after the cursor char, `P` before it; a
linewise register pastes below / above the cursor line. The editor
starts in insert mode (the composer's typing mode); `Esc` drops to
normal for motions. The mode label shows in the frame title
(`[NORMAL]`, `[INSERT]`, `[d-PENDING]`, ...), mirroring the pi
`formatStatus` output.

The cursor block: in normal, replace, and visual modes the inverted
block covers the char under the caret, so the line renders that char
exactly once (no duplicate to the right). In insert mode the block is
a blank cell at the caret and the char under it stays rendered. In
command-line mode the block sits on the prompt in the box title.

The arrow / home / end / delete keys map onto their vim equivalents in
normal mode: `Down`/`Enter` = `j`, `Up`/`Backspace` = `k`, `Left` =
`h`, `Right` = `l`, `Home` = `0`, `End` = `$`, `Delete` = `x`.

### 7.2 Thinking level

The input-area border color correlates with the active model's
thinking level. The level is published into the log as an
`ext_status` event with id `model_thinking` (the shared-UI-state
channel, docs/ui-extension.md section 5); the TUI reads the latest
value and maps it to a border color:

| Level | Meaning | Border |
|---|---|---|
| 0 | no thinking (default) | gray |
| 1 | low | blue |
| 2 | medium | cyan |
| 3 | high | green |
| 4+ | highest | yellow |

The mapping is host presentation only: the TUI does not decide the
level, it renders whatever the loop or a policy hook published. The
`frame` extension may override the border color; without one, the
host's built-in palette above applies. The loop publishes the
level as `model_thinking` from the resolved `reasoning_effort`
(docs/tui-thinking-level-input-box.md).
The assistant's `reasoning` content renders as a thinking block
above the message body (docs/tui-thinking-block.md): collapsed, a
one-line preview with the expand hint; expanded, the full reasoning
text in the lighter thinking tone. `Ctrl+T` collapses or expands
the block (the pi `app.thinking.toggle` keymap), `Ctrl+X`
shows or hides it. `Ctrl+L` cycles the active model's
`reasoning_effort` in `config.toml`; the border color and the
thinking capture follow the new level.

## 8. Rust stack

- [`ratatui`](https://crates.io/crates/ratatui) + [`crossterm`](https://crates.io/crates/crossterm) — the TUI
- ~~[`tui-textarea`](https://crates.io/crates/tui-textarea)~~ — dropped: the input box is now a native multi-line textarea with vim modal input (section 7.1), rendered by the host. `Ctrl+E` still shells out to `$EDITOR` for long messages
- `serde` / `serde_json` — event envelope parsing
- `clap` — `tui --session s1 --config harness.toml`
- `async_trait` — `SessionPort`
- an event-tailer behind `SessionPort` (phase 1: tail the file by tracking offset; later: Unix-socket subscription)

## 9. Skeleton

```rust
// src/bin/tui.rs — sketch; all session access is behind SessionPort

#[async_trait]
pub trait SessionPort {
    async fn list_sessions(&self) -> Vec<SessionId>;
    async fn read_events(&self, session: SessionId) -> Vec<Event>;
    async fn append_event(&self, session: SessionId, event: Event) -> Result<(), BusError>;
    async fn spawn_loop(&self, session: SessionId) -> Result<Box<dyn LoopHandle>, BusError>;
}

fn render_event(event: &Event) -> Line {
    match event.kind() {
        EventKind::UserMessage => render_user_message(event),
        EventKind::AssistantMessage => render_assistant_message(event),
        EventKind::ToolCall => render_tool_call(event),
        EventKind::ToolResult => render_tool_result(event),
        EventKind::ApprovalRequest => render_approval_prompt(event),
        EventKind::Approval => render_approval_decision(event),
        EventKind::Cancel => render_cancel(event),
        EventKind::Error => render_error(event),
        _ => render_fallback(event),   // unknown type: pretty-print raw JSON
    }
}

fn main() {
    let config = load_config();        // contains [loop] command + session root
    let port = FileSessionPort::new(config);  // later: SocketSessionPort

    // ratatui loop:
    //   draw: session list | transcript (render_event per event) | status bar
    //   keys: Enter/Ctrl+E append user_message,
    //         Ctrl+R port.spawn_loop(active),
    //         Ctrl+C stop loop handle,
    //         y/n/e append approval,
    //         Tab switch session
}
```

## 10. Guardrails

1. **The TUI must not contain decision logic.** No "should this tool be allowed", no "what does the agent owe next". It renders and appends.
2. **The TUI must not know the loop internals.** `turn.sh`, `step.sh`, `claim`, `assemble` are not valid strings in the TUI source — they are config values.
3. **The TUI must not know the storage layout.** `events.jsonl`, `state.json`, `pending/` are not valid strings in the TUI source — they are `FileSessionPort` implementation details.
4. **Rendering must have a fallback.** Unknown event type or future `v` never crashes the TUI; it renders raw JSON with a hint.
5. **Killing the TUI must not corrupt the session.** The log survives. Loops keep running as orphans. Only Ctrl+C stops a loop. Later this splits into a daemon (`harnessd`) that owns the loop and a TUI that attaches/detaches over a Unix socket. The TUI stays an adapter over the same hexagonal boundary.

## 11. Phase 1 implementation (illustrative, not normative)

This is one concrete implementation below `SessionPort`. It can change completely without touching the TUI above the port.

```rust
// FileSessionPort: sessions live as directories; the loop is an opaque command.
// - list_sessions: scan session_root for subdirs containing an event log
// - read_events:   tail <session_dir>/events.jsonl
// - append_event:  schema-checked, then one locked write(2) via LogLine (FT-005)
// - spawn_loop:    run [loop].command with [loop].args and pass the session id
```

Approval in phase 1 is simply two event types (`approval_request`, `approval`) in the same log; the loop polls the log for the answer. No separate `pending/` directory is needed.

Later, `SocketSessionPort` replaces this with RPC calls to `harnessd`; `render_event` and the TUI loop remain unchanged.

## 12. Future evolution

- **Daemon + attachable TUI.** `harnessd` owns the loop and session locks; `SocketSessionPort` replaces `FileSessionPort`. The TUI is the same binary, different port implementation.
- **Multi-session tabs.** The session switcher becomes tabs/panes once event tailing is stable.
- **Approval queue.** Render all pending `approval_request` events across sessions, not just the active one.
- **Inline tool diff cards.** Render `tool_call`/`tool_result` payloads as terminal cards (diff view for file edits, terminal view for shell output) — pure rendering, no logic.

The TUI completes the Unix architecture: both the human and the tools interact with the same core mechanism — **append events, render events**.

## 13. Implementation record (2026-08-27)

Phase 1 is built as `bin/tui`. It matches sections 1–10 and 11. The
record below lists the deviations and the wire formats it fixes. The
code stays the design record. See `tui-plan.html` for the visual plan
and verification record.

### 13.1 Deviations from this proposal

- `SessionPort` uses native `async fn`. The `async_trait` dependency is
  dropped. `SessionId` is passed by reference, not by value.
- `SessionPort::watch(session, from)` is sync. It returns a
  `std::sync::mpsc::Receiver<WatchItem>`. The tailer is a std thread
  that polls the log every 250 ms. The UI drains the receiver in the
  draw loop. This avoids runtime-context traps.
- `LoopHandle` is sync: `stop()`, `wait_exit() -> i32`,
  `take_lines() -> Option<UnboundedReceiver<LoopLine>>`. Lines carry
  `Stdout`, `Stderr` and exactly one final `Exited(code)`.
- No `tui-textarea` dependency. The input is a one-line widget. Long
  input and edit-then-allow shell out to `$VISUAL`/`$EDITOR`.
- `read_events` reads the last 50 MB of the log. Cursor-based
  pagination is a later enhancement.
- The CLI takes the session as a positional argument:
  `tui [SESSION] --config config.toml`. Without a session argument the
  TUI does not resume the most recent session: it opens a name input
  bar (`new session: _`), and Enter confirms the typed name as the
  active session. The session log is created on its first appended
  event. Esc cancels the input; an invalid name (empty, path-shaped,
  or `.`) keeps the input up with a hint. Tab cycles to an existing
  session and ends the input; with no session to cycle to, the input
  stays up.
- Approval wire format: an `approval` event may carry `arguments`
  (the edited JSON object). This is an additive field. The log stays
  at `v: 1`.
- `cancel` events carry `target: "turn"`.
- Quit is two-step: the first `q` arms, a second `q` inside 3 s
  quits, any other key disarms. `Ctrl+Q` maps to the same key. The
  quit gate (FT-012) applies to both: the key arms and fires only in
  normal mode with an empty draft; in every other state it types a
  plain `q` into the active input. This deviates from the single `q`
  of section 7 for mistouch safety.
- Message content wraps across lines: the `content` field of user
  and assistant messages wraps at the pane width and displays in
  full (docs/tui_feature_requests_from_human.md item 1, rescoped
  2026-09-03). Tool result bodies fold at render time: the
  preview cap per tool, the fold hint, and the expanded cap
  (docs/tui-tool-display-port.md). Newlines in the text are hard
  breaks.
- Tool results render the tool's `text` payload (or `stdout`/`stderr`
  when no `text`), not the raw JSON value envelope. The status line
  shows `exit <code>` and an `(error)` flag.
- The help row is short and puts the quit hint first, because terminal
  clipping eats the right end of the row.

### 13.2 Config shape (added to `config.toml`)

```toml
[loop]
command = "bash"
args = ["scripts/turn.sh"]
arg_style = "append_session"   # append_session | env | none
```

`spawn_loop` runs `command` with `args` plus the session id when
`arg_style` is `append_session`. The process runs with the config
file's directory as working directory and an absolute `CONFIG`
environment variable.

```toml
[tui]
color = "truecolor"   # truecolor | 256 | 16 | 8 (aliases: rgb, 24bit, 256color, 8color)
color_scheme = "catppuccin macchiato"   # absent: the default is also catppuccin macchiato
# custom schemes overlay the built-in palette role by role:
# [tui.custom_schemes.name]
# plain_text = "#cdd6f4"   # a partial table keeps the rest
```

`[tui] color` forces the terminal color capability level. Extension
hex wire colors and the built-in tones lower to it. Unknown names
are a hard error at load. Absent, the TUI detects from the
environment (COLORTERM, TERM; color.rs module docs).

`[tui] color_scheme` selects a named scheme. The default (and the
first internal scheme) is `catppuccin macchiato` (docs/tui-color-
scheme.md, the pi `catppuccin-macchiato` theme values, docs/tui-color-pi-
alignment.md):
every role resolves to a scheme hex, lowered to the capability
level. A `[tui.custom_schemes.<name>]` table overlays the
selected palette role by role; an unset role keeps its current
value. An unknown role name is a hard error at load.

```toml
[tui.tool_display]   # docs/tui-tool-display-port.md
preset = "opencode"  # opencode | balanced | verbose
# per-tool overrides, any of:
# read = "preview"              # hidden | summary | preview
# search = "preview"            # hidden | count | preview
# bash = "preview"              # hidden | summary | preview
# preview_lines = 8
# bash_collapsed_lines = 10
# diff_collapsed_lines = 24
# expanded_preview_max_lines = 4000
# diff_view = "auto"            # auto | unified | split
```

An override switches the effective preset to `custom`. The presets
are the `pi-tool-display` values: `opencode` keeps short output
full and folds the rest; `balanced` folds more; `verbose` folds
least.

### 13.3 Loop process supervision

The loop runs in its own process group (`setsid` in `pre_exec`).
Stop sends `SIGTERM` to the group. A 3 s grace timer escalates to
`SIGKILL`. A tokio reaper task waits for the child and for both
output pumps to hit EOF, then sends `Exited` exactly once. `Exited`
always trails the last output line.

On a TUI restart the app state is empty. The port probe
(`external_loop_pid`) reads `loop.pid`. It confirms the group is live
and names the session. It marks the session running without a local
handle. The probe runs at start, on every session switch, and once a
second in the main loop. The `[running]` bit shows real loop state,
not this process's memory. `Ctrl+R` blocks on the probe. `Ctrl+C`
resolves through the probe when no local handle exists (FT-003).

### 13.4 Tailer semantics

The tailer tracks a byte offset. It resets on truncation and on
rewrite (a byte just before the offset that is not a newline).
A partial line stays in a carry buffer until its newline lands.
When the channel is full, the tailer holds its position. It resumes
on the next poll. It never re-emits an event.

### 13.5 Known limits

- `approval_request`, `approval` and `cancel` have no schema files in
  `schemas/events/v1` yet, so the producer-side G3 check skips them.
- The minimal JSON-schema validator is a third copy (see
  `notes/itches.md`).
- A session that grows past 50 MB reads only its tail.
- The tailer is per active session. One std thread per switched
  session; a dropped receiver stops it on the next send.

### 13.6 Tool display, thinking block, queues, schemes (2026-09-03)

The 2026-08-29/09-02 request batch (docs/ tui_feature_requests_
from_human.md) ships in this pass:

- **Tool result display** (docs/tui-tool-display-port.md): the
  built-in tool result render ports the `pi-tool-display` style.
  A rounded box with the tool box background, the command header,
  the per-tool output modes, the preview caps (read and search
  `preview_lines`, bash `bash_collapsed_lines`, diff
  `diff_collapsed_lines`), the fold hint with the `Ctrl+O`
  expand, the `expanded_max_lines` cap, the unified/split diff
  layout at `DIFF_SPLIT_MIN_WIDTH`, and the three presets with
  per-tool overrides under `[tui.tool_display]`. `Edit` results
  render as a diff (before and after, split on wide panes). `Read`
  and unknown-tool results that are complete JSON documents get
  JSON syntax highlighting inside the box.
- **Marker-free markdown** (docs/tui-markdown-render.md): the
  transcript drops the `#`, `>`, `**`, `*`, and backtick markers;
  the styles stay. The list bullet keeps its marker. `|` tables
  draw as box-drawing grid tables (the header bold, the separator
  row dropped, columns elide on narrow panes). Fence markers stay
  dim; their content renders literal.
- **Thinking block** (docs/tui-thinking-block.md): the
  `reasoning` content of assistant messages renders as a block
  above the body. Collapsed, a one-line preview; expanded, the
  full text in the lighter thinking tone. `Ctrl+T` collapse/expand
  (the pi keymap), `Ctrl+X` show/hide.
  `Ctrl+L` cycles the active model's `reasoning_effort` in
  `config.toml` (a comment-preserving in-place edit; the table is
  created when absent).
- **Pending message queues** (docs/tui-pending-user-messages.md,
  stage 2): the `user_message` event takes an optional `queue`
  field, `steer` or `follow`. `bin/user` gains `--queue`. The
  loop side (bin/claim, bin/assemble, scripts/turn.sh, step.sh)
splits the queues: steer messages wake the loop at the next
  step; follow messages run as new turns at the idle boundary
  (`--inject-follow`). The TUI renders the two blocks and
  `Ctrl+F` toggles the composer between the queues.
- **Color schemes** (docs/tui-color-scheme.md): the 38-role
  palette. The no-scheme default is `catppuccin macchiato` (the pi
  `catppuccin-macchiato` theme values, docs/tui-color-pi-alignment.md);
  the pi built-in `dark` theme values are the fallback for an unset
  role in a user scheme. `[tui] color_scheme` selects a named scheme;
  `[tui.custom_schemes.<name>]` overlays the selected base palette
  role by role, a partial table allowed.
- **The reference renderers stop the gray abuse** (docs/
  tui-color-tones.md section 4): `ui_extensions-demos/tool_result/
  tool_result.sh` and `ext-rs/tool_result-rs` paint the body in
  one muted tone instead of a single gray, and a body that is a
  complete JSON document gets JSON syntax highlighting (the awk
  tokenizer in the bash reference, the native walk in the Rust
  port). The six rescoped "never truncate" comments ship with
  the same pass (docs/tui-tool-result-truncation.md section 4).

## Properties

Lean-style invariants for this spec (see `lean-driven-development.md`).

P1. unknown-event-fallback: given an event with an unrecognized `type`, observe the TUI render the raw type plus its JSON in order and not crash.
P2. version-fallback: given an event with an unsupported `v`, observe the TUI render it raw and show a newer-version hint.
P3. opaque-loop: given a config `[loop]` command, observe the TUI start and stop that command with no `turn.sh` or `claim` string in its source.
P4. append-only-writes: given a typed input, observe the TUI append a `user_message`, `approval`, or `cancel` event and leave the log otherwise intact.
P5. crash-survival: given a killed TUI process, observe the session log survive unchanged and the running loop continue as an orphan.
P6. pending-derivation: given a log whose last event is a `user_message`, observe the TUI hold it pending until a later `assistant_message` arrives.

## Verification

Each property maps to its proof. `proven` means the cited test exists and passes.

| P# | Property | Proof | Status |
|----|----------|-------|--------|
| P1 | unknown-event-fallback | `unknown_type_renders_raw_json_with_hint`, `malformed_line_renders_without_crash` in `bin/tui/src/render.rs` | proven |
| P2 | version-fallback | `unsupported_version_renders_hint` in `bin/tui/src/render.rs` | proven |
| P3 | opaque-loop | `storage_and_loop_strings_stay_behind_the_port`, `stage_names_are_not_strings_in_the_tui` in `bin/tui/src/main.rs` | proven |
| P4 | append-only-writes | `ctrl_r_emits_the_spawn_intent`, `approval_keys_answer_only_when_pending` in `bin/tui/src/app.rs` | proven |
| P5 | crash-survival | Blocked: no test kills the TUI and asserts the log stays intact and the loop stays live. Unblock with a process-supervision test. | open |
| P6 | pending-derivation | `pending_user_messages_stop_at_the_last_answer` in `bin/tui/src/app.rs` | proven |

## Gate

The acceptance commands. All must exit 0 for this spec to be proven.

```
cargo build
cargo test
scripts/tui-pty-smoke.py
```

