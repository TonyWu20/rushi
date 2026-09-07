//! Application state: what the TUI shows and how keys map to actions.
//!
//! This module never touches the port or I/O: it turns key events into
//! [`Action`]s and folds port results back in. `main.rs` executes the
//! actions; the tests here drive the state machine directly.

use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use ratatui::text::Line;

use crate::event::{Event, EventKind};
use crate::port::{LoopHandle, LoopLine, SessionId, WatchItem};
use crate::vim_editor::{Editor, Mode};
use serde_json::Value;

/// Normalized key input. crossterm-free so tests can drive the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Enter,
    Backspace,
    Tab,
    BackTab,
    PgUp,
    PgDn,
    CtrlR,
    CtrlC,
    CtrlE,
    CtrlU,
    CtrlD,
    /// The global tool fold/expand toggle (docs/tui-tool-display-port.md
    /// section 2, the expand part): every collapsed block expands to
    /// the full output, and back. The key follows `pi`'s expand key.
    CtrlO,
    /// The thinking-block show/hide toggle (docs/tui-thinking-block.md
    /// section 4). `Ctrl+H` is unsafe: most terminals send the
    /// backspace byte for it, so the toggle takes `Ctrl+T` instead.
    CtrlT,
    /// The thinking-block collapse/expand toggle (docs/tui-thinking-
    /// block.md section 4).
    CtrlX,
    /// The input queue toggle: the next draft sends to the follow
    /// queue (docs/tui-pending-user-messages.md stage 2).
    CtrlF,
    /// The reasoning-effort cycle: the next effort value in the
    /// effort order, written to the active model's config entry
    /// (docs/tui-thinking-block.md section 4, the effort control).
    CtrlL,
    /// The multi-line editor's newline key: in insert mode it
    /// inserts a hard newline; in normal mode it is the `j` motion.
    /// `Enter` sends the draft (docs/tui.md section 7).
    CtrlJ,
    /// The picker list navigation: Ctrl+K moves up, Ctrl+J moves
    /// down (docs/tui-file-picker.md section 5).
    CtrlK,
    /// The picker preview-pane toggle (docs/tui-file-picker.md
    /// section 4.4).
    CtrlP,
    /// The picker file-scope cycle (docs/tui-file-picker.md P9):
    /// standard → show git-ignored → also show hidden → back to
    /// standard. The item list is re-enumerated on each press.
    CtrlI,
    /// Alt+Up: recall the pending message queue into the editor
    /// (docs/user-message-editing.md).
    AltUp,
    Esc,
    Quit,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Delete,
    Wheel(i32),
    Char(char),
}

/// An answer to the oldest pending `approval_request`
/// (docs/tui.md section 7: y / n / e).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
    Edit,
}

/// Port-level work the UI thread must perform for a key press.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Cycle to the next/previous session, then reload its log.
    ///
    /// Reserved for the session-navigation design (docs/tui_feature_
    /// requests_from_human.md). The Tab / BackTab bindings were freed
    /// for the file picker, so this variant is currently unconstructed
    /// but kept as the seam for the redesigned session navigator.
    #[allow(dead_code)]
    CycleSessions(i32),
    /// `SessionPort::spawn_loop(active)` (docs/tui.md key Ctrl+R).
    /// Main gates the spawn on the persistent loop probe (FT-003): a
    /// live loop for the session blocks the start, across a TUI
    /// restart.
    RunLoop,
    /// Stop the active session's loop and append a `cancel` event.
    /// With no local handle, main stops the external group through the
    /// persistent `loop.pid` (FT-003).
    StopLoop,
    /// Append the draft as a `user_message` (docs/tui.md key Enter).
    SendDraft,
    /// Open `$EDITOR` on the draft (docs/tui.md key Ctrl+E).
    OpenEditor,
    /// Append an `approval` event (docs/tui.md keys y / n / e).
    AnswerApproval(Decision),
    /// Confirm the session name the user typed after starting `tui`
    /// without a session argument. The TUI activates it and opens its
    /// (possibly empty) log.
    ConfirmNewSession(String),
    /// Switch to the handoff session a `context_exhausted` event
    /// seeded, and start the loop there. The one-key resume of the
    /// automatic handoff (correction 57). The old session's local
    /// loop stops when it is still running: the handoff supersedes
    /// it.
    Handoff(String),
    /// Quit the TUI. Loops keep running: each lives in its own session
    /// and survives as an orphan. Only Ctrl+C stops a loop. The log
    /// stays intact, so a restart re-renders the live session.
    Quit,
    /// The global tool fold/expand toggle (Ctrl+O). Main redraws; the
    /// state lives on the app (docs/tui-tool-display-port.md section 2,
    /// the expand part).
    ToggleToolExpand,
    /// The thinking-block show/hide toggle (Ctrl+T, docs/tui-thinking-
    /// block.md section 4).
    ToggleThinking,
    /// The thinking-block collapse/expand toggle (Ctrl+X).
    ToggleThinkingExpand,
    /// The input queue toggle (Ctrl+F, docs/tui-pending-user-messages.md
    /// stage 2): the next draft sends to the follow queue.
    ToggleFollowQueue,
    /// The reasoning-effort cycle (Ctrl+L, docs/tui-thinking-block.md
    /// section 4, the effort control). Main writes the next effort
    /// value to the active model's config entry and flashes the
    /// change.
    CycleEffort,
    /// Switch to a session by name (docs/tui-command-palette.md §7).
    SwitchSession(String),
    /// Set the reasoning effort to a specific value (palette `effort` item).
    SetEffort(String),
    /// Invoke an extension-owned command (docs/tui-command-palette.md §10).
    InvokeExtCommand {
        ext: String,
        id: String,
        value: Option<String>,
    },
    /// Bulk recall of pending user messages (docs/user-message-editing.md).
    RecallQueue,
}

/// The oldest pending `approval_request` in the active session log.
/// A request is pending until an `approval` event with the same `id`
/// appears later in the log — approval recovery (refinement policy G6).
#[derive(Debug, Clone, PartialEq)]
pub struct PendingApproval {
    pub request_id: String,
    pub prompt: Option<String>,
    pub call_id: Option<String>,
    /// The tool_call arguments, so an edit-then-allow can carry edited
    /// arguments in the `approval` event.
    pub arguments: Option<Value>,
}

#[derive(Default)]
pub struct LoopState {
    pub handle: Option<Box<dyn LoopHandle>>,
    pub lines_rx: Option<tokio::sync::mpsc::UnboundedReceiver<LoopLine>>,
    pub running: bool,
    pub last_line: Option<String>,
    pub exit_code: Option<i32>,
}

/// In-progress model response accumulated from the stream channel
/// (docs/tui-streaming-response.md §3.2). One JSON line per SSE delta
/// event; the TUI polls the file each frame and folds new lines in.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct StreamBuf {
    /// Accumulated assistant text (`text` deltas).
    pub text: String,
    /// Accumulated reasoning text per item id (`reasoning` deltas).
    pub reasoning: std::collections::HashMap<String, String>,
    /// Partial tool-call arguments: call_id → (tool_name, args_so_far).
    pub tool_args: std::collections::HashMap<String, (String, String)>,
    /// Set when the channel's `done` line was read.
    pub done: bool,
}

/// The kind of a paced stream delta: the accumulator key the release
/// applies to (docs/tui-streaming-response.md §6.5).
#[derive(Clone, Debug)]
enum StreamDeltaKind {
    /// Response text.
    Text,
    /// Reasoning text, keyed by the reasoning item id.
    Reasoning(String),
    /// Partial tool-call arguments, keyed by the call id and the
    /// observed tool name.
    ToolArgs(String, String),
}

/// One stream delta waiting to be released into the live buffer by
/// [`App::pump_stream_pacing`]: the kind plus the raw delta text.
#[derive(Clone, Debug)]
struct PendingStreamDelta {
    kind: StreamDeltaKind,
    payload: String,
}

/// The paced-release window: the pending backlog clears in about this
/// many frames (docs/tui-streaming-response.md §6.5). At the 60 FPS
/// streaming cadence that is a quarter-second catch-up; the per-frame
/// release is `max(1, backlog / STREAM_PACE_FRAMES)` chars, so a small
/// backlog reads as a steady typewriter and an arrival burst smooths
/// out instead of jumping the view.
const STREAM_PACE_FRAMES: usize = 15;

pub struct App {
    sessions: Vec<SessionId>,
    active: Option<SessionId>,
    events: Vec<Event>,
    /// Visual lines scrolled up from the end. 0 means "follow the tail".
    scroll: usize,
    /// The multi-line message editor with vim modal input. The draft
    /// is `editor.lines`; sending trims and appends it as a
    /// `user_message`.
    editor: Editor,
    /// The first editor line shown in the input area (the area shows
    /// two editor lines plus its border; scrolling moves this window).
    edit_scroll: usize,
    loops: HashMap<SessionId, LoopState>,
    status: Option<(String, Instant)>,
    watch_rx: Option<std::sync::mpsc::Receiver<WatchItem>>,
    quitting: bool,
    /// Armed since the first `q`; a second `q` inside the window quits.
    /// Any other key disarms. Arms only while the quit gate is open
    /// (normal mode, empty draft; FT-012). Mistouch safety (single-
    /// key `q` is too easy to hit by accident mid-typing).
    quit_arm: Option<Instant>,
    /// The new-session name being typed, when the user started `tui`
    /// without a session argument. `None` means the name input is off.
    pending_name: Option<String>,
    /// The browse mode state machine (docs/tui-conversation-browsing.
    /// md section 4). The double-`s` overlay over the transcript.
    browse: crate::browse::Browse,
    /// Armed since the first `s`; a second `s` inside the window
    /// enters or leaves browse mode. Any other key disarms. Mirrors
    /// the `q q` arm of FT-012 for the `s s` gate (section 4.2).
    ss_arm: Option<Instant>,
    /// The last rendered browse layout: `(total, h, text_w, line
    /// texts)`, refreshed by the renderer each frame while browse is
    /// active. Browse motions and the search read it; tests prime it
    /// by hand.
    browse_layout: Option<(usize, usize, usize, Vec<String>)>,
    /// A rendered event landed since the last browse sync
    /// (section 4.6): the transcript growth is event growth, not a
    /// pane rewrap, so the browse view does not follow.
    events_grew: bool,
    /// The transcript pane height set by the last draw, in lines.
    /// Drives the half-page distance of Ctrl+U / Ctrl+D.
    viewport: usize,
    /// Bumped whenever the event list changes. The transcript cache is
    /// valid only while this number is unchanged.
    events_version: u64,
    /// Cached wrapped transcript lines, keyed by (events_version,
    /// width, ext reply version, palette). A scroll redraw reuses the
    /// cache: O(viewport) instead of O(total lines). The extension
    /// reply version folds in, so a new reply rebuilds the lines
    /// (ui-extension-plan stage 1). The palette folds in, so a
    /// scheme change rebuilds the lines (docs/tui-color-scheme.md).
    transcript_cache: Option<(
        u64,
        usize,
        u64,
        crate::color::Level,
        crate::color::Palette,
        Vec<Line<'static>>,
    )>,
    /// The terminal's color capability the built-in palette is lowered
    /// to, and the selected color scheme (docs/tui-color-scheme.md
    /// section 3). Set by the host in `main`; `new` defaults to the
    /// built-in palette at detected capability, so render tests are
    /// deterministic.
    palette: crate::color::Palette,
    /// The tool-result display config ([`tui] tool_display` table,
    /// docs/tui-tool-display-port.md section 2). Set from the harness
    /// config in `main`; `new` defaults to the `opencode` preset.
    tool_display: crate::tool_display::ToolDisplay,
    /// The global tool fold/expand toggle (Ctrl+O, docs/tui-tool-
    /// display-port.md section 2, the expand part). `false` shows each
    /// block at its output mode's lines; `true` expands every
    /// collapsed block to the full body, capped at
    /// `expanded_preview_max_lines`.
    tool_expanded: bool,
    /// The thinking-block visibility (Ctrl+T, docs/tui-thinking-block.md
    /// section 4). `true` renders the block; `false` hides it
    /// entirely.
    thinking_shown: bool,
    /// The thinking-block expand state (Ctrl+X). `false` shows the
    /// collapsed header row; `true` shows the full thinking text.
    thinking_expanded: bool,
    /// The input queue toggle (Ctrl+F, docs/tui-pending-user-messages.md
    /// stage 2). `true`: the next draft sends to the follow queue.
    follow_queue: bool,
    /// The `@` file-picker state machine (docs/tui-file-picker.md
    /// section 4.3). `open` is `true` while the floating window is
    /// visible.
    picker: crate::picker::state::PickerState,
    /// The background frizbee ranker for the picker. `None` while the
    /// picker is closed. Spawned when the picker opens, dropped when
    /// it closes.
    picker_matcher: Option<crate::picker::fuzzy::PickerMatcher>,
    /// The file source backing the picker (the section 4.5 seam where
    /// later symbol and git-file sources plug in).
    picker_source: Option<crate::picker::items::FileItemSource>,
    /// The search root the current matcher list was enumerated under.
    /// Path queries re-root the search; a changed root re-enumerates.
    picker_root: std::path::PathBuf,
    /// The `(row, col)` of the `@` token that opened the picker,
    /// so `sync_picker` can compute the query from that fixed
    /// position instead of re-detecting the token (which would
    /// fail if the user types another `@` into the query).
    picker_at: Option<(usize, usize)>,
    /// The `:` command-palette state machine
    /// (docs/tui-command-palette.md section 11).
    palette_state: crate::palette::state::PaletteState,
    /// Extension-provided palette commands, refreshed when a
    /// `commands_list` reply lands or a command times out.
    ext_commands: Vec<crate::palette::items::PaletteItem>,
    /// The active model's current reasoning effort, used to mark
    /// the current option in the palette `effort` item.
    effort_current: String,
    /// Set when the palette opens so the main loop can request
    /// `commands` from extensions.
    palette_cmd_requested: bool,
    /// Latest `ext_status` values, id to value, for the active
    /// session. Maintained incrementally: `set_active` builds it
    /// and each appended watch event updates it. A tick reads this
    /// map in O(1) instead of rescanning the whole log
    /// (ui-extension-plan stage 1, tick payload).
    ext_status_values: HashMap<String, Value>,
    /// The update order of the ext_status ids, most recent last.
    /// Drops the oldest id when the map holds the cap
    /// (ui-extension-plan stage 4: the log-growth bound).
    ext_status_order: VecDeque<String>,
    /// The timestamp of the event that last set each ext_status id
    /// value (docs/tui-model-wait-indicator.md). Each id maps to
    /// the raw `ts` string of that event. The map drops an id with
    /// the value map at the cap. An event without a `ts` field
    /// updates the value but leaves the entry untouched.
    ext_status_ts: HashMap<String, String>,
    /// The in-progress model response, accumulated from the session-local
    /// stream channel file (docs/tui-streaming-response.md §6.1).
    /// `None` when no model call is in flight or the response settled.
    stream_buf: Option<StreamBuf>,
    /// Byte offset into the stream channel file already consumed.
    /// Reset when the file is truncated or recreated.
    stream_offset: u64,
    /// Stream deltas queued for the paced release into the live buffer
    /// (docs/tui-streaming-response.md §6.5): the text advances a few
    /// characters per frame instead of jumping whole deltas.
    stream_pending: VecDeque<PendingStreamDelta>,
    /// The char count of `stream_pending` — the per-frame release
    /// budget is `max(1, chars / STREAM_PACE_FRAMES)` (§6.5).
    stream_pending_chars: usize,
}

/// The cap on the distinct ext_status ids the in-memory map holds.
/// A chatty publisher cannot grow the map without bound. At the cap
/// the least-recently-updated id drops first. The log keeps every
/// event: the log is the audit record, and the bound bounds the
/// TUI's memory only (ui-extension-plan stage 4, open items).
const EXT_STATUS_ID_CAP: usize = 128;

/// The `ext_status` id that carries the active loop's phase
/// (docs/tui-model-wait-indicator.md). The loop publishes `wait`
/// before the model call and `tools` before routing. The TUI reads
/// the last value, gated on the loop-running bit. No TUI decision
/// logic: the value is published log state.
pub const LOOP_PHASE_STATUS_ID: &str = "loop_phase";

/// A session name must stay a plain directory name inside the sessions
/// root. Mirrors `port_file::session_dir`: no absolute paths, no
/// parent-directory component, no empty or `.` name (a `.` session
/// would write its log into the sessions root itself, where
/// `list_sessions` never finds it).
fn valid_session_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/')
}

/// The `ext_status` values of one event list, id to value. Later
/// events win, like the log order. An event without a string `id`
/// adds no entry; a missing `value` counts as `null`. The triple
/// is the value map, the event-timestamp side map (id to the raw
/// `ts` of the event that last set the id), and the update order
/// (most recent last). Both maps cap at
/// [`EXT_STATUS_ID_CAP`] distinct ids: at the cap the
/// least-recently-updated id drops, like the incremental path.
fn ext_status_map(
    events: &[Event],
) -> (
    HashMap<String, Value>,
    HashMap<String, String>,
    VecDeque<String>,
) {
    let mut m = HashMap::new();
    let mut ts = HashMap::new();
    let mut order: VecDeque<String> = VecDeque::new();
    for e in events {
        if e.kind() != EventKind::ExtStatus {
            continue;
        }
        let Some(id) = e.get_str("id") else {
            continue;
        };
        let value = e.get("value").cloned().unwrap_or(Value::Null);
        let event_ts = e.get_str("ts").map(str::to_string);
        if m.insert(id.to_string(), value).is_none() {
            order.push_back(id.to_string());
            while order.len() > EXT_STATUS_ID_CAP {
                let old = order.pop_front().expect("cap keeps the order non-empty");
                m.remove(&old);
                ts.remove(&old);
            }
        } else {
            if let Some(pos) = order.iter().position(|x| x == id) {
                order.remove(pos);
            }
            order.push_back(id.to_string());
        }
        if let Some(t) = event_ts {
            ts.insert(id.to_string(), t);
        }
    }
    (m, ts, order)
}

const STATUS_TTL: std::time::Duration = std::time::Duration::from_secs(4);

/// The `ext_status` id that carries the active model's thinking
/// level. The loop (or a policy hook) publishes it; the TUI reads it
/// to color the input area.
pub const THINKING_STATUS_ID: &str = "model_thinking";
/// The thinking levels this build knows: 0 (no thinking) through 4.
pub const THINKING_LEVELS: u32 = 5;
/// The level shown while no `model_thinking` event is in the log.
pub const DEFAULT_THINKING_LEVEL: u32 = 0;

/// The window in which a second `q` confirms the quit.
const QUIT_ARM_TTL: std::time::Duration = std::time::Duration::from_secs(3);
/// The window in which a second `s` enters or leaves browse mode
/// (docs/tui-conversation-browsing.md section 4.2, the FT-012
/// mirror).
const SS_ARM_TTL: std::time::Duration = std::time::Duration::from_secs(3);
/// Scroll distance kept between the viewport top and the log end. The
/// transcript is capped anyway, so the scroll clamps at draw time.
const SCROLL_CAP: usize = 100_000;
/// Maximum number of events held in memory per session. The render
/// layer only displays the last `TRANSCRIPT_EVENT_CAP` events, but we
/// keep extra for scrollback and pending-approval lookups. Capping the
/// Vec prevents unbounded memory growth in long-running sessions
/// (a single TUI instance must not exhaust system RAM).
const EVENTS_CAP: usize = 10_000;

impl App {
    pub fn new() -> Self {
        App {
            sessions: Vec::new(),
            active: None,
            events: Vec::new(),
            scroll: 0,
            editor: Editor::new(),
            edit_scroll: 0,
            loops: HashMap::new(),
            status: None,
            watch_rx: None,
            quitting: false,
            quit_arm: None,
            pending_name: None,
            browse: crate::browse::Browse::new(),
            ss_arm: None,
            browse_layout: None,
            events_grew: false,
            ext_status_values: HashMap::new(),
            ext_status_order: VecDeque::new(),
            ext_status_ts: HashMap::new(),
            viewport: 0,
            events_version: 0,
            transcript_cache: None,
            palette: crate::color::Palette::builtin(crate::color::Level::detect()),
            tool_display: crate::tool_display::ToolDisplay::preset(
                crate::tool_display::Preset::OpenCode,
            ),
            tool_expanded: false,
            thinking_shown: true,
            thinking_expanded: true,
            follow_queue: false,
            picker: crate::picker::state::PickerState::new(),
            picker_matcher: None,
            picker_source: None,
            picker_root: std::path::PathBuf::new(),
            picker_at: None,
            palette_state: crate::palette::state::PaletteState::new(),
            ext_commands: Vec::new(),
            effort_current: "medium".to_string(),
            palette_cmd_requested: false,
            stream_buf: None,
            stream_offset: 0,
            stream_pending: VecDeque::new(),
            stream_pending_chars: 0,
        }
    }

    /// The color palette every built-in style lowers to: the
    /// capability level plus the selected color scheme
    /// (docs/tui-color-scheme.md section 3). Set from the harness
    /// config override in [main]; defaults to the built-in palette
    /// at environment detection, so tests and the no-config path
    /// keep their own detection result.
    pub fn palette(&self) -> &crate::color::Palette {
        &self.palette
    }

