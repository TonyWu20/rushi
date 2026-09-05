//! The multi-line message editor with native vim modal input.
//!
//! A faithful port of the `pi-vim` editor engine pinned in
//! pi-config (`burneikis/pi-vim`, rev `b53ce8f`, plus the upstream
//! compat fixes of `8b99ecc`). Reference file mapping:
//!
//! - `state.ts` → [`Mode`] and the fields of [`Editor`]
//! - `motions.ts` → the motion functions below
//! - `operators.ts` → the operator range functions
//! - `registers.ts` → the register functions
//! - `text-objects.ts` → the text object functions
//! - `repeat.ts` → [`RecordedChange`] and the dot-repeat fields
//! - `search.ts` → the search fields and functions
//! - `modes/*.ts` → the per-mode handlers in [`Editor::press`]
//!
//! The editor owns no key mapping and no I/O: `press` takes an
//! already-normalized [`crate::app::Key`], so the state machine
//! stays testable in isolation. Columns count characters (the
//! reference counts UTF-16 units; identical for our drafts).

use std::collections::HashMap;

use crate::app::Key;

/// The modal states (the pi-vim `VimMode` set). The idle state is
/// insert: the composer starts in typing mode, and `Esc` drops to
/// normal for motions. `CommandLine` is the search prompt state
/// (`/` and `?`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    Normal,
    #[default]
    Insert,
    Replace,
    /// Char-wise visual (`v`).
    Visual,
    /// Line-wise visual (`V`).
    VisualLine,
    /// The search command line (`/`, `?`).
    CommandLine,
}

impl Mode {
    /// The status label, one per state (the `vim-modal.ts`
    /// `MODE_LABEL` set).
    pub fn label(self) -> &'static str {
        match self {
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
            Mode::Replace => "REPLACE",
            Mode::Visual => "VISUAL",
            Mode::VisualLine => "V-LINE",
            Mode::CommandLine => "COMMAND",
        }
    }

    /// True when the cursor rests on the character at its column
    /// (the block cursor covers that char). In insert mode the
    /// caret sits between characters; the block is a blank cell
    /// there.
    pub fn cursor_on_char(self) -> bool {
        !matches!(self, Mode::Insert | Mode::CommandLine)
    }
}

/// A recorded change for dot-repeat (the pi-vim `RecordedChange`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecordedChange {
    /// The command keys of the change, without the count prefix
    /// (the operator count lives in `count`; motion counts stay in
    /// the keys).
    pub keys: Vec<char>,
    /// The count that prefixed the change (0 = none).
    pub count: u32,
    /// Text typed during the insert session (for dot-repeat).
    pub inserted_text: String,
    /// Whether the change entered insert or replace mode.
    pub entered_insert: bool,
}

/// The register content (the pi-vim `RegisterContent`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RegContent {
    pub text: String,
    pub linewise: bool,
}

/// A buffer snapshot for undo / redo (the pi-vim `vimUndo`
/// snapshots).
#[derive(Debug, Clone)]
struct Snapshot {
    lines: Vec<String>,
    row: usize,
    col: usize,
}

/// The direction of the last find-char search (`;` and `,`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SearchDir {
    Forward,
    Backward,
}

/// `f` finds the char itself; `t` stops one before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SearchKind {
    Find,
    Till,
}

/// A motion result (the pi-vim `MotionResult`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MotionResult {
    pos: (usize, usize),
    /// Whether the motion operates on whole lines (for operators).
    linewise: bool,
    /// Whether the end position is included in operator ranges.
    inclusive: bool,
}

/// An operator range (the pi-vim `OperatorRange`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OpRange {
    start: (usize, usize),
    end: (usize, usize),
    linewise: bool,
    inclusive: bool,
}

