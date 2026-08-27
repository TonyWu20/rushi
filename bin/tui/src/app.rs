//! Application state: what the TUI shows and how keys map to actions.
//!
//! This module never touches the port or I/O: it turns key events into
//! [`Action`]s and folds port results back in. `main.rs` executes the
//! actions; the tests here drive the state machine directly.

use std::collections::HashMap;
use std::time::Instant;

use ratatui::text::Line;

use crate::event::{Event, EventKind};
use crate::port::{LoopHandle, LoopLine, SessionId, WatchItem};
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
    Esc,
    Quit,
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
    RunLoop,
    /// Stop the active session's loop and append a `cancel` event.
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
    /// Quit: stop every running loop, leave the log intact.
    Quit,
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
    draft: String,
    loops: HashMap<SessionId, LoopState>,
    status: Option<(String, Instant)>,
    watch_rx: Option<std::sync::mpsc::Receiver<WatchItem>>,
    quitting: bool,
    /// Armed since the first `q`; a second `q` inside the window quits.
    /// Any other key disarms. Mistouch safety (single-key `q` is too
    /// easy to hit by accident mid-typing).
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
    /// width, ext reply version). A scroll redraw reuses the cache:
    /// O(viewport) instead of O(total lines). The extension reply
    /// version folds in, so a new reply rebuilds the lines
    /// (ui-extension-plan stage 1).
    transcript_cache: Option<(u64, usize, u64, Vec<Line<'static>>)>,
    /// Latest `ext_status` values, id to value, for the active
    /// session. Maintained incrementally: `set_active` builds it
    /// and each appended watch event updates it. A tick reads this
    /// map in O(1) instead of rescanning the whole log
    /// (ui-extension-plan stage 1, tick payload).
    ext_status_values: HashMap<String, Value>,
}

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
/// adds no entry; a missing `value` counts as `null`.
fn ext_status_map(events: &[Event]) -> HashMap<String, Value> {
    let mut m = HashMap::new();
    for e in events {
        if e.kind() != EventKind::ExtStatus {
            continue;
        }
        let Some(id) = e.get_str("id") else {
            continue;
        };
        let value = e.get("value").cloned().unwrap_or(Value::Null);
        m.insert(id.to_string(), value);
    }
    m
}