    /// Override the palette the built-in render path lowers to.
    pub fn set_palette(&mut self, palette: crate::color::Palette) {
        self.palette = palette;
    }

    /// The tool-result display config ([`tui] tool_display` table,
    /// docs/tui-tool-display-port.md section 2). Set from the harness
    /// config in `main`; defaults to the `opencode` preset.
    pub fn set_tool_display(&mut self, td: crate::tool_display::ToolDisplay) {
        self.tool_display = td;
    }

    /// The tool-result display config, for the render path.
    pub fn tool_display(&self) -> &crate::tool_display::ToolDisplay {
        &self.tool_display
    }

    /// The global fold/expand toggle state (Ctrl+O).
    pub fn tool_expanded(&self) -> bool {
        self.tool_expanded
    }

    /// The thinking-block visibility state (Ctrl+T).
    pub fn thinking_shown(&self) -> bool {
        self.thinking_shown
    }

    /// The thinking-block expand state (Ctrl+X).
    pub fn thinking_expanded(&self) -> bool {
        self.thinking_expanded
    }

    /// The input queue toggle state (Ctrl+F): the next draft sends
    /// to the follow queue when `true`.
    pub fn follow_queue(&self) -> bool {
        self.follow_queue
    }

    // ── sessions ────────────────────────────────────────────────

    pub fn set_sessions(&mut self, list: Vec<SessionId>) {
        self.sessions = list;
    }

    pub fn active(&self) -> Option<&SessionId> {
        self.active.as_ref()
    }

    /// Pick the session to show for `CycleSessions(delta)`.
    /// `delta` walks the list from the active session; a missing active
    /// session starts at the first entry.
    pub fn cycle_target(&self, delta: i32) -> Option<SessionId> {
        let n = self.sessions.len();
        if n == 0 {
            return None;
        }
        let idx = self
            .active
            .as_ref()
            .and_then(|a| self.sessions.iter().position(|s| s == a))
            .unwrap_or(0);
        let next = ((idx as i64 + delta as i64).rem_euclid(n as i64)) as usize;
        Some(self.sessions[next].clone())
    }

    /// Switch the visible session. Reloads its log and resets scroll.
    /// The caller restarts the port watch afterwards.
    pub fn set_active(&mut self, id: SessionId, mut events: Vec<Event>) {
        // Trim from the front to bound memory; the render layer only
        // shows the last TRANSCRIPT_EVENT_CAP events anyway.
        if events.len() > EVENTS_CAP {
            let excess = events.len() - EVENTS_CAP;
            events.drain(0..excess);
        }
        let (statuses, status_ts, order) = ext_status_map(&events);
        self.active = Some(id);
        self.events = events;
        self.scroll = 0;
        // The browse state never survives a session switch (section
        // 4.7): the reset rides the scroll reset.
        self.browse.reset(&mut self.scroll);
        self.ss_arm = None;
        self.events_grew = false;
        self.events_version += 1;
        self.ext_status_values = statuses;
        self.ext_status_ts = status_ts;
        self.ext_status_order = order;
        self.clear_stream();
    }

    // ── model stream channel (docs/tui-streaming-response.md §6) ──

    /// The in-progress model response, if one is streaming. Returns
    /// `None` when no model call is in flight or the response has
    /// already settled into the transcript.
    pub fn stream_buf(&self) -> Option<&StreamBuf> {
        self.stream_buf.as_ref()
    }

    /// Clear the live stream buffer and reset the byte offset. Called
    /// when a settle event (`assistant_message`, `error`, `cancel`) or
    /// a session switch replaces the in-progress content with the
    /// authoritative log entry.
    pub fn clear_stream(&mut self) {
        self.stream_buf = None;
        self.stream_offset = 0;
        self.stream_pending.clear();
        self.stream_pending_chars = 0;
    }

