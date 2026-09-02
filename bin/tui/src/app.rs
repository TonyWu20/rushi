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
/// Scroll distance kept between the viewport top and the log end. The
/// transcript is capped anyway, so the scroll clamps at draw time.
const SCROLL_CAP: usize = 100_000;

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
    pub fn set_active(&mut self, id: SessionId, events: Vec<Event>) {
        let (statuses, status_ts, order) = ext_status_map(&events);
        self.active = Some(id);
        self.events = events;
        self.scroll = 0;
        self.events_version += 1;
        self.ext_status_values = statuses;
        self.ext_status_ts = status_ts;
        self.ext_status_order = order;
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
                }
                self.events.push(event);
                self.events_version += 1;
            }
            WatchItem::Gone => self.flash("session log missing — waiting for it to come back"),
            WatchItem::Resumed => self.flash("session log restored — resuming"),
            WatchItem::IoError { message } => self.flash(format!("log read error: {message}")),
        }
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
        self.events[from..]
            .iter()
            .filter(|e| e.kind() == EventKind::UserMessage)
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
                        "name input cancelled — pass a session argument, or Tab an existing session",
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
                Key::Tab => {
                    if let Some(actions) = self.cycle_naming(1) {
                        return actions;
                    }
                    return Vec::new();
                }
                Key::BackTab => {
                    if let Some(actions) = self.cycle_naming(-1) {
                        return actions;
                    }
                    return Vec::new();
                }
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
            Key::Tab => vec![Action::CycleSessions(1)],
            Key::BackTab => vec![Action::CycleSessions(-1)],
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
                if let Some(h) = self.editor().press(Key::Char(c)) {
                    self.flash(h);
                }
                Vec::new()
            }
        }
    }

    pub fn should_quit(&self) -> bool {
        self.quitting
    }

    /// Tab / BackTab while the name input is up. Cycling to a real
    /// session ends the input: the typed name is abandoned. With no
    /// session to cycle to, the input stays up.
    /// Some(action) = end naming and emit the cycle; None = keep naming.
    fn cycle_naming(&mut self, delta: i32) -> Option<Vec<Action>> {
        if self.cycle_target(delta).is_none() {
            self.flash("no sessions to cycle to — keep typing the name");
            return None;
        }
        self.pending_name = None;
        self.flash("name input cancelled — cycling sessions");
        Some(vec![Action::CycleSessions(delta)])
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
    fn tab_while_naming_cycles_and_ends_the_input() {
        let mut app = App::new();
        app.set_sessions(vec![SessionId::new("a"), SessionId::new("b")]);
        app.start_naming();
        app.press(Key::Char('x'));
        assert_eq!(app.press(Key::Tab), vec![Action::CycleSessions(1)]);
        assert_eq!(app.pending_name(), None, "cycling ends the name input");
        assert!(app.status().is_some());
        // Without a session to cycle to, the input stays up.
        let mut app = App::new();
        app.start_naming();
        assert!(app.press(Key::Tab).is_empty());
        assert_eq!(app.pending_name(), Some(""), "no target: input stays");
        assert!(app.status().is_some());
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
}