const STATUS_TTL: std::time::Duration = std::time::Duration::from_secs(4);
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
            draft: String::new(),
            loops: HashMap::new(),
            status: None,
            watch_rx: None,
            quitting: false,
            quit_arm: None,
            pending_name: None,
            ext_status_values: HashMap::new(),
            viewport: 0,
            events_version: 0,
            transcript_cache: None,
        }
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
        let statuses = ext_status_map(&events);
        self.active = Some(id);
        self.events = events;
        self.scroll = 0;
        self.events_version += 1;
        self.ext_status_values = statuses;
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
                // Later events win, like the log order.
                if event.kind() == EventKind::ExtStatus {
                    let value = event.get("value").cloned().unwrap_or(Value::Null);
                    if let Some(id) = event.get_str("id") {
                        self.ext_status_values.insert(id.to_string(), value);
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
        if let Some((v, w, ev, _)) = &self.transcript_cache {
            if *v == self.events_version && *w == width && *ev == ext_ver {
                return &self.transcript_cache.as_ref().unwrap().3;
            }
        }
        let lines = crate::render::build_transcript_lines(self, width, ext);
        self.transcript_cache = Some((self.events_version, width, ext_ver, lines));
        &self.transcript_cache.as_ref().unwrap().3
    }

    /// Events of the active session, oldest first.
    pub fn events(&self) -> &[Event] {
        &self.events
    }

    /// Map tool_call id -> tool name, for result-line rendering.
    /// Pure presentation lookup, not decision logic.
    pub fn call_names(&self) -> HashMap<String, String> {
        let mut m = HashMap::new();
        for e in &self.events {
            if e.kind() == EventKind::ToolCall {
                if let (Some(id), Some(name)) = (e.get_str("id"), e.get_str("name")) {
                    m.insert(id.to_string(), name.to_string());
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

    pub fn draft(&self) -> &str {
        &self.draft
    }

    pub fn take_draft(&mut self) -> String {
        std::mem::take(&mut self.draft)
    }

    pub fn set_draft(&mut self, text: String) {
        self.draft = text;
    }

    // ── approvals ───────────────────────────────────────────────

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
            Key::Quit => match self.quit_arm {
                Some(at) if at.elapsed() < QUIT_ARM_TTL => {
                    self.quitting = true;
                    vec![Action::Quit]
                }
                _ => {
                    self.quit_arm = Some(Instant::now());
                    self.flash("press q again to quit");
                    Vec::new()
                }
            },
            Key::Enter => {
                if self.draft.trim().is_empty() {
                    self.flash("empty message — type something first");
                    Vec::new()
                } else if self.active.is_none() {
                    self.flash("no session — pass a session name argument");
                    Vec::new()
                } else {
                    vec![Action::SendDraft]
                }
            }
            Key::Backspace => {
                self.draft.pop();
                Vec::new()
            }
            Key::CtrlR => {
                if self.active.is_none() {
                    self.flash("no session to run the loop for");
                    return Vec::new();
                }
                let sid = self.active.clone().unwrap();
                if self.loop_running(&sid) {
                    self.flash("loop already running");
                    return Vec::new();
                }
                vec![Action::RunLoop]
            }
            Key::CtrlC => {
                let Some(sid) = self.active.clone() else {
                    return Vec::new();
                };
                if !self.loop_running(&sid) {
                    self.flash("no loop running");
                    return Vec::new();
                }
                vec![Action::StopLoop]
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
                // Half-page up, like vim: back toward the head of the
                // log. From the tail (scroll 0) this moves up half a
                // viewport.
                self.scroll_up(self.half_page());
                Vec::new()
            }
            Key::CtrlD => {
                // Half-page down, like vim: back toward the tail.
                self.scroll_down(self.half_page());
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
                self.draft.clear();
                Vec::new()
            }
            Key::Char(c) => {
                // y / n / e answer the oldest pending approval_request
                // (docs/tui.md section 7); without a pending request
                // they are ordinary typing.
                let lower = c.to_ascii_lowercase();
                if self.oldest_pending_approval().is_some() {
                    match lower {
                        'y' => return vec![Action::AnswerApproval(Decision::Allow)],
                        'n' => return vec![Action::AnswerApproval(Decision::Deny)],
                        'e' => return vec![Action::AnswerApproval(Decision::Edit)],
                        _ => {}
                    }
                }
                self.draft.push(c);
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
        assert_eq!(app.press(Key::Enter), vec![Action::SendDraft]);
        assert_eq!(app.take_draft(), "hi");
    }

    #[test]
    fn enter_without_draft_does_nothing() {
        let mut app = app_with(vec![], "s1");
        assert!(app.press(Key::Enter).is_empty());
        assert!(app.status().is_some());
    }

    #[test]
    fn ctrl_r_runs_loop_once_only() {
        let mut app = app_with(vec![], "s1");
        assert_eq!(app.press(Key::CtrlR), vec![Action::RunLoop]);
        // Simulate the main thread attaching the loop.
        attach_dummy(&mut app, "s1");
        assert!(app.loop_running(&SessionId::new("s1")));
        assert!(app.press(Key::CtrlR).is_empty(), "second Ctrl+R is ignored");
    }

    #[test]
    fn ctrl_c_stops_only_when_running() {
        let mut app = app_with(vec![], "s1");
        assert!(app.press(Key::CtrlC).is_empty());
        assert!(app.status().is_some());

        attach_dummy(&mut app, "s1");
        assert_eq!(app.press(Key::CtrlC), vec![Action::StopLoop]);
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
        // First q arms: no action, just a status hint.
        assert!(app.press(Key::Quit).is_empty());
        assert!(!app.should_quit(), "first q only arms the quit");
        assert!(app.status().is_some());
        // Typing another key disarms.
        let mut app = app_with(vec![], "s1");
        app.press(Key::Quit);
        app.press(Key::Char('x'));
        assert!(app.press(Key::Quit).is_empty(), "disarmed: q arms again");
        assert!(!app.should_quit());
        // A quick second q confirms.
        let mut app = app_with(vec![], "s1");
        app.press(Key::Quit);
        assert_eq!(app.press(Key::Quit), vec![Action::Quit]);
        assert!(app.should_quit());
        // No further keys after quit.
        assert!(app.press(Key::Char('x')).is_empty());
        assert_eq!(app.draft(), "");
    }

    #[test]
    fn expired_arm_requires_a_fresh_q() {
        let mut app = app_with(vec![], "s1");
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
    fn ext_statuses_update_on_watch_events() {
        let mut app = app_with(
            vec![ev(r#"{"v":1,"type":"ext_status","ts":"t","id":"vim_mode","value":"insert"}"#)],
            "s1",
        );
        // A new ext_status watch event updates the map in place.
        app.on_watch_item(WatchItem::Event {
            event: ev(r#"{"v":1,"type":"ext_status","ts":"t","id":"vim_mode","value":"normal"}"#),
            cursor: crate::port::TailCursor::end(),
        });
        assert_eq!(app.ext_statuses().get("vim_mode").unwrap(), &json!("normal"));
        // A non-ext_status event leaves the map untouched.
        app.on_watch_item(WatchItem::Event {
            event: ev(r#"{"v":1,"type":"user_message","ts":"t","content":"hi"}"#),
            cursor: crate::port::TailCursor::end(),
        });
        assert_eq!(app.ext_statuses().get("vim_mode").unwrap(), &json!("normal"));
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
        app.set_active(SessionId::new("s1"), vec![ev(r#"{"v":1,"type":"user_message","ts":"t","content":"one"}"#)]);
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
        assert!(app.transcript_lines(80, None).iter().any(|l| l.to_string().contains("two")));
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
        assert_eq!(app.pending_name(), Some("../x"), "the name stays for editing");
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
}