    /// Poll the session-local stream channel file and fold new lines
    /// into the live buffer (docs/tui-streaming-response.md §6.2).
    ///
    /// A missing file settles the buffer (the loop deleted it when the
    /// model call ended). A file that shrank is a new channel (the
    /// loop re-created it for the next call): reset and re-read from 0.
    /// A file that grew is the normal case: parse new complete lines
    /// and advance the offset. A trailing partial line stays for the
    /// next poll. Transient I/O errors are dropped: the buffer keeps
    /// its last good state.
    pub fn refresh_stream(&mut self, path: &std::path::Path) {
        let data = match std::fs::read_to_string(path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.clear_stream();
                return;
            }
            Err(_) => return,
        };
        if (data.len() as u64) < self.stream_offset {
            // The loop re-created the file for a new model call.
            self.clear_stream();
        }
        let region = &data[self.stream_offset as usize..];
        let complete_end: usize = region.rfind('\n').map(|i| i + 1).unwrap_or(0);
        for line in region[..complete_end].lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(delta) = rushi_common::stage::ModelDelta::from_json_line(line) {
                self.queue_stream_delta(delta);
            }
        }
        self.stream_offset += complete_end as u64;
    }

    /// Queue a new stream delta for the paced release. Content deltas
    /// (`text`, `reasoning`, `tool_call_delta`) enter `stream_pending`
    /// and are released into the live buffer a few characters at a
    /// time by [`App::pump_stream_pacing`]. `done` is applied
    /// immediately — the channel state flips now, and the queued
    /// content drains at once on the next pump, so the final text
    /// settles promptly instead of trickling.
    fn queue_stream_delta(&mut self, delta: rushi_common::stage::ModelDelta) {
        // The first delta of a call opens the live buffer (the header
        // row shows), even when the payload is empty.
        self.stream_buf.get_or_insert_with(StreamBuf::default);
        match delta {
            rushi_common::stage::ModelDelta::Text(t) => {
                if !t.is_empty() {
                    self.push_pending(StreamDeltaKind::Text, t);
                }
            }
            rushi_common::stage::ModelDelta::Reasoning {
                item_id,
                delta,
            } => {
                if !delta.is_empty() {
                    self.push_pending(StreamDeltaKind::Reasoning(item_id), delta);
                }
            }
            rushi_common::stage::ModelDelta::ToolCallDelta {
                call_id,
                name,
                args_delta,
            } => {
                if !args_delta.is_empty() {
                    self.push_pending(
                        StreamDeltaKind::ToolArgs(call_id, name),
                        args_delta,
                    );
                }
            }
            rushi_common::stage::ModelDelta::Done { .. } => {
                self.stream_buf
                    .as_mut()
                    .expect("opened above")
                    .done = true;
            }
        }
    }

    /// Append a delta to the pace queue and count its characters.
    fn push_pending(&mut self, kind: StreamDeltaKind, payload: String) {
        self.stream_pending_chars += payload.chars().count();
        self.stream_pending.push_back(PendingStreamDelta { kind, payload });
    }

    /// Release queued stream content into the live buffer at a smooth
    /// per-frame pace (docs/tui-streaming-response.md §6.5). The main
    /// loop calls this once per frame, right after
    /// [`App::refresh_stream`].
    ///
    /// Each call releases `max(1, backlog / STREAM_PACE_FRAMES)`
    /// characters: the backlog clears in about that many frames, so a
    /// fast stream lags by at most a fraction of a second and then
    /// catches up at a uniform rate, while a slow stream reads as a
    /// steady typewriter. Once the channel is `done`, the whole
    /// remainder releases at once. The release is strictly FIFO and
    /// character-bounded, so the displayed text is always a prefix of
    /// the channel's accumulated text (the P3 prefix growth of the
    /// Lean spec).
    pub fn pump_stream_pacing(&mut self) {
        if self.stream_pending.is_empty() {
            return;
        }
        let Some(buf) = self.stream_buf.as_mut() else {
            // Cleared by a settle event: the transcript is
            // authoritative, so drop the queue.
            self.clear_stream();
            return;
        };
        let backlog = self.stream_pending_chars;
        let budget = if buf.done {
            backlog
        } else {
            std::cmp::max(1, backlog / STREAM_PACE_FRAMES)
        };
        let mut remaining = budget;
        while remaining > 0 {
            let item = self
                .stream_pending
                .front()
                .cloned()
                .expect("the queue stays non-empty until it drains");
            let chars_left = item.payload.chars().count();
            if chars_left <= remaining {
                self.stream_pending.pop_front();
                self.stream_pending_chars -= chars_left;
                remaining -= chars_left;
                apply_paced(buf, &item.kind, &item.payload);
            } else {
                // Split at a character boundary: release the first
                // `remaining` characters, keep the rest queued.
                let take_len: usize = item
                    .payload
                    .chars()
                    .take(remaining)
                    .map(char::len_utf8)
                    .sum();
                let take = item.payload[..take_len].to_string();
                if let Some(front) = self.stream_pending.front_mut() {
                    front.payload = item.payload[take_len..].to_string();
                }
                self.stream_pending_chars -= remaining;
                apply_paced(buf, &item.kind, &take);
                remaining = 0;
            }
        }
    }

    /// True while a response is in flight: content is queued for the
    /// paced release, or the channel is still open. The main loop
    /// ticks at a high frame rate while this holds, so the paced text
    /// advances smoothly (docs/tui-streaming-response.md §6.5).
    pub fn stream_live(&self) -> bool {
        !self.stream_pending.is_empty()
            || self.stream_buf.as_ref().is_some_and(|b| !b.done)
    }

    /// The characters waiting in the pace queue (§6.5). Test and
    /// diagnostic use: the main loop decides the cadence with
    /// [`App::stream_live`].
    #[cfg(test)]
    pub fn stream_pending_chars(&self) -> usize {
        self.stream_pending_chars
    }

    // ── new-session name input ──────────────────────────────

    /// Begin the name input. Used at startup when `tui` runs without a
    /// session argument; editing keys route to the name until it is
    /// confirmed (Enter) or cancelled (Esc).
    pub fn start_naming(&mut self) {
        self.pending_name = Some(String::new());
    }

    /// The name being typed, when the name input is active.
    pub fn pending_name(&self) -> Option<&str> {
        self.pending_name.as_deref()
    }

    /// Swap the watcher receiver; the old tailer thread dies when its
    /// channel is dropped.
    pub fn set_watch_rx(&mut self, rx: std::sync::mpsc::Receiver<WatchItem>) {
        self.watch_rx = Some(rx);
    }

    pub fn drain_watch(&mut self) -> Option<WatchItem> {
        self.watch_rx.as_mut()?.try_recv().ok()
    }

    pub fn on_watch_item(&mut self, item: WatchItem) {
        match item {
            WatchItem::Event { event, .. } => {
                // A new ext_status event updates the map in place.
                // Later events win, like the log order. The id set
                // is capped: at the cap the least-recently-updated
                // id drops first.
                if event.kind() == EventKind::ExtStatus {
                    let value = event.get("value").cloned().unwrap_or(Value::Null);
                    if let Some(id) = event.get_str("id") {
                        self.record_ext_status(id, value, event.get_str("ts"));
                    }
                } else {
                    // A rendered event: the transcript total may
                    // grow (section 4.6). The browse sync uses this
                    // to tell event growth from a pane-rewrap
                    // growth (section 4.7).
                    self.events_grew = true;
                    // Settle the live stream buffer when the
                    // authoritative event lands (docs/tui-
                    // streaming-response.md §6.4).
                    if matches!(
                        event.kind(),
                        EventKind::AssistantMessage
                            | EventKind::Error
                            | EventKind::Cancel
                    ) {
                        self.clear_stream();
                    }
                }
                self.events.push(event);
                if self.events.len() > EVENTS_CAP {
                    let excess = self.events.len() - EVENTS_CAP;
                    self.events.drain(0..excess);
                }
                self.events_version += 1;
            }
            WatchItem::Gone => self.flash("session log missing — waiting for it to come back"),
            WatchItem::Resumed => self.flash("session log restored — resuming"),
            WatchItem::IoError { message } => self.flash(format!("log read error: {message}")),
        }
    }

    pub fn take_events_grew(&mut self) -> bool {
        let f = self.events_grew;
        self.events_grew = false;
        f
    }

    /// Set the transcript pane height (the renderer does this every
    /// frame). Used by the half-page keys Ctrl+U / Ctrl+D.
    pub fn set_viewport_height(&mut self, h: usize) {
        self.viewport = h;
    }

    /// The half-viewport scroll distance for Ctrl+U / Ctrl+D, like vim.
    /// A small or unset viewport falls back to the page-key distance.
    fn half_page(&self) -> usize {
        if self.viewport >= 4 {
            (self.viewport - 1) / 2
        } else {
            10
        }
    }

    /// The wrapped transcript lines at `width`, oldest first.
    ///
    /// Cached by (events_version, width, ext reply version): a scroll
    /// redraw or a draw with no new events reuses the cache instead of
    /// rewrapping every line. A new extension reply (or a session
    /// switch that clears the replies) bumps the ext version and
    /// rebuilds the lines.
    pub fn transcript_lines(
        &mut self,
        width: usize,
        ext: Option<&crate::ext::ExtHost>,
    ) -> &[Line<'static>] {
        let ext_ver = ext.map(|h| h.replies_version()).unwrap_or(0);
        if let Some((v, w, ev, cl, p, _)) = &self.transcript_cache {
            if *v == self.events_version
                && *w == width
                && *ev == ext_ver
                && *cl == self.palette.level()
                && *p == self.palette
            {
                return &self.transcript_cache.as_ref().unwrap().5;
            }
        }
        let lines = crate::render::build_transcript_lines(self, width, ext);
        self.transcript_cache = Some((
            self.events_version,
            width,
            ext_ver,
            self.palette.level(),
            self.palette.clone(),
            lines,
        ));
        &self.transcript_cache.as_ref().unwrap().5
    }

    /// Events of the active session, oldest first.
    pub fn events(&self) -> &[Event] {
        &self.events
    }


    /// Map tool_call id -> (name, arguments), for result rendering.
    /// The arguments are the call arguments verbatim: the write
    /// diff of docs/tui-tool-result-truncation.md needs the
    /// `content` argument of the write call, and the render holds
    /// it next to the result line (pure presentation lookup, not
    /// decision logic).
    pub fn call_details(&self) -> HashMap<String, (String, Value)> {
        let mut m: HashMap<String, (String, Value)> = HashMap::new();
        for e in &self.events {
            if e.kind() == EventKind::ToolCall {
                if let Some(id) = e.get_str("id") {
                    let name = e.get_str("name").unwrap_or("").to_string();
                    let args = e.get("arguments").cloned().unwrap_or(Value::Null);
                    m.insert(id.to_string(), (name, args));
                }
            }
        }
        m
    }

    /// Latest `ext_status` values, id to value, for the active
    /// session. The extension host sends this map in every `tick`
    /// op, so a statusline consumes shared UI state through the log
    /// (docs/ui-extension.md section 5). The map is maintained
    /// incrementally: each watch event updates it, so a tick reads
    /// O(1) state instead of a whole-log scan.
    pub fn ext_statuses(&self) -> &HashMap<String, Value> {
        &self.ext_status_values
    }

    /// Record one ext_status value in log order. A later event for
    /// the same id wins. The id set is capped:
    /// [`EXT_STATUS_ID_CAP`] distinct ids, oldest-updated first out.
    /// The timestamp side map follows: it records the event `ts` and
    /// drops an id with the value map.
    fn record_ext_status(&mut self, id: &str, value: Value, ts: Option<&str>) {
        if self
            .ext_status_values
            .insert(id.to_string(), value)
            .is_none()
        {
            self.ext_status_order.push_back(id.to_string());
            while self.ext_status_order.len() > EXT_STATUS_ID_CAP {
                let old = self
                    .ext_status_order
                    .pop_front()
                    .expect("the cap keeps the order non-empty");
                self.ext_status_values.remove(&old);
                self.ext_status_ts.remove(&old);
            }
        } else if let Some(pos) = self.ext_status_order.iter().position(|x| x == id) {
            self.ext_status_order.remove(pos);
            self.ext_status_order.push_back(id.to_string());
        }
        if let Some(t) = ts {
            self.ext_status_ts.insert(id.to_string(), t.to_string());
        }
    }

    /// The raw `ts` of the event that last set the [`LOOP_PHASE_STATUS_ID`]
    /// value of the active session. `None` when the log holds no
    /// marker, or the marker event carries no timestamp. The render
    /// parses the value with chrono; a parse failure hides the
    /// phase row, the title bit still shows the state
    /// (docs/tui-model-wait-indicator.md section 4).
    pub fn loop_phase_ts(&self) -> Option<&str> {
        self.ext_status_ts
            .get(LOOP_PHASE_STATUS_ID)
            .map(|s| s.as_str())
    }

    // ── thinking level ─────────────────────────────────────────

    /// The model's thinking level, 0 (none) through
    /// [`THINKING_LEVELS`] (the highest published). The level is
    /// published into the log as an `ext_status` event with id
    /// `model_thinking` (the shared-UI-state channel, docs/ui-
    /// extension.md section 5); a value outside the known range or a
    /// missing event falls back to the default. The input area's
    /// border color correlates to this value.
    pub fn thinking_level(&self) -> u32 {
        let v = self
            .ext_status_values
            .get(THINKING_STATUS_ID)
            .and_then(|v| v.as_u64());
        match v {
            Some(n) if (n as u32) < THINKING_LEVELS => n as u32,
            Some(_) => THINKING_LEVELS - 1,
            None => DEFAULT_THINKING_LEVEL,
        }
    }

    // ── scroll ──────────────────────────────────────────────────

    /// Visual lines kept between the viewport top and the end of the
    /// log. 0 = follow the tail.
    pub fn scroll(&self) -> usize {
        self.scroll
    }

    pub fn scroll_up(&mut self, lines: usize) {
        self.scroll = self.scroll.saturating_add(lines).min(SCROLL_CAP);
    }

    pub fn scroll_down(&mut self, lines: usize) {
        self.scroll = self.scroll.saturating_sub(lines);
    }

    pub fn set_scroll(&mut self, s: usize) {
        self.scroll = s.min(SCROLL_CAP);
    }

    // ── browse mode (docs/tui-conversation-browsing.md) ──────

    /// The browse state machine (section 4).
    pub fn browse(&mut self) -> &mut crate::browse::Browse {
        &mut self.browse
    }

    /// The browse state, read-only.
    pub fn browse_ref(&self) -> &crate::browse::Browse {
        &self.browse
    }

    /// The browse layout of the last render: `(total, h, text_w,`
    /// `texts)`. The renderer refreshes it each frame while browse is
    /// active; browse motions and the search read it (section 4.1).
    pub fn set_browse_layout(
        &mut self,
        total: usize,
        h: usize,
        text_w: usize,
        texts: Vec<String>,
    ) {
        self.browse_layout = Some((total, h, text_w, texts));
    }

    /// The browse highlight inputs for one frame: the match-line
    /// cache (cloned: the cache borrows the machine) and the active
    /// match, owned so no app borrow crosses the lines borrow in the
    /// renderer.
    pub fn browse_highlight(
        &mut self,
        total: usize,
    ) -> (std::collections::HashSet<usize>, Option<(usize, usize)>) {
        match &self.browse_layout {
            Some((_, _, _, texts)) => {
                let (hl, am) = self.browse.highlight_lines(total, texts.as_slice());
                (hl.clone(), am)
            }
            None => (std::collections::HashSet::new(), None),
        }
    }

    /// The total of the last browse layout: the gutter width hint of
    /// the width fixpoint (section 4.3).
    pub fn browse_layout_total(&self) -> usize {
        self.browse_layout.as_ref().map(|l| l.0).unwrap_or(0)
    }

    /// The browse entry gate: the same two conditions as the `q q`
    /// exit path (section 4.2): an empty draft, the editor in normal
    /// mode, and no name input up.
    fn browse_gate_open(&self) -> bool {
        self.pending_name.is_none()
            && self.editor.mode() == Mode::Normal
            && self.editor.text().trim().is_empty()
    }

    /// One browse-owned key (section 4.4): the key table over the
    /// rendered transcript. The view comes from the last rendered
    /// layout, so a key before the first frame is a no-op.
    fn browse_key(&mut self, key: Key) {
        // The double-`s` exit arm (section 4.2): the shared arm
        // window, the FT-012 mirror.
        if let Key::Char('s') = key {
            match self.ss_arm {
                Some(at) if at.elapsed() < SS_ARM_TTL => {
                    self.ss_arm = None;
                    self.browse.exit();
                    self.flash("browse left — view kept");
                }
                _ => {
                    self.ss_arm = Some(Instant::now());
                    self.flash("ss: press s again to leave browse");
                }
            }
            return;
        }
        let (total, h, texts) = match &self.browse_layout {
            Some((total, h, _w, texts)) => (*total, *h, texts.as_slice()),
            None => return,
        };
        let half = self.half_page();
        let mut view = crate::browse::View {
            total,
            h,
            scroll: &mut self.scroll,
            half,
            texts,
        };
        if let Some(hint) = self.browse.key(key, &mut view) {
            self.flash(hint);
        }
    }

    // ── @ file picker (docs/tui-file-picker.md) ─────────────────

    /// The picker state machine, read-only.
    pub fn picker_ref(&self) -> &crate::picker::state::PickerState {
        &self.picker
    }

    /// The picker state machine, mutable. The renderer reads it; the
    /// key handler mutates it through the public method on the state.
    pub fn picker(&mut self) -> &mut crate::picker::state::PickerState {
        &mut self.picker
    }

    /// The background ranker, for the renderer to read the latest
    /// snapshot without blocking.
    pub fn picker_matcher_ref(&self) -> &Option<crate::picker::fuzzy::PickerMatcher> {
        &self.picker_matcher
    }

    /// Open the picker: enumerate files for the seed query's root,
    /// spawn the matcher, open the state, and push the initial query.
    fn open_picker(&mut self) {
        if self.picker.open {
            return;
        }
        let cwd = std::env::current_dir().unwrap_or_default();
        // The source is a `FileItemSource` for now; later sources
        // (symbols, git files) plug in at this seam
        // (docs/tui-file-picker.md section 4.5).
        let source = crate::picker::items::FileItemSource::new(cwd.clone());
        // Seed the query from the editor's `@` token if present.
        let (at_col, query) = match self.editor().at_token_info() {
            Some(info) => info,
            None => return,
        };
        self.picker_at = Some((self.editor().row, at_col));
        // Path queries (`/abs`, `~/x`, `../y`) re-root the search;
        // plain queries stay under the cwd.
        let (root, tail) = crate::picker::items::query_root_tail(&query, &cwd);
        let items = source.collect_in(&root, self.picker.scope);
        let matcher = crate::picker::fuzzy::PickerMatcher::new(items);
        self.picker.open(&query, 10);
        if !tail.is_empty() {
            matcher.query_ranked(&query, &tail);
        }
        self.picker_source = Some(source);
        self.picker_root = root;
        self.picker_matcher = Some(matcher);
    }

    /// Keep the picker in sync with the editor `@query` token:
    /// - token present and picker open: refresh the query in the
    ///   matcher (re-roots when the query crosses into a new path).
    /// - token gone and picker open: close the picker (the draft keeps
    ///   the text as typed; nothing is replaced).
    ///
    /// The picker only *opens* in direct response to a freshly typed
    /// `@` keypress (see the key handler below); a stale `@` left in
    /// the draft after a previous pick or dismiss is inert and does
    /// not re-trigger the overlay.
    fn sync_picker(&mut self) {
        if !self.picker.open {
            return;
        }
        let (row, at_col) = match self.picker_at {
            Some(p) => p,
            None => {
                self.picker.close();
                self.picker_matcher = None;
                self.picker_source = None;
                return;
            }
        };
        // Verify the `@` is still at the tracked position.
        let chars: Vec<char> = match self.editor.lines.get(row) {
            Some(line) => line.chars().collect(),
            None => {
                self.picker.close();
                self.picker_matcher = None;
                self.picker_source = None;
                self.picker_at = None;
                return;
            }
        };
        if chars.get(at_col) != Some(&'@') {
            // The `@` was deleted or the line changed; close the picker.
            self.picker.close();
            self.picker_matcher = None;
            self.picker_source = None;
            self.picker_at = None;
            return;
        }
        // Compute the query: text from just after the `@` to the cursor.
        let cursor_col = self.editor.col.min(chars.len());
        let query = if cursor_col > at_col + 1 {
            chars[at_col + 1..cursor_col].iter().collect()
        } else {
            String::new()
        };
        self.picker.query = query.clone();
        self.update_picker_ranker(&query);
    }

    /// Re-root and re-rank the picker when the `@` query changes.
    /// Path queries (`/abs`, `~/x`, `../y`) move the search root;
    /// a changed root re-enumerates the item list before the rank.
    fn update_picker_ranker(&mut self, query: &str) {
        let Some(src) = self.picker_source.as_ref() else {
            return;
        };
        let base = src.base().to_path_buf();
        let (root, tail) = crate::picker::items::query_root_tail(query, &base);
        let need_replace = root != self.picker_root;
        let new_items = if need_replace {
            Some(src.collect_in(&root, self.picker.scope))
        } else {
            None
        };
        if let Some(m) = &mut self.picker_matcher {
            if let Some(items) = new_items {
                m.replace_items(items);
                self.picker_root = root;
            }
            m.query_ranked(query, &tail);
        }
    }

    /// Re-enumerate the picker's item list after `Ctrl+I` / `Tab`
    /// cycled the file scope (docs/tui-file-picker.md P9): collect the
    /// current search root at the new scope, swap the matcher's list,
    /// and re-rank the live query against the fresh list. The flash
    /// line names the new mode so the user sees the switch without
    /// inspecting the list.
    fn recollect_picker_items(&mut self) {
        let Some(src) = self.picker_source.as_ref() else {
            return;
        };
        let base = src.base().to_path_buf();
        let scope = self.picker.scope;
        let items = src.collect_in(&self.picker_root, scope);
        let (_, tail) = crate::picker::items::query_root_tail(&self.picker.query, &base);
        if let Some(m) = &mut self.picker_matcher {
            m.replace_items(items);
            m.query_ranked(&self.picker.query, &tail);
        }
        self.flash(scope.hint());
    }

    /// Commit the picker: replace the `@query` token with the chosen
    /// item's value prefixed with `@` (the model sees `@path` as an
    /// explicit file reference). Zero results: keep the `@` and query
    /// text as-is in the draft — no stripping. The caller already
    /// closed the state; this drops the matcher. The caret stays at
    /// the replacement end.
    fn commit_picker(&mut self, sel: Option<usize>) {
        match sel {
            Some(idx) => {
                if let Some(m) = &self.picker_matcher {
                    let snap = m.snapshot();
                    if let Some(item) = snap.items.get(idx) {
                        if let Some((_, at_col)) = self.picker_at {
                            // Keep the `@` so the model receives an
                            // unambiguous file-reference marker.
                            self.editor()
                                .replace_at_token(at_col, &format!("@{}", item.value));
                        }
                    }
                }
            }
            None => {
                // Zero results: keep the `@` and query text as-is in
                // the draft. The user can keep editing or delete the
                // text manually.
            }
        }
        self.picker_matcher = None;
        self.picker_at = None;
    }

    // ── command palette ────────────────────────────────────────

    pub fn palette_state(&self) -> &crate::palette::state::PaletteState {
        &self.palette_state
    }

    pub fn palette_state_mut(&mut self) -> &mut crate::palette::state::PaletteState {
        &mut self.palette_state
    }

    /// Open the `:` command palette. The pending_name bar takes
    /// priority: if it is active the palette does not open.
    pub fn open_palette(&mut self) {
        if self.pending_name.is_some() {
            return;
        }
        self.palette_state.open(self.viewport.saturating_sub(4).max(5));
        self.palette_cmd_requested = true;
    }

    /// The full un-rank palette item list (built-ins + extension commands).
    pub fn palette_items(&self) -> Vec<crate::palette::items::PaletteItem> {
        let mut items = crate::palette::items::builtins(&self.effort_current);
        items.extend(self.ext_commands.iter().cloned());
        items
    }

    /// The ranked palette items for the current stage and query.
    pub fn palette_ranked(&self) -> Vec<crate::palette::items::PaletteItem> {
        let state = &self.palette_state;
        if state.stage == crate::palette::state::PaletteStage::SessionList {
            let filter = state.filter_query();
            let items: Vec<crate::palette::items::PaletteItem> = self
                .sessions
                .iter()
                .enumerate()
                .map(|(i, sid)| {
                    let is_active = self.active.as_ref() == Some(sid);
                    let running = self.loop_running(sid);
                    let mut help = String::new();
                    help.push_str(&format!("recency {} of {}\n", i + 1, self.sessions.len()));
                    help.push_str(&format!(
                        "active: {}\n",
                        if is_active { "yes" } else { "no" }
                    ));
                    help.push_str(&format!(
                        "loop: {}\n",
                        if running { "running" } else { "stopped" }
                    ));
                    crate::palette::items::PaletteItem {
                        id: sid.as_str().to_string(),
                        label: sid.as_str().to_string(),
                        kind: crate::palette::items::CmdKind::Goto,
                        hint: if is_active { "active".to_string() } else { String::new() },
                        help,
                        options: Vec::new(),
                        ext: None,
                    }
                })
                .collect();
            // Rank by the filter portion of the query (text after the goto prefix).
            let labels: Vec<String> = items.iter().map(|i| i.label.clone()).collect();
            let ranked = crate::picker::fuzzy::rank_fuzzy(&labels, filter);
            ranked
                .into_iter()
                .map(|i| items[i].clone())
                .collect()
        } else {
            let items = self.palette_items();
            let labels: Vec<String> = items.iter().map(|i| {
                format!("{} {}", i.label, i.hint)
            })
            .collect();
            let query = state.filter_query();
            let ranked = crate::picker::fuzzy::rank_fuzzy(&labels, query);
            ranked
                .into_iter()
                .map(|i| items[i].clone())
                .collect()
        }
    }

    /// Handle the `Enter` key when the palette is open. Returns the
    /// actions to execute (if any). The caller (press) dispatches
    /// them through the main loop.
    pub fn commit_palette(&mut self) -> Vec<Action> {
        let state = self.palette_state();
        let ranked = self.palette_ranked();
        let cursor = state.cursor().min(ranked.len().saturating_sub(1));
        let item = match ranked.get(cursor) {
            Some(item) => item,
            None => {
                self.palette_state_mut().close();
                return Vec::new();
            }
        };
        use crate::palette::items::CmdKind;
        match item.kind {
            CmdKind::Goto => {
                // Entering the session sub-list (Root stage only).
                if state.stage == crate::palette::state::PaletteStage::Root {
                    self.palette_state_mut().goto_session_list();
                } else {
                    // Session sub-list: switch session.
                    self.palette_state_mut().close();
                    return vec![Action::SwitchSession(item.id.clone())];
                }
                Vec::new()
            }
            CmdKind::Run => {
                self.palette_state_mut().close();
                match item.id.as_str() {
                    "toggle-tools" => {
                        self.tool_expanded = !self.tool_expanded;
                        self.events_version += 1;
                        vec![Action::ToggleToolExpand]
                    }
                    "toggle-thinking" => {
                        self.thinking_shown = !self.thinking_shown;
                        self.events_version += 1;
                        vec![Action::ToggleThinking]
                    }
                    "expand-thinking" => {
                        self.thinking_expanded = !self.thinking_expanded;
                        self.events_version += 1;
                        vec![Action::ToggleThinkingExpand]
                    }
                    "bn" => vec![Action::CycleSessions(1)],
                    "bp" => vec![Action::CycleSessions(-1)],
                    "new-session" => {
                        self.start_naming();
                        Vec::new()
                    }
                    "edit-queue" => vec![Action::RecallQueue],
                    "e" => vec![Action::OpenEditor],
                    "q" => vec![Action::Quit],
                    _ => Vec::new(),
                }
            }
            CmdKind::Set => {
                // Pick the option at option_cursor.
                let opts = &item.options;
                let idx = state
                    .option_cursor
                    .min(opts.len().saturating_sub(1));
                let value = opts.get(idx).map(|o| o.value.clone()).unwrap_or_default();
                self.palette_state_mut().close();
                match item.id.as_str() {
                    "thinking-level" => vec![Action::SetEffort(value)],
                    _ => Vec::new(),
                }
            }
            CmdKind::Ext => {
                let value = if item.options.is_empty() {
                    None
                } else {
                    let opts = &item.options;
                    let idx = state
                        .option_cursor
                        .min(opts.len().saturating_sub(1));
                    opts.get(idx)
                        .map(|o| o.value.clone())
                };
                let ext_name = item.ext.clone().unwrap_or_default();
                // Extract the command id (part after the first dot).
                let cmd_id = item.id.split('.').nth(1).unwrap_or(&item.id).to_string();
                self.palette_state_mut().close();
                vec![Action::InvokeExtCommand {
                    ext: ext_name,
                    id: cmd_id,
                    value,
                }]
            }
        }
    }

    pub fn set_ext_commands(&mut self, items: Vec<crate::palette::items::PaletteItem>) {
        self.ext_commands = items;
    }

    pub fn drop_ext_commands(&mut self, ext_name: &str) {
        self.ext_commands.retain(|i| i.ext.as_deref() != Some(ext_name));
    }

    pub fn set_effort_current(&mut self, effort: String) {
        self.effort_current = effort;
    }

    /// Update the in-memory thinking level so the input-area border
    /// colour reflects the new value immediately (docs/tui-thinking-
    /// block.md section 4).
    pub fn set_thinking_level(&mut self, level: u32) {
        let value = serde_json::Value::from(level);
        self.record_ext_status(THINKING_STATUS_ID, value, None);
    }

    pub fn palette_cmd_requested(&mut self) -> bool {
        let requested = self.palette_cmd_requested;
        self.palette_cmd_requested = false;
        requested
    }

    /// Auto-enter the Goto sub-stage when the user types a space
    /// after a Goto command label in the Root stage. This makes the
    /// natural ":b <session-name>" flow work without a separate
    /// Enter press (docs/tui-command-palette.md section 7).
    pub fn maybe_autogoto(&mut self) {
        let st = &self.palette_state;
        if !st.open || st.stage != crate::palette::state::PaletteStage::Root {
            return;
        }
        let query = st.query.clone();
        let space_pos = match query.find(' ') {
            Some(p) => p,
            None => return,
        };
        let token = &query[..space_pos];
        let items = crate::palette::items::builtins(&self.effort_current);
        let is_goto = items.iter().any(|i| {
            i.kind == crate::palette::items::CmdKind::Goto && i.label == token
        });
        if is_goto {
            self.palette_state_mut().goto_session_list();
        }
    }

    /// Bulk recall: load the combined text of all pending user
    /// messages into the editor, move the cursor to the end, and
    /// switch to insert mode (docs/user-message-editing.md).
    /// Returns the ids of the retracted messages so the caller can
    /// append `user_message_retract` events to the log.
    pub fn recall_queue(&mut self) -> Vec<String> {
        let pending: Vec<(String, String, Option<String>)> = self
            .pending_user_messages()
            .into_iter()
            .map(|e| {
                let queue = e.get_str("queue").unwrap_or("steer").to_string();
                let content = e.get_str("content").unwrap_or("").to_string();
                let id = e.get_str("id").map(String::from);
                (queue, content, id)
            })
            .collect();
        if pending.is_empty() {
            self.flash("no pending messages to recall");
            return Vec::new();
        }
        // Build the joined text with queue markers.
        let blocks: Vec<String> = pending
            .iter()
            .map(|(queue, content, _)| format!("[{queue}] {content}"))
            .collect();
        let combined = blocks.join("\n\n");
        let n = pending.len();
        self.editor().set_text(&combined);
        // Move the cursor to the end of the text.
        let ed = self.editor();
        let last_row = ed.lines.len().saturating_sub(1);
        ed.row = last_row;
        ed.col = ed.lines[last_row].chars().count();
        ed.mode = crate::vim_editor::Mode::Insert;
        self.flash(format!(
            "recalled {n} queued message(s) into the editor"
        ));
        pending.into_iter().filter_map(|(_, _, id)| id).collect()
    }

    // ── draft / editor ──────────────────────────────────────────

    /// The message editor: multi-line textarea plus the vim modal
    /// state machine (docs/tui.md section 7.1; modes
    /// normal/insert/replace/visual, operator-pending).
    pub fn editor(&mut self) -> &mut Editor {
        &mut self.editor
    }

    /// The editor's scroll window, so the renderer keeps the cursor
    /// line in view.
    pub fn edit_scroll(&self) -> usize {
        self.edit_scroll
    }

    /// How many display rows the draft wraps to at `width` columns
    /// (at least 1). The renderer sizes the input box to this so a
    /// long line wraps to the box instead of running off the edge, and
    /// a multi-line message is shown in full, not just a two-line
    /// scroll window.
    pub fn draft_lines(&self, width: usize) -> usize {
        self.editor.display_row_count(width)
    }

    /// Scroll the editor window so the cursor row is inside a window
    /// of `height` display rows. A short draft keeps scroll 0; a long
    /// one follows the cursor. `width` is the box interior width, so a
    /// wrapped cursor line scrolls on display rows, not logical lines.
    pub fn editor_scroll_to_cursor(&mut self, height: usize, width: usize) {
        let row = self.editor.cursor_display(width).0;
        let window = height.max(1).saturating_sub(1);
        if row <= window {
            self.edit_scroll = 0;
        } else if row.saturating_sub(window) > self.edit_scroll {
            self.edit_scroll = row.saturating_sub(window);
        } else if row < self.edit_scroll {
            self.edit_scroll = row;
        }
        // Never scroll past the end of the text.
        let max_scroll = self.editor.display_row_count(width).saturating_sub(window);
        self.edit_scroll = self.edit_scroll.min(max_scroll);
    }

    /// The draft text as it would be sent: the editor lines joined
    /// with newlines, trimmed.
    pub fn draft(&self) -> String {
        self.editor.text().trim().to_string()
    }

    /// The quit gate (docs/tui.md section 7, FT-012): the quit key
    /// (`q`, `Ctrl+Q`) arms and fires only in normal mode with an
    /// empty draft, like the pi Ctrl-d rule that blocks the exit
    /// key over a live prompt. In every other state `q` is plain
    /// text for the editor.
    fn quit_gate_open(&self) -> bool {
        self.editor.mode() == Mode::Normal && self.editor.text().trim().is_empty()
    }

    /// Take the draft for sending, leaving the editor empty.
    pub fn take_draft(&mut self) -> String {
        let t = self.editor.text().trim().to_string();
        self.editor.clear();
        self.edit_scroll = 0;
        t
    }

    pub fn set_draft(&mut self, text: String) {
        self.editor.set_text(&text);
        self.edit_scroll = 0;
    }

    /// The editor's modal state label, for the status row
    /// (`[NORMAL]`, `[INSERT]`, ...; `[d-PENDING]` while an
    /// operator waits for its motion). Mirrors the pi-vim
    /// `formatStatus` output.
    pub fn editor_mode_label(&self) -> String {
        if let Some(p) = self.editor.pending_label() {
            p
        } else {
            format!("[{}]", self.editor.mode().label())
        }
    }

    // ── handoff ───────────────────────────────────────────────

    /// The pending handoff of the active session's log: the
    /// `new_session` of the last `context_exhausted` event that
    /// seeded a session. `None` when the log holds no marker, or the
    /// last marker seeded none (the summary call failed). A marker
    /// followed by a user message is superseded by that turn's own
    /// marker, closer to the log end.
    pub fn pending_handoff(&self) -> Option<String> {
        self.events
            .iter()
            .rev()
            .find(|e| e.kind() == EventKind::ContextExhausted)
            .and_then(|e| {
                e.get_str("new_session")
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            })
    }

    // ── pending user messages ─────────────────────────────────

    /// The unconsumed `user_message` events of the active session,
    /// in log order. A message is consumed when the loop answers
    /// it: an `assistant_message` event follows it in the log. A
    /// message sent while the loop is busy stays pending until the
    /// next step answers it (the loop's steering behavior;
    /// docs/tui_feature_requests_from_human.md 2026-08-31, stage 1).
    /// The derivation is log-only, like
    /// [`App::oldest_pending_approval`]: it survives a TUI restart.
    pub fn pending_user_messages(&self) -> Vec<&Event> {
        let last_answered = self
            .events
            .iter()
            .rposition(|e| e.kind() == EventKind::AssistantMessage);
        let from = last_answered.map(|i| i + 1).unwrap_or(0);
        let retracted: std::collections::HashSet<String> = self
            .events
            .iter()
            .filter(|e| e.kind() == EventKind::UserMessageRetract)
            .filter_map(|e| e.get_str("target").map(String::from))
            .collect();
        self.events[from..]
            .iter()
            .filter(|e| {
                e.kind() == EventKind::UserMessage
                    && e.get_str("id").map(|id| !retracted.contains(id)).unwrap_or(true)
            })
            .collect()
    }

    /// The pending `steer` queue: the unconsumed messages without the
    /// `follow` marker (a missing field means steer; stage 2 of
    /// docs/tui-pending-user-messages.md). They inject at the next
    /// step of the running loop.
    pub fn pending_steering(&self) -> Vec<&Event> {
        self.pending_user_messages()
            .into_iter()
            .filter(|e| e.get_str("queue") != Some("follow"))
            .collect()
    }

    /// The pending `follow` queue: the unconsumed messages marked
    /// `queue: "follow"` (stage 2 of docs/tui-pending-user-messages.
    /// md). They run only after the loop would stop, as new turns.
    pub fn pending_follows(&self) -> Vec<&Event> {
        self.pending_user_messages()
            .into_iter()
            .filter(|e| e.get_str("queue") == Some("follow"))
            .collect()
    }

    // ── approvals ───────────────────────────────────────

    /// Oldest pending `approval_request`, or `None` when the log has no
    /// unanswered request. Derivation is log-only (G6: pending state
    /// survives a TUI restart and is answered from the log alone).
    pub fn oldest_pending_approval(&self) -> Option<PendingApproval> {
        let approvals: Vec<(usize, String)> = self
            .events
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                if e.kind() == EventKind::Approval {
                    e.get_str("id").map(|id| (i, id.to_string()))
                } else {
                    None
                }
            })
            .collect();
        for (i, e) in self.events.iter().enumerate() {
            if e.kind() != EventKind::ApprovalRequest {
                continue;
            }
            let Some(request_id) = e.get_str("id").map(|s| s.to_string()) else {
                continue;
            };
            let answered = approvals.iter().any(|(j, id)| *j > i && id == &request_id);
            if answered {
                continue;
            }
            let call_id = e.get_str("call_id").map(|s| s.to_string());
            let arguments = call_id.as_ref().and_then(|cid| {
                self.events
                    .iter()
                    .find(|e| {
                        e.kind() == EventKind::ToolCall && e.get_str("id") == Some(cid.as_str())
                    })
                    .and_then(|e| e.get("arguments").cloned())
            });
            return Some(PendingApproval {
                prompt: e.get_str("prompt").map(|s| s.to_string()),
                request_id,
                call_id,
                arguments,
            });
        }
        None
    }

    // ── loops ───────────────────────────────────────────────────

    pub fn attach_loop(
        &mut self,
        sid: SessionId,
        handle: Box<dyn LoopHandle>,
        lines: tokio::sync::mpsc::UnboundedReceiver<LoopLine>,
    ) {
        let st = self.loops.entry(sid).or_default();
        st.handle = Some(handle);
        st.lines_rx = Some(lines);
        st.running = true;
        st.exit_code = None;
    }

    /// Register a live loop this TUI did not start (FT-003): the
    /// persistent probe found a live group for the session. The state
    /// marks the session running without a local handle. Stop goes
    /// through the port's external path. Idempotent.
    pub fn attach_external_loop(&mut self, sid: SessionId) {
        let st = self.loops.entry(sid).or_default();
        st.running = true;
        st.exit_code = None;
    }

    /// Clear the running flag of one session's loop state. The
    /// persistent probe found no live group (FT-003). A session with
    /// no loop state is a no-op.
    pub fn clear_loop_running(&mut self, sid: &SessionId) {
        if let Some(st) = self.loops.get_mut(sid) {
            st.running = false;
        }
    }

    /// True when the session's loop runs without a local handle: this
    /// TUI did not start it, the persistent probe reattached it.
    pub fn is_external_loop(&self, sid: &SessionId) -> bool {
        self.loop_state(sid)
            .is_some_and(|s| s.running && s.handle.is_none())
    }

    pub fn loop_state(&self, sid: &SessionId) -> Option<&LoopState> {
        self.loops.get(sid)
    }

    pub fn loop_running(&self, sid: &SessionId) -> bool {
        self.loop_state(sid).is_some_and(|s| s.running)
    }

    /// Mutable access to one session's loop state (taking the handle
    /// out on stop).
    pub fn loops_mut_for(&mut self, sid: &SessionId) -> Option<&mut LoopState> {
        self.loops.get_mut(sid)
    }

    pub fn running_loops(&self) -> usize {
        self.loops.values().filter(|s| s.running).count()
    }

    pub fn other_running_loops(&self) -> usize {
        self.running_loops()
            - self
                .active
                .as_ref()
                .map(|a| usize::from(self.loop_running(a)))
                .unwrap_or(0)
    }

    /// Drain loop output lines for every running loop.
    pub fn drain_loop_lines(&mut self) -> Vec<(SessionId, LoopLine)> {
        let mut out = Vec::new();
        for (sid, st) in self.loops.iter_mut() {
            let Some(rx) = st.lines_rx.as_mut() else {
                continue;
            };
            while let Ok(line) = rx.try_recv() {
                match &line {
                    LoopLine::Exited(code) => {
                        st.running = false;
                        st.exit_code = Some(*code);
                    }
                    LoopLine::Stdout(s) | LoopLine::Stderr(s) => {
                        st.last_line = Some(s.clone());
                    }
                }
                out.push((sid.clone(), line));
            }
        }
        out
    }

    /// Take all loop handles so the caller can stop them before exit.
    pub fn detach_all_handles(&mut self) -> Vec<(SessionId, Box<dyn LoopHandle>)> {
        let mut out = Vec::new();
        for (sid, st) in self.loops.iter_mut() {
            if let Some(h) = st.handle.take() {
                out.push((sid.clone(), h));
            }
            st.running = false;
        }
        out
    }

    // ── status line ─────────────────────────────────────────────

    pub fn flash(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), Instant::now()));
    }

    /// The transient status message, if it is still fresh.
    pub fn status(&self) -> Option<&str> {
        self.status
            .as_ref()
            .and_then(|(msg, at)| (at.elapsed() < STATUS_TTL).then_some(msg.as_str()))
    }

    // ── key handling ────────────────────────────────────────────

    /// Handle one key. Returns the port-level actions for `main` to
    /// execute. Keys that only touch local state (typing, scroll)
    /// return an empty list.
    pub fn press(&mut self, key: Key) -> Vec<Action> {
        if self.quitting {
            return Vec::new();
        }
        // Any key other than the second `q` disarms a pending quit.
        if key != Key::Quit {
            self.quit_arm = None;
        }
        // Any key other than the second `s` disarms a pending browse
        // arm (the FT-012 mirror, section 4.2).
        if !matches!(key, Key::Char('s')) {
            self.ss_arm = None;
        }
        // The browse overlay owns its key table while it holds
        // (section 4.4). The host roles fall through: the loop keys,
        // the toggles, `Tab` (which leaves the mode first), and the
        // `q q` quit gate.
        if self.browse.active() {
            match key {
                Key::Tab | Key::BackTab => {
                    // Leave browse (section 4.4). Tab is free for the
                    // picker; no session cycle for now.
                    self.browse.exit();
                    self.ss_arm = None;
                    return Vec::new();
                }
                Key::CtrlC => {
                    // The host stop role: the editor stays frozen
                    // under the overlay, so no session means a
                    // flash instead of an editor insert-exit.
                    if self.active().is_none() {
                        self.flash("no session to stop the loop for");
                        return Vec::new();
                    }
                    return vec![Action::StopLoop];
                }
                Key::CtrlR => {
                    // The host start role (docs/tui.md key Ctrl+R).
                    if self.active().is_none() {
                        self.flash("no session to run the loop for");
                        return Vec::new();
                    }
                    return vec![Action::RunLoop];
                }
                // The unowned keys act in the browse state machine.
                Key::Quit
                | Key::CtrlO
                | Key::CtrlT
                | Key::CtrlX
                | Key::CtrlF
                | Key::CtrlL => {}
                _ => {
                    self.browse_key(key);
                    return Vec::new();
                }
            }
        }
        // The `@` picker overlay owns its key table while open
        // (docs/tui-file-picker.md section 5). The host roles fall
        // through: Ctrl+C, Ctrl+R, and Tab.
        if self.picker.open {
            let count = self.picker_matcher.as_ref()
                .map(|m| m.snapshot().items.len())
                .unwrap_or(0);
            match key {
                // Backspace and printable chars edit the draft: the
                // editor owns the text, and the picker re-syncs its
                // query from the `@` token.
                Key::Backspace => {
                    if let Some(h) = self.editor().press(Key::Backspace) {
                        self.flash(h);
                    }
                    self.sync_picker();
                    return Vec::new();
                }
                Key::CtrlJ | Key::CtrlK => {
                    let _ = self.picker.press(
                        &key,
                        count,
                        crate::picker::render::PREVIEW_PAGE,
                        crate::picker::render::PREVIEW_CUTOFF,
                    );
                    return Vec::new();
                }
                Key::Char(c) => {
                    if let Some(h) = self.editor().press(Key::Char(c)) {
                        self.flash(h);
                    }
                    self.sync_picker();
                    return Vec::new();
                }
                // Host keys pass through even while the picker is open.
                // `Tab` is deliberately not here: in a standard terminal
                // it is the `Ctrl+I` key (both send byte 0x09, which
                // crossterm parses as `KeyCode::Tab`), and the picker
                // binds it to the file-scope cycle (docs/tui-file-picker.md
                // P9), so it reaches the state machine below.
                Key::Quit | Key::CtrlC | Key::CtrlR
                | Key::BackTab => {}
                // Esc, Enter, arrows, paging, preview keys: the state
                // machine decides (docs/tui-file-picker.md section 5).
                _ => {
                    match self.picker.press(
                        &key,
                        count,
                        crate::picker::render::PREVIEW_PAGE,
                        crate::picker::render::PREVIEW_CUTOFF,
                    ) {
                        crate::picker::state::PickAction::Commit(sel) => {
                            self.commit_picker(sel);
                        }
                        crate::picker::state::PickAction::Closed => {
                            self.picker_matcher = None;
                            self.picker_at = None;
                            // No stripping: the `@` stays in the draft
                            // as inert text. The picker only reopens
                            // on a freshly typed `@` keypress.
                            self.picker.close();
                            self.flash("picker closed — draft kept");
                        }
                        crate::picker::state::PickAction::Query => {
                            // Re-rank after an in-state query edit.
                            if let Some(m) = &self.picker_matcher {
                                m.query(&self.picker.query);
                            }
                        }
                        crate::picker::state::PickAction::Move
                        | crate::picker::state::PickAction::ScrollPreview
                        | crate::picker::state::PickAction::TogglePreview
                        | crate::picker::state::PickAction::Nothing => {}
                        crate::picker::state::PickAction::Recollect => {
                            // `Ctrl+I` / `Tab` cycled the file scope
                            // (docs/tui-file-picker.md P9): re-enumerate
                            // the current search root at the new scope,
                            // re-rank the live query, and flash the
                            // scope line so the user sees the switch.
                            self.recollect_picker_items();
                        }
                    }
                    return Vec::new();
                }
            }
            // Ctrl+C, Ctrl+R, and Tab fall through to the normal handler.
        }
        // The `:` command palette owns its key table while open
        // (docs/tui-command-palette.md).
        if self.palette_state.open {
            let items = self.palette_ranked();
            let highlighted = if items.is_empty() { 0 } else {
                let cursor = self.palette_state.cursor().min(items.len() - 1);
                items[cursor].options.len()
            };
            let n = items.len();
            // `q` is mapped to `Key::Quit` in main.rs. Inside the palette,
            // `q` is a filter character (types into the query to match the
            // quit command), not the quit gate. Normalize it to a char.
            let palette_key = if key == Key::Quit {
                Key::Char('q')
            } else {
                key
            };
            let pa = self.palette_state.press(
                &palette_key,
                n,
                highlighted,
                crate::picker::render::PREVIEW_PAGE,
                crate::picker::render::PREVIEW_CUTOFF,
            );
            use crate::palette::state::PaletteAction;
            match pa {
                PaletteAction::Query => {
                    // Auto-transition: when the user types a space
                    // after a Goto item's label in Root stage, enter
                    // the Goto sub-stage so the typed text filters
                    // the session list (docs/tui-command-palette.md
                    // section 7: "The typed text after b filters
                    // the list").
                    self.maybe_autogoto();
                    return Vec::new();
                }
                PaletteAction::Move
                | PaletteAction::OptionMove
                | PaletteAction::ScrollPreview
                | PaletteAction::TogglePreview
                | PaletteAction::Nothing => {
                    return Vec::new();
                }
                PaletteAction::Commit => {
                    return self.commit_palette();
                }
                PaletteAction::DropSubStage => {
                    // Stay open, back to root stage.
                    return Vec::new();
                }
                PaletteAction::Closed => {
                    // Palette closed; fall through to normal handling.
                    return Vec::new();
                }
            }
        }
        // The name input swallows editing keys while it is active.
        // Other keys (q, Ctrl+R, Tab, ...) fall through unchanged.
        if self.pending_name.is_some() {
            match key {
                Key::Char(c) => {
                    self.pending_name.as_mut().unwrap().push(c);
                    return Vec::new();
                }
                Key::Backspace => {
                    self.pending_name.as_mut().unwrap().pop();
                    return Vec::new();
                }
                Key::Esc => {
                    self.pending_name = None;
                    self.flash(
                        "name input cancelled — pass a session argument",
                    );
                    return Vec::new();
                }
                Key::Enter => {
                    let name = self.pending_name.take().unwrap_or_default();
                    if !valid_session_name(&name) {
                        self.pending_name = Some(name);
                        self.flash("invalid session name — plain directory name only");
                        return Vec::new();
                    }
                    return vec![Action::ConfirmNewSession(name)];
                }
                // Tab / BackTab are free: reserved for a future
                // session-navigation design. No-op for now.
                Key::Tab | Key::BackTab => {}
                _ => {}
            }
        }
        match key {
            Key::Quit => {
                // The quit gate (docs/tui.md section 7, FT-012):
                // the key arms and fires only in normal mode with
                // an empty draft, like the pi Ctrl-d rule. In every
                // other state it is a plain `q`: it types into the
                // name input, the search box, or the composer.
                if self.quit_gate_open() {
                    match self.quit_arm {
                        Some(at) if at.elapsed() < QUIT_ARM_TTL => {
                            self.quitting = true;
                            vec![Action::Quit]
                        }
                        _ => {
                            self.quit_arm = Some(Instant::now());
                            self.flash("press q again to quit");
                            Vec::new()
                        }
                    }
                } else if self.pending_name.is_some() {
                    // The name input is up: `q` is a name char.
                    self.pending_name.as_mut().unwrap().push('q');
                    Vec::new()
                } else if self.editor.mode() == Mode::Normal {
                    // Normal mode types nothing: the draft holds
                    // text, so the gate stays closed. Hint the
                    // escape instead of acting.
                    self.flash("clear the draft, then q q quits");
                    Vec::new()
                } else {
                    // A typing mode or the search box: `q` is text.
                    if let Some(h) = self.editor().press(Key::Char('q')) {
                        self.flash(h);
                    }
                    Vec::new()
                }
            }
            Key::Enter => {
                // Enter sends the whole draft; Ctrl-J inserts a
                // newline in it (docs/tui.md: Enter = send, multi-line
                // via Ctrl-J). The naming bar confirms on Enter.
                if self.editor().mode() == Mode::CommandLine {
                    // The search command line runs its query on Enter.
                    if let Some(h) = self.editor().press(Key::Enter) {
                        self.flash(h);
                    }
                    return Vec::new();
                }
                if let Some(name) = self.pending_name().map(str::to_string) {
                    if name.trim().is_empty() {
                        self.flash("empty name — type a session name first");
                        return Vec::new();
                    }
                    return vec![Action::ConfirmNewSession(name)];
                }
                if self.draft().is_empty() {
                    self.flash("empty message — type something first");
                    Vec::new()
                } else if self.active.is_none() {
                    self.flash("no session — pass a session name argument");
                    Vec::new()
                } else {
                    vec![Action::SendDraft]
                }
            }
            Key::CtrlJ => {
                // The multi-line editor's newline key. In insert mode
                // it splits the line; in normal mode it moves down.
                if self.pending_name().is_some() {
                    // The naming bar is single-line: ignore.
                    return Vec::new();
                }
                if let Some(h) = self.editor().press(Key::CtrlJ) {
                    self.flash(h);
                }
                Vec::new()
            }
            Key::CtrlK => {
                // The picker list navigation key (docs/tui-file-picker.md
                // section 5): Ctrl+K moves up in the picker list. The
                // picker handles it when open; when closed it is a no-op.
                Vec::new()
            }
            Key::CtrlI => {
                // The picker file-scope cycle (docs/tui-file-picker.md
                // P9): Ctrl+I cycles standard → ignored → hidden. The
                // picker handles it when open; when closed it is a no-op.
                Vec::new()
            }
            Key::Backspace
            | Key::Delete
            | Key::Left
            | Key::Right
            | Key::Up
            | Key::Down
            | Key::Home
            | Key::End => {
                // The editor decides what these do in its current
                // mode: in insert they edit or move (the newline key
                // is Ctrl-J; Enter sends the draft); in normal, they
                // are motions.
                if let Some(h) = self.editor().press(key) {
                    self.flash(h);
                }
                self.sync_picker();
                Vec::new()
            }
            Key::CtrlR => {
                // Redo (the pi-vim mapping). The duplicate-start
                // guard is not here: main resolves it through the
                // persistent loop.pid probe (FT-003), so a restarted
                // TUI blocks a second loop for a live session instead
                // of starting one.
                if self.editor().mode() == Mode::Normal && self.editor().has_redo() {
                    self.editor().redo();
                    return Vec::new();
                }
                if self.active.is_none() {
                    self.flash("no session to run the loop for");
                    Vec::new()
                } else {
                    vec![Action::RunLoop]
                }
            }
            Key::CtrlC => {
                // The stop resolves in main against the persistent
                // loop.pid probe (FT-003): it stops a live loop this
                // TUI did not start. Main flashes the outcome. In a
                // pre-session editor, Ctrl-C is the vim insert-exit
                // key and the editor decides.
                if self.active.is_none() {
                    if let Some(h) = self.editor().press(Key::CtrlC) {
                        self.flash(h);
                    }
                    Vec::new()
                } else {
                    vec![Action::StopLoop]
                }
            }
            Key::CtrlE => {
                if self.active.is_none() {
                    self.flash("no session — pass a session name argument");
                    Vec::new()
                } else {
                    vec![Action::OpenEditor]
                }
            }
            // Tab / BackTab are free: reserved for a future session
            // navigation design (docs/tui_feature_requests_from_human.md).
            Key::Tab | Key::BackTab => Vec::new(),
            Key::PgUp => {
                self.scroll_up(10);
                Vec::new()
            }
            Key::PgDn => {
                self.scroll_down(10);
                Vec::new()
            }
            Key::CtrlU => {
                // In the search command line it clears the input
                // (the pi-vim mapping). In the idle composer's
                // insert mode it kills the current line (the base
                // editor's Ctrl-U). Otherwise it is the half-page
                // log scroll.
                if self.editor().mode() == Mode::CommandLine {
                    self.editor().press(Key::CtrlU);
                    return Vec::new();
                }
                if self.editor().mode() == Mode::Insert && self.active.is_none() {
                    self.editor().clear_current_line();
                    return Vec::new();
                }
                self.scroll_up(self.half_page());
                Vec::new()
            }
            Key::CtrlD => {
                // Half-page down, like vim: back toward the tail.
                self.scroll_down(self.half_page());
                Vec::new()
            }
            Key::CtrlO => {
                // The global tool fold/expand toggle (docs/tui-tool-
                // display-port.md section 2, the expand part): every
                // collapsed block expands to the full output, and
                // back. The state is app-local; the version bump
                // rebuilds the transcript on the next draw.
                self.tool_expanded = !self.tool_expanded;
                self.events_version += 1;
                vec![Action::ToggleToolExpand]
            }
            Key::CtrlT => {
                // The thinking-block collapse/expand toggle (docs/tui-
                // thinking-block.md section 4, the pi
                // `app.thinking.toggle` keymap): `false` collapses every
                // block to its one-line label; `true` expands the full
                // reasoning text. `Ctrl+X` is the separate show/hide.
                self.thinking_expanded = !self.thinking_expanded;
                self.events_version += 1;
                vec![Action::ToggleThinkingExpand]
            }
            Key::CtrlX => {
                // The thinking-block show/hide toggle (docs/tui-
                // thinking-block.md section 4): `false` hides every
                // thinking block; `true` restores them. `Ctrl+T` is
                // the pi collapse/expand key, so hide takes `Ctrl+X`.
                self.thinking_shown = !self.thinking_shown;
                self.events_version += 1;
                vec![Action::ToggleThinking]
            }
            Key::CtrlF => {
                // The input queue toggle (docs/tui-pending-user-
                // messages.md stage 2): the next draft sends to the
                // follow queue, and back to steer. The state is the
                // input area's own: no transcript rebuild.
                self.follow_queue = !self.follow_queue;
                self.flash(if self.follow_queue {
                    "queue: follow — the next message waits for the loop to stop"
                } else {
                    "queue: steer — the next message injects at the next step"
                });
                vec![Action::ToggleFollowQueue]
            }
            Key::CtrlL => {
                // The reasoning-effort cycle (docs/tui-thinking-level-
                // input-box.md section 3). Main owns the config
                // write-back: it reads the active model's effort,
                // steps to the next value in the effort order, and
                // writes the active model's config entry.
                vec![Action::CycleEffort]
            }
            Key::CtrlP => {
                // Preview pane toggle for the @ picker
                // (docs/tui-file-picker.md section 4.4).
                // No-op when the picker is closed.
                let _ = self.picker.toggle_preview(0, crate::picker::render::PREVIEW_CUTOFF);
                Vec::new()
            }
            Key::AltUp => {
                // Bulk recall of pending user messages
                // (docs/user-message-editing.md).
                self.recall_queue();
                Vec::new()
            }
            Key::Wheel(delta) => {
                // Wheel up scrolls back in history; wheel down chases
                // the tail.
                if delta < 0 {
                    self.scroll_up(3);
                } else {
                    self.scroll_down(3);
                }
                Vec::new()
            }
            Key::Esc => {
                // Esc only ever cancels: it drops the editing mode to
                // normal and cancels a pending operator. It must never
                // touch the draft text — vim's Esc cancels, it does
                // not delete. The draft is cleared only by send (Enter)
                // or an explicit delete motion.
                if let Some(h) = self.editor().press(Key::Esc) {
                    self.flash(h);
                }
                Vec::new()
            }
            Key::Char(c) => {
                // The one-key handoff (correction 57): the log holds a
                // seeded `context_exhausted` marker and no loop runs.
                // It preempts the editor in that state only; in a
                // live session `h` stays the vim left motion.
                if c == 'h'
                    && self.active().is_some_and(|s| !self.loop_running(s))
                    && self.pending_handoff().is_some()
                {
                    return vec![Action::Handoff(self.pending_handoff().unwrap())];
                }
                // y / n / e answer the oldest pending approval_request
                // (docs/tui.md section 7); without a pending request
                // they are ordinary keys for the editor.
                let lower = c.to_ascii_lowercase();
                if self.oldest_pending_approval().is_some() {
                    match lower {
                        'y' => return vec![Action::AnswerApproval(Decision::Allow)],
                        'n' => return vec![Action::AnswerApproval(Decision::Deny)],
                        'e' => return vec![Action::AnswerApproval(Decision::Edit)],
                        _ => {}
                    }
                }
                // The double-`s` browse gate (section 4.2): the same
                // two conditions as the `q q` exit path. In the
                // gated state the first `s` arms; the second `s`
                // inside the window enters browse. A held draft keeps
                // the editor `s` role and hints the browse path.
                if c == 's' && self.browse_gate_open() {
                    match self.ss_arm {
                        Some(at) if at.elapsed() < SS_ARM_TTL => {
                            self.ss_arm = None;
                            self.browse.enter();
                            self.flash(crate::browse::BROWSE_HINT);
                            return Vec::new();
                        }
                        _ => {
                            self.ss_arm = Some(Instant::now());
                            self.flash("ss: press s again to browse");
                            return Vec::new();
                        }
                    }
                }
                if c == 's'
                    && self.editor.mode() == Mode::Normal
                    && !self.editor.text().trim().is_empty()
                    && self.pending_name.is_none()
                {
                    // The editor keeps the `s` role (change one
                    // char); the hint names the browse path.
                    if let Some(h) = self.editor().press(Key::Char('s')) {
                        self.flash(h);
                    }
                    self.flash("ss browses — clear the draft first");
                    return Vec::new();
                }
                // `:` opens the command palette (docs/tui-command-palette.md).
                // Only in normal mode; in insert mode `:` is a regular
                // character typed into the draft.
                if c == ':'
                    && self.editor.mode() == Mode::Normal
                    && self.pending_name.is_none()
                {
                    self.open_palette();
                    return Vec::new();
                }
                if let Some(h) = self.editor().press(Key::Char(c)) {
                    self.flash(h);
                }
                // Open the picker when `@` is freshly typed at a valid
                // position (start of line or preceded only by
                // whitespace). A stale `@` left in the draft after a
                // previous pick or dismiss does not re-trigger it.
                if c == '@'
                    && !self.picker.open
                    && self.editor().at_token_info().is_some()
                {
                    self.open_picker();
                }
                // Sync the picker after any editor mutation so the
                // `@query` text stays in step with the draft.
                self.sync_picker();
                Vec::new()
            }
        }
    }

    pub fn should_quit(&self) -> bool {
        self.quitting
    }

}