/// The multi-line textarea plus the vim modal state. The invariant
/// is at least one line; `new` holds it.
#[derive(Debug, Clone)]
pub struct Editor {
    pub lines: Vec<String>,
    /// The cursor line index (0-based).
    pub row: usize,
    /// The cursor column, in characters, within `lines[row]`.
    pub col: usize,
    pub mode: Mode,
    // ── vim state (the pi-vim `VimState` set) ──
    /// The numeric prefix accumulator (0 = none, capped at 99999).
    count: u32,
    /// Whether digits are currently accumulating a count.
    count_started: bool,
    /// The pending operator awaiting a motion or text object.
    pending_operator: Option<char>,
    /// The count captured when the operator was pressed.
    pending_operator_count: u32,
    /// The active register (`"` = default).
    register: char,
    /// The visual-mode anchor (the other end of the selection).
    visual_anchor: Option<(usize, usize)>,
    /// `f F t T r` awaiting their character.
    pending_char_motion: Option<char>,
    /// The first `g` of `gg` was seen.
    pending_g: bool,
    /// `i` / `a` awaiting the text object key.
    pending_text_object_prefix: Option<char>,
    /// `"` awaiting the register name.
    pending_register: bool,
    /// Remaining copies for a counted `O` insertion.
    open_line_repeat_count: u32,
    /// The last f/F/t/T search (`;` and `,` repeat it).
    last_char_search: Option<(char, SearchDir, SearchKind)>,
    // ── search state (the pi-vim `SearchState`) ──
    last_search_pattern: Option<String>,
    last_search_forward: bool,
    search_input: String,
    search_active: bool,
    search_prompt: char,
    search_return_mode: Mode,
    // ── replace mode ──
    /// Originals replaced during this replace session; backspace
    /// restores them. `None` marks a split line.
    replaced_chars: Vec<Option<char>>,
    // ── dot-repeat (the pi-vim `repeat.ts`) ──
    last_change: Option<RecordedChange>,
    current_recording: Option<RecordedChange>,
    is_recording_insert: bool,
    is_replaying: bool,
    // ── registers and undo / redo ──
    registers: HashMap<char, RegContent>,
    undo_stack: Vec<Snapshot>,
    redo_stack: Vec<Snapshot>,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

// ── character classification (the reference helper set) ────────

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_blank_char(c: char) -> bool {
    c == ' ' || c == '\t'
}

fn is_punct_char(c: char) -> bool {
    !is_word_char(c) && !is_blank_char(c)
}

fn is_blank_line(line: &str) -> bool {
    line.chars().all(char::is_whitespace)
}

/// The chars of a line (empty lines yield an empty vec).
fn chars_of(lines: &[String], row: usize) -> Vec<char> {
    lines.get(row).map(|l| l.chars().collect()).unwrap_or_default()
}

/// The char count of a line.
fn line_len(lines: &[String], row: usize) -> usize {
    lines.get(row).map(|l| l.chars().count()).unwrap_or(0)
}

/// The first non-blank column of a line (the `^` position).
fn first_nonblank(line: &str) -> usize {
    line.char_indices()
        .find(|(_, c)| !c.is_whitespace())
        .map(|(i, _)| i)
        .unwrap_or(0)
}

/// The leading whitespace of a line (`o` / `O` auto-indent).
fn leading_whitespace(line: &str) -> String {
    line.chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

fn clamp_line(n_lines: usize, line: usize) -> usize {
    line.min(n_lines.saturating_sub(1))
}

// ── the editor core ─────────────────────────────────────────────

impl Editor {
    /// A fresh editor: one empty line, idle in insert mode (the
    /// composer's typing mode; `Esc` drops to normal).
    pub fn new() -> Self {
        Self {
            lines: vec![String::new()],
            row: 0,
            col: 0,
            mode: Mode::Insert,
            count: 0,
            count_started: false,
            pending_operator: None,
            pending_operator_count: 1,
            register: '"',
            visual_anchor: None,
            pending_char_motion: None,
            pending_g: false,
            pending_text_object_prefix: None,
            pending_register: false,
            open_line_repeat_count: 1,
            last_char_search: None,
            last_search_pattern: None,
            last_search_forward: true,
            search_input: String::new(),
            search_active: false,
            search_prompt: '/',
            search_return_mode: Mode::Normal,
            replaced_chars: Vec::new(),
            last_change: None,
            current_recording: None,
            is_recording_insert: false,
            is_replaying: false,
            registers: HashMap::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }

    // ── buffer access ───────────────────────────────────────────

    /// The draft text: lines joined with `\n`.
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// Load a whole draft. The cursor is clamped to the new text.
    /// External text resets the vim command state and the undo
    /// stacks; the registers survive (like vim across buffers).
    pub fn set_text(&mut self, text: &str) {
        let mut ls: Vec<String> = text.lines().map(str::to_string).collect();
        if ls.is_empty() {
            ls = vec![String::new()];
        }
        self.lines = ls;
        self.row = clamp_line(self.lines.len(), self.row);
        self.clamp_col();
        self.reset_operator_state();
        self.visual_anchor = None;
        self.search_active = false;
        self.search_input.clear();
        self.last_change = None;
        self.current_recording = None;
        self.is_recording_insert = false;
        self.is_replaying = false;
        self.undo_stack.clear();
        self.redo_stack.clear();
    }

    /// Clear the whole buffer and return to the idle state.
    pub fn clear(&mut self) {
        *self = Self::new();
    }

    /// The number of lines the text currently holds (at least 1).
    pub fn n_lines(&self) -> usize {
        if self.lines.is_empty() {
            1
        } else {
            self.lines.len()
        }
    }

    /// The visible lines at `scroll` (the index of the first
    /// visible line), at most `height` rows.
    pub fn display(&self, scroll: usize, height: usize) -> Vec<String> {
        let n = self.n_lines();
        let start = scroll.min(n.saturating_sub(1));
        let end = start.saturating_add(height).min(n);
        self.lines[start..end].to_vec()
    }

    /// The cursor position in the document: `(row, col)`.
    pub fn cursor(&self) -> (usize, usize) {
        (self.row, self.col)
    }

    /// The current mode, for the status row and the border color.
    pub fn mode(&self) -> Mode {
        self.mode
    }

    // ── status labels (the vim-modal.ts format) ────────────────

    /// A pending operator label for the status row
    /// (`[d-PENDING]`), mirroring the pi-vim `formatStatus`
    /// (operator-pending = normal mode plus a pending operator).
    pub fn pending_label(&self) -> Option<String> {
        self.pending_operator.map(|o| format!("[{o}-PENDING]"))
    }

    /// The command-line prompt, rendered in the box title
    /// (`/pat█`); `None` outside command-line mode.
    pub fn command_line_label(&self) -> Option<String> {
        if self.mode == Mode::CommandLine && self.search_active {
            Some(format!(
                "{}{}█",
                self.search_prompt, self.search_input
            ))
        } else {
            None
        }
    }

    // ── undo / redo (the pi-vim `vimUndo` / `vimRedo`) ─────────

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            lines: self.lines.clone(),
            row: self.row,
            col: line_len(&self.lines, self.row),
        }
    }

    fn push_undo(&mut self) {
        self.undo_stack.push(self.snapshot());
        if self.undo_stack.len() > 200 {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
    }

    /// Undo the last change (`u`).
    pub fn undo(&mut self) -> bool {
        if let Some(snap) = self.undo_stack.pop() {
            self.redo_stack.push(self.snapshot());
            self.lines = snap.lines;
            self.row = snap.row;
            self.col = snap.col;
            self.clamp_col();
            true
        } else {
            false
        }
    }

    /// Redo the last undone change (`Ctrl+R` with redo state).
    pub fn redo(&mut self) -> bool {
        if let Some(snap) = self.redo_stack.pop() {
            self.undo_stack.push(self.snapshot());
            self.lines = snap.lines;
            self.row = snap.row;
            self.col = snap.col;
            self.clamp_col();
            true
        } else {
            false
        }
    }

    /// Whether redo state is held (the host routes `Ctrl+R` to the
    /// editor only in that case; otherwise it keeps its host role).
    pub fn has_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Clamp the cursor to the document (col within its line).
    fn clamp_col(&mut self) {
        self.row = clamp_line(self.lines.len(), self.row);
        self.col = self.col.min(line_len(&self.lines, self.row));
    }

    /// Move the cursor to `(row, col)`, clamped to the document.
    fn go_to(&mut self, target: (usize, usize)) {
        self.row = clamp_line(self.lines.len(), target.0);
        self.col = target.1.min(line_len(&self.lines, self.row));
    }

    /// Clear the pending operator, count, and helper states (the
    /// pi-vim `resetOperatorState`).
    fn reset_operator_state(&mut self) {
        self.count = 0;
        self.count_started = false;
        self.pending_operator = None;
        self.pending_operator_count = 1;
        self.pending_char_motion = None;
        self.pending_g = false;
        self.pending_text_object_prefix = None;
        self.pending_register = false;
        self.register = '"';
    }

    // ── key entry point ─────────────────────────────────────────

    /// Handle one key. Returns a one-line hint for a key that
    /// changed nothing the user should know (a cancelled operator,
    /// an unhandled key in an editing mode). Unknown keys in
    /// normal mode are silently ignored, like vim.
    pub fn press(&mut self, key_in: Key) -> Option<String> {
        // `CtrlJ` is the multi-line newline key: the host maps it
        // here so every mode's `Enter` arm applies (insert splits
        // the line, normal moves down, command-line confirms).
        let key = match key_in {
            Key::CtrlJ => Key::Enter,
            k => k,
        };
        match self.mode {
            Mode::Insert => self.insert_press(key),
            Mode::Replace => self.replace_press(key),
            Mode::Visual | Mode::VisualLine => self.visual_press(key),
            Mode::CommandLine => self.command_line_press(key),
            Mode::Normal => self.normal_press(key),
        }
    }
}
