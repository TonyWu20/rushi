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

## 7. Key bindings

| Key | Action |
|---|---|
| `Enter` | Append the typed text as `user_message`: sends the whole multi-line draft, in any modal state |
| `Ctrl-J` | Insert mode: a hard newline (multi-line draft). Normal mode: the `j` motion |
| `Ctrl+E` | Open `$EDITOR` for long input, then append |
| `Ctrl+R` | `SessionPort::spawn_loop(active_session)`. The persistent `loop.pid` probe blocks the start when a live loop holds the session (FT-003) |
| `Ctrl+C` | Stop the loop and append a `cancel` event. A local handle stops its group. Without one, the `loop.pid` probe stops the external group (FT-003) |
| `y` / `n` / `e` | Answer the oldest pending `approval_request`: allow / deny / edit-then-allow |
| `h` | One-key handoff resume (correction 57). Only when the log holds a `context_exhausted` marker that seeded a session and no loop runs. Switches to the seeded session and starts its loop. The old session's local loop stops. Without those conditions, `h` stays the editor key |
| `Tab` | Switch session |
| `q` ×2 | Quit the TUI. Loops keep running as orphans. Only `Ctrl+C` stops a loop |

The input area is a multi-line textarea with native vim modal input
(section 7.1), in a rounded-corner border whose color correlates with
the model thinking level (section 7.2). The frame is customizable by
a `frame` extension (docs/ui-extension.md: the `frame` capability
owns the border, label, and height; the TUI renders the draft and
cursor).

### 7.1 Vim modal input

The draft is a `Vec` of lines plus a modal key state machine
(`vim_editor.rs`). The modes and the operator-pending convention are
taken from the pi-config vim extension (`vim-modal.ts`):

- **normal**: `h j k l`, `w b e`, `0 $`, `gg G`, `x` (with a count,
  `X`), `d c y` + motion (`dd cc`, `d$` is `D`, `c$` is `C`), `yy`
  (line yank), `p P` (with a count), `i a I A o O`, `r` (replace one
  character, the pending `r` operator), `R` (overwrite mode), `v V`
  (char-wise / line-wise visual)
- **insert**: chars append, `Ctrl-J` inserts a hard newline (`Enter`
  sends the draft, so it never reaches the editor),
  `Backspace` joins lines at column 0, `Esc` returns to normal
- **replace** (`R` in normal): each typed char overwrites the one
  under the cursor; `Esc` returns to normal
- **visual / visual-line** (`v` / `V`): `d x c y p P` act on the
  mark-to-cursor span; `Esc` or `v` leaves visual

Counts prefix operators and motions: `3dd`, `2w`, `3c`. The yank
buffer is a single slot; a delete sets it too, like vim. The editor
starts in insert mode (the composer's typing mode); `Esc` drops to
normal for motions. The mode label shows in the frame title
(`[NORMAL]`, `[INSERT]`, `[d-PENDING]`, ...), mirroring the pi
`formatStatus` output.

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
host's built-in palette above applies.

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
  quits, any other key disarms. `Ctrl+Q` maps to the same key. This
  deviates from the single `q` of section 7 for mistouch safety.
- Text content wraps across lines: user/assistant messages and tool
  output wrap at the pane width, capped per event with a `… +N more
  lines` hint. Newlines in the text are hard breaks.
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