/// Fold one paced release into the live buffer accumulators (the
/// consumer of the §6.5 pace queue).
fn apply_paced(buf: &mut StreamBuf, kind: &StreamDeltaKind, payload: &str) {
    match kind {
        StreamDeltaKind::Text => {
            buf.text.push_str(payload);
        }
        StreamDeltaKind::Reasoning(id) => {
            buf.reasoning.entry(id.clone()).or_default().push_str(payload);
        }
        StreamDeltaKind::ToolArgs(call_id, name) => {
            let entry = buf
                .tool_args
                .entry(call_id.clone())
                .or_insert_with(|| (String::new(), String::new()));
            if !name.is_empty() {
                entry.0 = name.clone();
            }
            entry.1.push_str(payload);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::produce;
    use serde_json::json;

    /// A no-op loop handle for state-machine tests.
    struct DummyHandle;
    impl crate::port::LoopHandle for DummyHandle {
        fn stop(&self) {}
        fn wait_exit(&self) -> i32 {
            0
        }
        fn take_lines(&self) -> Option<tokio::sync::mpsc::UnboundedReceiver<LoopLine>> {
            None
        }
    }

    fn attach_dummy(app: &mut App, sid: &str) {
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel::<LoopLine>();
        app.attach_loop(SessionId::new(sid), Box::new(DummyHandle), rx);
    }

    fn app_with(events: Vec<Event>, active: &str) -> App {
        let mut app = App::new();
        app.set_sessions(vec![SessionId::new(active)]);
        app.set_active(SessionId::new(active), events);
        app
    }

    fn ev(json_str: &str) -> Event {
        Event::parse_line(json_str).unwrap()
    }

    fn request(id: &str, call_id: Option<&str>) -> Event {
        let mut o = json!({"v":1,"type":"approval_request","ts":"t","id":id,"prompt":"ok?"});
        if let Some(c) = call_id {
            o["call_id"] = json!(c);
        }
        Event::Json { obj: o }
    }

    fn tool_call(_id: &str) -> Event {
        ev(
            r#"{"v":1,"type":"tool_call","ts":"t","id":"call-1","name":"bash","arguments":{"command":"mv a b"}}"#,
        )
    }

    #[test]
    fn enter_with_draft_sends() {
        let mut app = app_with(vec![], "s1");
        app.press(Key::Char('h'));
        app.press(Key::Char('i'));
        // Enter sends the whole draft. The main loop's `SendDraft`
        // handler is what clears it via `take_draft`.
        assert_eq!(app.press(Key::Enter), vec![Action::SendDraft]);
        assert_eq!(app.take_draft(), "hi");
    }

    #[test]
    fn enter_without_draft_does_nothing() {
        let mut app = app_with(vec![], "s1");
        // Empty draft: no SendDraft, but a flash tells the user.
        assert!(app.press(Key::Enter).is_empty());
        assert!(app.status().is_some());
    }

    #[test]
    fn ctrl_j_newlines_and_send_keeps_draft() {
        // Ctrl-J is the multi-line newline key: in normal mode it is
        // the `j` motion; in insert mode it inserts a hard newline.
        // Enter sends regardless of the modal state.
        let mut app = app_with(vec![], "s1");
        app.editor().set_text("ab\ncd");
        app.editor().mode = crate::vim_editor::Mode::Normal;
        app.press(Key::CtrlJ);
        assert_eq!(
            app.editor().cursor(),
            (1, 0),
            "Ctrl-J is the j motion in normal mode"
        );
        app.press(Key::Char('i'));
        app.press(Key::CtrlJ);
        assert_eq!(
            app.draft(),
            "ab\n\ncd",
            "Ctrl-J is a newline in insert mode"
        );
    }

    #[test]
    fn ctrl_r_emits_the_spawn_intent() {
        let mut app = app_with(vec![], "s1");
        assert_eq!(app.press(Key::CtrlR), vec![Action::RunLoop]);
        // Simulate the main thread attaching the loop. The duplicate
        // guard is not in the app state: main resolves it through the
        // persistent loop.pid probe (FT-003), so a second press still
        // emits the intent.
        attach_dummy(&mut app, "s1");
        assert!(app.loop_running(&SessionId::new("s1")));
        assert_eq!(app.press(Key::CtrlR), vec![Action::RunLoop]);
    }

    #[test]
    fn ctrl_c_emits_the_stop_intent() {
        let mut app = app_with(vec![], "s1");
        // No loop known: the key still emits the intent. Main
        // resolves the stop through the persistent loop.pid probe
        // (FT-003), which stops a loop this TUI did not start.
        assert_eq!(app.press(Key::CtrlC), vec![Action::StopLoop]);
        attach_dummy(&mut app, "s1");
        assert_eq!(app.press(Key::CtrlC), vec![Action::StopLoop]);
        let mut none = App::new();
        assert!(none.press(Key::CtrlC).is_empty(), "no session: no intent");
    }

    #[test]
    fn approval_keys_answer_only_when_pending() {
        // No pending approval: y/n/e are ordinary typing.
        let mut app = app_with(vec![], "s1");
        app.press(Key::Char('y'));
        app.press(Key::Char('n'));
        app.press(Key::Char('e'));
        assert_eq!(app.draft(), "yne");

        // With a pending request: y answers, the char is not typed.
        let req = request("appr-1", None);
        let mut app = app_with(vec![req], "s1");
        assert_eq!(
            app.press(Key::Char('y')),
            vec![Action::AnswerApproval(Decision::Allow)]
        );
        assert_eq!(app.draft(), "");

        let req = request("appr-2", None);
        let mut app = app_with(vec![req], "s1");
        assert_eq!(
            app.press(Key::Char('n')),
            vec![Action::AnswerApproval(Decision::Deny)]
        );

        let req = request("appr-3", None);
        let mut app = app_with(vec![req], "s1");
        assert_eq!(
            app.press(Key::Char('e')),
            vec![Action::AnswerApproval(Decision::Edit)]
        );
    }

    #[test]
    fn pending_approval_is_derived_from_the_log() {
        // Request answered later in the log: not pending.
        let events = vec![
            request("a1", Some("call-1")),
            ev(r#"{"v":1,"type":"approval","ts":"t","id":"a1","decision":"allow"}"#),
            request("a2", Some("call-1")),
        ];
        let app = app_with(events, "s1");
        let p = app.oldest_pending_approval().expect("a2 must be pending");
        assert_eq!(p.request_id, "a2");

        // The pending request resolves call arguments for edit-then-allow.
        let events = vec![tool_call("call-1"), request("a3", Some("call-1"))];
        let app = app_with(events, "s1");
        let p = app.oldest_pending_approval().unwrap();
        assert_eq!(
            p.arguments,
            Some(json!({"command": "mv a b"})),
            "arguments come from the tool_call with the request's call_id"
        );

        // Oldest first: two unanswered requests -> the first one.
        let events = vec![request("a1", None), request("a2", None)];
        let app = app_with(events, "s1");
        assert_eq!(app.oldest_pending_approval().unwrap().request_id, "a1");
    }

    #[test]
    fn cycle_sessions_wraps_and_needs_a_list() {
        let mut app = App::new();
        app.set_sessions(vec![SessionId::new("a"), SessionId::new("b")]);
        app.set_active(SessionId::new("a"), Vec::new());
        assert_eq!(app.cycle_target(1), Some(SessionId::new("b")));
        assert_eq!(
            app.cycle_target(-1),
            Some(SessionId::new("b")),
            "wraps backward"
        );

        let app = App::new();
        assert_eq!(app.cycle_target(1), None);
    }

    #[test]
    fn quit_needs_two_q_within_the_window() {
        let mut app = app_with(vec![], "s1");
        assert!(!app.should_quit());
        // The gate (FT-012): the editor starts in insert mode, where
        // `q` is text. Esc drops to normal; the draft is empty, so
        // the gate is open.
        app.press(Key::Esc);
        // First q arms: no action, just a status hint.
        assert!(app.press(Key::Quit).is_empty());
        assert!(!app.should_quit(), "first q only arms the quit");
        assert!(app.status().is_some());
        // Typing another key disarms.
        let mut app = app_with(vec![], "s1");
        app.press(Key::Esc);
        app.press(Key::Quit);
        app.press(Key::Char('x'));
        assert!(app.press(Key::Quit).is_empty(), "disarmed: q arms again");
        assert!(!app.should_quit());
        // A quick second q confirms.
        let mut app = app_with(vec![], "s1");
        app.press(Key::Esc);
        app.press(Key::Quit);
        assert_eq!(app.press(Key::Quit), vec![Action::Quit]);
        assert!(app.should_quit());
        // No further keys after quit.
        assert!(app.press(Key::Char('x')).is_empty());
        assert_eq!(app.draft(), "");
    }

    #[test]
    fn q_types_a_char_in_insert_mode() {
        // The gate (FT-012): insert mode types `q`; it never arms
        // the quit. A second q types a second q, not a quit.
        let mut app = app_with(vec![], "s1");
        assert!(app.press(Key::Quit).is_empty());
        assert_eq!(app.draft(), "q");
        assert!(!app.should_quit());
        assert!(app.press(Key::Quit).is_empty());
        assert_eq!(app.draft(), "qq");
        assert!(!app.should_quit());
    }

    #[test]
    fn q_types_a_char_in_replace_mode() {
        let mut app = app_with(vec![], "s1");
        app.editor().set_text("abc");
        app.press(Key::Esc);
        app.press(Key::Char('R'));
        assert!(app.press(Key::Quit).is_empty());
        assert_eq!(app.draft(), "qbc");
        assert!(!app.should_quit());
    }

    #[test]
    fn q_types_into_the_search_command_line() {
        let mut app = app_with(vec![], "s1");
        app.editor().set_text("alpha beta");
        app.press(Key::Esc);
        app.press(Key::Char('/'));
        assert!(app.press(Key::Quit).is_empty());
        assert_eq!(
            app.editor().command_line_label(),
            Some("/q\u{2588}".to_string())
        );
        assert!(!app.should_quit());
    }

    #[test]
    fn q_types_into_the_name_input() {
        let mut app = App::new();
        app.start_naming();
        assert!(app.press(Key::Quit).is_empty());
        assert_eq!(app.pending_name(), Some("q"));
        assert!(!app.should_quit());
    }

    #[test]
    fn q_hints_the_gate_with_a_nonempty_draft() {
        // Normal mode, draft not empty: the gate is closed and the
        // key leaves the text intact. The hint states the escape.
        let mut app = app_with(vec![], "s1");
        app.editor().set_text("hi");
        app.press(Key::Esc);
        assert!(app.press(Key::Quit).is_empty());
        assert!(!app.should_quit());
        assert_eq!(app.status(), Some("clear the draft, then q q quits"));
        assert_eq!(app.draft(), "hi");
    }

    #[test]
    fn expired_arm_requires_a_fresh_q() {
        let mut app = app_with(vec![], "s1");
        app.press(Key::Esc);
        app.press(Key::Quit);
        // Simulate the window closing.
        app.quit_arm = Some(Instant::now() - QUIT_ARM_TTL - std::time::Duration::from_millis(1));
        assert!(
            app.press(Key::Quit).is_empty(),
            "stale arm re-arms instead of quitting"
        );
        assert!(!app.should_quit());
        app.press(Key::Quit);
        assert!(app.should_quit());
    }

    #[test]
    fn ctrl_u_d_scroll_half_a_viewport() {
        let mut app = app_with(vec![], "s1");
        app.set_viewport_height(24);
        assert_eq!(app.press(Key::CtrlU), Vec::<Action>::new());
        assert_eq!(app.scroll(), 11, "half of a 24-line viewport is 11");
        assert_eq!(app.press(Key::CtrlD), Vec::<Action>::new());
        assert_eq!(app.scroll(), 0, "half-page down returns to the tail");
        // From the tail, Ctrl+U moves up; further Ctrl+D clamps at 0.
        app.press(Key::CtrlU);
        app.press(Key::CtrlU);
        assert_eq!(app.scroll(), 22);
        app.scroll_down(1_000_000);
        assert_eq!(app.scroll(), 0);
    }

    #[test]
    fn ctrl_u_d_fallback_without_viewport() {
        let mut app = app_with(vec![], "s1");
        // No draw has set the viewport yet: the page-key distance
        // (10 lines) is used instead of a half-page.
        assert_eq!(app.press(Key::CtrlU), Vec::<Action>::new());
        assert_eq!(app.scroll(), 10);
    }

    #[test]
    fn ext_statuses_track_latest_value_per_id() {
        let evs = vec![
            ev(r#"{"v":1,"type":"ext_status","ts":"t","id":"vim_mode","value":"insert"}"#),
            ev(r#"{"v":1,"type":"ext_status","ts":"t","id":"vim_mode","value":"normal"}"#),
            ev(r#"{"v":1,"type":"ext_status","ts":"t","id":"team","value":{"on_call":"t"}}"#),
        ];
        let app = app_with(evs, "s1");
        let m = app.ext_statuses();
        assert_eq!(m.len(), 2);
        assert_eq!(
            m.get("vim_mode").unwrap(),
            &json!("normal"),
            "the latest event wins per id"
        );
        assert_eq!(m.get("team").unwrap(), &json!({"on_call": "t"}));
    }

    #[test]
    fn ext_statuses_drop_the_oldest_id_at_the_cap() {
        // The cap bounds the distinct ids a chatty publisher can
        // hold in memory: 130 ids drop the first two.
        let evs: Vec<Event> = (0..130)
            .map(|i| {
                ev(&format!(
                    r#"{{"v":1,"type":"ext_status","ts":"t","id":"id-{i}","value":{i}}}"#,
                    i = i
                ))
            })
            .collect();
        let app = app_with(evs, "s1");
        let m = app.ext_statuses();
        assert_eq!(m.len(), EXT_STATUS_ID_CAP, "the cap holds");
        assert!(m.get("id-0").is_none(), "the oldest id drops");
        assert!(m.get("id-1").is_none(), "the next oldest drops");
        assert!(m.get("id-2").is_some(), "the rest keep");
        assert!(m.get("id-129").is_some(), "the newest keeps");
    }

    #[test]
    fn ext_statuses_rebuild_keeps_the_cap_semantics() {
        // A session switch rebuilds from the log: the same cap and
        // the same update order apply.
        let evs: Vec<Event> = (0..130)
            .map(|i| {
                ev(&format!(
                    r#"{{"v":1,"type":"ext_status","ts":"t","id":"id-{i}","value":{i}}}"#,
                    i = i
                ))
            })
            .collect();
        let mut app = app_with(Vec::new(), "s1");
        app.set_active(SessionId::new("s2"), evs);
        assert_eq!(app.ext_statuses().len(), EXT_STATUS_ID_CAP);
        assert!(app.ext_statuses().get("id-0").is_none());
        // An in-place update moves the id to most recent: it is the
        // last one to drop. 127 new ids drop the untouched ids; the
        // updated id still holds.
        app.on_watch_item(WatchItem::Event {
            event: ev(r#"{"v":1,"type":"ext_status","ts":"t","id":"id-2","value":99}"#),
            cursor: crate::port::TailCursor::end(),
        });
        for i in 3..130 {
            app.on_watch_item(WatchItem::Event {
                event: ev(&format!(
                    r#"{{"v":1,"type":"ext_status","ts":"t","id":"fill-{i}","value":{i}}}"#,
                    i = i
                )),
                cursor: crate::port::TailCursor::end(),
            });
        }
        let m = app.ext_statuses();
        assert_eq!(m.len(), EXT_STATUS_ID_CAP);
        assert!(m.get("id-3").is_none(), "an untouched id drops");
        assert!(m.get("id-129").is_none(), "the untouched ids drop");
        assert_eq!(
            m.get("id-2").unwrap(),
            &json!(99),
            "the updated id is the most recent"
        );
    }

    #[test]
    fn ext_statuses_update_on_watch_events() {
        let mut app = app_with(
            vec![ev(
                r#"{"v":1,"type":"ext_status","ts":"t","id":"vim_mode","value":"insert"}"#,
            )],
            "s1",
        );
        // A new ext_status watch event updates the map in place.
        app.on_watch_item(WatchItem::Event {
            event: ev(r#"{"v":1,"type":"ext_status","ts":"t","id":"vim_mode","value":"normal"}"#),
            cursor: crate::port::TailCursor::end(),
        });
        assert_eq!(
            app.ext_statuses().get("vim_mode").unwrap(),
            &json!("normal")
        );
        // A non-ext_status event leaves the map untouched.
        app.on_watch_item(WatchItem::Event {
            event: ev(r#"{"v":1,"type":"user_message","ts":"t","content":"hi"}"#),
            cursor: crate::port::TailCursor::end(),
        });
        assert_eq!(
            app.ext_statuses().get("vim_mode").unwrap(),
            &json!("normal")
        );
        // A session switch rebuilds the map from that session's log.
        app.set_active(SessionId::new("s2"), vec![]);
        assert!(
            app.ext_statuses().is_empty(),
            "a fresh session has no ext_status values"
        );
    }

    #[test]
    fn transcript_cache_reuses_until_events_change() {
        let mut app = app_with(vec![], "s1");
        app.set_active(
            SessionId::new("s1"),
            vec![ev(
                r#"{"v":1,"type":"user_message","ts":"t","content":"one"}"#,
            )],
        );
        let a = app.transcript_lines(80, None) as *const _;
        let b = app.transcript_lines(80, None) as *const _;
        assert_eq!(
            a, b,
            "an unchanged event list and width must reuse the cache"
        );
        let c = app.transcript_lines(60, None) as *const _;
        assert_ne!(b, c, "a width change rebuilds the lines");
        // A new event bumps the version: the cache rebuilds.
        app.on_watch_item(WatchItem::Event {
            event: ev(r#"{"v":1,"type":"user_message","ts":"t","content":"two"}"#),
            cursor: crate::port::TailCursor::end(),
        });
        let d = app.transcript_lines(80, None) as *const _;
        assert_ne!(a, d, "a new event must invalidate the cache");
        assert!(app
            .transcript_lines(80, None)
            .iter()
            .any(|l| l.to_string().contains("two")));
    }

    #[test]
    fn scroll_bounded_and_follows_tail() {
        let evs: Vec<Event> = (0..100)
            .map(|i| {
                ev(&format!(
                    r#"{{"v":1,"type":"user_message","ts":"t","content":"{i}"}}"#
                ))
            })
            .collect();
        let mut app = app_with(evs, "s1");
        assert_eq!(app.scroll(), 0, "starts at the tail");
        app.scroll_up(10);
        assert_eq!(app.scroll(), 10);
        app.scroll_up(1_000_000);
        assert_eq!(
            app.scroll(),
            SCROLL_CAP,
            "scroll clamps at the cap, not the event count"
        );
        app.scroll_down(1_000_000);
        assert_eq!(app.scroll(), 0, "down clamps back to the tail");
    }

    #[test]
    fn loop_lines_update_state() {
        let mut app = app_with(vec![], "s1");
        attach_dummy(&mut app, "s1");
        // The dummy handle takes no channel, so push lines through a
        // fresh receiver: replace the loop state's receiver with a live one.
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<LoopLine>();
        app.attach_loop(SessionId::new("s1"), Box::new(DummyHandle), rx);
        tx.send(LoopLine::Stderr("model call starting".into()))
            .unwrap();
        tx.send(LoopLine::Exited(0)).unwrap();
        let drained = app.drain_loop_lines();
        assert_eq!(drained.len(), 2);
        let st = app.loop_state(&SessionId::new("s1")).unwrap();
        assert!(!st.running, "Exited flips running to false");
        assert_eq!(st.exit_code, Some(0));
        assert_eq!(st.last_line.as_deref(), Some("model call starting"));
    }

    #[test]
    fn produced_events_are_consumed_by_the_state_machine() {
        // Approval events the TUI itself produces must clear the pending
        // request through the same derivation.
        let req = request("x1", None);
        let approval = produce::approval("x1", "allow", None);
        let mut app = app_with(vec![], "s1");
        app.on_watch_item(WatchItem::Event {
            event: req,
            cursor: crate::port::TailCursor::end(),
        });
        assert!(app.oldest_pending_approval().is_some());
        app.on_watch_item(WatchItem::Event {
            event: approval,
            cursor: crate::port::TailCursor::end(),
        });
        assert!(app.oldest_pending_approval().is_none());
    }

    #[test]
    fn watch_items_malformed_events_are_kept() {
        let mut app = app_with(vec![], "s1");
        app.on_watch_item(WatchItem::Event {
            event: Event::MalformedLine { line: "###".into() },
            cursor: crate::port::TailCursor::end(),
        });
        assert_eq!(app.events().len(), 1);
        assert_eq!(app.events()[0].kind(), EventKind::BadLine);
    }

    #[test]
    fn naming_input_edits_the_pending_name() {
        let mut app = App::new();
        assert_eq!(app.pending_name(), None);
        app.start_naming();
        assert_eq!(app.pending_name(), Some(""));
        for c in ['a', 'b'] {
            assert!(app.press(Key::Char(c)).is_empty(), "typing is local");
        }
        assert_eq!(app.pending_name(), Some("ab"));
        assert!(app.press(Key::Backspace).is_empty());
        assert_eq!(app.pending_name(), Some("a"));
        // Enter confirms and switches the input off.
        assert_eq!(
            app.press(Key::Enter),
            vec![Action::ConfirmNewSession("a".to_string())]
        );
        assert_eq!(app.pending_name(), None);
    }

    #[test]
    fn naming_confirm_keeps_the_input_on_bad_names() {
        let mut app = App::new();
        app.start_naming();
        // An empty name cannot be confirmed; the input stays up.
        assert!(app.press(Key::Enter).is_empty());
        assert_eq!(app.pending_name(), Some(""));
        assert!(app.status().is_some());
        // A path-shaped name is rejected the same way.
        for c in ['.', '.', '/', 'x'] {
            app.press(Key::Char(c));
        }
        assert!(app.press(Key::Enter).is_empty());
        assert_eq!(
            app.pending_name(),
            Some("../x"),
            "the name stays for editing"
        );
        // A bare `.` would land the log in the sessions root itself;
        // it is rejected like any path-shaped name.
        let mut app = App::new();
        app.set_sessions(vec![SessionId::new("s1"), SessionId::new("s2")]);
        app.start_naming();
        app.press(Key::Char('.'));
        assert!(app.press(Key::Enter).is_empty());
        assert_eq!(app.pending_name(), Some("."));
        // Esc cancels the input entirely.
        let mut app = App::new();
        app.start_naming();
        app.press(Key::Char('z'));
        assert!(app.press(Key::Esc).is_empty());
        assert_eq!(app.pending_name(), None);
        assert!(app.status().is_some());
    }

    #[test]
    fn tab_while_naming_is_free() {
        // Tab / BackTab are reserved for a future session-navigation
        // design; they are no-ops in the name input for now.
        let mut app = App::new();
        app.set_sessions(vec![SessionId::new("a"), SessionId::new("b")]);
        app.start_naming();
        app.press(Key::Char('x'));
        assert!(app.press(Key::Tab).is_empty());
        assert_eq!(app.pending_name(), Some("x"), "Tab is a no-op");
        assert!(app.press(Key::BackTab).is_empty());
        assert_eq!(app.pending_name(), Some("x"), "BackTab is a no-op");
    }

    #[test]
    fn fall_through_keys_keep_the_naming_input() {
        // Ctrl+R with no active session flashes a hint; the name input
        // is untouched.
        let mut app = App::new();
        app.start_naming();
        app.press(Key::Char('s'));
        assert!(app.press(Key::CtrlR).is_empty());
        assert_eq!(app.pending_name(), Some("s"), "Ctrl+R falls through");
        assert!(app.status().is_some());
        // Plain typing still edits the name, not the draft.
        app.press(Key::Char('e'));
        assert_eq!(app.pending_name(), Some("se"));
        assert_eq!(app.draft(), "");
    }

    fn exhausted_event(new_session: &str) -> Event {
        let mut o = serde_json::json!({
            "v": 1,
            "type": "context_exhausted",
            "ts": "t",
            "message": "context budget exhausted after compaction"
        });
        o["new_session"] = serde_json::json!(new_session);
        Event::Json { obj: o }
    }

    #[test]
    fn pending_handoff_reads_the_last_seeded_marker() {
        let app = app_with(vec![exhausted_event("s1_h1")], "s1");
        assert_eq!(app.pending_handoff(), Some("s1_h1".to_string()));

        // The newest marker is the word: a later marker with no
        // seeded session (a failed summary call) hides the older one.
        let app = app_with(vec![exhausted_event("s1_h1"), exhausted_event("")], "s1");
        assert_eq!(app.pending_handoff(), None);

        // No marker: nothing pending.
        let app = app_with(vec![], "s1");
        assert_eq!(app.pending_handoff(), None);
    }

    #[test]
    fn pending_user_messages_empty_without_events() {
        let app = app_with(vec![], "s1");
        assert!(app.pending_user_messages().is_empty());
    }

    #[test]
    fn pending_user_messages_stop_at_the_last_answer() {
        // Answered messages drop off: only the messages after the
        // last `assistant_message` stay pending.
        let evs = vec![
            ev(r#"{"v":1,"type":"user_message","ts":"t","content":"a"}"#),
            ev(r#"{"v":1,"type":"assistant_message","ts":"t","content":"A"}"#),
            ev(r#"{"v":1,"type":"user_message","ts":"t","content":"b"}"#),
            ev(r#"{"v":1,"type":"assistant_message","ts":"t","content":"B"}"#),
            ev(r#"{"v":1,"type":"user_message","ts":"t","content":"c"}"#),
        ];
        let app = app_with(evs, "s1");
        let p = app.pending_user_messages();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].get_str("content"), Some("c"));
    }

    #[test]
    fn busy_time_messages_wait_together() {
        // Two messages land while the loop is busy. Both wait for
        // the next step's answer, in log order.
        let evs = vec![
            ev(r#"{"v":1,"type":"user_message","ts":"t","content":"a"}"#),
            ev(r#"{"v":1,"type":"assistant_message","ts":"t","content":"A"}"#),
            ev(r#"{"v":1,"type":"user_message","ts":"t","content":"b"}"#),
            ev(r#"{"v":1,"type":"user_message","ts":"t","content":"c"}"#),
        ];
        let app = app_with(evs, "s1");
        let p = app.pending_user_messages();
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].get_str("content"), Some("b"));
        assert_eq!(p[1].get_str("content"), Some("c"));
    }

    #[test]
    fn pending_user_messages_survive_a_cancel() {
        // A cancel kills the in-flight step. The unanswered message
        // still waits for the next loop run.
        let evs = vec![
            ev(r#"{"v":1,"type":"user_message","ts":"t","content":"a"}"#),
            ev(r#"{"v":1,"type":"cancel","ts":"t","target":"turn"}"#),
        ];
        let app = app_with(evs, "s1");
        assert_eq!(app.pending_user_messages().len(), 1);
    }

    #[test]
    fn h_key_hands_off_when_seeded_and_idle() {
        let mut app = app_with(vec![exhausted_event("s1_h1")], "s1");
        assert_eq!(
            app.press(Key::Char('h')),
            vec![Action::Handoff("s1_h1".to_string())]
        );
        assert_eq!(app.draft(), "", "the key preempts the editor");
    }

    #[test]
    fn h_key_stays_editor_motion_without_a_seed() {
        // No marker: the key is ordinary typing in the insert-mode
        // editor.
        let mut app = app_with(vec![], "s1");
        assert!(app.press(Key::Char('h')).is_empty());
        assert_eq!(app.draft(), "h");

        // A marker that seeded no session: the key stays with the
        // editor, the user starts the session by hand.
        let mut app = app_with(vec![exhausted_event("")], "s1");
        assert!(app.press(Key::Char('h')).is_empty());
        assert_eq!(app.draft(), "h");
    }

    #[test]
    fn h_key_stays_editor_motion_while_the_loop_runs() {
        let mut app = app_with(vec![exhausted_event("s1_h1")], "s1");
        attach_dummy(&mut app, "s1");
        assert!(app.loop_running(&SessionId::new("s1")));
        assert!(
            app.press(Key::Char('h')).is_empty(),
            "the running loop owns the session"
        );
        assert_eq!(app.draft(), "h");
    }

    #[test]
    fn shift_a_types_uppercase_a_in_the_composer() {
        // The idle composer is in insert mode: Shift+a types `A`
        // at the caret; Enter still sends the whole draft.
        let mut app = App::new();
        app.editor().set_text("hi there");
        app.editor().row = 0;
        app.editor().col = 2;
        app.press(Key::Char('A'));
        assert_eq!(app.editor().cursor(), (0, 3));
        app.press(Key::Char('!'));
        assert_eq!(app.draft(), "hiA! there");
    }

    #[test]
    fn enter_in_the_search_command_line_runs_the_search() {
        let mut app = App::new();
        app.editor().set_text("abc abc");
        app.editor().mode = crate::vim_editor::Mode::Normal;
        app.press(Key::Char('/'));
        app.press(Key::Char('b'));
        app.press(Key::Char('c'));
        assert_eq!(
            app.editor().mode(),
            crate::vim_editor::Mode::CommandLine,
            "/ starts the command line"
        );
        assert!(
            app.press(Key::Enter).is_empty(),
            "the search runs in the editor, not a draft send"
        );
        assert_eq!(app.editor().mode(), crate::vim_editor::Mode::Normal);
        assert_eq!(
            app.editor().cursor(),
            (0, 1),
            "the caret lands on the first match"
        );
        assert_eq!(
            app.draft(),
            "abc abc",
            "the draft text is the search buffer"
        );
    }

    #[test]
    fn ctrl_c_without_a_session_leaves_the_editor() {
        let mut app = App::new();
        app.editor().set_text("abc");
        assert_eq!(app.editor().mode(), crate::vim_editor::Mode::Insert);
        assert!(
            app.press(Key::CtrlC).is_empty(),
            "no session: no stop intent"
        );
        assert_eq!(app.editor().mode(), crate::vim_editor::Mode::Normal);
        assert_eq!(app.draft(), "abc", "the text is untouched");
    }

    #[test]
    fn ctrl_r_redoes_before_starting_the_loop() {
        let mut app = app_with(vec![], "s1");
        app.editor().set_text("abc");
        app.editor().mode = crate::vim_editor::Mode::Normal;
        app.press(Key::Char('i'));
        app.press(Key::Char('X'));
        app.press(Key::Esc);
        app.press(Key::Char('u'));
        assert_eq!(app.draft(), "abc", "the undo restored the text");
        assert!(
            app.press(Key::CtrlR).is_empty(),
            "the redo swallows the key instead of starting the loop"
        );
        assert_eq!(app.draft(), "Xabc", "the redo restored the change");
        // Without redo state the key falls through to the loop intent.
        assert_eq!(app.press(Key::CtrlR), vec![Action::RunLoop]);
    }

    #[test]
    fn ctrl_u_kills_the_line_in_the_idle_composer() {
        let mut app = App::new();
        app.editor().set_text("one\ntwo");
        app.editor().mode = crate::vim_editor::Mode::Insert;
        app.editor().row = 1;
        app.editor().col = 1;
        assert!(app.press(Key::CtrlU).is_empty());
        assert_eq!(
            app.editor().text(),
            "one\n",
            "the current line is emptied, structure kept"
        );
    }

    #[test]
    fn ctrl_u_scrolls_the_log_outside_insert_mode() {
        let events = (0..50)
            .map(|i| produce::user_message(&format!("log line {i}")))
            .collect();
        let mut app = app_with(events, "s1");
        app.editor().set_text("abc");
        app.editor().mode = crate::vim_editor::Mode::Normal;
        let before = app.scroll();
        assert!(app.press(Key::CtrlU).is_empty());
        assert!(
            app.scroll() > before,
            "normal mode keeps the half-page log scroll"
        );
    }

    // ── loop_phase marker (docs/tui-model-wait-indicator.md) ──

    fn loop_phase_marker(ts: &str, value: &str) -> Event {
        ev(&format!(
            r#"{{"v":1,"type":"ext_status","ts":"{ts}","id":"loop_phase","value":"{value}"}}"#,
            ts = ts,
            value = value
        ))
    }

    #[test]
    fn loop_phase_value_and_ts_track_the_last_event() {
        // The value map and the timestamp side map both point at the
        // last event for the id, in log order.
        let evs = vec![
            loop_phase_marker("2026-01-01T00:00:00Z", "wait"),
            loop_phase_marker("2026-01-01T00:00:05Z", "tools"),
            loop_phase_marker("2026-01-01T00:00:09Z", "wait"),
        ];
        let app = app_with(evs, "s1");
        assert_eq!(
            app.ext_statuses().get(LOOP_PHASE_STATUS_ID),
            Some(&json!("wait")),
            "the last value wins"
        );
        assert_eq!(
            app.loop_phase_ts(),
            Some("2026-01-01T00:00:09Z"),
            "the last event ts wins"
        );
    }

    #[test]
    fn loop_phase_ts_updates_on_watch_events() {
        let mut app = app_with(vec![loop_phase_marker("t1", "wait")], "s1");
        assert_eq!(app.loop_phase_ts(), Some("t1"));
        app.on_watch_item(WatchItem::Event {
            event: loop_phase_marker("t2", "tools"),
            cursor: crate::port::TailCursor::end(),
        });
        assert_eq!(
            app.ext_statuses().get(LOOP_PHASE_STATUS_ID),
            Some(&json!("tools")),
            "the watch event updates the value"
        );
        assert_eq!(
            app.loop_phase_ts(),
            Some("t2"),
            "the watch event updates the ts"
        );
        // An event without a ts field updates the value, leaves the
        // ts entry untouched.
        app.on_watch_item(WatchItem::Event {
            event: ev(r#"{"v":1,"type":"ext_status","id":"loop_phase","value":"wait"}"#),
            cursor: crate::port::TailCursor::end(),
        });
        assert_eq!(
            app.ext_statuses().get(LOOP_PHASE_STATUS_ID),
            Some(&json!("wait"))
        );
        assert_eq!(app.loop_phase_ts(), Some("t2"), "the ts entry keeps");
    }

    #[test]
    fn loop_phase_marker_survives_the_id_cap() {
        // 129 distinct ids precede a fresh loop_phase marker: the cap
        // (128 ids) drops the two oldest ids, the marker survives
        // (docs/tui-model-wait-indicator.md section 4).
        let mut evs: Vec<Event> = (0..129)
            .map(|i| {
                ev(&format!(
                    r#"{{"v":1,"type":"ext_status","ts":"t","id":"id-{i}","value":{i}}}"#,
                    i = i
                ))
            })
            .collect();
        evs.push(loop_phase_marker("2026-01-01T00:00:00Z", "wait"));
        let app = app_with(evs, "s1");
        let m = app.ext_statuses();
        assert_eq!(m.len(), EXT_STATUS_ID_CAP, "the cap holds");
        assert!(m.get("id-0").is_none(), "the oldest id drops");
        assert!(m.get("id-1").is_none(), "the next oldest drops");
        assert_eq!(
            m.get(LOOP_PHASE_STATUS_ID),
            Some(&json!("wait")),
            "the fresh marker survives"
        );
        assert_eq!(
            app.loop_phase_ts(),
            Some("2026-01-01T00:00:00Z"),
            "the marker ts survives the drop"
        );
    }

    #[test]
    fn loop_phase_restart_rebuilds_from_the_log() {
        // A TUI restart reads the whole log into set_active. The
        // state is a pure function of the log and the running bit,
        // so the rebuild restores the marker and its ts.
        let events = vec![
            ev(r#"{"v":1,"type":"user_message","ts":"t","content":"hi"}"#),
            loop_phase_marker("2026-01-01T00:00:00Z", "wait"),
            loop_phase_marker("2026-01-01T00:00:10Z", "tools"),
        ];
        let app = app_with(events, "s1");
        assert_eq!(
            app.ext_statuses().get(LOOP_PHASE_STATUS_ID),
            Some(&json!("tools"))
        );
        assert_eq!(app.loop_phase_ts(), Some("2026-01-01T00:00:10Z"));
    }

    #[test]
    fn loop_phase_session_switch_keeps_own_marker() {
        // Two sessions hold different markers. Each switch rebuilds
        // both maps from that session's log, so each session shows
        // its own last value and ts.
        let mut app = App::new();
        app.set_sessions(vec![SessionId::new("a"), SessionId::new("b")]);
        app.set_active(SessionId::new("a"), vec![loop_phase_marker("ta", "wait")]);
        assert_eq!(
            app.ext_statuses().get(LOOP_PHASE_STATUS_ID),
            Some(&json!("wait"))
        );
        assert_eq!(app.loop_phase_ts(), Some("ta"));
        app.set_active(SessionId::new("b"), vec![loop_phase_marker("tb", "tools")]);
        assert_eq!(
            app.ext_statuses().get(LOOP_PHASE_STATUS_ID),
            Some(&json!("tools")),
            "the other session's marker wins"
        );
        assert_eq!(
            app.loop_phase_ts(),
            Some("tb"),
            "the other session's ts wins"
        );
    }

    // ── thinking level (docs/tui.md section 7.2) ──

    fn thinking_marker(ts: &str, value: &str) -> Event {
        ev(&format!(
            r#"{{"v":1,"type":"ext_status","ts":"{ts}","id":"model_thinking","value":{value}}}"#,
            ts = ts,
            value = value
        ))
    }

    #[test]
    fn thinking_level_tracks_the_last_published_value() {
        // The TUI does not decide the level: it renders whatever
        // the loop or a policy hook published, last event wins.
        for level in 0..THINKING_LEVELS {
            let app = app_with(vec![thinking_marker("t", &level.to_string())], "s1");
            assert_eq!(app.thinking_level(), level, "level {level}");
        }
    }

    #[test]
    fn thinking_level_later_event_wins() {
        let evs = vec![
            thinking_marker("t1", "1"),
            thinking_marker("t2", "3"),
            thinking_marker("t3", "2"),
        ];
        let app = app_with(evs, "s1");
        assert_eq!(app.thinking_level(), 2, "the last value wins");
    }

    #[test]
    fn thinking_level_clamps_an_out_of_range_value() {
        // 4+ is the highest known bucket: a published 7 renders
        // like 4, never past the palette.
        let app = app_with(vec![thinking_marker("t", "7")], "s1");
        assert_eq!(app.thinking_level(), THINKING_LEVELS - 1);
    }

    #[test]
    fn thinking_level_falls_back_to_the_default() {
        // No event, or a value that is not a non-negative integer:
        // the default level (0, no thinking) shows.
        let none = app_with(Vec::new(), "s1");
        assert_eq!(none.thinking_level(), DEFAULT_THINKING_LEVEL);
        for raw in [r#""low"#, "null", "true", "1.5", "-1"] {
            let app = app_with(
                vec![ev(&format!(
                    r#"{{"v":1,"type":"ext_status","ts":"t","id":"model_thinking","value":{raw}}}"#
                ))],
                "s1",
            );
            assert_eq!(
                app.thinking_level(),
                DEFAULT_THINKING_LEVEL,
                "value {raw} falls back to the default"
            );
        }
    }

    #[test]
    fn thinking_level_updates_on_watch_events() {
        let mut app = app_with(vec![thinking_marker("t1", "1")], "s1");
        app.on_watch_item(WatchItem::Event {
            event: thinking_marker("t2", "4"),
            cursor: crate::port::TailCursor::end(),
        });
        assert_eq!(app.thinking_level(), 4, "a new marker recolors");
        app.on_watch_item(WatchItem::Event {
            event: thinking_marker("t3", "0"),
            cursor: crate::port::TailCursor::end(),
        });
        assert_eq!(
            app.thinking_level(),
            DEFAULT_THINKING_LEVEL,
            "a zero marker returns to the default"
        );
    }

    #[test]
    fn thinking_level_session_switch_rebuilds() {
        // Each session carries its own last value; a session with
        // no marker in its log shows the default, not the other
        // session's level.
        let mut app = App::new();
        app.set_sessions(vec![SessionId::new("a"), SessionId::new("b")]);
        app.set_active(SessionId::new("a"), vec![thinking_marker("ta", "3")]);
        assert_eq!(app.thinking_level(), 3);
        app.set_active(SessionId::new("b"), Vec::new());
        assert_eq!(
            app.thinking_level(),
            DEFAULT_THINKING_LEVEL,
            "no marker in b's log: the default applies"
        );
    }

    // ── the browse gate (docs/tui-conversation-browsing.md
    // section 9, the gate rows) ─────────────────────────────

    /// An app in the gated browse state: an active session, the
    /// editor in normal mode, an empty draft.
    fn browse_gated_app() -> App {
        let mut app = app_with(vec![], "s1");
        app.editor().press(Key::Esc); // insert to normal
        app
    }

    #[test]
    fn browse_gate_insert_types_into_the_editor() {
        // "gate: insert": insert mode, empty draft — the first `s`
        // types into the editor, no arm.
        let mut app = app_with(vec![], "s1"); // the editor starts in insert
        assert!(app.press(Key::Char('s')).is_empty());
        assert_eq!(app.take_draft(), "s", "s types into the editor");
        assert!(!app.browse_ref().active(), "no browse");
        assert!(app.ss_arm.is_none(), "no arm in insert mode");
    }

    #[test]
    fn browse_gate_disarm_s_then_i() {
        // "gate: disarm": `s` then `i` inside 3 s — no browse, the
        // armed `s` drops, `i` enters insert alone.
        let mut app = browse_gated_app();
        assert!(app.press(Key::Char('s')).is_empty());
        assert!(app.ss_arm.is_some(), "the first s arms");
        assert!(app.press(Key::Char('i')).is_empty());
        assert!(!app.browse_ref().active(), "no browse");
        assert_eq!(
            app.editor().mode(),
            Mode::Insert,
            "i enters insert alone"
        );
        assert!(app.ss_arm.is_none(), "the disarming key drops the arm");
    }

    #[test]
    fn browse_gate_non_empty_draft_hints() {
        // "gate: non-empty": a held draft in normal mode — the `s`
        // keeps its editor role, no arm, the hint names the path.
        let mut app = browse_gated_app();
        app.editor().set_text("hi");
        assert!(app.press(Key::Char('s')).is_empty());
        assert!(!app.browse_ref().active(), "no browse");
        assert!(app.ss_arm.is_none(), "no arm over a held draft");
        assert!(
            app.status().is_some_and(|s| s.contains("ss browses — clear the draft first")),
            "the hint names the browse path"
        );
    }

    #[test]
    fn browse_gate_enter_s_s() {
        // "gate: enter": `s s` inside 3 s — browse, the cursor is
        // the first visible line col 0, the view does not move.
        let mut app = browse_gated_app();
        assert!(app.press(Key::Char('s')).is_empty());
        assert!(!app.browse_ref().active(), "the first s only arms");
        assert!(app.press(Key::Char('s')).is_empty());
        assert!(app.browse_ref().active(), "the second s enters browse");
        assert_eq!(app.scroll(), 0, "the view does not move on entry");
        // Resolve the entry against a primed layout: the cursor is
        // the first visible line, col 0 (the renderer does this at
        // the first frame).
        let total = 50usize;
        let h = 24usize;
        let scroll = app.scroll();
        app.set_browse_layout(total, h, 40, vec!["line".to_string(); total]);
        app.browse.sync(total, h, &mut app.scroll, false);
        let (l, c) = app.browse_ref().line_col();
        assert_eq!(l, total - h, "the cursor is the first visible line");
        assert_eq!(c, 0, "col 0");
        assert_eq!(app.scroll(), scroll, "the view does not move");
    }

    #[test]
    fn browse_gate_expired_arm_rearms() {
        // "gate: expired arm": the arm expires; a fresh `s` re-arms,
        // no entry.
        let mut app = browse_gated_app();
        assert!(app.press(Key::Char('s')).is_empty());
        app.ss_arm = Some(Instant::now() - SS_ARM_TTL - std::time::Duration::from_millis(1));
        assert!(app.press(Key::Char('s')).is_empty());
        assert!(!app.browse_ref().active(), "an expired arm does not enter");
        assert!(app.ss_arm.is_some(), "a fresh s re-arms");
    }

    #[test]
    fn browse_gate_exit_s_s() {
        // "gate: exit": in browse, `s s` inside 3 s — normal mode,
        // scroll 0, the gutter and the bar drop (the renderer reads
        // the mode).
        let mut app = browse_gated_app();
        app.press(Key::Char('s'));
        app.press(Key::Char('s'));
        assert!(app.browse_ref().active());
        app.scroll_up(30); // back in history
        assert_eq!(app.scroll(), 30);
        assert!(app.press(Key::Char('s')).is_empty());
        assert!(app.press(Key::Char('s')).is_empty());
        assert!(!app.browse_ref().active(), "the double s leaves browse");
        assert_eq!(app.scroll(), 30, "the view stays where browse left it");
        assert_eq!(app.editor().mode(), Mode::Normal, "the editor is in normal mode");
        assert!(app.editor().text().trim().is_empty(), "the draft is still empty");
    }

    #[test]
    fn browse_quit_gate_holds() {
        // "quit in browse": in browse, `q q` — the gate still holds
        // (empty draft, the editor in normal under the overlay), so
        // a double q quits as today.
        let mut app = browse_gated_app();
        app.press(Key::Char('s'));
        app.press(Key::Char('s'));
        assert!(app.browse_ref().active());
        assert!(app.press(Key::Quit).is_empty(), "the first q arms, no quit");
        assert!(!app.should_quit());
        let actions = app.press(Key::Quit);
        assert_eq!(actions, vec![Action::Quit], "the second q quits");
        assert!(app.should_quit());
    }

    #[test]
    fn browse_tab_exits_without_cycling() {
        // Section 4.4: `Tab` in browse mode leaves it. Tab is free
        // for the picker; no session cycle for now.
        let mut app = app_with(vec![], "s1");
        app.set_sessions(vec![SessionId::new("s1"), SessionId::new("s2")]);
        app.editor().press(Key::Esc);
        app.press(Key::Char('s'));
        app.press(Key::Char('s'));
        assert!(app.browse_ref().active());
        let actions = app.press(Key::Tab);
        assert!(actions.is_empty(), "Tab leaves browse without cycling");
        assert!(!app.browse_ref().active(), "browse left");
    }

    #[test]
    fn browse_ctrl_c_and_r_keep_the_host_roles() {
        // Section 4.4: `Ctrl+C` / `Ctrl+R` keep the host stop /
        // start roles under the overlay.
        let mut app = app_with(vec![], "s1");
        app.editor().press(Key::Esc);
        app.press(Key::Char('s'));
        app.press(Key::Char('s'));
        assert_eq!(app.press(Key::CtrlR), vec![Action::RunLoop], "the start role");
        assert_eq!(app.press(Key::CtrlC), vec![Action::StopLoop], "the stop role");
        assert!(app.browse_ref().active(), "the overlay holds");
    }

    #[test]
    fn browse_grow_pins_the_cursor_and_keeps_the_view() {
        // "grow while browsing": a new event lands — the total
        // rises, the cursor pins, the view does not follow.
        let mut app = browse_gated_app();
        app.press(Key::Char('s'));
        app.press(Key::Char('s'));
        let total = 30usize;
        let h = 24usize;
        app.set_browse_layout(total, h, 40, vec!["line".to_string(); total]);
        app.browse.sync(total, h, &mut app.scroll, false);
        let (l0, _) = app.browse_ref().line_col();
        let scroll0 = app.scroll();
        // A new event lands: the total rises by one rendered line.
        app.on_watch_item(WatchItem::Event {
            event: crate::event::produce::user_message("more"),
            cursor: crate::port::TailCursor::end(),
        });
        let total2 = total + 1;
        app.set_browse_layout(total2, h, 40, vec!["line".to_string(); total2]);
        app.browse.sync(total2, h, &mut app.scroll, true);
        let (l1, _) = app.browse_ref().line_col();
        assert_eq!(l1, l0, "the cursor pins to its line number");
        assert_eq!(app.scroll(), scroll0, "the view does not follow the growth");
    }

    #[test]
    fn browse_exit_after_grow_keeps_the_view() {
        // "exit after grow": the growth above, the view moves to
        // the top, then `s s` — the view stays where browse left
        // it, no reset to the tail.
        let mut app = browse_gated_app();
        app.press(Key::Char('s'));
        app.press(Key::Char('s'));
        let total = 30usize;
        let h = 24usize;
        app.set_browse_layout(total, h, 40, vec!["line".to_string(); total]);
        app.browse.sync(total, h, &mut app.scroll, false);
        app.on_watch_item(WatchItem::Event {
            event: crate::event::produce::user_message("more"),
            cursor: crate::port::TailCursor::end(),
        });
        let total2 = total + 1;
        app.set_browse_layout(total2, h, 40, vec!["line".to_string(); total2]);
        app.browse.sync(total2, h, &mut app.scroll, true);
        // The view to the top inside browse.
        app.press(Key::Char('g'));
        app.press(Key::Char('g'));
        let kept = app.scroll();
        assert!(kept > 0, "the view left the tail");
        app.press(Key::Char('s'));
        app.press(Key::Char('s'));
        assert!(!app.browse_ref().active(), "`s s` leaves browse mode");
        assert_eq!(app.scroll(), kept, "the view stays where browse left it");
    }

    #[test]
    fn browse_session_switch_resets_the_state() {
        // "a session switch": the browse state resets with the
        // scroll (section 4.7).
        let mut app = app_with(vec![], "s1");
        app.set_sessions(vec![SessionId::new("s1"), SessionId::new("s2")]);
        app.editor().press(Key::Esc);
        app.press(Key::Char('s'));
        app.press(Key::Char('s'));
        assert!(app.browse_ref().active());
        app.scroll_up(20);
        app.set_active(SessionId::new("s2"), vec![]);
        assert!(!app.browse_ref().active(), "the browse state resets");
        assert_eq!(app.scroll(), 0, "the scroll resets with it");
        assert!(app.ss_arm.is_none(), "the arm resets with it");
    }

    // ── @ picker (docs/tui-file-picker.md) ─────────────────────

    #[test]
    fn picker_opens_when_typing_at_in_insert_mode() {
        let mut app = app_with(vec![], "s1");
        assert!(!app.picker_ref().open);
        app.press(Key::Char('@'));
        assert!(
            app.picker_ref().open,
            "typing @ in insert mode should open the picker"
        );
    }

    #[test]
    fn picker_not_open_for_at_after_word_chars() {
        let mut app = app_with(vec![], "s1");
        app.press(Key::Char('f'));
        app.press(Key::Char('o'));
        app.press(Key::Char('o'));
        app.press(Key::Char('@'));
        assert!(
            !app.picker_ref().open,
            "@ after word chars (foo@) should NOT open the picker"
        );
        assert_eq!(app.draft(), "foo@");
    }

    #[test]
    fn picker_opens_for_at_after_only_spaces() {
        let mut app = app_with(vec![], "s1");
        app.press(Key::Char(' '));
        app.press(Key::Char(' '));
        app.press(Key::Char('@'));
        assert!(
            app.picker_ref().open,
            "@ preceded only by spaces should open the picker"
        );
    }

    #[test]
    fn picker_opens_for_at_after_text_and_space() {
        // After picking a file the draft is e.g. `@docs/file.md`.
        // Typing a space + @ on the same line must still open the
        // picker, because the `@` is preceded by a space.
        let mut app = app_with(vec![], "s1");
        app.press(Key::Char('f'));
        app.press(Key::Char('o'));
        app.press(Key::Char('o'));
        app.press(Key::Char(' '));
        app.press(Key::Char('@'));
        assert!(
            app.picker_ref().open,
            "@ preceded by a space should open the picker even mid-line"
        );
    }

    #[test]
    fn picker_esc_closes_and_keeps_draft() {
        let mut app = app_with(vec![], "s1");
        app.press(Key::Char('@'));
        assert!(app.picker_ref().open);
        app.press(Key::Esc);
        assert!(!app.picker_ref().open, "Esc closes the picker");
        assert_eq!(
            app.draft(),
            "@",
            "ESC keeps the @ token in the draft; no stripping"
        );
    }

    #[test]
    fn picker_enter_commits_selection() {
        let mut app = app_with(vec![], "s1");
        app.press(Key::Char('@'));
        assert!(app.picker_ref().open);
        let item_count = app
            .picker_matcher_ref()
            .as_ref()
            .map(|m| m.snapshot().items.len())
            .unwrap_or(0);
        app.press(Key::Enter);
        assert!(!app.picker_ref().open, "Enter closes the picker");
        if item_count > 0 {
            assert!(
                app.draft().starts_with('@'),
                "the @ marker is preserved before the selected path: draft={}",
                app.draft()
            );
        } else {
            assert_eq!(app.draft(), "@", "zero results: @ kept in draft");
        }
    }

    #[test]
    fn picker_ctrl_j_k_moves_cursor() {
        let mut app = app_with(vec![], "s1");
        app.press(Key::Char('@'));
        assert!(app.picker_ref().open);
        app.press(Key::CtrlJ);
        app.press(Key::CtrlJ);
        assert_eq!(
            app.picker_ref().cursor(),
            2,
            "ctrl+j moves the cursor down two positions"
        );
        app.press(Key::CtrlK);
        assert_eq!(
            app.picker_ref().cursor(),
            1,
            "ctrl+k moves the cursor up one position"
        );
    }

    // ── command palette: `:b ` auto-goto session list ───────────

    #[test]
    fn palette_b_space_enters_session_list() {
        let mut app = app_with(vec![], "s1");
        app.set_sessions(vec![
            SessionId::new("alpha"),
            SessionId::new("beta"),
            SessionId::new("gamma"),
        ]);
        app.editor().press(Key::Esc);
        // Open the palette with `:`.
        app.press(Key::Char(':'));
        assert!(app.palette_state().open);
        assert_eq!(
            app.palette_state().stage,
            crate::palette::state::PaletteStage::Root,
            "palette starts in Root stage"
        );
        // Type 'b' to narrow to the Goto item.
        app.press(Key::Char('b'));
        assert_eq!(
            app.palette_state().stage,
            crate::palette::state::PaletteStage::Root,
            "still in Root after typing 'b'"
        );
        // Type space to trigger auto-goto.
        app.press(Key::Char(' '));
        assert_eq!(
            app.palette_state().stage,
            crate::palette::state::PaletteStage::SessionList,
            "typing space after 'b' enters SessionList"
        );
        // The query now has the "b " prefix recorded.
        assert_eq!(app.palette_state().goto_prefix_len, 2);
        // filter_query should return the empty string after trimming.
        assert_eq!(app.palette_state().filter_query(), "");

        // Now type a filter to narrow sessions.
        app.press(Key::Char('b'));
        assert_eq!(
            app.palette_state().filter_query(),
            "b",
            "filter is the text after the 'b ' prefix"
        );

        // Esc drops back to Root.
        app.press(Key::Esc);
        assert_eq!(
            app.palette_state().stage,
            crate::palette::state::PaletteStage::Root,
            "Esc in SessionList drops back to Root"
        );
        assert!(app.palette_state().open, "palette stays open");
    }

    #[test]
    fn palette_b_filter_shows_matching_sessions() {
        let mut app = app_with(vec![], "s1");
        app.set_sessions(vec![
            SessionId::new("alpha"),
            SessionId::new("beta"),
            SessionId::new("gamma"),
        ]);
        app.editor().press(Key::Esc);
        app.press(Key::Char(':'));
        app.press(Key::Char('b'));
        app.press(Key::Char(' '));
        // Type "al" to filter for "alpha".
        app.press(Key::Char('a'));
        app.press(Key::Char('l'));
        let items = app.palette_ranked();
        assert_eq!(items.len(), 1, "filter 'al' should match only alpha");
        assert_eq!(items[0].id, "alpha");
    }

    #[test]
    fn palette_q_key_types_into_query() {
        let mut app = app_with(vec![], "s1");
        app.editor().press(Key::Esc);
        app.press(Key::Char(':'));
        assert!(app.palette_state().open);
        // `q` is mapped to Key::Quit in main.rs; in the palette it
        // must type the character into the query, not trigger quit.
        let result = app.press(Key::Quit);
        assert!(result.is_empty(), "Key::Quit in palette should not quit");
        assert_eq!(
            app.palette_state().query,
            "q",
            "typing q should filter the palette query"
        );
        // Typing more: `q q` should not quit (the quit gate only
        // fires outside the palette).
        let result2 = app.press(Key::Quit);
        assert!(result2.is_empty(), "second q should also just type");
        assert_eq!(app.palette_state().query, "qq");
    }

    #[test]
    fn events_vec_is_capped_on_session_load() {
        let evs: Vec<Event> = (0..EVENTS_CAP + 100)
            .map(|i| {
                ev(&format!(
                    r#"{{"v":1,"type":"user_message","ts":"{i}","content":"{i}"}}"#
                ))
            })
            .collect();
        let app = app_with(evs, "s1");
        assert_eq!(
            app.events().len(),
            EVENTS_CAP,
            "set_active trims the log down to the cap"
        );
        assert_eq!(
            app.events().last().unwrap().kind(),
            EventKind::UserMessage,
            "the newest events survive the trim"
        );
    }

    #[test]
    fn events_vec_is_capped_by_the_tailer() {
        let evs: Vec<Event> = (0..EVENTS_CAP).map(|_| ev(r#"{"v":1,"type":"user_message","ts":"t","content":"x"}"#)).collect();
        let mut app = app_with(evs, "s1");
        for _ in 0..EVENTS_CAP {
            app.on_watch_item(WatchItem::Event {
                event: ev(r#"{"v":1,"type":"user_message","ts":"t","content":"y"}"#),
                cursor: crate::port::TailCursor::end(),
            });
        }
        assert_eq!(
            app.events().len(),
            EVENTS_CAP,
            "the tailer must not grow the Vec past the cap"
        );
        assert_eq!(
            app.events()[0].get_str("content"),
            Some("y"),
            "the oldest (x) lines drop first"
        );
    }

    /// The P9 user flow through the real key dispatch, git-repo
    /// variant: the `@sessions` case (docs/tui-file-picker.md P9).
    /// A git-ignored `sessions/` dir is invisible at the default
    /// scope; each `Ctrl+I` widens the list until the session files
    /// surface.
    #[test]
    fn ctrl_i_surfaces_git_ignored_session_files() {
        // A throwaway git repo with an ignored `sessions/` dir,
        // mirroring the real harness layout.
        let tmp = std::env::temp_dir().join("picker_test_appscope_git");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("sessions/demo")).unwrap();
        std::fs::create_dir_all(tmp.join("src")).unwrap();
        std::fs::write(tmp.join(".gitignore"), "sessions/\n").unwrap();
        std::fs::write(tmp.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(tmp.join("sessions/demo/notes.txt"), "{}\n").unwrap();
        let git_init = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&tmp)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        let git_add = std::process::Command::new("git")
            .args(["add", "src", ".gitignore"])
            .current_dir(&tmp)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !git_init || !git_add {
            eprintln!("skipped: git init/add unavailable in the fixture");
            let _ = std::fs::remove_dir_all(&tmp);
            return;
        }

        let mut app = App::new();
        let src = crate::picker::items::FileItemSource::new(tmp.clone());
        // Mirror `open_picker` for the `@sessions` seed query: the
        // picker opens, the source and root are set, the live query
        // is pushed.
        let std_items = src.collect_in(&tmp, crate::picker::items::FileScope::Standard);
        let matcher = crate::picker::fuzzy::PickerMatcher::new(std_items);
        app.picker.open("sessions", 10);
        matcher.query_ranked("sessions", "sessions");
        app.picker_matcher = Some(matcher);
        app.picker_source = Some(src);
        app.picker_root = tmp.clone();

        // Default scope: the git-ignored session file is absent, so
        // the query matches nothing.
        std::thread::sleep(std::time::Duration::from_millis(100));
        let snap = app.picker_matcher_ref().as_ref().unwrap().snapshot();
        assert!(
            snap.items.iter().all(|i| !i.label.starts_with("sessions/")),
            "default scope hides the ignored session dir: {:?}",
            snap.items.iter().map(|i| i.label.clone()).collect::<Vec<_>>()
        );

        // 1st Ctrl+I: the git-ignored files enter the list.
        app.press(Key::CtrlI);
        assert_eq!(
            app.picker_ref().scope,
            crate::picker::items::FileScope::IncludeIgnored
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
        let snap = app.picker_matcher_ref().as_ref().unwrap().snapshot();
        assert!(
            snap.items.iter().any(|i| i.label.starts_with("sessions/demo/")),
            "the ignored session files surface after the first Ctrl+I: {:?}",
            snap.items.iter().map(|i| i.label.clone()).collect::<Vec<_>>()
        );

        // 2nd and 3rd: hidden too, then back to the default.
        app.press(Key::CtrlI);
        assert_eq!(
            app.picker_ref().scope,
            crate::picker::items::FileScope::IncludeHidden
        );
        app.press(Key::CtrlI);
        assert_eq!(
            app.picker_ref().scope,
            crate::picker::items::FileScope::Standard
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
        let snap = app.picker_matcher_ref().as_ref().unwrap().snapshot();
        assert!(
            snap.items.iter().all(|i| !i.label.starts_with("sessions/")),
            "the third Ctrl+I returns to the default scope"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn events_vec_capped_in_set_active() {
        let evs: Vec<Event> = (0..EVENTS_CAP + 500)
            .map(|i| {
                ev(&format!(
                    r#"{{"v":1,"type":"user_message","ts":"{i}","content":"x"}}"#
                ))
            })
            .collect();
        let mut app = App::new();
        app.set_sessions(vec![SessionId::new("s1")]);
        app.set_active(SessionId::new("s1"), evs);
        assert_eq!(
            app.events().len(),
            EVENTS_CAP,
            "set_active must trim the Vec to EVENTS_CAP"
        );
    }

    /// A non-git fixture directory for the P9 app-level test: prefer
    /// the system temp dir; some sandboxes carry a `.git` into /tmp,
    /// so fall back to the home dir when every temp ancestor is inside
    /// a repo.
    fn non_git_fixture(name: &str) -> Option<std::path::PathBuf> {
        let mut bases = vec![std::env::temp_dir()];
        if let Ok(home) = std::env::var("HOME") {
            bases.push(std::path::PathBuf::from(home));
        }
        for base in bases {
            let dir = base.join(name);
            if std::fs::create_dir_all(&dir).is_ok()
                && !dir.ancestors().any(|a| a.join(".git").exists())
            {
                return Some(dir);
            }
        }
        None
    }

    /// The P9 flow through the real key dispatch: with the picker
    /// open, `Ctrl+I` cycles the file scope and the item list is
    /// re-enumerated and re-ranked under the new scope
    /// (docs/tui-file-picker.md P9).
    #[test]
    fn ctrl_i_recollects_picker_items_under_new_scope() {
        let fixture = match non_git_fixture("picker_test_appscope") {
            Some(f) => f,
            None => {
                eprintln!("skipped: no non-git fixture base available");
                return;
            }
        };
        let _ = std::fs::remove_dir_all(&fixture);
        std::fs::create_dir_all(&fixture).unwrap();
        std::fs::write(fixture.join("visible.txt"), "x").unwrap();
        std::fs::create_dir_all(fixture.join("target")).unwrap();
        std::fs::write(fixture.join("target/out.bin"), "x").unwrap();
        std::fs::write(fixture.join(".env"), "x").unwrap();
        std::fs::create_dir_all(fixture.join(".hid")).unwrap();
        std::fs::write(fixture.join(".hid/x.txt"), "x").unwrap();

        let mut app = App::new();
        let src = crate::picker::items::FileItemSource::new(fixture.clone());
        let std_items = src.collect_in(&fixture, crate::picker::items::FileScope::Standard);
        assert!(
            std_items
                .iter()
                .all(|i| !i.label.starts_with('.') && !i.label.starts_with("target/")),
            "premise: the default scope hides hidden and build files: {std_items:?}"
        );
        app.picker.open("", 10);
        app.picker_matcher = Some(crate::picker::fuzzy::PickerMatcher::new(std_items));
        app.picker_source = Some(src);
        app.picker_root = fixture.clone();

        // 1st press: show the ignored set (build dirs in a plain walk).
        app.press(Key::CtrlI);
        assert_eq!(
            app.picker_ref().scope,
            crate::picker::items::FileScope::IncludeIgnored,
            "1st press advances to IncludeIgnored"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
        let snap = app.picker_matcher_ref().as_ref().unwrap().snapshot();
        let labels: Vec<String> = snap.items.iter().map(|i| i.label.clone()).collect();
        assert!(
            labels.iter().any(|l| l == "target/out.bin"),
            "ignored files now show: {labels:?}"
        );
        assert!(
            !labels.iter().any(|l| l == ".env"),
            "hidden files stay hidden at the ignored scope: {labels:?}"
        );
        assert_eq!(app.status(), Some("scope: showing git-ignored files"));

        // 2nd press: show the hidden set too.
        app.press(Key::CtrlI);
        assert_eq!(
            app.picker_ref().scope,
            crate::picker::items::FileScope::IncludeHidden,
            "2nd press advances to IncludeHidden"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
        let snap = app.picker_matcher_ref().as_ref().unwrap().snapshot();
        let labels: Vec<String> = snap.items.iter().map(|i| i.label.clone()).collect();
        assert!(
            labels.iter().any(|l| l == ".env"),
            "hidden files now show: {labels:?}"
        );
        assert!(
            labels.iter().any(|l| l.starts_with(".hid/")),
            "hidden dirs now show: {labels:?}"
        );
        assert_eq!(app.status(), Some("scope: showing hidden files too"));

        // 3rd press: back to the default.
        app.press(Key::CtrlI);
        assert_eq!(
            app.picker_ref().scope,
            crate::picker::items::FileScope::Standard,
            "3rd press returns to the default"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
        let snap = app.picker_matcher_ref().as_ref().unwrap().snapshot();
        let labels: Vec<String> = snap.items.iter().map(|i| i.label.clone()).collect();
        assert!(
            !labels.iter().any(|l| l == "target/out.bin"),
            "build files are hidden again: {labels:?}"
        );
        assert!(
            !labels.iter().any(|l| l == ".env"),
            "hidden files are hidden again: {labels:?}"
        );

        let _ = std::fs::remove_dir_all(&fixture);
    }

    /// The real-terminal half of P9: crossterm delivers a physical
    /// `Ctrl+I` press as `KeyCode::Tab` (both are byte 0x09), so
    /// `Key::Tab` must drive the scope cycle and recollect exactly
    /// like `Key::CtrlI` does.
    #[test]
    fn tab_recollects_picker_items_under_new_scope() {
        let fixture = match non_git_fixture("picker_test_appscope_tab") {
            Some(f) => f,
            None => {
                eprintln!("skipped: no non-git fixture base available");
                return;
            }
        };
        let _ = std::fs::remove_dir_all(&fixture);
        std::fs::create_dir_all(&fixture).unwrap();
        std::fs::write(fixture.join("visible.txt"), "x").unwrap();
        std::fs::create_dir_all(fixture.join("target")).unwrap();
        std::fs::write(fixture.join("target/out.bin"), "x").unwrap();

        let mut app = App::new();
        let src = crate::picker::items::FileItemSource::new(fixture.clone());
        let std_items = src.collect_in(&fixture, crate::picker::items::FileScope::Standard);
        app.picker.open("", 10);
        app.picker_matcher = Some(crate::picker::fuzzy::PickerMatcher::new(std_items));
        app.picker_source = Some(src);
        app.picker_root = fixture.clone();

        // The press a terminal actually delivers for Ctrl+I.
        app.press(Key::Tab);
        assert_eq!(
            app.picker_ref().scope,
            crate::picker::items::FileScope::IncludeIgnored,
            "Tab (== Ctrl+I) advances to IncludeIgnored"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
        let snap = app.picker_matcher_ref().as_ref().unwrap().snapshot();
        let labels: Vec<String> = snap.items.iter().map(|i| i.label.clone()).collect();
        assert!(
            labels.iter().any(|l| l == "target/out.bin"),
            "the re-collected list shows the widened set: {labels:?}"
        );
        assert_eq!(app.status(), Some("scope: showing git-ignored files"));

        // BackTab stays a no-op: it is a distinct CSI sequence, not
        // the 0x09 byte.
        app.press(Key::BackTab);
        assert_eq!(
            app.picker_ref().scope,
            crate::picker::items::FileScope::IncludeIgnored,
            "BackTab does not cycle the scope"
        );

        let _ = std::fs::remove_dir_all(&fixture);
    }

    // ── stream-channel tests (docs/tui-streaming-response.md P4–P7) ──

    /// P5: a fresh TUI (or session switch) reads the stream file from
    /// byte 0 and rebuilds the live buffer.
    #[test]
    fn refresh_stream_reads_from_byte_zero_on_fresh_app() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".model-stream");
        std::fs::write(
            &path,
            concat!(
                r#"{"kind":"text","delta":"Hello "}
"#,
                r#"{"kind":"text","delta":"world"}
"#,
                r#"{"kind":"done","stop_reason":"stop"}
"#,
            ),
        ).unwrap();

        let app = app_with(Vec::new(), "s1");
        assert!(app.stream_buf().is_none());

        let mut app = app;
        app.refresh_stream(&path);
        // The content is paced (§6.5): the `done` line already landed,
        // so one release drains the whole queue.
        app.pump_stream_pacing();

        let buf = app.stream_buf().expect("stream buffer should be set");
        assert_eq!(buf.text, "Hello world");
        assert!(buf.done);
        assert_eq!(app.stream_pending_chars(), 0, "the queue drained");
        drop(dir);
    }

    /// P4: the live buffer settles (clears) when the authoritative
    /// `assistant_message` event arrives in the log.
    #[test]
    fn settle_event_clears_stream_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".model-stream");
        std::fs::write(
            &path,
            r#"{"kind":"text","delta":"Hello"}
"#,
        ).unwrap();

        let mut app = app_with(Vec::new(), "s1");
        app.refresh_stream(&path);
        assert!(app.stream_buf().is_some(), "buffer populated before settle");

        // Simulate the assistant_message event arriving in the log.
        let settle = ev(r#"{"v":1,"type":"assistant_message","ts":"t","id":"a1","content":"Hello world"}"#);
        app.on_watch_item(WatchItem::Event {
            event: settle,
            cursor: crate::port::TailCursor::end(),
        });
        assert!(
            app.stream_buf().is_none(),
            "settle event must clear the stream buffer"
        );
        drop(dir);
    }

    /// P7: a session without a `.model-stream` file draws no live block.
    #[test]
    fn refresh_stream_missing_file_clears_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join(".model-stream");

        let mut app = app_with(Vec::new(), "s1");
        // Seed the buffer first so the test is meaningful.
        app.refresh_stream(&missing); // file does not exist → clear
        assert!(app.stream_buf().is_none());

        // Manually seed a buffer, then call refresh on a missing file.
        let seed = dir.path().join(".seed");
        std::fs::write(&seed, r#"{"kind":"text","delta":"partial"}
"#).unwrap();
        app.refresh_stream(&seed);
        assert!(app.stream_buf().is_some());

        app.refresh_stream(&missing); // missing → clear
        assert!(app.stream_buf().is_none(), "missing file must clear the buffer");
        drop(dir);
    }

    /// P5 (shrink): when the stream file is re-created shorter (new model
    /// call), the offset resets and the buffer clears.
    #[test]
    fn refresh_stream_shrunk_file_resets_offset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".model-stream");

        let mut app = app_with(Vec::new(), "s1");
        // Two complete lines plus a partial third line (no newline):
        // the offset parks after line 2, holding the partial for the
        // next poll.
        std::fs::write(
            &path,
            concat!(
                r#"{"kind":"text","delta":"a"}
"#,
                r#"{"kind":"text","delta":"b"}
"#,
                r#"{"kind":"tex"# // partial line, no newline
            ),
        )
        .unwrap();
        let full = std::fs::metadata(&path).unwrap().len() as u64;
        app.refresh_stream(&path);
        assert!(app.stream_offset > 0, "offset advanced past complete lines");
        assert!(
            app.stream_offset < full,
            "the partial line must stay for the next poll: offset {} of {full}",
            app.stream_offset
        );

        // Simulate a new model call: the loop re-created the file and it
        // is now shorter than the parked offset.
        std::fs::write(&path, r#"{"kind":"text","delta":"c"}
"#).unwrap();
        app.refresh_stream(&path);
        // Paced release (§6.5): release the queued "c" into the
        // buffer.
        while app.stream_pending_chars() > 0 {
            app.pump_stream_pacing();
        }
        let buf = app
            .stream_buf()
            .expect("buffer re-seeded from the new file");
        assert_eq!(buf.text, "c", "old buffer content must not linger");
        assert_eq!(
            app.stream_offset,
            std::fs::metadata(&path).unwrap().len() as u64
        );
        drop(dir);
    }

    /// P6 (error): a model-call error event clears the live block; the
    /// `error` event in the log is the authoritative record.
    #[test]
    fn error_event_clears_stream_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".model-stream");
        std::fs::write(&path, r#"{"kind":"text","delta":"partial"}
"#).unwrap();

        let mut app = app_with(Vec::new(), "s1");
        app.refresh_stream(&path);
        assert!(app.stream_buf().is_some());

        let err = ev(r#"{"v":1,"type":"error","ts":"t","id":"e1","message":"upstream 500"}"#);
        app.on_watch_item(WatchItem::Event {
            event: err,
            cursor: crate::port::TailCursor::end(),
        });
        assert!(app.stream_buf().is_none(), "error event must clear the buffer");
        drop(dir);
    }

    /// ex_empty_response (lean/TuiStreamSpec.lean): an empty response
    /// (no deltas at all) still settles exactly once. Consumer side:
    /// a done-only channel opens the live buffer with empty text and
    /// marks it done, so the settled state is reached, not lost.
    #[test]
    fn ex_empty_response_done_only_channel_settles() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".model-stream");
        std::fs::write(
            &path,
            r#"{"kind":"done","stop_reason":"stop"}
"#,
        )
        .unwrap();

        let mut app = app_with(Vec::new(), "s1");
        app.refresh_stream(&path);

        let buf = app
            .stream_buf()
            .expect("a done-only channel still opens the live buffer");
        assert_eq!(buf.text, "");
        assert!(buf.done, "the empty response is marked done");
        drop(dir);
    }

    /// ex_two_responses (lean/TuiStreamSpec.lean): two consecutive
    /// responses settle in order. Consumer side: the first call
    /// settles via the authoritative `assistant_message` event (the
    /// live buffer clears), the second call re-uses the same channel
    /// file and carries only its own content; the transcript holds
    /// both entries in order.
    #[test]
    fn ex_two_responses_settle_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".model-stream");

        let mut app = app_with(Vec::new(), "s1");

        // First response: a delta, then the log's assistant_message
        // settles it out of the live buffer.
        std::fs::write(
            &path,
            concat!(
                r#"{"kind":"text","delta":"A"}
"#,
                r#"{"kind":"done","stop_reason":"stop"}
"#,
            ),
        )
        .unwrap();
        app.refresh_stream(&path);
        app.pump_stream_pacing(); // the `done` line drains the queue (§6.5)
        assert_eq!(
            app.stream_buf().expect("first call in flight").text,
            "A"
        );
        let settle1 =
            ev(r#"{"v":1,"type":"assistant_message","ts":"t","id":"a1","content":"A"}"#);
        app.on_watch_item(WatchItem::Event {
            event: settle1,
            cursor: crate::port::TailCursor::end(),
        });
        assert!(
            app.stream_buf().is_none(),
            "the first response settles out of the live buffer"
        );

        // Second response on the same channel file (the loop re-created
        // it; after the settle the offset was cleared, so it is read
        // from byte 0).
        std::fs::write(
            &path,
            concat!(
                r#"{"kind":"text","delta":"B"}
"#,
                r#"{"kind":"done","stop_reason":"stop"}
"#,
            ),
        )
        .unwrap();
        app.refresh_stream(&path);
        app.pump_stream_pacing(); // the `done` line drains the queue (§6.5)
        assert_eq!(
            app.stream_buf()
                .expect("second call in flight")
                .text,
            "B",
            "only the second response's content is live"
        );

        let settle2 =
            ev(r#"{"v":1,"type":"assistant_message","ts":"t","id":"a2","content":"B"}"#);
        app.on_watch_item(WatchItem::Event {
            event: settle2,
            cursor: crate::port::TailCursor::end(),
        });
        assert!(
            app.stream_buf().is_none(),
            "the second response settles out of the live buffer"
        );

        // Both responses settled into the transcript, in order.
        let settled: Vec<String> = app
            .events
            .iter()
            .filter(|e| e.kind() == crate::event::EventKind::AssistantMessage)
            .filter_map(|e| e.get_str("content").map(str::to_string))
            .collect();
        assert_eq!(settled, vec!["A".to_string(), "B".to_string()]);
        drop(dir);
    }

    /// §6.5: a small backlog reads as a steady typewriter — one
    /// character per release while the backlog stays under the pace
    /// window.
    #[test]
    fn stream_pacing_typewriter_on_small_backlog() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".model-stream");
        std::fs::write(
            &path,
            r#"{"kind":"text","delta":"0123456789"}
"#,
        )
        .unwrap();

        let mut app = app_with(Vec::new(), "s1");
        app.refresh_stream(&path);
        assert_eq!(
            app.stream_buf().expect("buffer opened").text,
            "",
            "nothing is released before the first pump"
        );
        assert_eq!(app.stream_pending_chars(), 10);

        for (n, want) in ["0", "01", "012"].iter().enumerate() {
            app.pump_stream_pacing();
            assert_eq!(
                app.stream_buf().as_ref().unwrap().text,
                *want,
                "release {n}"
            );
        }
        assert_eq!(app.stream_pending_chars(), 7, "three chars released");
        drop(dir);
    }

    /// §6.5: a large backlog catches up — the release budget is
    /// `backlog / STREAM_PACE_FRAMES`, so the queue clears in about
    /// that many frames instead of trickling.
    #[test]
    fn stream_pacing_catches_up_a_large_backlog() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".model-stream");
        // The channel writer terminates every line; `refresh_stream`
        // consumes complete lines only.
        std::fs::write(
            &path,
            format!(
                r#"{{"kind":"text","delta":"{}"}}
"#,
                "a".repeat(3000)
            ),
        )
        .unwrap();

        let mut app = app_with(Vec::new(), "s1");
        app.refresh_stream(&path);
        app.pump_stream_pacing();

        let buf = app.stream_buf().expect("buffer opened");
        assert_eq!(buf.text.len(), 200, "3000 / 15 chars released");
        assert_eq!(app.stream_pending_chars(), 2800);
        drop(dir);
    }

    /// §6.5: once the channel is `done`, the whole remainder
    /// releases at once (the final text settles promptly).
    #[test]
    fn stream_pacing_drains_all_when_done() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".model-stream");
        std::fs::write(
            &path,
            concat!(
                r#"{"kind":"text","delta":"xy"}
"#,
                r#"{"kind":"done","stop_reason":"stop"}
"#,
            ),
        )
        .unwrap();

        let mut app = app_with(Vec::new(), "s1");
        app.refresh_stream(&path);
        assert_eq!(app.stream_pending_chars(), 2, "queued until the pump");
        app.pump_stream_pacing();
        assert_eq!(app.stream_buf().expect("buffer opened").text, "xy");
        assert_eq!(app.stream_pending_chars(), 0);
        drop(dir);
    }

    /// §6.5: the release is strictly FIFO across kinds — reasoning
    /// content queued before text content reaches the buffer first.
    #[test]
    fn stream_pacing_keeps_reasoning_before_text_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".model-stream");
        std::fs::write(
            &path,
            concat!(
                r#"{"kind":"reasoning","item_id":"r1","delta":"aa"}
"#,
                r#"{"kind":"text","delta":"bb"}
"#,
                r#"{"kind":"done","stop_reason":"stop"}
"#,
            ),
        )
        .unwrap();

        let mut app = app_with(Vec::new(), "s1");
        app.refresh_stream(&path);
        while app.stream_pending_chars() > 0 {
            app.pump_stream_pacing();
        }
        let buf = app.stream_buf().expect("buffer opened");
        assert_eq!(buf.reasoning.get("r1").map(String::as_str), Some("aa"));
        assert_eq!(buf.text, "bb");
        drop(dir);
    }

    /// §6.5: a release that ends mid-delta splits at a character
    /// boundary (multi-byte safe), keeping the remainder queued.
    #[test]
    fn stream_pacing_splits_at_char_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".model-stream");
        std::fs::write(
            &path,
            r#"{"kind":"text","delta":"héllo"}
"#,
        )
        .unwrap();

        let mut app = app_with(Vec::new(), "s1");
        app.refresh_stream(&path);
        app.pump_stream_pacing();
        assert_eq!(app.stream_buf().expect("buffer opened").text, "h");
        app.pump_stream_pacing();
        assert_eq!(
            app.stream_buf().expect("buffer opened").text,
            "hé",
            "the release respects the multi-byte boundary"
        );
        assert_eq!(app.stream_pending_chars(), 3);
        drop(dir);
    }

    /// §6.5: clearing the live buffer (a settle event) drops the
    /// pace queue — the transcript is authoritative from here.
    #[test]
    fn clear_stream_drops_the_pace_queue() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".model-stream");
        std::fs::write(
            &path,
            r#"{"kind":"text","delta":"abc"}
"#,
        )
        .unwrap();

        let mut app = app_with(Vec::new(), "s1");
        app.refresh_stream(&path);
        assert_eq!(app.stream_pending_chars(), 3);
        app.clear_stream();
        assert!(app.stream_buf().is_none());
        assert_eq!(app.stream_pending_chars(), 0);
        drop(dir);
    }
}
