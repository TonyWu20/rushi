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

/// One visible row of the editor.
///
/// A long draft line is word-wrapped to the terminal width instead of
/// being clipped at the border: `text` is one wrapped piece and `caret`
/// is the display column of the cursor within `text` when the cursor
/// falls on this row (`None` otherwise). The host renders `text` into a
/// 1-row paragraph and puts the hardware cursor at `caret` — which is
/// what makes a wrapped line usable: scrolling the editor, navigating
/// past the box edge and pasting into it all keep the caret in view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorRow {
    pub text: String,
    /// Display column of the cursor within `text`; `None` when the
    /// cursor is not on this row.
    pub caret: Option<usize>,
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
    lines
        .get(row)
        .map(|l| l.chars().collect())
        .unwrap_or_default()
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

/// Hard-wrap one logical editor line into rows of at most `width`
/// display columns. The input box is a composer, not a viewer: nothing
/// is truncated, every character stays visible on some row.
///
/// Display width is the character count (the host composer is ASCII;
/// this matches the transcript `wrap_flow`, which makes the same
/// assumption). A break falls mid-word when a word runs wider than the
/// box; the caret position maps onto this wrap with plain integer
/// division (see `caret_display`).
fn wrap_row(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let cs: Vec<char> = text.chars().collect();
    if cs.is_empty() {
        return vec![String::new()];
    }
    cs.chunks(width)
        .map(|chunk| chunk.iter().collect())
        .collect()
}

/// Map the cursor — the logical character index `col` within `line`
/// (where `col` may equal the line's length: end-of-line) — to the
/// display position the caret lands on once `line` is wrapped to
/// `width` columns: `(row, col_in_row)`, both measured within this
/// line's wrap. `display_rows` and `cursor_display` add the display
/// rows of the preceding lines for the global row.
///
/// This is exact for the character-based hard wrap `wrap_row` uses:
/// fragment `f` of a wrapped line covers source columns
/// `[f * width, (f + 1) * width)`, so the fragment owning column `c`
/// is `c / width` and the caret column within it is `c % width`.
fn caret_display(line: &str, width: usize, col: usize) -> (usize, usize) {
    if width == 0 {
        return (0, col);
    }
    let n = line.chars().count();
    let target = col.min(n);
    if target == 0 {
        return (0, 0);
    }
    // The caret sits just past the character at index `target - 1`.
    let prev = target - 1;
    let row = prev / width;
    let ccol = (prev % width) + 1;
    if ccol < width {
        return (row, ccol);
    }
    // The caret is one past a full-width row.
    if target < n {
        // More characters follow, so the next row exists.
        return (row + 1, 0);
    }
    // End-of-line on a line whose last row is exactly full: the caret
    // stays on that last row, one column past its final character.
    (row, width)
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

    /// The `@` token info on the cursor line, when the editor is in
    /// insert mode and an `@` sits at the start of the line (preceded
    /// by nothing or only whitespace) before the cursor.  Returns the
    /// `@` column and the query text between the `@` and the cursor.
    ///
    /// The picker only activates when the `@` is the first non-space
    /// character on the line (docs/tui-file-picker.md section 5).
    /// An `@` typed after word characters or path fragments (e.g.
    /// `foo@bar`, `src/main.rs@`) is inert and does not open the
    /// picker.
    /// (docs/tui-file-picker.md section 5.)
    pub fn at_token_info(&self) -> Option<(usize, String)> {
        if self.mode != Mode::Insert {
            return None;
        }
        let line = self.lines.get(self.row)?;
        let chars: Vec<char> = line.chars().collect();
        let col = self.col.min(chars.len());
        // Find the nearest `@` at or before the cursor (the last
        // `@` on the line up to the caret).
        let at_pos = chars[..col].iter().rposition(|&c| c == '@')?;
        // The `@` must be at start of line or preceded by whitespace.
        // A preceding word or punctuation character means the `@` is
        // part of a larger token (e.g. `user@domain`) and should not
        // trigger the picker (docs/tui-file-picker.md section 5).
        if at_pos > 0 && !chars[at_pos - 1].is_whitespace() {
            return None;
        }
        let query: String = chars[at_pos + 1..col].iter().collect();
        Some((at_pos, query))
    }

    /// Replace the `@` token starting at `at_col` with `value`, and
    /// place the cursor just past the replacement. Pushes an undo
    /// snapshot first (docs/tui-file-picker.md section 5).
    pub fn replace_at_token(&mut self, at_col: usize, value: &str) {
        let line = self.lines[self.row].clone();
        let chars: Vec<char> = line.chars().collect();
        let cursor_col = self.col.min(chars.len());
        let prefix: String = chars[..at_col].iter().collect();
        let suffix: String = chars[cursor_col..].iter().collect();
        self.push_undo();
        self.lines[self.row] = format!("{prefix}{value}{suffix}");
        self.col = at_col + value.chars().count();
    }

    /// The number of lines the text currently holds (at least 1).
    /// Test-only accessor: no production caller.
    #[cfg(test)]
    pub fn n_lines(&self) -> usize {
        if self.lines.is_empty() {
            1
        } else {
            self.lines.len()
        }
    }

    /// The cursor position in the document: `(row, col)`. Test-only
    /// accessor: no production caller.
    #[cfg(test)]
    pub fn cursor(&self) -> (usize, usize) {
        (self.row, self.col)
    }

    /// The visible display rows at `scroll` (the index of the first
    /// visible *display row*), at most `height` rows, with long lines
    /// word-wrapped to `width` columns. Both `scroll` and `height` are
    /// in display-row units, so a long logical line can occupy several
    /// rows; the window is a slice of the flattened, wrapped view.
    ///
    /// A logical line longer than the box yields several rows. Each
    /// row carries `text` (one wrapped piece) and, when the cursor
    /// falls on that piece, `caret` — the display column within the
    /// piece where the hardware cursor must land (end-of-line points
    /// one past the last character, at the block cell).
    pub fn display_rows(&self, scroll: usize, height: usize, width: usize) -> Vec<EditorRow> {
        // `scroll` and `height` are in *display-row* units, not logical
        // lines: the editor box shows `height` visual rows starting at
        // the `scroll`-th display row. Flatten the wrapped rows with
        // their absolute display-row index, then take the window. This
        // matches `display_row_count` (the total row count) and the
        // scroll offset `App` keeps, which is derived from
        // `cursor_display`.
        if width == 0 || height == 0 {
            return Vec::new();
        }
        let mut all: Vec<EditorRow> = Vec::new();
        for (i, line) in self.lines.iter().enumerate() {
            // Where the cursor lands, in display-row/col, within this
            // line's wrap (`None` when the cursor is on another line).
            let (caret_row, caret_col) = if i == self.row {
                caret_display(line, width, self.col)
            } else {
                (usize::MAX, 0)
            };
            for (ci, chunk) in wrap_row(line, width).into_iter().enumerate() {
                let caret = (ci == caret_row).then_some(caret_col);
                all.push(EditorRow { text: chunk, caret });
            }
        }
        if scroll >= all.len() {
            return Vec::new();
        }
        all[scroll..scroll.saturating_add(height).min(all.len())].to_vec()
    }

    /// The document rows the cursor occupies: `(row, col)` where both
    /// are measured in display rows/columns of a wrap at `width`. A
    /// cursor in a wrapped line is scrolled to the visible window and
    /// its caret lands on the wrapping piece that contains it, so the
    /// caret never falls off the box edge.
    pub fn cursor_display(&self, width: usize) -> (usize, usize) {
        if width == 0 {
            return (self.row, self.col);
        }
        let mut disp_row = 0usize;
        for (i, line) in self.lines.iter().enumerate() {
            if i == self.row {
                let (cr, cc) = caret_display(line, width, self.col);
                return (disp_row + cr, cc);
            }
            disp_row += wrap_row(line, width).len();
        }
        (disp_row, 0)
    }

    /// The total number of display rows the document wraps to at
    /// `width` (at least 1). Used to size the input box and bound the
    /// scroll.
    pub fn display_row_count(&self, width: usize) -> usize {
        let mut total = 0usize;
        for line in self.lines.iter() {
            total += wrap_row(line, width).len();
        }
        total.max(1)
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
            Some(format!("{}{}█", self.search_prompt, self.search_input))
        } else {
            None
        }
    }

    /// Delete the whole current line (the host maps `Ctrl+U` here
    /// in insert mode, matching the base editor's line kill).
    pub fn clear_current_line(&mut self) {
        if self.lines[self.row].is_empty() {
            return;
        }
        self.push_undo();
        self.lines[self.row] = String::new();
        self.col = 0;
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

// ── insert / replace modes ──────────────────────────────────────

impl Editor {
    /// Insert mode (the pi-vim `modes/insert.ts`). Chars append at
    /// the caret; `Ctrl-J` (host-normalized to `Enter`) splits the
    /// line; `Backspace` removes the char left of the caret and
    /// joins lines at column 0. `Esc` returns to normal and steps
    /// the cursor back one char (counted `O` copies the inserted
    /// line first). `Ctrl+C` returns to normal without stepping.
    fn insert_press(&mut self, key: Key) -> Option<String> {
        match key {
            Key::Esc => {
                self.mode = Mode::Normal;
                if self.open_line_repeat_count > 1 {
                    // Counted `O`: the inserted line repeats, like
                    // vim (the pi-vim `openLineRepeatCount`).
                    let copies = vec![
                        self.lines[self.row].clone();
                        self.open_line_repeat_count as usize - 1
                    ];
                    self.push_undo();
                    self.lines
                        .splice(self.row + 1..self.row + 1, copies.iter().cloned());
                    self.row += self.open_line_repeat_count as usize - 1;
                    self.col = self.col.saturating_sub(1);
                    self.open_line_repeat_count = 1;
                } else if self.col > 0 {
                    // Vim steps the cursor back one on Esc; at
                    // column 0 it stays (the pi-vim rule).
                    self.col -= 1;
                }
                self.finalize_change_recording();
            }
            Key::CtrlC => {
                self.mode = Mode::Normal;
                self.finalize_change_recording();
            }
            Key::Backspace => {
                if self.is_recording_insert() {
                    self.record_insert_backspace();
                }
                if self.col > 0 || self.row > 0 {
                    self.push_undo();
                    self.back_one();
                }
            }
            Key::Delete => {
                let len = line_len(&self.lines, self.row);
                if self.col < len {
                    self.push_undo();
                    self.delete_char_at(self.col);
                }
            }
            Key::Char(c) => {
                if self.is_recording_insert() {
                    self.record_insert_text(c);
                }
                self.push_undo();
                self.insert_char(c);
            }
            Key::Enter => {
                if self.is_recording_insert() {
                    self.record_insert_text('\n');
                }
                self.push_undo();
                self.insert_char('\n');
            }
            Key::Left => {
                self.col = self.col.saturating_sub(1);
            }
            Key::Right => {
                let max = line_len(&self.lines, self.row);
                self.col = (self.col + 1).min(max);
            }
            Key::Up => {
                self.row = self.row.saturating_sub(1);
                self.clamp_col();
            }
            Key::Down => {
                self.row = (self.row + 1).min(self.lines.len().saturating_sub(1));
                self.clamp_col();
            }
            Key::Home => {
                self.col = 0;
            }
            Key::End => {
                self.col = line_len(&self.lines, self.row);
            }
            _ => return Some("insert mode: type · Ctrl-J newline · Esc normal".to_string()),
        }
        None
    }

    /// Replace mode (the pi-vim `modes/replace.ts`): each typed
    /// char overwrites the char under the cursor (the last char of
    /// a line is overwritten, not appended); at end of line it
    /// appends. `Backspace` restores the original char and steps
    /// back. `Enter` splits the line. `Esc` returns to normal with
    /// a step back.
    fn replace_press(&mut self, key: Key) -> Option<String> {
        match key {
            Key::Esc => {
                self.mode = Mode::Normal;
                if self.col > 0 {
                    self.col -= 1;
                }
                self.finalize_change_recording();
                self.replaced_chars.clear();
            }
            Key::CtrlC => {
                self.mode = Mode::Normal;
                self.finalize_change_recording();
                self.replaced_chars.clear();
            }
            Key::Backspace => {
                if self.col > 0 && !self.replaced_chars.is_empty() {
                    let original = self.replaced_chars.pop().unwrap();
                    let col = self.col - 1;
                    self.push_undo();
                    let mut chs: Vec<char> = self.lines[self.row].chars().collect();
                    match original {
                        Some(c) => {
                            if col < chs.len() {
                                chs[col] = c;
                            }
                        }
                        // A split line: drop one char, like the
                        // reference restore with its empty marker.
                        None => {
                            chs.remove(col.min(chs.len().saturating_sub(1)));
                        }
                    }
                    self.lines[self.row] = chs.into_iter().collect();
                    self.col = col;
                    if self.is_recording_insert() {
                        self.record_insert_backspace();
                    }
                }
            }
            Key::Enter => {
                // Split the line at the cursor (the pi-vim
                // replace-Enter rule).
                self.push_undo();
                let chs: Vec<char> = self.lines[self.row].chars().collect();
                let cut = self.col.min(chs.len());
                let before: String = chs[..cut].iter().collect();
                let after: String = chs[cut..].iter().collect();
                self.lines[self.row] = before;
                self.lines.insert(self.row + 1, after);
                self.row += 1;
                self.col = 0;
                self.replaced_chars.push(None);
                if self.is_recording_insert() {
                    self.record_insert_text('\n');
                }
            }
            Key::Char(c) if (c as u32) >= 32 => {
                self.push_undo();
                let mut chs: Vec<char> = self.lines[self.row].chars().collect();
                if self.col < chs.len() {
                    self.replaced_chars.push(Some(chs[self.col]));
                    chs[self.col] = c;
                } else {
                    self.replaced_chars.push(None);
                    chs.push(c);
                }
                self.lines[self.row] = chs.into_iter().collect();
                self.col += 1;
                if self.is_recording_insert() {
                    self.record_insert_text(c);
                }
            }
            _ => {
                return Some("replace mode: type overwrites · Esc ends".to_string());
            }
        }
        None
    }

    // ── low-level mutators (each pushes an undo snapshot) ──────

    /// Insert one char at the caret; `'\n'` splits the line.
    fn insert_char(&mut self, c: char) {
        let line = self.lines[self.row].clone();
        let chs: Vec<char> = line.chars().collect();
        let cut = self.col.min(chs.len());
        if c == '\n' {
            let prefix: String = chs[..cut].iter().collect();
            let suffix: String = chs[cut..].iter().collect();
            self.lines[self.row] = prefix;
            self.lines.insert(self.row + 1, suffix);
            self.row += 1;
            self.col = 0;
        } else {
            let mut new_chs = chs[..cut].to_vec();
            new_chs.push(c);
            new_chs.extend(chs[cut..].iter().cloned());
            self.lines[self.row] = new_chs.into_iter().collect();
            self.col = cut + 1;
        }
    }

    /// `Backspace` in insert mode: remove the char left of the
    /// caret; at column 0 join the previous line.
    fn back_one(&mut self) {
        if self.col > 0 {
            let mut chs: Vec<char> = self.lines[self.row].chars().collect();
            chs.remove(self.col - 1);
            self.lines[self.row] = chs.into_iter().collect();
            self.col -= 1;
        } else if self.row > 0 {
            let prev = self.lines.remove(self.row - 1);
            let cur = self.lines[self.row - 1].clone();
            self.lines[self.row - 1] = format!("{}{}", prev, cur);
            self.row -= 1;
            self.col = line_len(&self.lines, self.row);
        }
    }

    /// Remove the char at `idx` on the cursor line.
    fn delete_char_at(&mut self, idx: usize) {
        let mut chs: Vec<char> = self.lines[self.row].chars().collect();
        if idx < chs.len() {
            chs.remove(idx);
            self.lines[self.row] = chs.into_iter().collect();
        }
    }

    // ── dot-repeat recording (the pi-vim `repeat.ts`) ──────────

    fn is_recording(&self) -> bool {
        self.current_recording.is_some()
    }

    fn is_recording_insert(&self) -> bool {
        self.is_recording_insert
    }

    fn start_recording(&mut self, count: u32) {
        self.current_recording = Some(RecordedChange {
            keys: Vec::new(),
            count,
            inserted_text: String::new(),
            entered_insert: false,
        });
        self.is_recording_insert = false;
    }

    fn record_key(&mut self, k: char) {
        if let Some(r) = self.current_recording.as_mut() {
            r.keys.push(k);
        }
    }

    fn mark_insert_entry(&mut self) {
        if let Some(r) = self.current_recording.as_mut() {
            r.entered_insert = true;
        }
        self.is_recording_insert = true;
    }

    fn record_insert_text(&mut self, c: char) {
        if self.is_recording_insert {
            if let Some(r) = self.current_recording.as_mut() {
                r.inserted_text.push(c);
            }
        }
    }

    fn record_insert_backspace(&mut self) {
        if self.is_recording_insert {
            if let Some(r) = self.current_recording.as_mut() {
                r.inserted_text.pop();
            }
        }
    }

    fn finalize_recording(&mut self) {
        if let Some(r) = self.current_recording.take() {
            if !r.keys.is_empty() {
                self.last_change = Some(r);
            }
        }
        self.is_recording_insert = false;
    }

    /// Finalize the recording unless replaying (the pi-vim
    /// `finalizeChangeRecording`).
    fn finalize_change_recording(&mut self) {
        if self.is_replaying {
            return;
        }
        if self.is_recording() {
            self.finalize_recording();
        }
    }

    /// Begin recording a change (`key` + the count prefix, like
    /// the pi-vim `beginChangeRecording`).
    fn begin_change_recording(&mut self, key: char, count: u32) {
        if self.is_replaying || self.is_recording() {
            return;
        }
        self.start_recording(count);
        if count > 1 {
            for d in count.to_string().chars() {
                self.record_key(d);
            }
        }
        self.record_key(key);
    }
}

// ── normal mode (the pi-vim `modes/normal.ts`) ──────────────────

impl Editor {
    /// Normal mode. The pending states are resolved first (register
    /// selection, text object key, char motion, `g`), then the
    /// count prefix, then the command keys.
    fn normal_press(&mut self, key: Key) -> Option<String> {
        // --- pending register selection (after `"`) ---
        if self.pending_register {
            self.pending_register = false;
            if let Key::Char(c) = key {
                if is_valid_register(c) {
                    self.register = c;
                    if self.is_recording() {
                        self.record_key('"');
                        self.record_key(c);
                    }
                } else {
                    // An invalid register cancels.
                    self.reset_operator_state();
                }
            } else {
                self.reset_operator_state();
            }
            return None;
        }

        // --- pending text object key (after `i` or `a` in
        // operator-pending mode) ---
        if let Some(prefix) = self.pending_text_object_prefix {
            self.pending_text_object_prefix = None;
            if let Key::Char(c) = key {
                if let Some(obj) = resolve_text_object(prefix, c) {
                    let cursor = (self.row, self.col);
                    if let Some(range) = obj(&self.lines, cursor) {
                        if let Some(op) = self.pending_operator {
                            if self.is_recording() {
                                self.record_key(prefix);
                                self.record_key(c);
                            }
                            let obj_range = text_object_to_range(&range);
                            self.apply_operator_to_range(op, &obj_range);
                        } else {
                            // Text objects without an operator do
                            // nothing in normal mode.
                            self.reset_operator_state();
                        }
                    } else {
                        self.reset_operator_state();
                    }
                } else {
                    self.reset_operator_state();
                }
            } else {
                self.reset_operator_state();
            }
            return None;
        }

        // --- pending character input for f / F / t / T / r ---
        if let Some(pending) = self.pending_char_motion {
            self.pending_char_motion = None;
            match key {
                Key::Char(c) if (c as u32) >= 32 => {
                    let count = self.count.max(1);
                    let cursor = (self.row, self.col);
                    match pending {
                        'r' => {
                            // Replace the character under the
                            // cursor (only without a pending
                            // operator), like the pi-vim `r`.
                            if self.pending_operator.is_none() {
                                self.begin_change_recording('r', count);
                                self.record_key(c);
                                self.replace_char(c, count as usize);
                                self.finalize_change_recording();
                            }
                            self.reset_operator_state();
                            return None;
                        }
                        'f' => {
                            let res = find_char_forward(
                                &self.lines,
                                cursor,
                                count,
                                c,
                                &mut self.last_char_search,
                            );
                            self.run_motion(res);
                        }
                        'F' => {
                            let res = find_char_backward(
                                &self.lines,
                                cursor,
                                count,
                                c,
                                &mut self.last_char_search,
                            );
                            self.run_motion(res);
                        }
                        't' => {
                            let res = till_char_forward(
                                &self.lines,
                                cursor,
                                count,
                                c,
                                &mut self.last_char_search,
                            );
                            self.run_motion(res);
                        }
                        _ => {
                            let res = till_char_backward(
                                &self.lines,
                                cursor,
                                count,
                                c,
                                &mut self.last_char_search,
                            );
                            self.run_motion(res);
                        }
                    }
                    if self.is_recording() {
                        self.record_key(pending);
                        self.record_key(c);
                    }
                    if self.pending_operator.is_none() {
                        self.reset_operator_state();
                    }
                    return None;
                }
                // A non-printable cancels the pending motion.
                _ => {
                    self.reset_operator_state();
                    return None;
                }
            }
        }

        // --- pending `g` prefix ---
        if self.pending_g {
            self.pending_g = false;
            if key == Key::Char('g') {
                let count_explicit = self.count_started;
                let n = if count_explicit { self.count.max(1) } else { 1 };
                if self.is_recording() {
                    self.record_key('g');
                    self.record_key('g');
                }
                let res = go_to_first_line(&self.lines, (self.row, self.col), n);
                self.run_motion(res);
                if self.pending_operator.is_none() {
                    self.reset_operator_state();
                }
                return None;
            }
            // An unrecognized g-command cancels.
            self.reset_operator_state();
            return None;
        }

        // --- count prefix: 1-9 start a count, 0 continues one ---
        if let Key::Char(d) = key {
            if d.is_ascii_digit() && (d != '0' || self.count_started) {
                self.count = (self.count * 10 + d as u32 - '0' as u32).min(99999);
                self.count_started = true;
                if self.pending_operator.is_some() && self.is_recording() {
                    self.record_key(d);
                }
                return None;
            }
        }

        let count = self.count.max(1);
        let count_explicit = self.count_started;
        let motion_count = match self.pending_operator {
            Some(_) => self.pending_operator_count * count,
            None => count,
        };

        match key {
            // --- register selection prefix ---
            Key::Char('"') => {
                self.pending_register = true;
                return None;
            }
            // --- dot repeat (the pi-vim `.`) ---
            Key::Char('.') => {
                self.replay_last_change(if count_explicit { count } else { 0 });
                self.reset_operator_state();
                return None;
            }
            // --- operators ---
            Key::Char(c) if matches!(c, 'd' | 'c' | 'y' | '>' | '<') => {
                if self.pending_operator == Some(c) {
                    // A doubled operator (`dd`, `cc`, ...) is
                    // linewise on the counted lines.
                    if self.is_recording() {
                        self.record_key(c);
                    }
                    let n = self.pending_operator_count * count;
                    self.apply_linewise_operator(c, n);
                    return None;
                }
                if self.pending_operator.is_some() {
                    // A different operator while one is pending
                    // cancels it.
                    self.reset_operator_state();
                    return None;
                }
                // Open the operator. Yanks are not changes: no
                // recording.
                if c != 'y' && !self.is_replaying && !self.is_recording() {
                    self.start_recording(count);
                    if count_explicit {
                        for d in count.to_string().chars() {
                            self.record_key(d);
                        }
                    }
                    self.record_key(c);
                }
                self.pending_operator = Some(c);
                self.pending_operator_count = count;
                // The motion has its own count: vim multiplies the
                // operator count into it (`2d3w` is six words).
                self.count = 0;
                self.count_started = false;
                return None;
            }
            // --- shortcut operators (D, C, Y) ---
            Key::Char('D') => {
                // D = d$ (delete to the line end).
                self.begin_change_recording('D', count);
                let res = line_end(&self.lines, (self.row, self.col), 1);
                let range = motion_to_range((self.row, self.col), &res);
                self.apply_operator_to_range('d', &range);
                return None;
            }
            Key::Char('C') => {
                // C = c$ (change to the line end).
                self.begin_change_recording('C', count);
                let res = line_end(&self.lines, (self.row, self.col), 1);
                let range = motion_to_range((self.row, self.col), &res);
                self.apply_operator_to_range('c', &range);
                // No finalize: it enters insert, finalized on Esc.
                return None;
            }
            Key::Char('Y') => {
                // Y = yy (yank the whole line).
                self.apply_linewise_operator('y', count);
                return None;
            }
            // --- paste commands ---
            Key::Char('p') | Key::Char('P') => {
                let before = key == Key::Char('P');
                self.begin_change_recording(if before { 'P' } else { 'p' }, count);
                if let Some(reg) = get_register(&self.registers, self.register) {
                    self.paste(&reg, before, count as usize);
                }
                self.finalize_change_recording();
                self.reset_operator_state();
                return None;
            }
            // --- text object prefixes (only valid while an
            // operator is pending) ---
            Key::Char(c) if matches!(c, 'i' | 'a') && self.pending_operator.is_some() => {
                self.pending_text_object_prefix = Some(c);
                return None;
            }
            _ => {}
        }

        self.normal_motion(key, count, motion_count, count_explicit)
    }

    /// The motion and command keys of normal mode (the pi-vim
    /// normal switch). The arrow / home / end keys of the host map
    /// onto their vim equivalents: `Enter` and `Down` are `j`,
    /// `Up` and `Backspace` are `k`, `Left` is `h`, `Right` is
    /// `l`, `Home` is `0`, `End` is `$`, `Delete` is `x`.
    fn normal_motion(
        &mut self,
        key: Key,
        count: u32,
        motion_count: u32,
        count_explicit: bool,
    ) -> Option<String> {
        let cursor = (self.row, self.col);
        let ch: Option<char> = match key {
            Key::Char(c) => Some(c),
            _ => None,
        };
        let mapped: Option<char> = match key {
            Key::Enter | Key::Down => Some('j'),
            Key::Up | Key::Backspace => Some('k'),
            Key::Left => Some('h'),
            Key::Right => Some('l'),
            Key::Home => Some('0'),
            Key::End => Some('$'),
            Key::Delete => Some('x'),
            _ => None,
        };
        let k = ch.or(mapped);
        match k {
            Some('h') => {
                if self.is_recording() && self.pending_operator.is_some() {
                    self.record_key('h');
                }
                // Vim: h stops at column 0; it never wraps to the
                // previous line (the compat fix).
                let res = char_left(&self.lines, cursor, motion_count);
                self.run_motion(res);
                if self.pending_operator.is_none() {
                    self.reset_operator_state();
                }
                None
            }
            Some('l') => {
                if self.is_recording() && self.pending_operator.is_some() {
                    self.record_key('l');
                }
                // The reference `executeMotionForOperator` keeps `l`
                // inclusive when an operator is pending (`dl`
                // deletes two chars). A bare `l` is the plain
                // non-inclusive motion.
                let res = if self.pending_operator.is_some() {
                    let len = line_len(&self.lines, cursor.0);
                    let target = if len == 0 {
                        0
                    } else {
                        (cursor.1.min(len - 1) + motion_count as usize).min(len - 1)
                    };
                    MotionResult {
                        pos: (cursor.0, target),
                        linewise: false,
                        inclusive: true,
                    }
                } else {
                    char_right(&self.lines, cursor, motion_count)
                };
                self.run_motion(res);
                if self.pending_operator.is_none() {
                    self.reset_operator_state();
                }
                None
            }
            Some('j') => {
                if self.pending_operator.is_some() {
                    if self.is_recording() {
                        self.record_key('j');
                    }
                    // j with an operator is linewise.
                    let end_line =
                        (cursor.0 + motion_count as usize).min(self.lines.len().saturating_sub(1));
                    let range = OpRange {
                        start: (cursor.0, 0),
                        end: (end_line, line_len(&self.lines, end_line)),
                        linewise: true,
                        inclusive: true,
                    };
                    if let Some(op) = self.pending_operator {
                        self.apply_operator_to_range(op, &range);
                    }
                } else {
                    let mut r = cursor.0;
                    for _ in 0..count.max(1) {
                        r = (r + 1).min(self.lines.len().saturating_sub(1));
                    }
                    self.row = r;
                    self.clamp_col();
                    self.reset_operator_state();
                }
                None
            }
            Some('k') => {
                if self.pending_operator.is_some() {
                    if self.is_recording() {
                        self.record_key('k');
                    }
                    // k with an operator is linewise.
                    let start_line = cursor.0.saturating_sub(motion_count as usize);
                    let range = OpRange {
                        start: (start_line, 0),
                        end: (cursor.0, line_len(&self.lines, cursor.0)),
                        linewise: true,
                        inclusive: true,
                    };
                    if let Some(op) = self.pending_operator {
                        self.apply_operator_to_range(op, &range);
                    }
                } else {
                    let mut r = cursor.0;
                    for _ in 0..count.max(1) {
                        r = r.saturating_sub(1);
                    }
                    self.row = r;
                    self.clamp_col();
                    self.reset_operator_state();
                }
                None
            }
            Some('0') => {
                if self.is_recording() && self.pending_operator.is_some() {
                    self.record_key('0');
                }
                let res = line_start(&self.lines, cursor, 1);
                self.run_motion(res);
                if self.pending_operator.is_none() {
                    self.reset_operator_state();
                }
                None
            }
            Some('$') => {
                if self.is_recording() && self.pending_operator.is_some() {
                    self.record_key('$');
                }
                let res = line_end(&self.lines, cursor, count);
                self.run_motion(res);
                if self.pending_operator.is_none() {
                    self.reset_operator_state();
                }
                None
            }
            Some('^') => {
                if self.is_recording() && self.pending_operator.is_some() {
                    self.record_key('^');
                }
                let res = first_nonblank_motion(&self.lines, cursor, 1);
                self.run_motion(res);
                if self.pending_operator.is_none() {
                    self.reset_operator_state();
                }
                None
            }
            Some('w') => {
                if self.is_recording() && self.pending_operator.is_some() {
                    self.record_key('w');
                }
                self.execute_word_forward(motion_count);
                if self.pending_operator.is_none() {
                    self.reset_operator_state();
                }
                None
            }
            Some(c @ 'b') | Some(c @ 'e') | Some(c @ 'W') | Some(c @ 'B') | Some(c @ 'E') => {
                if self.is_recording() && self.pending_operator.is_some() {
                    self.record_key(c);
                }
                let mut res = match c {
                    'b' => word_backward(&self.lines, cursor, motion_count),
                    'e' => word_end(&self.lines, cursor, motion_count),
                    'W' => WORD_forward(&self.lines, cursor, motion_count),
                    'B' => WORD_backward(&self.lines, cursor, motion_count),
                    _ => WORD_end(&self.lines, cursor, motion_count),
                };
                if c == 'W' && self.pending_operator.is_some() {
                    res = extend_w_eol(&self.lines, cursor, res);
                }
                self.run_motion(res);
                if self.pending_operator.is_none() {
                    self.reset_operator_state();
                }
                None
            }
            Some(c @ 'f') | Some(c @ 'F') | Some(c @ 't') | Some(c @ 'T') => {
                self.pending_char_motion = Some(c);
                None
            }
            Some(c @ ';') | Some(c @ ',') => {
                if self.is_recording() && self.pending_operator.is_some() {
                    self.record_key(c);
                }
                let res = if c == ';' {
                    repeat_char_search(&self.lines, cursor, count, &self.last_char_search)
                } else {
                    reverse_char_search(&self.lines, cursor, count, &self.last_char_search)
                };
                self.run_motion(res);
                if self.pending_operator.is_none() {
                    self.reset_operator_state();
                }
                None
            }
            Some('g') => {
                self.pending_g = true;
                None
            }
            Some('G') => {
                if self.is_recording() && self.pending_operator.is_some() {
                    self.record_key('G');
                }
                // G without a count goes to the last line; with a
                // count to line N.
                let n = if count_explicit {
                    count
                } else {
                    self.lines.len() as u32
                };
                let res = go_to_last_line(&self.lines, cursor, n.max(1));
                self.run_motion(res);
                if self.pending_operator.is_none() {
                    self.reset_operator_state();
                }
                None
            }
            Some(c @ '{') | Some(c @ '}') => {
                if self.is_recording() && self.pending_operator.is_some() {
                    self.record_key(c);
                }
                let res = if c == '{' {
                    paragraph_backward(&self.lines, cursor, count)
                } else {
                    paragraph_forward(&self.lines, cursor, count)
                };
                self.run_motion(res);
                if self.pending_operator.is_none() {
                    self.reset_operator_state();
                }
                None
            }
            Some('%') => {
                if self.is_recording() && self.pending_operator.is_some() {
                    self.record_key('%');
                }
                let res = matching_bracket(&self.lines, cursor, 1);
                self.run_motion(res);
                if self.pending_operator.is_none() {
                    self.reset_operator_state();
                }
                None
            }
            Some(c @ 'n') | Some(c @ 'N') => {
                if self.is_recording() && self.pending_operator.is_some() {
                    self.record_key(c);
                }
                let res = if c == 'n' {
                    self.search_repeat(count, self.last_search_forward)
                } else {
                    self.search_repeat(count, !self.last_search_forward)
                };
                self.run_motion(res);
                if self.pending_operator.is_none() {
                    self.reset_operator_state();
                }
                None
            }
            Some(c @ '*') | Some(c @ '#') => {
                if self.is_recording() && self.pending_operator.is_some() {
                    self.record_key(c);
                }
                let res = self.search_word_under_cursor(c == '*');
                self.run_motion(res);
                self.reset_operator_state();
                None
            }
            Some(c @ '/') | Some(c @ '?') => {
                self.begin_search(c == '/');
                None
            }
            // --- single-char change (`s`) and line replace (`S`) ---
            Some('s') if self.pending_operator.is_none() => {
                // `s` deletes `count` chars from the cursor and
                // enters insert mode (change without motion).
                self.begin_change_recording('s', count);
                let end_col = (cursor.1 + count as usize - 1)
                    .min(line_len(&self.lines, cursor.0).saturating_sub(1));
                let range = OpRange {
                    start: cursor,
                    end: (cursor.0, end_col),
                    linewise: false,
                    inclusive: true,
                };
                self.apply_operator_to_range('c', &range);
                self.reset_operator_state();
                None
            }
            Some('S') if self.pending_operator.is_none() => {
                // `S` replaces `count` lines from the cursor line
                // (linewise change; insert mode).
                self.begin_change_recording('S', count);
                let n = count.max(1) as usize;
                let end_line = (cursor.0 + n - 1).min(self.lines.len().saturating_sub(1));
                let range = OpRange {
                    start: (cursor.0, 0),
                    end: (end_line, line_len(&self.lines, end_line)),
                    linewise: true,
                    inclusive: true,
                };
                self.apply_operator_to_range('c', &range);
                self.reset_operator_state();
                None
            }
            // --- insert mode entry ---
            Some(c @ 'i') | Some(c @ 'a') => {
                self.begin_change_recording(c, count);
                self.mark_insert_entry();
                self.mode = Mode::Insert;
                if c == 'a' {
                    let max = line_len(&self.lines, self.row);
                    self.col = (self.col + 1).min(max);
                }
                self.reset_operator_state();
                None
            }
            Some('I') => {
                self.begin_change_recording('I', count);
                self.mark_insert_entry();
                self.col = first_nonblank(&self.lines[self.row]);
                self.mode = Mode::Insert;
                self.reset_operator_state();
                None
            }
            Some('A') => {
                self.begin_change_recording('A', count);
                self.mark_insert_entry();
                self.col = line_len(&self.lines, self.row);
                self.mode = Mode::Insert;
                self.reset_operator_state();
                None
            }
            Some('o') | Some('O') => {
                let below = key == Key::Char('o');
                let c = if below { 'o' } else { 'O' };
                self.begin_change_recording(c, count);
                self.mark_insert_entry();
                self.push_undo();
                if below {
                    self.lines.insert(self.row + 1, String::new());
                    self.row += 1;
                    self.col = 0;
                    self.open_line_repeat_count = 1;
                } else {
                    // O opens an auto-indented line above, keeping
                    // the current line intact (the compat fix).
                    let indent = leading_whitespace(&self.lines[self.row]);
                    self.lines.insert(self.row, indent.clone());
                    self.col = indent.chars().count();
                    self.open_line_repeat_count = count.max(1);
                }
                self.mode = Mode::Insert;
                self.reset_operator_state();
                None
            }
            // --- visual mode entry ---
            Some('v') => {
                self.visual_anchor = Some(cursor);
                self.mode = Mode::Visual;
                self.reset_operator_state();
                None
            }
            Some('V') => {
                self.visual_anchor = Some(cursor);
                self.mode = Mode::VisualLine;
                self.reset_operator_state();
                None
            }
            // --- basic editing ---
            Some('x') => {
                self.delete_forward_compat(count);
                None
            }
            Some('X') => {
                self.delete_backward_compat(count);
                None
            }
            Some('r') => {
                // Replace character: wait for the next char.
                self.pending_char_motion = Some('r');
                None
            }
            Some('R') => {
                // Enter replace mode (overtype).
                self.begin_change_recording('R', count);
                self.mark_insert_entry();
                self.replaced_chars.clear();
                self.mode = Mode::Replace;
                self.reset_operator_state();
                None
            }
            Some('u') => {
                let _ = self.undo();
                self.reset_operator_state();
                None
            }
            Some('J') => {
                self.join_lines(count);
                None
            }
            Some('~') => {
                self.toggle_case(count);
                None
            }
            Some(_) => {
                if self.pending_operator.is_some() {
                    self.reset_operator_state();
                    Some("operator cancelled".to_string())
                } else {
                    self.reset_operator_state();
                    None
                }
            }
            None => match key {
                Key::Esc => {
                    if self.pending_operator.is_some() {
                        self.reset_operator_state();
                        Some("operator cancelled".to_string())
                    } else {
                        // A bare Esc in normal mode cancels the
                        // pending state: a digit count, the `g`
                        // prefix. The pinned reference leaks the
                        // count. A stale prefix makes a later `dw`
                        // delete N words or `db` eat the whole
                        // line (the reported bug).
                        self.reset_operator_state();
                        None
                    }
                }
                _ => None,
            },
        }
    }

    // ── motion execution and operator application ──────────────

    /// Run a motion: with a pending operator it applies the
    /// operator to the motion range; otherwise it moves the cursor
    /// (the pi-vim `executeMotion`).
    fn run_motion(&mut self, res: MotionResult) {
        if let Some(op) = self.pending_operator {
            let cursor = (self.row, self.col);
            let range = motion_to_range(cursor, &res);
            self.apply_operator_to_range(op, &range);
        } else {
            self.go_to(res.pos);
        }
    }

    /// The operator-specific word-motion rules (the pi-vim
    /// `executeWordForward`): `cw` behaves as `ce` off a blank; a
    /// single `dw` on the last word of a line does not consume the
    /// newline, but bigger counts do.
    fn execute_word_forward(&mut self, n: u32) {
        let cursor = (self.row, self.col);
        let op = self.pending_operator;
        let current = chars_of(&self.lines, cursor.0).get(cursor.1).copied();
        let mut res = if op == Some('c') && current.is_some_and(|c| !is_blank_char(c)) {
            word_end(&self.lines, cursor, n)
        } else {
            word_forward(&self.lines, cursor, n)
        };
        if op == Some('d')
            && n == 1
            && res.pos.0 > cursor.0
            && chars_of(&self.lines, cursor.0)[cursor.1..]
                .iter()
                .any(|c| !c.is_whitespace())
        {
            res = MotionResult {
                pos: (cursor.0, line_len(&self.lines, cursor.0).saturating_sub(1)),
                linewise: false,
                inclusive: true,
            };
        }
        res = if self.pending_operator.is_some() {
            extend_w_eol(&self.lines, cursor, res)
        } else {
            res // plain movement stays on the clamped last char
        };
        if let Some(op) = self.pending_operator {
            let range = motion_to_range(cursor, &res);
            self.apply_operator_to_range(op, &range);
        } else {
            self.go_to(res.pos);
        }
    }

    /// Apply a pending operator to a range (the pi-vim
    /// `applyOperatorToRange`).
    fn apply_operator_to_range(&mut self, op: char, range: &OpRange) {
        let lines = self.lines.clone();
        let reg = self.register;
        let (new_lines, cursor, enter_insert) =
            apply_operator(op, &lines, range, &mut self.registers, reg);
        self.push_undo();
        self.lines = new_lines;
        self.row = clamp_line(self.lines.len(), cursor.0);
        self.col = cursor.1.min(line_len(&self.lines, self.row));
        if enter_insert {
            self.mode = Mode::Insert;
            if self.is_recording() {
                self.mark_insert_entry();
            }
        } else {
            self.finalize_change_recording();
        }
        self.reset_operator_state();
    }

    /// Apply a linewise operator to the doubled form
    /// (`dd`, `cc`, `3dd`, the pi-vim `applyLinewiseOperator`).
    fn apply_linewise_operator(&mut self, op: char, n: u32) {
        let n = n.max(1) as usize;
        let end_line = (self.row + n - 1).min(self.lines.len().saturating_sub(1));
        let range = OpRange {
            start: (self.row, 0),
            end: (end_line, line_len(&self.lines, end_line)),
            linewise: true,
            inclusive: true,
        };
        self.apply_operator_to_range(op, &range);
    }

    // ── counted char deletes (the compat fixes) ────────────────

    /// `x` with a count: delete up to `count` chars under the
    /// cursor, clamped to the line end. Vim never joins lines
    /// with `x` (the compat fix).
    fn delete_forward_compat(&mut self, count: u32) {
        self.begin_change_recording('x', count);
        let len = line_len(&self.lines, self.row);
        if self.col < len {
            let end = (self.col + count.saturating_sub(1) as usize).min(len - 1);
            let range = OpRange {
                start: (self.row, self.col),
                end: (self.row, end),
                linewise: false,
                inclusive: true,
            };
            self.apply_operator_to_range('d', &range);
        } else {
            self.finalize_change_recording();
            self.reset_operator_state();
        }
    }

    /// `X` with a count: delete up to `count` chars before the
    /// cursor, clamped to the line start (the compat fix).
    fn delete_backward_compat(&mut self, count: u32) {
        self.begin_change_recording('X', count);
        if self.col > 0 {
            let start = self.col.saturating_sub(count as usize);
            let range = OpRange {
                start: (self.row, start),
                end: (self.row, self.col - 1),
                linewise: false,
                inclusive: true,
            };
            self.apply_operator_to_range('d', &range);
        } else {
            self.finalize_change_recording();
            self.reset_operator_state();
        }
    }

    // ── replace one char (the pi-vim `replaceChar`) ────────────

    /// `r{char}`: replace up to `count` chars at the cursor; the
    /// cursor stays on the last replaced char.
    fn replace_char(&mut self, c: char, count: usize) {
        let len = line_len(&self.lines, self.row);
        if self.col >= len {
            return;
        }
        let end = (self.col + count).min(len);
        self.push_undo();
        let mut chs: Vec<char> = self.lines[self.row].chars().collect();
        for ch in chs.iter_mut().skip(self.col).take(end - self.col) {
            *ch = c;
        }
        self.lines[self.row] = chs.into_iter().collect();
        self.col = end - 1;
    }

    // ── join lines (the pi-vim `J`) ─────────────────────────────

    /// `J` joins the next `count` lines: a single space between
    /// non-empty lines, and no leading blanks on the joined line.
    fn join_lines(&mut self, count: u32) {
        self.begin_change_recording('J', count);
        let join_count = (count.max(1) as usize).min(self.lines.len().saturating_sub(self.row + 1));
        if join_count > 0 {
            self.push_undo();
            let mut join_col = 0usize;
            for _ in 0..join_count {
                let idx = self.row;
                if idx + 1 < self.lines.len() {
                    let cur = self.lines[idx].clone();
                    let next = self.lines[idx + 1].trim_start();
                    join_col = cur.chars().count();
                    if next.is_empty() {
                        self.lines[idx] = cur;
                    } else {
                        self.lines[idx] = format!("{} {}", cur, next);
                    }
                    self.lines.splice(idx + 1..idx + 2, std::iter::empty());
                }
            }
            let final_len = line_len(&self.lines, self.row);
            self.col = join_col.min(final_len.saturating_sub(1));
        }
        self.finalize_change_recording();
        self.reset_operator_state();
    }

    // ── toggle case (the pi-vim `~`) ───────────────────────────

    /// `~` toggles the case of up to `count` chars from the
    /// cursor.
    fn toggle_case(&mut self, count: u32) {
        self.begin_change_recording('~', count);
        let len = line_len(&self.lines, self.row);
        if self.col < len {
            self.push_undo();
            let end = (self.col + count as usize).min(len);
            let mut chs: Vec<char> = self.lines[self.row].chars().collect();
            for ch in chs.iter_mut().skip(self.col).take(end - self.col) {
                *ch = if ch.is_lowercase() {
                    ch.to_uppercase().next().unwrap_or(*ch)
                } else {
                    ch.to_lowercase().next().unwrap_or(*ch)
                };
            }
            self.lines[self.row] = chs.into_iter().collect();
            let new_len = line_len(&self.lines, self.row);
            self.col = end.min(new_len.saturating_sub(1));
        }
        self.finalize_change_recording();
        self.reset_operator_state();
    }

    // ── paste (the pi-vim `paste`) ──────────────────────────────

    /// Paste the register: a linewise register splices its lines
    /// after (`p`) or before (`P`) the cursor line; a char-wise
    /// register splices inline at `col+1` (`p`) / `col` (`P`).
    /// The cursor lands on the last pasted char (`p`) or the
    /// pasted start (`P`), like vim. `n` copies are pasted (the
    /// count rule; the reference omits it).
    fn paste(&mut self, reg: &RegContent, before: bool, n: usize) {
        if reg.text.is_empty() {
            return;
        }
        let n = n.max(1);
        let mut text = String::new();
        for _ in 0..n {
            text.push_str(&reg.text);
        }
        let cursor = (self.row, self.col);
        if reg.linewise {
            let paste_lines: Vec<String> = text.split('\n').map(str::to_string).collect();
            self.push_undo();
            if before {
                self.lines.splice(cursor.0..cursor.0, paste_lines.clone());
                self.row = cursor.0;
                self.col = first_nonblank(&self.lines[self.row]);
            } else {
                let pos = cursor.0 + 1;
                self.lines.splice(pos..pos, paste_lines.clone());
                self.row = pos.min(self.lines.len().saturating_sub(1));
                self.col = first_nonblank(&self.lines[self.row]);
            }
        } else {
            let line = self.lines[cursor.0].clone();
            let chs: Vec<char> = line.chars().collect();
            let len = chs.len();
            if before {
                let col = cursor.1.min(len);
                let mut new_chs: Vec<char> = chs[..col].to_vec();
                new_chs.extend(text.chars());
                new_chs.extend(chs.iter().skip(col).cloned());
                self.push_undo();
                self.lines[cursor.0] = new_chs.into_iter().collect();
                self.row = cursor.0;
                self.col = if text.contains('\n') {
                    col
                } else {
                    (col + text.chars().count().saturating_sub(1))
                        .min(line_len(&self.lines, self.row).saturating_sub(1))
                };
            } else {
                let insert_col = (cursor.1 + 1).min(len);
                if !text.contains('\n') {
                    let mut new_chs: Vec<char> = chs[..insert_col].to_vec();
                    new_chs.extend(text.chars());
                    new_chs.extend(chs.iter().skip(insert_col).cloned());
                    self.push_undo();
                    self.lines[cursor.0] = new_chs.into_iter().collect();
                    let new_len = line_len(&self.lines, self.row);
                    self.col = (insert_col + text.chars().count().saturating_sub(1))
                        .min(new_len.saturating_sub(1));
                } else {
                    let paste_lines: Vec<&str> = text.split('\n').collect();
                    let before_s: String = chs[..insert_col].iter().collect();
                    let after_s: String = chs[insert_col..].iter().collect();
                    let mut here: Vec<String> = vec![before_s + paste_lines[0]];
                    for mid in &paste_lines[1..paste_lines.len() - 1] {
                        here.push(mid.to_string());
                    }
                    here.push(paste_lines[paste_lines.len() - 1].to_string() + &after_s);
                    self.push_undo();
                    self.lines.splice(cursor.0..cursor.0 + 1, here);
                    let last_idx =
                        (cursor.0 + paste_lines.len() - 1).min(self.lines.len().saturating_sub(1));
                    self.row = last_idx;
                    self.col = line_len(&self.lines, self.row).saturating_sub(1);
                }
            }
        }
    }

    // ── dot-repeat replay (the pi-vim `replayLastChange`) ──────

    /// Replay the last recorded change (`.`, `2.` with a count
    /// override). Replay suppresses new recording.
    ///
    /// Faithful to the pinned reference: the recorded key sequence
    /// (with any recorded count digits baked in) is replayed as-is,
    /// and an insert-session text is typed once. A bare `N.`
    /// override does not multiply the replayed change (the reference
    /// ignores the override in the replay loop).
    fn replay_last_change(&mut self, _count_override: u32) {
        let rec = match self.last_change.clone() {
            Some(r) => r,
            None => return,
        };
        self.is_replaying = true;
        for k in rec.keys.iter() {
            self.normal_press(Key::Char(*k));
        }
        if rec.entered_insert {
            let was_replace = self.mode == Mode::Replace;
            let text = rec.inserted_text.clone();
            for c in text.chars() {
                if c == '\n' {
                    if was_replace {
                        self.replay_split_line();
                    } else {
                        self.insert_char('\n');
                    }
                } else if was_replace {
                    self.replay_overtype(c);
                } else {
                    self.insert_char(c);
                }
            }
            self.mode = Mode::Normal;
            if was_replace {
                self.col = self.col.saturating_sub(1);
            } else if self.col > 0 {
                self.col -= 1;
            }
        }
        self.is_replaying = false;
    }

    /// Split the line at the caret (the replace-replay newline).
    fn replay_split_line(&mut self) {
        let chs: Vec<char> = self.lines[self.row].chars().collect();
        let cut = self.col.min(chs.len());
        let before: String = chs[..cut].iter().collect();
        let after: String = chs[cut..].iter().collect();
        self.push_undo();
        self.lines[self.row] = before;
        self.lines.insert(self.row + 1, after);
        self.row += 1;
        self.col = 0;
    }

    /// Overtype one char in replace-replay (append at end of
    /// line).
    fn replay_overtype(&mut self, c: char) {
        let len = line_len(&self.lines, self.row);
        if self.col < len {
            let mut chs: Vec<char> = self.lines[self.row].chars().collect();
            chs[self.col] = c;
            self.push_undo();
            self.lines[self.row] = chs.into_iter().collect();
            self.col += 1;
        } else {
            self.push_undo();
            self.insert_char(c);
        }
    }

    // ── search state (the pi-vim `search.ts`) ──────────────────

    /// Open the search command line (`/` forward, `?` backward),
    /// remembering the mode to return to.
    fn begin_search(&mut self, forward: bool) {
        self.search_active = true;
        self.search_input.clear();
        self.search_prompt = if forward { '/' } else { '?' };
        self.last_search_forward = forward;
        self.search_return_mode = match self.mode {
            Mode::Visual | Mode::VisualLine => self.mode,
            _ => Mode::Normal,
        };
        self.mode = Mode::CommandLine;
        self.reset_operator_state();
    }

    /// The `n` / `N` search motion (the pi-vim `searchNext` /
    /// `searchPrev`): repeat the last search, wrapping the
    /// buffer.
    fn search_repeat(&self, count: u32, forward: bool) -> MotionResult {
        let pattern = match &self.last_search_pattern {
            Some(p) => p.clone(),
            None => {
                return MotionResult {
                    pos: (self.row, self.col),
                    linewise: false,
                    inclusive: false,
                };
            }
        };
        let mut pos = (self.row, self.col);
        for _ in 0..count.max(1) {
            match find_next_match(&self.lines, pos, &pattern, forward) {
                Some(m) => pos = m,
                None => break,
            }
        }
        MotionResult {
            pos,
            linewise: false,
            inclusive: false,
        }
    }

    /// `*` / `#`: the word under the cursor becomes the search
    /// (the pi-vim `searchWordUnderCursor`).
    fn search_word_under_cursor(&mut self, forward: bool) -> MotionResult {
        let word = match word_under_cursor(&self.lines, (self.row, self.col)) {
            Some(w) => w,
            None => {
                return MotionResult {
                    pos: (self.row, self.col),
                    linewise: false,
                    inclusive: false,
                };
            }
        };
        self.last_search_pattern = Some(word.clone());
        self.last_search_forward = forward;
        match find_next_match(&self.lines, (self.row, self.col), &word, forward) {
            Some(m) => MotionResult {
                pos: m,
                linewise: false,
                inclusive: false,
            },
            None => MotionResult {
                pos: (self.row, self.col),
                linewise: false,
                inclusive: false,
            },
        }
    }
}

// ── visual modes (the pi-vim `modes/visual.ts`) ────────────────

impl Editor {
    /// Char-wise (`v`) and line-wise (`V`) visual mode. The anchor
    /// and the cursor bound the selection; motions move the
    /// cursor against the anchor; operators act on the selection.
    fn visual_press(&mut self, key: Key) -> Option<String> {
        // --- Escape / Ctrl+C: back to normal ---
        if key == Key::Esc || key == Key::CtrlC {
            self.visual_anchor = None;
            self.mode = Mode::Normal;
            self.reset_operator_state();
            return None;
        }

        // --- pending register selection (after `"`) ---
        if self.pending_register {
            self.pending_register = false;
            if let Key::Char(c) = key {
                if is_valid_register(c) {
                    self.register = c;
                } else {
                    self.register = '"';
                }
            } else {
                self.register = '"';
            }
            return None;
        }

        // --- pending text object key: set the selection onto the
        // object (the pi-vim visual text-object rule) ---
        if let Some(prefix) = self.pending_text_object_prefix {
            self.pending_text_object_prefix = None;
            if let Key::Char(c) = key {
                if let Some(obj) = resolve_text_object(prefix, c) {
                    let cursor = (self.row, self.col);
                    if let Some(range) = obj(&self.lines, cursor) {
                        self.visual_anchor = Some(range.start);
                        self.go_to(range.end);
                    }
                }
            }
            self.reset_operator_state();
            return None;
        }

        // --- pending character input for f / F / t / T ---
        if let Some(pending) = self.pending_char_motion {
            self.pending_char_motion = None;
            if let Key::Char(c) = key {
                if (c as u32) >= 32 {
                    let count = self.count.max(1);
                    let cursor = (self.row, self.col);
                    let res = match pending {
                        'f' => find_char_forward(
                            &self.lines,
                            cursor,
                            count,
                            c,
                            &mut self.last_char_search,
                        ),
                        'F' => find_char_backward(
                            &self.lines,
                            cursor,
                            count,
                            c,
                            &mut self.last_char_search,
                        ),
                        't' => till_char_forward(
                            &self.lines,
                            cursor,
                            count,
                            c,
                            &mut self.last_char_search,
                        ),
                        _ => till_char_backward(
                            &self.lines,
                            cursor,
                            count,
                            c,
                            &mut self.last_char_search,
                        ),
                    };
                    self.go_to(res.pos);
                }
            }
            self.reset_operator_state();
            return None;
        }

        // --- pending `g` prefix ---
        if self.pending_g {
            self.pending_g = false;
            if key == Key::Char('g') {
                let count_explicit = self.count_started;
                let n = if count_explicit { self.count.max(1) } else { 1 };
                let res = go_to_first_line(&self.lines, (self.row, self.col), n);
                self.go_to(res.pos);
            }
            self.reset_operator_state();
            return None;
        }

        // --- count prefix ---
        if let Key::Char(d) = key {
            if d.is_ascii_digit() && (d != '0' || self.count_started) {
                self.count = (self.count * 10 + d as u32 - '0' as u32).min(99999);
                self.count_started = true;
                return None;
            }
        }
        let count = self.count.max(1);
        let count_explicit = self.count_started;

        // --- operators on the selection ---
        let op: Option<char> = match key {
            Key::Char(c @ 'd') | Key::Char(c @ 'x') | Key::Char(c @ 'D') => Some(c),
            Key::Char(c @ 'c') | Key::Char(c @ 's') | Key::Char(c @ 'C') => Some(c),
            Key::Char(c @ 'y') | Key::Char(c @ 'Y') => Some(c),
            Key::Char('>') => Some('>'),
            Key::Char('<') => Some('<'),
            _ => None,
        };
        if let Some(c) = op {
            let op_norm = match c {
                'd' | 'x' | 'D' => 'd',
                'c' | 's' | 'C' => 'c',
                'y' | 'Y' => 'y',
                _ => c,
            };
            self.apply_visual_operator(op_norm);
            return None;
        }

        // --- paste replaces the selection with the register ---
        if key == Key::Char('p') || key == Key::Char('P') {
            self.paste_visual(key == Key::Char('P'));
            return None;
        }

        // --- register selection prefix ---
        if key == Key::Char('"') {
            self.pending_register = true;
            return None;
        }

        // --- text object prefixes: the object becomes the
        // selection ---
        if let Key::Char(c) = key {
            if c == 'i' || c == 'a' {
                self.pending_text_object_prefix = Some(c);
                return None;
            }
        }

        // --- mode switching ---
        if key == Key::Char('v') {
            if self.mode == Mode::VisualLine {
                self.mode = Mode::Visual;
            } else {
                self.visual_anchor = None;
                self.mode = Mode::Normal;
            }
            self.reset_operator_state();
            return None;
        }
        if key == Key::Char('V') {
            if self.mode == Mode::Visual {
                self.mode = Mode::VisualLine;
            } else {
                self.visual_anchor = None;
                self.mode = Mode::Normal;
            }
            self.reset_operator_state();
            return None;
        }

        // --- `o` / `O` swap the cursor and the anchor ---
        if key == Key::Char('o') || key == Key::Char('O') {
            if let Some(anchor) = self.visual_anchor {
                let cursor = (self.row, self.col);
                self.visual_anchor = Some(cursor);
                self.go_to(anchor);
            }
            self.reset_operator_state();
            return None;
        }

        // --- join the selected lines ---
        if key == Key::Char('J') {
            self.visual_join();
            return None;
        }

        // --- toggle case in the selection ---
        if key == Key::Char('~') {
            self.visual_toggle_case();
            return None;
        }

        // --- motions extend the selection ---
        self.visual_motion(key, count, count_explicit)
    }

    /// The visual range (the pi-vim `getVisualRange`): the anchor
    /// and cursor ends, ordered; line-wise covers whole lines.
    fn visual_range(&self, cursor: (usize, usize)) -> OpRange {
        let anchor = self.visual_anchor.unwrap_or(cursor);
        let is_linewise = self.mode == Mode::VisualLine;
        let (start, end) = if anchor < cursor || (anchor.0 == cursor.0 && anchor.1 <= cursor.1) {
            (anchor, cursor)
        } else {
            (cursor, anchor)
        };
        if is_linewise {
            OpRange {
                start: (start.0, 0),
                end: (end.0, line_len(&self.lines, end.0)),
                linewise: true,
                inclusive: true,
            }
        } else {
            OpRange {
                start,
                end,
                linewise: false,
                inclusive: true,
            }
        }
    }

    /// Apply an operator to the visual selection and return to the
    /// insert or normal mode (the pi-vim `applyVisualOperator`).
    fn apply_visual_operator(&mut self, op: char) {
        let cursor = (self.row, self.col);
        let lines = self.lines.clone();
        let range = self.visual_range(cursor);
        let (new_lines, cur, enter_insert) =
            apply_operator(op, &lines, &range, &mut self.registers, self.register);
        self.push_undo();
        self.lines = new_lines;
        self.row = clamp_line(self.lines.len(), cur.0);
        self.col = cur.1.min(line_len(&self.lines, self.row));
        self.visual_anchor = None;
        self.mode = if enter_insert {
            Mode::Insert
        } else {
            Mode::Normal
        };
        self.reset_operator_state();
    }

    /// `p` / `P` in visual: replace the selection with the register
    /// (the pi-vim visual paste); the deleted text goes to the
    /// unnamed register.
    fn paste_visual(&mut self, before: bool) {
        let _ = before; // visual p and P paste alike (reference rule)
        let cursor = (self.row, self.col);
        let lines = self.lines.clone();
        let range = self.visual_range(cursor);
        let deleted_text = extract_text(&lines, &range);
        let (mut new_lines, pos) = delete_range(&lines, &range);
        let reg = get_register(&self.registers, self.register);
        match reg {
            None => {
                // No register: the selection is simply deleted.
                self.push_undo();
                self.lines = new_lines;
                self.row = clamp_line(self.lines.len(), pos.0);
                self.col = pos.1.min(line_len(&self.lines, self.row));
            }
            Some(reg) => {
                let paste_lines: Vec<String> = reg.text.split('\n').map(str::to_string).collect();
                if reg.linewise {
                    if range.linewise {
                        new_lines.splice(pos.0..pos.0, paste_lines.clone());
                    } else {
                        new_lines.splice(pos.0 + 1..pos.0 + 1, paste_lines.clone());
                    }
                    self.push_undo();
                    self.lines = new_lines;
                    let target_line = if range.linewise { pos.0 } else { pos.0 + 1 };
                    self.row = target_line.min(self.lines.len().saturating_sub(1));
                    self.col = first_nonblank(&self.lines[self.row]);
                } else {
                    let line = new_lines[pos.0].clone();
                    let lchs: Vec<char> = line.chars().collect();
                    let cut = pos.1.min(lchs.len());
                    if paste_lines.len() == 1 {
                        let mut nc: Vec<char> = lchs[..cut].to_vec();
                        nc.extend(reg.text.chars());
                        nc.extend(lchs.iter().skip(cut).cloned());
                        self.push_undo();
                        new_lines[pos.0] = nc.into_iter().collect();
                        self.lines = new_lines;
                        self.row = pos.0;
                        self.col = (pos.1 + reg.text.chars().count().saturating_sub(1))
                            .min(line_len(&self.lines, self.row).saturating_sub(1));
                    } else {
                        let before_s: String = lchs[..cut].iter().collect();
                        let after_s: String = lchs[cut..].iter().collect();
                        let mut merged: Vec<String> = vec![before_s + paste_lines[0].as_str()];
                        for mid in &paste_lines[1..paste_lines.len() - 1] {
                            merged.push(mid.clone());
                        }
                        merged.push(paste_lines[paste_lines.len() - 1].to_string() + &after_s);
                        self.push_undo();
                        new_lines.splice(pos.0..pos.0 + 1, merged);
                        self.lines = new_lines;
                        let last_idx =
                            (pos.0 + paste_lines.len() - 1).min(self.lines.len().saturating_sub(1));
                        self.row = last_idx;
                        self.col = line_len(&self.lines, self.row).saturating_sub(1);
                    }
                }
                delete_to_register(&mut self.registers, '"', &deleted_text, range.linewise);
            }
        }
        self.visual_anchor = None;
        self.mode = Mode::Normal;
        self.reset_operator_state();
    }

    /// Join the selected lines (the pi-vim visual `J`).
    fn visual_join(&mut self) {
        let cursor = (self.row, self.col);
        let range = self.visual_range(cursor);
        let start = range.start.0;
        let end = range.end.0;
        if end > start {
            self.push_undo();
            let mut nl = self.lines.clone();
            for i in start..end {
                let cur = nl[i].clone();
                let next = nl[i + 1].trim_start().to_string();
                if next.is_empty() {
                    nl[i] = cur;
                } else {
                    nl[i] = format!("{} {}", cur, next);
                }
                nl.splice(i + 1..i + 2, std::iter::empty());
            }
            self.lines = nl;
            self.row = start.min(self.lines.len().saturating_sub(1));
            self.col = 0;
        }
        self.visual_anchor = None;
        self.mode = Mode::Normal;
        self.reset_operator_state();
    }

    /// Toggle the case in the selection (the pi-vim visual `~`).
    fn visual_toggle_case(&mut self) {
        let cursor = (self.row, self.col);
        let range = self.visual_range(cursor);
        self.push_undo();
        if range.linewise {
            for ln in range.start.0..=range.end.0 {
                self.lines[ln] = toggle_case_line(&self.lines[ln]);
            }
        } else if range.start.0 == range.end.0 {
            let mut chs: Vec<char> = self.lines[range.start.0].chars().collect();
            let seg: Vec<char> = chs[range.start.1..=range.end.1]
                .iter()
                .map(|&c| {
                    if c.is_lowercase() {
                        c.to_uppercase().next().unwrap_or(c)
                    } else {
                        c.to_lowercase().next().unwrap_or(c)
                    }
                })
                .collect();
            chs.splice(range.start.1..=range.end.1, seg);
            self.lines[range.start.0] = chs.into_iter().collect();
        } else {
            let first = self.lines[range.start.0].clone();
            let fchs: Vec<char> = first.chars().collect();
            self.lines[range.start.0] =
                toggle_case_line(&fchs[range.start.1..].iter().collect::<String>());
            for ln in range.start.0 + 1..range.end.0 {
                self.lines[ln] = toggle_case_line(&self.lines[ln]);
            }
            let last = self.lines[range.end.0].clone();
            let lchs: Vec<char> = last.chars().collect();
            let head: String = lchs[..range.end.1 + 1].iter().collect();
            let tail: String = lchs[range.end.1 + 1..].iter().collect();
            self.lines[range.end.0] = format!("{}{}", toggle_case_line(&head), tail);
        }
        self.row = range.start.0.min(self.lines.len().saturating_sub(1));
        self.col = range.start.1;
        self.clamp_col();
        self.visual_anchor = None;
        self.mode = Mode::Normal;
        self.reset_operator_state();
    }

    /// A motion in visual mode: it moves the cursor, extending the
    /// selection. The host arrow keys map onto the vim motions
    /// (`Enter` / `Down` are `j`, `Up` / `Backspace` are `k`,
    /// `Left` is `h`, `Right` is `l`, `Home` is `0`, `End` is
    /// `$`).
    fn visual_motion(&mut self, key: Key, count: u32, count_explicit: bool) -> Option<String> {
        let ch: Option<char> = match key {
            Key::Char(c) => Some(c),
            _ => None,
        };
        let mapped: Option<char> = match key {
            Key::Enter | Key::Down => Some('j'),
            Key::Up | Key::Backspace => Some('k'),
            Key::Left => Some('h'),
            Key::Right => Some('l'),
            Key::Home => Some('0'),
            Key::End => Some('$'),
            _ => None,
        };
        let cursor = (self.row, self.col);
        match ch.or(mapped) {
            Some('h') => {
                // Vim arrows wrap across lines in visual (the
                // base-editor semantics the reference keeps).
                for _ in 0..count.max(1) {
                    let len = line_len(&self.lines, self.row);
                    if self.col > 0 {
                        self.col -= 1;
                    } else if self.row > 0 {
                        self.row -= 1;
                        self.col = line_len(&self.lines, self.row).saturating_sub(1);
                    } else {
                        let _ = len;
                    }
                }
            }
            Some('l') => {
                for _ in 0..count.max(1) {
                    let len = line_len(&self.lines, self.row);
                    if self.col < len {
                        self.col += 1;
                    } else if self.row < self.lines.len().saturating_sub(1) {
                        self.row += 1;
                        self.col = 0;
                    }
                }
            }
            Some('j') => {
                self.row =
                    (self.row + count.max(1) as usize).min(self.lines.len().saturating_sub(1));
                self.clamp_col();
            }
            Some('k') => {
                self.row = self.row.saturating_sub(count.max(1) as usize);
                self.clamp_col();
            }
            Some('0') => self.col = 0,
            Some('$') => {
                let res = line_end(&self.lines, cursor, count);
                self.go_to(res.pos);
            }
            Some('^') => self.col = first_nonblank(&self.lines[self.row]),
            Some('w') => {
                let res = word_forward(&self.lines, cursor, count);
                self.go_to(res.pos);
            }
            Some('b') | Some('e') | Some('W') | Some('B') | Some('E') => {
                let c = ch.unwrap();
                let res = match c {
                    'b' => word_backward(&self.lines, cursor, count),
                    'B' => WORD_backward(&self.lines, cursor, count),
                    'e' => word_end(&self.lines, cursor, count),
                    'W' => WORD_forward(&self.lines, cursor, count),
                    _ => WORD_end(&self.lines, cursor, count),
                };
                self.go_to(res.pos);
            }
            Some(c @ 'f') | Some(c @ 'F') | Some(c @ 't') | Some(c @ 'T') => {
                self.pending_char_motion = Some(c);
                return None;
            }
            Some(';') | Some(',') => {
                let res = if key == Key::Char(';') {
                    repeat_char_search(&self.lines, cursor, count, &self.last_char_search)
                } else {
                    reverse_char_search(&self.lines, cursor, count, &self.last_char_search)
                };
                self.go_to(res.pos);
            }
            Some('g') => {
                self.pending_g = true;
                return None;
            }
            Some('G') => {
                let n = if count_explicit {
                    count.max(1)
                } else {
                    self.lines.len() as u32
                };
                let res = go_to_last_line(&self.lines, cursor, n);
                self.go_to(res.pos);
            }
            Some('{') | Some('}') => {
                let res = if key == Key::Char('{') {
                    paragraph_backward(&self.lines, cursor, count)
                } else {
                    paragraph_forward(&self.lines, cursor, count)
                };
                self.go_to(res.pos);
            }
            Some('%') => {
                let res = matching_bracket(&self.lines, cursor, 1);
                self.go_to(res.pos);
            }
            Some('n') | Some('N') => {
                let forward = key == Key::Char('n');
                let res = self.search_repeat(
                    count,
                    if forward {
                        self.last_search_forward
                    } else {
                        !self.last_search_forward
                    },
                );
                self.go_to(res.pos);
            }
            Some('*') | Some('#') => {
                let res = self.search_word_under_cursor(key == Key::Char('*'));
                self.go_to(res.pos);
            }
            Some('/') | Some('?') => {
                self.begin_search(key == Key::Char('/'));
                return None;
            }
            _ => {}
        }
        self.count = 0;
        self.count_started = false;
        None
    }
}

// ── command-line mode (the pi-vim search prompt) ───────────────

impl Editor {
    /// The search command line: `/` and `?` input. `Enter` runs
    /// the search and returns to the opening mode; `Esc` or a
    /// backspace on the empty buffer cancels; `Ctrl+U` clears the
    /// buffer (the host routes it here in this mode).
    fn command_line_press(&mut self, key: Key) -> Option<String> {
        match key {
            Key::Esc => {
                self.search_active = false;
                self.search_input.clear();
                self.mode = Mode::Normal;
                self.visual_anchor = None;
                None
            }
            Key::Enter => {
                // Run the search only when the buffer has a pattern;
                // an empty Enter is a no-op (the reference rule).
                let pat = std::mem::take(&mut self.search_input);
                if !pat.is_empty() {
                    self.last_search_pattern = Some(pat.clone());
                    if let Some(m) = find_next_match(
                        &self.lines,
                        (self.row, self.col),
                        &pat,
                        self.last_search_forward,
                    ) {
                        self.go_to(m);
                    }
                }
                self.search_active = false;
                self.mode = self.search_return_mode;
                self.clamp_col();
                None
            }
            Key::Backspace => {
                if self.search_input.pop().is_none() {
                    // A backspace on the empty buffer cancels.
                    self.search_active = false;
                    self.search_input.clear();
                    self.mode = Mode::Normal;
                    self.visual_anchor = None;
                }
                None
            }
            Key::CtrlU => {
                self.search_input.clear();
                None
            }
            Key::Char(c) if (c as u32) >= 32 => {
                self.search_input.push(c);
                None
            }
            _ => None,
        }
    }
}

// ── motions (the pi-vim `motions.ts`) ──────────────────────────

/// `w` — the start of the next word. A word boundary is a
/// transition between the word / punctuation / blank classes.
/// At the end of the file the cursor stays on the last character;
/// a `w` on the last word of a line lands on that word's last
/// char, or crosses to the next line.
fn word_forward(lines: &[String], cursor: (usize, usize), count: u32) -> MotionResult {
    let mut pos = cursor;
    for _ in 0..count.max(1) {
        pos = next_word_start(lines, pos.0, pos.1);
    }
    MotionResult {
        pos,
        linewise: false,
        inclusive: false,
    }
}

/// Neovim rule for `w` / `W`: when the landing is the last char
/// of the same line (the final word has no trailing blank, or the
/// motion did not move), an operator consumes to the end of the
/// line. The pinned reference stops one char short. A plain
/// movement ignores this: `go_to` clamps the column anyway.
fn extend_w_eol(lines: &[String], cursor: (usize, usize), res: MotionResult) -> MotionResult {
    if res.pos.0 != cursor.0 {
        return res; // cross-line landing: the merge rule applies
    }
    let text = chars_of(lines, res.pos.0);
    let line_len = text.len();
    let at_last_char = line_len > 0 && res.pos.1 == line_len - 1;
    // Extend when the motion did not move (the cursor sits on the
    // last char), or the landing is inside the final word (no
    // blank before it). A landing on the start of a real next
    // word (a blank before it, after a move) stays exclusive.
    let no_move = res.pos == (cursor.0, cursor.1);
    let in_final_word = line_len == 1 || !is_blank_char(text[res.pos.1 - 1]);
    if at_last_char && (no_move || in_final_word) && !res.inclusive {
        MotionResult {
            pos: (res.pos.0, line_len),
            linewise: res.linewise,
            inclusive: false,
        }
    } else {
        res
    }
}

fn next_word_start(lines: &[String], line: usize, col: usize) -> (usize, usize) {
    if lines.is_empty() {
        return (0, 0);
    }
    let len = lines.len();
    let mut line = line;
    let mut text = chars_of(lines, line);
    let mut col = col.min(text.len());

    // If at end of file, stay.
    if line >= len.saturating_sub(1) && col >= text.len().saturating_sub(1) {
        return (line, text.len().saturating_sub(1));
    }

    let ch = text.get(col).copied();
    if ch.is_some_and(is_word_char) {
        while col < text.len() && is_word_char(text[col]) {
            col += 1;
        }
    } else if ch.is_some_and(is_punct_char) {
        while col < text.len() && is_punct_char(text[col]) {
            col += 1;
        }
    }

    // Skip blanks, possibly across lines.
    loop {
        while col < text.len() && is_blank_char(text[col]) {
            col += 1;
        }
        if col < text.len() {
            break;
        }
        line += 1;
        if line >= len {
            line = len - 1;
            let t = chars_of(lines, line);
            return (line, t.len().saturating_sub(1));
        }
        text = chars_of(lines, line);
        col = 0;
        // Empty lines are word boundaries in vim; on a non-empty
        // line, continue through indentation.
        if text.is_empty() {
            break;
        }
    }
    (line, col)
}

/// `b` — the start of the previous word.
fn word_backward(lines: &[String], cursor: (usize, usize), count: u32) -> MotionResult {
    let mut pos = cursor;
    for _ in 0..count.max(1) {
        pos = prev_word_start(lines, pos.0, pos.1);
    }
    MotionResult {
        pos,
        linewise: false,
        inclusive: false,
    }
}

fn prev_word_start(lines: &[String], line: usize, col: usize) -> (usize, usize) {
    if lines.is_empty() {
        return (0, 0);
    }
    let mut line = line as i64;
    let mut col = col as i64 - 1;
    loop {
        let text = chars_of(lines, line.max(0) as usize);
        while col >= 0 && (col as usize) < text.len() && is_blank_char(text[col as usize]) {
            col -= 1;
        }
        if col >= 0 {
            break;
        }
        line -= 1;
        if line < 0 {
            return (0, 0);
        }
        col = chars_of(lines, line as usize).len() as i64 - 1;
    }
    let text = chars_of(lines, line as usize);
    let mut c = col as usize;
    let ch = text.get(c).copied();
    if ch.is_some_and(is_word_char) {
        while c > 0 && is_word_char(text[c - 1]) {
            c -= 1;
        }
    } else if ch.is_some_and(is_punct_char) {
        while c > 0 && is_punct_char(text[c - 1]) {
            c -= 1;
        }
    }
    (line as usize, c)
}

/// `e` — the end of the current / next word (inclusive motion).
fn word_end(lines: &[String], cursor: (usize, usize), count: u32) -> MotionResult {
    let mut pos = cursor;
    for _ in 0..count.max(1) {
        pos = next_word_end(lines, pos.0, pos.1);
    }
    MotionResult {
        pos,
        linewise: false,
        inclusive: true,
    }
}

fn next_word_end(lines: &[String], line: usize, col: usize) -> (usize, usize) {
    if lines.is_empty() {
        return (0, 0);
    }
    let len = lines.len();
    let mut line = line;
    let mut text = chars_of(lines, line);
    let mut col = col + 1;

    // Skip blanks, possibly across lines.
    loop {
        while col < text.len() && is_blank_char(text[col]) {
            col += 1;
        }
        if col < text.len() {
            break;
        }
        line += 1;
        if line >= len {
            line = len - 1;
            let t = chars_of(lines, line);
            return (line, t.len().saturating_sub(1));
        }
        text = chars_of(lines, line);
        col = 0;
    }

    // Run through the word chars of the same class.
    let ch = text.get(col).copied();
    if ch.is_some_and(is_word_char) {
        while col + 1 < text.len() && is_word_char(text[col + 1]) {
            col += 1;
        }
    } else if ch.is_some_and(is_punct_char) {
        while col + 1 < text.len() && is_punct_char(text[col + 1]) {
            col += 1;
        }
    }
    (line, col)
}

#[allow(non_snake_case)]
/// `W` — the start of the next WORD (blank-delimited).
fn WORD_forward(lines: &[String], cursor: (usize, usize), count: u32) -> MotionResult {
    let mut pos = cursor;
    for _ in 0..count.max(1) {
        pos = next_WORD_start(lines, pos.0, pos.1);
    }
    MotionResult {
        pos,
        linewise: false,
        inclusive: false,
    }
}

#[allow(non_snake_case)]
fn next_WORD_start(lines: &[String], line: usize, col: usize) -> (usize, usize) {
    if lines.is_empty() {
        return (0, 0);
    }
    let len = lines.len();
    let mut line = line;
    let mut text = chars_of(lines, line);
    let mut col = col.min(text.len());

    // Skip non-blank.
    while col < text.len() && !is_blank_char(text[col]) {
        col += 1;
    }

    // Skip blanks, possibly across lines.
    loop {
        while col < text.len() && is_blank_char(text[col]) {
            col += 1;
        }
        if col < text.len() {
            break;
        }
        line += 1;
        if line >= len {
            line = len - 1;
            let t = chars_of(lines, line);
            return (line, t.len().saturating_sub(1));
        }
        text = chars_of(lines, line);
        col = 0;
        if text.is_empty() {
            break;
        }
    }
    (line, col)
}

#[allow(non_snake_case)]
/// `B` — the start of the previous WORD.
fn WORD_backward(lines: &[String], cursor: (usize, usize), count: u32) -> MotionResult {
    let mut pos = cursor;
    for _ in 0..count.max(1) {
        pos = prev_WORD_start(lines, pos.0, pos.1);
    }
    MotionResult {
        pos,
        linewise: false,
        inclusive: false,
    }
}

#[allow(non_snake_case)]
fn prev_WORD_start(lines: &[String], line: usize, col: usize) -> (usize, usize) {
    if lines.is_empty() {
        return (0, 0);
    }
    let mut line = line as i64;
    let mut col = col as i64 - 1;
    loop {
        let text = chars_of(lines, line.max(0) as usize);
        while col >= 0 && (col as usize) < text.len() && is_blank_char(text[col as usize]) {
            col -= 1;
        }
        if col >= 0 {
            break;
        }
        line -= 1;
        if line < 0 {
            return (0, 0);
        }
        col = chars_of(lines, line as usize).len() as i64 - 1;
    }
    let text = chars_of(lines, line as usize);
    let mut c = col as usize;
    while c > 0 && !is_blank_char(text[c - 1]) {
        c -= 1;
    }
    (line as usize, c)
}

#[allow(non_snake_case)]
/// `E` — the end of the current / next WORD (inclusive motion).
fn WORD_end(lines: &[String], cursor: (usize, usize), count: u32) -> MotionResult {
    let mut pos = cursor;
    for _ in 0..count.max(1) {
        pos = next_WORD_end(lines, pos.0, pos.1);
    }
    MotionResult {
        pos,
        linewise: false,
        inclusive: true,
    }
}

#[allow(non_snake_case)]
fn next_WORD_end(lines: &[String], line: usize, col: usize) -> (usize, usize) {
    if lines.is_empty() {
        return (0, 0);
    }
    let len = lines.len();
    let mut line = line;
    let mut text = chars_of(lines, line);
    let mut col = col + 1;

    loop {
        while col < text.len() && is_blank_char(text[col]) {
            col += 1;
        }
        if col < text.len() {
            break;
        }
        line += 1;
        if line >= len {
            line = len - 1;
            let t = chars_of(lines, line);
            return (line, t.len().saturating_sub(1));
        }
        text = chars_of(lines, line);
        col = 0;
    }

    // Run through the non-blank run.
    while col + 1 < text.len() && !is_blank_char(text[col + 1]) {
        col += 1;
    }
    (line, col)
}

/// `gg` — to the first line, or line N with a count (linewise).
fn go_to_first_line(lines: &[String], _cursor: (usize, usize), count: u32) -> MotionResult {
    let target = clamp_line(lines.len(), (count.max(1) as usize).saturating_sub(1));
    MotionResult {
        pos: (target, first_nonblank(&lines[target])),
        linewise: true,
        inclusive: false,
    }
}

/// `G` — to the last line, or line N with a count (linewise).
fn go_to_last_line(lines: &[String], _cursor: (usize, usize), count: u32) -> MotionResult {
    let target = clamp_line(lines.len(), (count.max(1) as usize).saturating_sub(1));
    MotionResult {
        pos: (target, first_nonblank(&lines[target])),
        linewise: true,
        inclusive: false,
    }
}

/// `^` — the first non-blank char of the line.
fn first_nonblank_motion(lines: &[String], cursor: (usize, usize), _count: u32) -> MotionResult {
    MotionResult {
        pos: (cursor.0, first_nonblank(&lines[cursor.0])),
        linewise: false,
        inclusive: false,
    }
}

/// `0` — the start of the line.
fn line_start(_lines: &[String], cursor: (usize, usize), _count: u32) -> MotionResult {
    MotionResult {
        pos: (cursor.0, 0),
        linewise: false,
        inclusive: false,
    }
}

/// `$` — the end of the line (the last char; a count moves down
/// first). Inclusive motion.
fn line_end(lines: &[String], cursor: (usize, usize), count: u32) -> MotionResult {
    let target = clamp_line(
        lines.len(),
        cursor.0 + (count.max(1) as usize).saturating_sub(1),
    );
    MotionResult {
        pos: (target, line_len(lines, target).saturating_sub(1)),
        linewise: false,
        inclusive: true,
    }
}

/// `h` — left within the line; it never crosses the line start
/// (the compat fix).
fn char_left(_lines: &[String], cursor: (usize, usize), count: u32) -> MotionResult {
    MotionResult {
        pos: (cursor.0, cursor.1.saturating_sub(count as usize)),
        linewise: false,
        inclusive: false,
    }
}

/// `l` — right within the line; it never crosses the line end,
/// and the cursor cannot rest past the last character in normal
/// mode (the compat fix).
fn char_right(lines: &[String], cursor: (usize, usize), count: u32) -> MotionResult {
    let cap = line_len(lines, cursor.0).saturating_sub(1);
    let col = if cap == 0 && line_len(lines, cursor.0) == 0 {
        0
    } else {
        (cursor.1 + count as usize).min(cap)
    };
    MotionResult {
        pos: (cursor.0, col),
        linewise: false,
        inclusive: false,
    }
}

// --- find / till character motions (the pi-vim f / F / t / T) ---

fn char_search(
    lines: &[String],
    cursor: (usize, usize),
    count: u32,
    ch: char,
    forward: bool,
    kind: SearchKind,
) -> MotionResult {
    let text = chars_of(lines, cursor.0);
    let stay = |pos: (usize, usize)| MotionResult {
        pos,
        linewise: false,
        inclusive: true,
    };
    let mut col: i64 = cursor.1 as i64;
    let len = text.len() as i64;
    for _ in 0..count.max(1) {
        if forward {
            if kind == SearchKind::Till {
                col += 1;
            }
            while col < len && text[col as usize] != ch {
                col += 1;
            }
            if col >= len {
                return stay(cursor);
            }
        } else {
            col -= 1;
            while col >= 0 && text[col as usize] != ch {
                col -= 1;
            }
            if col < 0 {
                return stay(cursor);
            }
        }
    }
    let col = match (forward, kind) {
        (true, SearchKind::Find) => col,
        (true, SearchKind::Till) => col - 1,
        (false, SearchKind::Find) => col,
        (false, SearchKind::Till) => col + 1,
    };
    MotionResult {
        pos: (cursor.0, (col.max(0)) as usize),
        linewise: false,
        inclusive: true,
    }
}

/// `f{char}` — find the char forward on the line (inclusive).
fn find_char_forward(
    lines: &[String],
    cursor: (usize, usize),
    count: u32,
    ch: char,
    last: &mut Option<(char, SearchDir, SearchKind)>,
) -> MotionResult {
    *last = Some((ch, SearchDir::Forward, SearchKind::Find));
    char_search(lines, cursor, count, ch, true, SearchKind::Find)
}

/// `F{char}` — find the char backward on the line (inclusive).
fn find_char_backward(
    lines: &[String],
    cursor: (usize, usize),
    count: u32,
    ch: char,
    last: &mut Option<(char, SearchDir, SearchKind)>,
) -> MotionResult {
    *last = Some((ch, SearchDir::Backward, SearchKind::Find));
    char_search(lines, cursor, count, ch, false, SearchKind::Find)
}

/// `t{char}` — stop one before the char (inclusive).
fn till_char_forward(
    lines: &[String],
    cursor: (usize, usize),
    count: u32,
    ch: char,
    last: &mut Option<(char, SearchDir, SearchKind)>,
) -> MotionResult {
    *last = Some((ch, SearchDir::Forward, SearchKind::Till));
    char_search(lines, cursor, count, ch, true, SearchKind::Till)
}

/// `T{char}` — stop one after the char (inclusive).
fn till_char_backward(
    lines: &[String],
    cursor: (usize, usize),
    count: u32,
    ch: char,
    last: &mut Option<(char, SearchDir, SearchKind)>,
) -> MotionResult {
    *last = Some((ch, SearchDir::Backward, SearchKind::Till));
    char_search(lines, cursor, count, ch, false, SearchKind::Till)
}

/// `;` — repeat the last find / till in the same direction.
fn repeat_char_search(
    lines: &[String],
    cursor: (usize, usize),
    count: u32,
    last: &Option<(char, SearchDir, SearchKind)>,
) -> MotionResult {
    let Some((ch, dir, kind)) = last else {
        return MotionResult {
            pos: cursor,
            linewise: false,
            inclusive: true,
        };
    };
    char_search(
        lines,
        cursor,
        count,
        *ch,
        matches!(dir, SearchDir::Forward),
        *kind,
    )
}

/// `,` — repeat the last find / till in the opposite direction.
fn reverse_char_search(
    lines: &[String],
    cursor: (usize, usize),
    count: u32,
    last: &Option<(char, SearchDir, SearchKind)>,
) -> MotionResult {
    let Some((ch, dir, kind)) = last else {
        return MotionResult {
            pos: cursor,
            linewise: false,
            inclusive: true,
        };
    };
    char_search(
        lines,
        cursor,
        count,
        *ch,
        !matches!(dir, SearchDir::Forward),
        *kind,
    )
}

// --- paragraph motions (the pi-vim `{` / `}`) ------------------

fn paragraph_backward(lines: &[String], cursor: (usize, usize), count: u32) -> MotionResult {
    let mut line = cursor.0;
    for _ in 0..count.max(1) {
        while line > 0 && is_blank_line(&lines[line]) {
            line -= 1;
        }
        while line > 0 && !is_blank_line(&lines[line]) {
            line -= 1;
        }
    }
    MotionResult {
        pos: (line, 0),
        linewise: true,
        inclusive: false,
    }
}

fn paragraph_forward(lines: &[String], cursor: (usize, usize), count: u32) -> MotionResult {
    let last = lines.len().saturating_sub(1);
    let mut line = cursor.0;
    for _ in 0..count.max(1) {
        while line < last && is_blank_line(&lines[line]) {
            line += 1;
        }
        while line < last && !is_blank_line(&lines[line]) {
            line += 1;
        }
    }
    MotionResult {
        pos: (line, 0),
        linewise: true,
        inclusive: false,
    }
}

// --- matching bracket (the pi-vim `%`) -------------------------

const BRACKET_PAIRS: [(char, char); 6] = [
    ('(', ')'),
    (')', '('),
    ('[', ']'),
    (']', '['),
    ('{', '}'),
    ('}', '{'),
];

fn bracket_match(c: char) -> Option<char> {
    BRACKET_PAIRS.iter().find(|(o, _)| *o == c).map(|(_, m)| *m)
}

fn is_open_bracket(c: char) -> bool {
    matches!(c, '(' | '[' | '{')
}

/// `%` — jump to the matching bracket (depth-aware).
fn matching_bracket(lines: &[String], cursor: (usize, usize), _count: u32) -> MotionResult {
    const STAY: fn((usize, usize)) -> MotionResult = |p| MotionResult {
        pos: p,
        linewise: false,
        inclusive: true,
    };
    let text = chars_of(lines, cursor.0);
    let mut bracket_col = cursor.1.min(text.len().saturating_sub(1));
    while bracket_col < text.len() && bracket_match(text[bracket_col]).is_none() {
        bracket_col += 1;
    }
    if bracket_col >= text.len() {
        return STAY(cursor);
    }
    let bracket = text[bracket_col];
    let match_c = bracket_match(bracket).unwrap();
    let depth_start = 1;
    let mut depth = depth_start;
    if is_open_bracket(bracket) {
        let mut line = cursor.0;
        let mut col = bracket_col + 1;
        while line < lines.len() {
            let lt = chars_of(lines, line);
            while col < lt.len() {
                if lt[col] == bracket {
                    depth += 1;
                } else if lt[col] == match_c {
                    depth -= 1;
                }
                if depth == 0 {
                    return MotionResult {
                        pos: (line, col),
                        linewise: false,
                        inclusive: true,
                    };
                }
                col += 1;
            }
            line += 1;
            col = 0;
        }
    } else {
        let mut line = cursor.0;
        let mut col = bracket_col.saturating_sub(1);
        loop {
            let lt = chars_of(lines, line);
            loop {
                if lt.get(col) == Some(&bracket) {
                    depth += 1;
                } else if lt.get(col) == Some(&match_c) {
                    depth -= 1;
                }
                if depth == 0 {
                    return MotionResult {
                        pos: (line, col),
                        linewise: false,
                        inclusive: true,
                    };
                }
                if col == 0 {
                    break;
                }
                col -= 1;
            }
            if line == 0 {
                break;
            }
            line -= 1;
            col = line_len(lines, line).saturating_sub(1);
        }
    }
    STAY(cursor)
}

// ── operators (the pi-vim `operators.ts`) ──────────────────────

/// Convert a motion result (from the cursor) into an operator
/// range.
fn motion_to_range(cursor: (usize, usize), motion: &MotionResult) -> OpRange {
    let p = motion.pos;
    let (start, end) = if p < cursor { (p, cursor) } else { (cursor, p) };
    OpRange {
        start,
        end,
        linewise: motion.linewise,
        inclusive: motion.inclusive,
    }
}

/// Convert a text object range into an operator range (text
/// objects are always inclusive).
fn text_object_to_range(range: &OpRange) -> OpRange {
    OpRange {
        start: range.start,
        end: range.end,
        linewise: false,
        inclusive: true,
    }
}

/// Extract the text of a range within the buffer lines.
fn extract_text(lines: &[String], r: &OpRange) -> String {
    if r.linewise {
        return lines[r.start.0..=r.end.0].join("\n");
    }
    if r.start.0 == r.end.0 {
        let s = chars_of(lines, r.start.0);
        let end_col = if r.inclusive { r.end.1 + 1 } else { r.end.1 };
        return s[r.start.1..end_col.min(s.len())].iter().collect();
    }
    let first = chars_of(lines, r.start.0)[r.start.1..]
        .iter()
        .collect::<String>();
    let mut out = vec![first];
    for line in &lines[r.start.0 + 1..r.end.0] {
        out.push(line.clone());
    }
    let last_s = chars_of(lines, r.end.0);
    let end_col = if r.inclusive { r.end.1 + 1 } else { r.end.1 };
    out.push(last_s[..end_col.min(last_s.len())].iter().collect());
    out.join("\n")
}

/// Delete a range and return the new lines plus the cursor
/// position.
fn delete_range(lines: &[String], r: &OpRange) -> (Vec<String>, (usize, usize)) {
    let mut new_lines = lines.to_vec();
    if r.linewise {
        let count = r.end.0 - r.start.0 + 1;
        new_lines.splice(r.start.0..r.start.0 + count, std::iter::empty());
        if new_lines.is_empty() {
            new_lines.push(String::new());
        }
        let cursor_line = r.start.0.min(new_lines.len().saturating_sub(1));
        let col = first_nonblank(&new_lines[cursor_line]);
        return (new_lines, (cursor_line, col));
    }
    if r.start.0 == r.end.0 {
        let end_col = if r.inclusive { r.end.1 + 1 } else { r.end.1 };
        let s: Vec<char> = lines[r.start.0].chars().collect();
        let mut new_chars: Vec<char> = s[..r.start.1].to_vec();
        new_chars.extend(s.iter().skip(end_col.min(s.len())).cloned());
        new_lines[r.start.0] = new_chars.into_iter().collect();
        let result_len = new_lines[r.start.0].chars().count();
        let col = r.start.1.min(result_len.saturating_sub(1));
        return (new_lines, (r.start.0, col));
    }
    let first: Vec<char> = lines[r.start.0].chars().collect();
    let last: Vec<char> = lines[r.end.0].chars().collect();
    let end_col = if r.inclusive { r.end.1 + 1 } else { r.end.1 };
    let merged: String = first[r.start.1..].iter().collect::<String>()
        + &last[end_col.min(last.len())..].iter().collect::<String>();
    new_lines.splice(r.start.0..=r.end.0, std::iter::once(merged));
    if new_lines.is_empty() {
        new_lines.push(String::new());
    }
    let col = r
        .start
        .1
        .min(line_len(&new_lines, r.start.0).saturating_sub(1));
    (new_lines, (r.start.0, col))
}

/// Indent the lines of a range by two spaces (the `>` operator).
fn indent_range(lines: &[String], r: &OpRange) -> (Vec<String>, (usize, usize)) {
    let mut nl = lines.to_vec();
    for line in nl.iter_mut().skip(r.start.0).take(r.end.0 - r.start.0 + 1) {
        if !line.is_empty() {
            *line = format!("  {line}");
        }
    }
    let cursor = (r.start.0, first_nonblank(&nl[r.start.0.min(nl.len() - 1)]));
    (nl, cursor)
}

/// Dedent the lines of a range (the `<` operator): remove up to
/// two leading spaces, or one leading tab.
fn dedent_range(lines: &[String], r: &OpRange) -> (Vec<String>, (usize, usize)) {
    let mut nl = lines.to_vec();
    for line in nl.iter_mut().skip(r.start.0).take(r.end.0 - r.start.0 + 1) {
        let chs: Vec<char> = line.chars().collect();
        let mut removed = 0;
        while removed < 2 && removed < chs.len() && chs[removed] == ' ' {
            removed += 1;
        }
        if removed == 0 && chs.first() == Some(&'\t') {
            removed = 1;
        }
        *line = chs[removed..].iter().collect();
    }
    let cursor = (r.start.0, first_nonblank(&nl[r.start.0.min(nl.len() - 1)]));
    (nl, cursor)
}

/// Apply an operator to a range: delete (`d`), change (`c`),
/// yank (`y`), indent (`>`), dedent (`<`).
fn apply_operator(
    op: char,
    lines: &[String],
    r: &OpRange,
    registers: &mut HashMap<char, RegContent>,
    reg: char,
) -> (Vec<String>, (usize, usize), bool) {
    let text = extract_text(lines, r);
    match op {
        'd' => {
            delete_to_register(registers, reg, &text, r.linewise);
            let (nl, cursor) = delete_range(lines, r);
            (nl, cursor, false)
        }
        'c' => {
            delete_to_register(registers, reg, &text, r.linewise);
            if r.linewise {
                // A linewise change replaces the lines with a
                // single empty line and enters insert.
                let mut nl = lines.to_vec();
                nl.splice(r.start.0..=r.end.0, std::iter::once(String::new()));
                (nl, (r.start.0, 0), true)
            } else {
                let (nl, _) = delete_range(lines, r);
                (nl, (r.start.0, r.start.1), true)
            }
        }
        'y' => {
            yank_to_register(registers, reg, &text, r.linewise);
            (lines.to_vec(), (r.start.0, r.start.1), false)
        }
        '>' => {
            let (nl, cursor) = indent_range(lines, r);
            (nl, cursor, false)
        }
        '<' => {
            let (nl, cursor) = dedent_range(lines, r);
            (nl, cursor, false)
        }
        _ => (lines.to_vec(), r.start, false),
    }
}

/// Toggle the case of a whole line (the `~` operator helper).
fn toggle_case_line(line: &str) -> String {
    line.chars()
        .map(|c| {
            if c.is_lowercase() {
                c.to_uppercase().next().unwrap_or(c)
            } else {
                c.to_lowercase().next().unwrap_or(c)
            }
        })
        .collect()
}

// ── registers (the pi-vim `registers.ts`) ──────────────────────

/// The valid register names: `"` default, `_` black hole, `0-9`
/// numbered, `a-z` named, `A-Z` append, `+` / `*` clipboard.
fn is_valid_register(name: char) -> bool {
    matches!(
        name,
        '"' | '_' | '0'..='9' | 'a'..='z' | 'A'..='Z' | '+' | '*'
    )
}

/// Read a register (the pi-vim `getRegister`).
fn get_register(registers: &HashMap<char, RegContent>, name: char) -> Option<RegContent> {
    registers.get(&name).cloned()
}

/// Store text after a yank into the register set (the pi-vim
/// `yankToRegister`): unnamed + `0` on yanks, named reads and
/// writes, `A-Z` append to lowercase, `_` discards, `+` / `*`
/// alias the clipboard.
fn yank_to_register(
    registers: &mut HashMap<char, RegContent>,
    name: char,
    text: &str,
    linewise: bool,
) {
    if name == '_' {
        return;
    }
    let content = RegContent {
        text: text.to_string(),
        linewise,
    };
    if name.is_ascii_uppercase() {
        let lower = name.to_ascii_lowercase();
        let merged = match registers.get(&lower) {
            Some(existing) => {
                let sep = if existing.linewise || linewise {
                    "\n"
                } else {
                    ""
                };
                RegContent {
                    text: format!("{}{}{}", existing.text, sep, text),
                    linewise: existing.linewise || linewise,
                }
            }
            None => content.clone(),
        };
        registers.insert(lower, merged.clone());
        registers.insert('"', merged);
        return;
    }
    match name {
        '+' | '*' => {
            registers.insert('+', content.clone());
            registers.insert('*', content.clone());
            registers.insert('"', content.clone());
            registers.insert('0', content);
        }
        '"' => {
            registers.insert('"', content.clone());
            registers.insert('0', content);
        }
        _ => {
            registers.insert(name, content.clone());
            registers.insert('"', content);
        }
    }
}

/// Store text after a delete / change into the register set (the
/// pi-vim `deleteToRegister`): the numbered registers shift on
/// every unnamed delete.
fn delete_to_register(
    registers: &mut HashMap<char, RegContent>,
    name: char,
    text: &str,
    linewise: bool,
) {
    if name == '_' {
        return;
    }
    let content = RegContent {
        text: text.to_string(),
        linewise,
    };
    if name.is_ascii_uppercase() {
        let lower = name.to_ascii_lowercase();
        let merged = match registers.get(&lower) {
            Some(existing) => {
                let sep = if existing.linewise || linewise {
                    "\n"
                } else {
                    ""
                };
                RegContent {
                    text: format!("{}{}{}", existing.text, sep, text),
                    linewise: existing.linewise || linewise,
                }
            }
            None => content.clone(),
        };
        registers.insert(lower, merged.clone());
        registers.insert('"', merged);
        return;
    }
    match name {
        '+' | '*' => {
            registers.insert('+', content.clone());
            registers.insert('*', content.clone());
            registers.insert('"', content);
        }
        '"' => {
            // Shift 9 <- 8 <- ... <- 2 <- 1.
            for i in (2..=9).rev() {
                if let Some(prev) = registers.get(&(char::from((i - 1) as u8))) {
                    registers.insert(char::from(i as u8), prev.clone());
                }
            }
            registers.insert('1', content.clone());
            registers.insert('"', content);
        }
        _ => {
            registers.insert(name, content.clone());
            registers.insert('"', content);
        }
    }
}

// ── text objects (the pi-vim `text-objects.ts`) ─────────────────

/// The word class of the text objects (ASCII, like the reference).
fn is_obj_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

type TextObjectFn = fn(&[String], (usize, usize)) -> Option<OpRange>;

fn range_on_line(row: usize, start: usize, end: usize) -> OpRange {
    OpRange {
        start: (row, start),
        end: (row, end),
        linewise: false,
        inclusive: true,
    }
}

/// `iw` — the word under the cursor (no surrounding blanks).
fn inner_word(lines: &[String], cursor: (usize, usize)) -> Option<OpRange> {
    let chs = chars_of(lines, cursor.0);
    if chs.is_empty() {
        return None;
    }
    let col = cursor.1.min(chs.len() - 1);
    let c = chs[col];
    let mut start = col;
    let mut end = col;
    if is_obj_word_char(c) {
        while start > 0 && is_obj_word_char(chs[start - 1]) {
            start -= 1;
        }
        while end + 1 < chs.len() && is_obj_word_char(chs[end + 1]) {
            end += 1;
        }
    } else if is_blank_char(c) {
        while start > 0 && is_blank_char(chs[start - 1]) {
            start -= 1;
        }
        while end + 1 < chs.len() && is_blank_char(chs[end + 1]) {
            end += 1;
        }
    } else {
        while start > 0 && !is_obj_word_char(chs[start - 1]) && !is_blank_char(chs[start - 1]) {
            start -= 1;
        }
        while end + 1 < chs.len() && !is_obj_word_char(chs[end + 1]) && !is_blank_char(chs[end + 1])
        {
            end += 1;
        }
    }
    Some(range_on_line(cursor.0, start, end))
}

/// `aw` — the word plus its trailing (or leading) blanks.
fn a_word(lines: &[String], cursor: (usize, usize)) -> Option<OpRange> {
    let inner = inner_word(lines, cursor)?;
    let chs = chars_of(lines, cursor.0);
    let mut start = inner.start.1;
    let mut end = inner.end.1;
    if end + 1 < chs.len() && is_blank_char(chs[end + 1]) {
        end += 1;
        while end + 1 < chs.len() && is_blank_char(chs[end + 1]) {
            end += 1;
        }
    } else if start > 0 && is_blank_char(chs[start - 1]) {
        start -= 1;
        while start > 0 && is_blank_char(chs[start - 1]) {
            start -= 1;
        }
    }
    Some(range_on_line(cursor.0, start, end))
}

#[allow(non_snake_case)]
/// `iW` — the blank-delimited word under the cursor.
fn inner_WORD(lines: &[String], cursor: (usize, usize)) -> Option<OpRange> {
    let chs = chars_of(lines, cursor.0);
    if chs.is_empty() {
        return None;
    }
    let col = cursor.1.min(chs.len() - 1);
    let c = chs[col];
    let mut start = col;
    let mut end = col;
    if is_blank_char(c) {
        while start > 0 && is_blank_char(chs[start - 1]) {
            start -= 1;
        }
        while end + 1 < chs.len() && is_blank_char(chs[end + 1]) {
            end += 1;
        }
    } else {
        while start > 0 && !is_blank_char(chs[start - 1]) {
            start -= 1;
        }
        while end + 1 < chs.len() && !is_blank_char(chs[end + 1]) {
            end += 1;
        }
    }
    Some(range_on_line(cursor.0, start, end))
}

#[allow(non_snake_case)]
/// `aW` — the WORD plus its trailing (or leading) blanks.
fn a_WORD(lines: &[String], cursor: (usize, usize)) -> Option<OpRange> {
    let inner = inner_WORD(lines, cursor)?;
    let chs = chars_of(lines, cursor.0);
    let mut start = inner.start.1;
    let mut end = inner.end.1;
    if end + 1 < chs.len() && is_blank_char(chs[end + 1]) {
        end += 1;
        while end + 1 < chs.len() && is_blank_char(chs[end + 1]) {
            end += 1;
        }
    } else if start > 0 && is_blank_char(chs[start - 1]) {
        start -= 1;
        while start > 0 && is_blank_char(chs[start - 1]) {
            start -= 1;
        }
    }
    Some(range_on_line(cursor.0, start, end))
}

/// A quote text object: pair the quote chars of the line and find
/// the pair containing the cursor.
fn quote_object_impl(
    lines: &[String],
    cursor: (usize, usize),
    quote: char,
    inner: bool,
) -> Option<OpRange> {
    let line = &lines[cursor.0];
    let col = cursor.1;
    let positions: Vec<usize> = line
        .chars()
        .enumerate()
        .filter_map(|(i, c)| (c == quote).then_some(i))
        .collect();

    let pair_range = |open: usize, close: usize| -> OpRange {
        if inner {
            if close - open <= 1 {
                // Empty quotes: a zero-width range.
                OpRange {
                    start: (cursor.0, open + 1),
                    end: (cursor.0, open),
                    linewise: false,
                    inclusive: true,
                }
            } else {
                OpRange {
                    start: (cursor.0, open + 1),
                    end: (cursor.0, close - 1),
                    linewise: false,
                    inclusive: true,
                }
            }
        } else {
            OpRange {
                start: (cursor.0, open),
                end: (cursor.0, close),
                linewise: false,
                inclusive: true,
            }
        }
    };

    // Try to find a pair that contains the cursor.
    for i in 0..positions.len().saturating_sub(1) {
        let open = positions[i];
        let close = positions[i + 1];
        if col >= open && col <= close {
            return Some(pair_range(open, close));
        }
    }

    // If the cursor is before the first pair, use the first.
    if positions.len() >= 2 && col < positions[0] {
        return Some(pair_range(positions[0], positions[1]));
    }

    // No matching quotes found.
    None
}

/// `i"` / `a"` — the double-quoted region.
fn inner_double_quote(lines: &[String], cursor: (usize, usize)) -> Option<OpRange> {
    quote_object_impl(lines, cursor, '"', true)
}

fn a_double_quote(lines: &[String], cursor: (usize, usize)) -> Option<OpRange> {
    quote_object_impl(lines, cursor, '"', false)
}

/// `i'` / `a'` — the single-quoted region.
fn inner_single_quote(lines: &[String], cursor: (usize, usize)) -> Option<OpRange> {
    quote_object_impl(lines, cursor, '\'', true)
}

fn a_single_quote(lines: &[String], cursor: (usize, usize)) -> Option<OpRange> {
    quote_object_impl(lines, cursor, '\'', false)
}

/// `i`` / `a`` — the backtick-quoted region.
fn inner_backtick(lines: &[String], cursor: (usize, usize)) -> Option<OpRange> {
    quote_object_impl(lines, cursor, '`', true)
}

fn a_backtick(lines: &[String], cursor: (usize, usize)) -> Option<OpRange> {
    quote_object_impl(lines, cursor, '`', false)
}

/// Resolve a text object key sequence (`iw`, `a(`, ...). The
/// prefix is `i` or `a`; the key is the object.
fn resolve_text_object(prefix: char, key: char) -> Option<TextObjectFn> {
    let inner = prefix == 'i';
    match key {
        'w' => Some(if inner { inner_word } else { a_word }),
        'W' => Some(if inner { inner_WORD } else { a_WORD }),
        '"' => Some(if inner {
            inner_double_quote
        } else {
            a_double_quote
        }),
        '\'' => Some(if inner {
            inner_single_quote
        } else {
            a_single_quote
        }),
        '`' => Some(if inner { inner_backtick } else { a_backtick }),
        '(' | ')' | 'b' => Some(if inner { inner_paren } else { a_paren }),
        '{' | '}' | 'B' => Some(if inner { inner_brace } else { a_brace }),
        '[' | ']' => Some(if inner { inner_square } else { a_square }),
        '<' | '>' => Some(if inner { inner_angle } else { a_angle }),
        _ => None,
    }
}

fn inner_paren(lines: &[String], cursor: (usize, usize)) -> Option<OpRange> {
    bracket_object(lines, cursor, '(', ')', true)
}

fn a_paren(lines: &[String], cursor: (usize, usize)) -> Option<OpRange> {
    bracket_object(lines, cursor, '(', ')', false)
}

fn inner_brace(lines: &[String], cursor: (usize, usize)) -> Option<OpRange> {
    bracket_object(lines, cursor, '{', '}', true)
}

fn a_brace(lines: &[String], cursor: (usize, usize)) -> Option<OpRange> {
    bracket_object(lines, cursor, '{', '}', false)
}

fn inner_square(lines: &[String], cursor: (usize, usize)) -> Option<OpRange> {
    bracket_object(lines, cursor, '[', ']', true)
}

fn a_square(lines: &[String], cursor: (usize, usize)) -> Option<OpRange> {
    bracket_object(lines, cursor, '[', ']', false)
}

fn inner_angle(lines: &[String], cursor: (usize, usize)) -> Option<OpRange> {
    bracket_object(lines, cursor, '<', '>', true)
}

fn a_angle(lines: &[String], cursor: (usize, usize)) -> Option<OpRange> {
    bracket_object(lines, cursor, '<', '>', false)
}

/// A nesting-aware bracket object (the pi-vim bracket objects).
fn bracket_object(
    lines: &[String],
    cursor: (usize, usize),
    open_c: char,
    close_c: char,
    inner: bool,
) -> Option<OpRange> {
    // Backward search for the opening bracket (nesting aware).
    let mut depth = 0;
    let mut open_line = cursor.0;
    let mut open_col = cursor.1;
    let mut found = false;
    'back: for ln in (0..=cursor.0).rev() {
        let text = chars_of(lines, ln);
        let start_col = if ln == cursor.0 {
            cursor.1
        } else {
            text.len().saturating_sub(1)
        };
        let mut c = start_col;
        while (c as i64) >= 0 {
            let ch = text.get(c).copied();
            if ch == Some(close_c) && !(ln == cursor.0 && c == cursor.1) {
                depth += 1;
            } else if ch == Some(open_c) {
                if depth == 0 {
                    open_line = ln;
                    open_col = c;
                    found = true;
                    break 'back;
                }
                depth -= 1;
            }
            if c == 0 {
                break;
            }
            c -= 1;
        }
    }
    if !found {
        return None;
    }

    // Forward search for the matching close bracket.
    depth = 0;
    let mut close_line = cursor.0;
    let mut close_col = cursor.1;
    found = false;
    'fwd: for ln in open_line..lines.len() {
        let text = chars_of(lines, ln);
        let start_col = if ln == open_line { open_col + 1 } else { 0 };
        let mut c = start_col;
        while c < text.len() {
            let ch = text[c];
            if ch == open_c {
                depth += 1;
            } else if ch == close_c {
                if depth == 0 {
                    close_line = ln;
                    close_col = c;
                    found = true;
                    break 'fwd;
                }
                depth -= 1;
            }
            c += 1;
        }
    }
    if !found {
        return None;
    }

    if inner {
        if open_line == close_line && close_col.saturating_sub(open_col) <= 1 {
            Some(OpRange {
                start: (open_line, open_col + 1),
                end: (close_line, open_col),
                linewise: false,
                inclusive: true,
            })
        } else {
            Some(OpRange {
                start: (open_line, open_col + 1),
                end: (close_line, close_col - 1),
                linewise: false,
                inclusive: true,
            })
        }
    } else {
        Some(OpRange {
            start: (open_line, open_col),
            end: (close_line, close_col),
            linewise: false,
            inclusive: true,
        })
    }
}

// ── search (the pi-vim `search.ts`) ─────────────────────────────

/// The word under the cursor (the pi-vim `*` / `#` rule): ASCII
/// word chars only.
fn word_under_cursor(lines: &[String], cursor: (usize, usize)) -> Option<String> {
    let line = lines.get(cursor.0)?;
    let chs: Vec<char> = line.chars().collect();
    if cursor.1 >= chs.len() {
        return None;
    }
    if !is_obj_word_char(chs[cursor.1]) {
        return None;
    }
    let mut start = cursor.1;
    while start > 0 && is_obj_word_char(chs[start - 1]) {
        start -= 1;
    }
    let mut end = cursor.1;
    while end + 1 < chs.len() && is_obj_word_char(chs[end + 1]) {
        end += 1;
    }
    Some(chs[start..=end].iter().collect())
}

/// All literal (case-insensitive) match starts of a pattern, per
/// line.
fn find_all_matches(lines: &[String], pattern: &str) -> Vec<(usize, usize)> {
    if pattern.is_empty() {
        return Vec::new();
    }
    let pat = pattern.to_lowercase();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let hay = line.to_lowercase();
        let mut from = 0usize;
        while let Some(rel) = hay[from..].find(&pat) {
            let abs = from + rel;
            out.push((i, abs));
            from = abs + pat.len().max(1);
        }
    }
    out
}

/// The next match after the cursor in the given direction,
/// wrapping the buffer.
fn find_next_match(
    lines: &[String],
    cursor: (usize, usize),
    pattern: &str,
    forward: bool,
) -> Option<(usize, usize)> {
    let matches = find_all_matches(lines, pattern);
    if matches.is_empty() {
        return None;
    }
    if forward {
        for m in &matches {
            if m.0 > cursor.0 || (m.0 == cursor.0 && m.1 > cursor.1) {
                return Some(*m);
            }
        }
        Some(matches[0])
    } else {
        for m in matches.iter().rev() {
            if m.0 < cursor.0 || (m.0 == cursor.0 && m.1 < cursor.1) {
                return Some(*m);
            }
        }
        Some(*matches.last()?)
    }
}

// ── tests ───────────────────────────────────────────────────────

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use super::*;

    /// A new editor with `text` loaded, in insert mode.
    fn ed(text: &str) -> Editor {
        let mut e = Editor::new();
        e.set_text(text);
        e
    }

    /// Press key strings. Single chars become `Key::Char`; the
    /// known names map onto their host keys.
    fn press(e: &mut Editor, keys: &[&str]) {
        for k in keys {
            match *k {
                "esc" => {
                    e.press(Key::Esc);
                }
                "enter" => {
                    e.press(Key::Enter);
                }
                "bs" => {
                    e.press(Key::Backspace);
                }
                "tab" => {
                    e.press(Key::Tab);
                }
                "ctrl-c" => {
                    e.press(Key::CtrlC);
                }
                "ctrl-d" => {
                    e.press(Key::CtrlD);
                }
                "ctrl-u" => {
                    e.press(Key::CtrlU);
                }
                "ctrl-j" => {
                    e.press(Key::CtrlJ);
                }
                "left" => {
                    e.press(Key::Left);
                }
                "right" => {
                    e.press(Key::Right);
                }
                "up" => {
                    e.press(Key::Up);
                }
                "down" => {
                    e.press(Key::Down);
                }
                "home" => {
                    e.press(Key::Home);
                }
                "end" => {
                    e.press(Key::End);
                }
                "delete" => {
                    e.press(Key::Delete);
                }
                s => {
                    // A plain string: one key press per char.
                    // `"a` selects register a; `ab` types two chars
                    // in insert mode.
                    for c in s.chars() {
                        e.press(Key::Char(c));
                    }
                }
            }
        }
    }

    /// Normal-mode edits on `text` starting at `(row, col)`.
    fn norm(text: &str, row: usize, col: usize, keys: &[&str]) -> Editor {
        let mut e = ed(text);
        e.mode = Mode::Normal;
        e.row = row;
        e.col = col;
        press(&mut e, keys);
        e
    }

    // ── bug 1: `w` on the last word ────────────────────────────

    /// The reported bug: `w` on a single-word draft must move onto
    /// the last char of the word, like neovim.
    #[test]
    fn w_on_last_word_of_single_word_draft() {
        let e = norm("hi", 0, 0, &["w"]);
        assert_eq!(e.cursor(), (0, 1), "w lands on the last char");
    }

    #[test]
    fn w_on_last_char_of_last_word_stays_put() {
        let e = norm("hi", 0, 0, &["w", "w"]);
        assert_eq!(e.cursor(), (0, 1));
    }

    #[test]
    fn w_crosses_to_the_next_line() {
        let e = norm("a b\nc d", 0, 2, &["w"]);
        assert_eq!(e.cursor(), (1, 0));
    }

    #[test]
    fn w_skips_blanks_and_lands_on_next_word() {
        let e = norm("a    b", 0, 0, &["w"]);
        assert_eq!(e.cursor(), (0, 5));
    }

    #[test]
    fn w_on_word_followed_by_blank_lands_on_last_char() {
        // "word " — the reference clamps to the last line column,
        // which is the trailing blank (col 4).
        let e = norm("word ", 0, 0, &["w"]);
        assert_eq!(e.cursor(), (0, 4));
    }

    #[test]
    fn e_lands_on_word_end() {
        let e = norm("this", 0, 0, &["e"]);
        assert_eq!(e.cursor(), (0, 3));
    }

    #[test]
    fn w_and_b_round_trip() {
        let mut e = norm("a b c", 0, 0, &["w", "w"]);
        assert_eq!(e.cursor(), (0, 4));
        e.press(Key::Char('b'));
        assert_eq!(e.cursor(), (0, 2));
        e.press(Key::Char('b'));
        assert_eq!(e.cursor(), (0, 0));
    }

    #[test]
    fn W_is_blank_delimited() {
        // `W` skips the whole non-blank run `a.b` to the `c`.
        let e = norm("a.b c", 0, 0, &["W"]);
        assert_eq!(e.cursor(), (0, 4));
        // `w` skips the word run only: it lands on the punct.
        let e = norm("a.b c", 0, 0, &["w"]);
        assert_eq!(e.cursor(), (0, 1));
    }

    #[test]
    fn B_backward_blank_delimited() {
        // `B` lands on the start of the previous blank-delimited
        // WORD: from the end of `c` it reaches `c`, then `b`,
        // then `a`.
        let e = norm("a b c", 0, 6, &["B", "B", "B"]);
        assert_eq!(e.cursor(), (0, 0));
        let e = norm("a b c", 0, 6, &["B", "B"]);
        assert_eq!(e.cursor(), (0, 2));
    }

    // ── bug 3: `dw` then `p` ───────────────────────────────────

    /// The reported bug: `dw` on `b` of `a b c d` deletes the word
    /// (plus the following blank); `p` pastes it back inline. No
    /// line is opened by the char-wise paste.
    #[test]
    fn dw_then_p_pastes_inline_after_cursor() {
        // The reference `p` inserts after the char under the
        // cursor: `dw` leaves the cursor on `c`; the register `b
        // ` lands between `c` and the rest, inline. No new line.
        let e = norm("a b c d", 0, 2, &["d", "w", "p"]);
        assert_eq!(e.text(), "a cb  d");
    }

    #[test]
    fn dw_then_double_p_stays_inline() {
        // Repeated `p` pastes another copy after the cursor
        // char, inline. It never opens a new line.
        let e = norm("a b c d", 0, 2, &["d", "w", "p", "p"]);
        assert_eq!(e.text(), "a cb b  d");
    }

    #[test]
    fn dw_registers_word_with_trailing_blank() {
        let e = norm("a b c d", 0, 2, &["d", "w"]);
        assert_eq!(e.text(), "a c d");
        assert_eq!(e.cursor(), (0, 2), "cursor stays on `c`");
        assert_eq!(e.registers.get(&'"').unwrap().text, "b ");
    }

    #[test]
    fn dd_then_p_pastes_line_below() {
        // `dd` on the last line deletes it; `p` pastes the line
        // back below.
        let e = norm("one\ntwo", 1, 0, &["d", "d", "p"]);
        assert_eq!(e.text(), "one\ntwo");
        assert_eq!(e.cursor(), (1, 0));
    }

    #[test]
    fn yy_then_p_pastes_line_below_and_P_pastes_above() {
        let mut e = norm("one\ntwo", 1, 0, &["y", "y", "p"]);
        assert_eq!(e.text(), "one\ntwo\ntwo");
        assert_eq!(e.cursor(), (2, 0));
        e.press(Key::Char('P'));
        assert_eq!(e.text(), "one\ntwo\ntwo\ntwo");
        assert_eq!(e.cursor(), (2, 0), "P keeps the cursor row index");
    }

    #[test]
    fn dd_on_last_line_keeps_one_line() {
        let e = norm("x", 0, 0, &["d", "d"]);
        assert_eq!(e.text(), "");
        assert_eq!(e.n_lines(), 1);
    }

    // ── operators and motions ──────────────────────────────────

    #[test]
    fn D_deletes_to_line_end() {
        let e = norm("abc def", 0, 3, &["D"]);
        assert_eq!(e.text(), "abc");
        let e = norm("abc def", 0, 4, &["D"]);
        assert_eq!(e.text(), "abc ");
    }

    #[test]
    fn c_w_changes_word_and_enters_insert() {
        let mut e = norm("foo bar", 0, 0, &["c", "w"]);
        assert_eq!(e.text(), " bar");
        assert_eq!(e.mode(), Mode::Insert);
        press(&mut e, &["baz", "esc"]);
        assert_eq!(e.text(), "baz bar");
        assert_eq!(e.mode(), Mode::Normal);
    }

    #[test]
    fn x_deletes_under_cursor() {
        let e = norm("abcd", 0, 1, &["x"]);
        assert_eq!(e.text(), "acd");
    }

    #[test]
    fn r_replaces_char_under_cursor() {
        let e = norm("abcd", 0, 1, &["r", "Z"]);
        assert_eq!(e.text(), "aZcd");
    }

    #[test]
    fn dl_deletes_two_chars() {
        let e = norm("abcd", 0, 0, &["d", "l"]);
        assert_eq!(e.text(), "cd");
    }

    #[test]
    fn dw_with_count_deletes_two_words() {
        let e = norm("a b c d", 0, 0, &["2", "d", "w"]);
        assert_eq!(e.text(), "c d");
    }

    #[test]
    fn gg_and_G_move_lines() {
        let mut e = norm("l1\nl2\nl3\nl4", 0, 5, &["g", "g"]);
        assert_eq!(e.cursor(), (0, 0));
        press(&mut e, &["G"]);
        assert_eq!(e.cursor(), (3, 0));
        press(&mut e, &["2", "g", "g"]);
        assert_eq!(e.cursor(), (1, 0));
    }

    #[test]
    fn h_l_k_j_moves() {
        // `l` stops on the last char of the line (col 3).
        let mut e = norm("aaaa\nbbbb", 0, 2, &["j", "l", "l"]);
        assert_eq!(e.cursor(), (1, 3));
        press(&mut e, &["k", "h"]);
        assert_eq!(e.cursor(), (0, 2));
    }

    #[test]
    fn h_at_col_zero_stays() {
        let e = norm("aaaa\nbbbb", 0, 0, &["h"]);
        assert_eq!(e.cursor(), (0, 0));
    }

    #[test]
    fn count_prefix_moves() {
        let e = norm("aaaaaaaaaa", 0, 0, &["5", "l"]);
        assert_eq!(e.cursor(), (0, 5));
    }

    #[test]
    fn zero_and_dollar() {
        let mut e = norm("  abc", 0, 3, &["0"]);
        assert_eq!(e.cursor(), (0, 0));
        press(&mut e, &["$"]);
        assert_eq!(e.cursor(), (0, 4), "`$` rests on the last char");
    }

    #[test]
    fn caret_goes_to_first_nonblank() {
        let e = norm("  abc", 0, 0, &["^"]);
        assert_eq!(e.cursor(), (0, 2));
    }

    #[test]
    fn f_and_t_and_repeat() {
        let mut e = norm("abcde", 0, 0, &["f", "d"]);
        assert_eq!(e.cursor(), (0, 3));
        press(&mut e, &[";"]);
        assert_eq!(e.cursor(), (0, 3), "no second `d`: stays put");
        press(&mut e, &["t", "b"]);
        assert_eq!(e.cursor(), (0, 3), "no `b` ahead of col 3: stays put");
        press(&mut e, &[","]);
        assert_eq!(e.cursor(), (0, 2), "`T` lands one after the `b`");
    }

    #[test]
    fn matching_bracket_jumps() {
        // `%` lands on the matching bracket itself.
        let e = norm("f(x)", 0, 1, &["%"]);
        assert_eq!(e.cursor(), (0, 3));
        let e = norm("f(x)", 0, 3, &["%"]);
        assert_eq!(e.cursor(), (0, 1));
    }

    // ── insert / replace modes ─────────────────────────────────

    #[test]
    fn i_inserts_at_cursor() {
        let mut e = norm("ab", 0, 1, &["i"]);
        press(&mut e, &["X", "esc"]);
        assert_eq!(e.text(), "aXb");
        assert_eq!(e.mode(), Mode::Normal);
    }

    #[test]
    fn a_appends_after_cursor() {
        let mut e = norm("ab", 0, 0, &["a"]);
        press(&mut e, &["X", "esc"]);
        assert_eq!(e.text(), "aXb");
    }

    #[test]
    fn I_inserts_at_first_nonblank() {
        // The first non-blank of "  ab" is col 2.
        let mut e = norm("  ab", 0, 2, &["I"]);
        assert_eq!(e.cursor(), (0, 2));
        press(&mut e, &["X", "esc"]);
        assert_eq!(e.text(), "  Xab");
    }

    #[test]
    fn A_appends_at_line_end() {
        let mut e = norm("ab", 0, 0, &["A"]);
        assert_eq!(e.cursor(), (0, 2));
        press(&mut e, &["X", "esc"]);
        assert_eq!(e.text(), "abX");
    }

    #[test]
    fn o_opens_line_below() {
        let mut e = norm("ab\ncd", 0, 0, &["o"]);
        assert_eq!(e.mode(), Mode::Insert);
        press(&mut e, &["x", "esc"]);
        assert_eq!(e.text(), "ab\nx\ncd");
    }

    #[test]
    fn O_opens_line_above_with_indent() {
        let mut e = norm("ab\n  cd", 1, 0, &["O"]);
        press(&mut e, &["x", "esc"]);
        assert_eq!(e.text(), "ab\n  x\n  cd");
    }

    #[test]
    fn R_overtypes_then_esc() {
        let mut e = norm("abcd", 0, 1, &["R"]);
        assert_eq!(e.mode(), Mode::Replace);
        press(&mut e, &["Z"]);
        assert_eq!(e.text(), "aZcd");
        press(&mut e, &["esc"]);
        assert_eq!(e.mode(), Mode::Normal);
    }

    #[test]
    fn s_deletes_char_and_inserts() {
        let mut e = norm("abcd", 0, 1, &["s"]);
        assert_eq!(e.mode(), Mode::Insert);
        assert_eq!(e.text(), "acd");
        press(&mut e, &["Z", "esc"]);
        assert_eq!(e.text(), "aZcd");
    }

    #[test]
    fn S_replaces_whole_line() {
        let mut e = norm("hello\nworld", 0, 0, &["S"]);
        press(&mut e, &["bye", "esc"]);
        assert_eq!(e.text(), "bye\nworld");
    }

    // ── dot repeat, undo, redo ──────────────────────────────────

    #[test]
    fn dot_repeats_insert_change() {
        // `i` inserts before the cursor: `ab` lands before `x`.
        // `.` replays the insert at the (stepped-back) cursor.
        let mut e = norm("x", 0, 0, &["i"]);
        press(&mut e, &["ab", "esc"]);
        assert_eq!(e.text(), "abx");
        press(&mut e, &["."]);
        assert_eq!(e.text(), "aabbx");
    }

    #[test]
    fn dot_replays_the_recorded_keys() {
        // `.` replays the recorded key sequence once. The pinned
        // reference ignores a bare `N.` override in the replay
        // loop, so `2.` after `dw` deletes one word. The replayed
        // `dw` now consumes the whole final line (the neovim
        // end-of-line rule).
        let mut e = norm("abc def", 0, 0, &["d", "w"]);
        assert_eq!(e.text(), "def");
        press(&mut e, &["2", "."]);
        assert_eq!(e.text(), "");
    }

    #[test]
    fn u_undoes() {
        let mut e = norm("abc", 0, 0, &["i"]);
        press(&mut e, &["X", "esc"]);
        assert_eq!(e.text(), "Xabc");
        press(&mut e, &["u"]);
        assert_eq!(e.text(), "abc");
    }

    #[test]
    fn redo_restores_after_undo() {
        let mut e = norm("abc", 0, 0, &["i"]);
        press(&mut e, &["X", "esc", "u"]);
        assert_eq!(e.text(), "abc");
        assert!(e.has_redo());
        e.redo();
        assert_eq!(e.text(), "Xabc");
        assert!(!e.has_redo());
    }

    // ── search ──────────────────────────────────────────────────

    #[test]
    fn slash_search_jumps_and_n_repeats() {
        let mut e = norm("baz bar baz", 0, 0, &["/", "b", "a", "z", "enter"]);
        assert_eq!(e.mode(), Mode::Normal);
        assert_eq!(
            e.cursor(),
            (0, 8),
            "the search runs on Enter, after the caret"
        );
        press(&mut e, &["n"]);
        assert_eq!(e.cursor(), (0, 0), "n wraps the buffer");
    }

    #[test]
    fn slash_search_esc_cancel_keeps_text() {
        let mut e = norm("abc", 0, 0, &["/", "z"]);
        assert_eq!(e.command_line_label().as_deref(), Some("/z\u{2588}"));
        press(&mut e, &["esc"]);
        assert_eq!(e.mode(), Mode::Normal);
        assert!(e.command_line_label().is_none());
        assert_eq!(e.text(), "abc");
    }

    #[test]
    fn star_searches_word_under_cursor() {
        // The word under the cursor is `abc`; `*` jumps to the
        // next occurrence after the cursor.
        let e = norm("abc abc", 0, 0, &["*"]);
        assert_eq!(e.cursor(), (0, 4));
    }

    #[test]
    fn question_searches_backward() {
        // `?` searches backward to the closest match before the
        // cursor: the second `abc`.
        let e = norm("abc abc", 0, 6, &["?", "a", "b", "c", "enter"]);
        assert_eq!(e.cursor(), (0, 4));
    }

    // ── visual mode ─────────────────────────────────────────────

    #[test]
    fn visual_delete_selection() {
        // `v` anchors at col 1; two `l` reach col 3; the visual
        // range is inclusive: `bcd` is removed.
        let e = norm("abcdef", 0, 1, &["v", "l", "l", "d"]);
        assert_eq!(e.text(), "aef");
    }

    #[test]
    fn visual_yank_then_paste() {
        // The selection `bc` is yanked; after the operator the
        // cursor returns to the anchor (col 1); `p` inserts the
        // copy after that char, inline.
        let e = norm("abcdef", 0, 1, &["v", "l", "y", "p"]);
        assert_eq!(e.text(), "abbccdef");
    }

    #[test]
    fn visual_change_selection() {
        let mut e = norm("abcdef", 0, 1, &["v", "l", "c"]);
        assert_eq!(e.mode(), Mode::Insert);
        press(&mut e, &["X", "esc"]);
        assert_eq!(e.text(), "aXdef");
    }

    #[test]
    fn visual_line_delete_then_paste() {
        // Lines 1-2 (`two`, `three`) are deleted; `p` pastes them
        // back below the cursor line (the reference keeps the
        // cursor on the same row index).
        let e = norm("one\ntwo\nthree", 1, 0, &["V", "j", "d", "p"]);
        assert_eq!(e.text(), "one\ntwo\nthree");
        assert_eq!(e.cursor(), (1, 0));
    }

    #[test]
    fn visual_toggle_case() {
        // `v` then one `l` selects `ab`.
        let e = norm("abc def", 0, 0, &["v", "l", "~"]);
        assert_eq!(e.text(), "ABc def");
    }

    #[test]
    fn visual_join() {
        let e = norm("one\ntwo", 0, 0, &["V", "j", "J"]);
        assert_eq!(e.text(), "one two");
    }

    // ── line commands ───────────────────────────────────────────

    #[test]
    fn J_joins_next_line() {
        let e = norm("one\ntwo", 0, 0, &["J"]);
        assert_eq!(e.text(), "one two");
    }

    #[test]
    fn J_on_last_line_is_noop() {
        let e = norm("one\n", 0, 0, &["J"]);
        assert_eq!(e.text(), "one");
    }

    #[test]
    fn indent_and_dedent() {
        let mut e = norm("  a\nb", 0, 0, &["V", "j", ">"]);
        assert_eq!(e.text(), "    a\n  b");
        press(&mut e, &["V", "j", "<"]);
        assert_eq!(e.text(), "  a\nb");
    }

    // ── registers ───────────────────────────────────────────────

    #[test]
    fn named_register_yank_and_paste() {
        // `yw` on the last word consumes to the end of line (the
        // neovim rule), so the yank covers `cd`. The paste lands
        // after the cursor char, inline.
        let e = norm("ab cd", 0, 3, &["\"a", "y", "w", "\"a", "p"]);
        assert_eq!(e.text(), "ab ccdd");
        assert_eq!(e.registers.get(&'a').unwrap().text, "cd");
    }

    #[test]
    fn append_to_named_register() {
        // The reference yank keeps the cursor on the range start,
        // so the second `yw` yanks the same `aa ` word. Char-wise
        // append joins without a separator.
        let mut e = norm("aa bb", 0, 0, &["\"A", "y", "w", "\"A", "y", "w"]);
        assert_eq!(e.registers.get(&'a').unwrap().text, "aa aa ");
        press(&mut e, &["\"a", "p"]);
        assert_eq!(e.text(), "aaa aa a bb");
    }

    #[test]
    fn black_hole_register_discards() {
        // `"_dw` deletes the word without touching any register;
        // a following `p` pastes nothing. `dw` on the two-char
        // line consumes to the end of line.
        let mut e = norm("ab", 0, 0, &["\"_", "d", "w"]);
        assert_eq!(e.text(), "");
        press(&mut e, &["p"]);
        assert_eq!(e.text(), "");
    }

    // ── host-key mappings in normal mode ───────────────────────

    #[test]
    fn arrows_map_to_vim_motions() {
        // `down` clamps the column to the shorter second line.
        let mut e = norm("ab\ncd", 0, 0, &["right", "right", "down"]);
        assert_eq!(e.cursor(), (1, 1));
        press(&mut e, &["up", "left"]);
        assert_eq!(e.cursor(), (0, 0));
    }

    #[test]
    fn backspace_moves_up_in_normal_mode() {
        let e = norm("ab\ncd", 1, 0, &["bs"]);
        assert_eq!(e.cursor(), (0, 0));
    }

    // ── the reported cursor duplication is a render concern ───

    #[test]
    fn insert_mode_ctrl_c_leaves_text_untouched() {
        let mut e = ed("abc");
        assert_eq!(e.mode(), Mode::Insert);
        press(&mut e, &["ctrl-c"]);
        assert_eq!(e.mode(), Mode::Normal);
        assert_eq!(e.text(), "abc");
    }

    #[test]
    fn esc_from_insert_is_a_noop_on_text() {
        let mut e = ed("abc");
        press(&mut e, &["esc"]);
        assert_eq!(e.mode(), Mode::Normal);
        assert_eq!(e.text(), "abc");
    }

    #[test]
    fn dw_on_last_word_deletes_to_end_of_line() {
        // Neovim rule: `dw` on the final word of a line (no
        // trailing blank) consumes to the end of the line.
        let e = norm("word", 0, 0, &["d", "w"]);
        assert_eq!(e.text(), "");
    }

    #[test]
    fn dw_from_last_char_deletes_that_char() {
        // A no-op `w` at the last char: the operator deletes the
        // single char under the cursor.
        let e = norm("word", 0, 3, &["d", "w"]);
        assert_eq!(e.text(), "wor");
    }

    #[test]
    fn dw_with_trailing_blank_includes_the_blank() {
        // Cursor on the word start: the whole word and the
        // trailing blank die. The blank before the word stays.
        let e = norm("a word ", 0, 2, &["d", "w"]);
        assert_eq!(e.text(), "a ");
    }

    #[test]
    fn dw_stops_before_a_real_next_word() {
        // The last char of the line starts a real word: the range
        // stays exclusive.
        let e = norm("a b c", 0, 2, &["d", "w"]);
        assert_eq!(e.text(), "a c");
    }

    #[test]
    fn dw_on_a_single_char_line() {
        let e = norm("x", 0, 0, &["d", "w"]);
        assert_eq!(e.text(), "");
    }

    #[test]
    fn dw_cross_line_keeps_the_compatibility_rule() {
        let e = norm("foo\n  bar baz", 0, 0, &["d", "w"]);
        assert_eq!(e.text(), "\n  bar baz");
    }

    #[test]
    fn counted_dw_on_last_word() {
        let e = norm("word", 0, 0, &["2", "d", "w"]);
        assert_eq!(e.text(), "");
    }

    #[test]
    fn dw_from_last_char_deletes_only_that_char() {
        // `w` does not move at the last char, so the operator
        // deletes the single char under the cursor. The trailing
        // blank before it survives.
        let e = norm("a b c d e", 0, 8, &["d", "w"]); // cursor on 'e'
        assert_eq!(e.text(), "a b c d ");
    }

    #[test]
    fn yw_on_last_word_yanks_to_end_of_line() {
        let e = norm("word", 0, 0, &["y", "w", "P"]);
        assert_eq!(e.text(), "wordword");
    }

    #[test]
    fn cw_on_last_word_changes_to_end_of_line() {
        let e = norm("word", 0, 0, &["c", "w", "x", "esc"]);
        assert_eq!(e.text(), "x");
    }

    #[test]
    fn dW_on_first_WORD_keeps_the_rest() {
        // `W` lands on the start of a real word: the range stays
        // exclusive, only the first WORD plus the blank dies.
        let e = norm("foo bar-baz", 0, 0, &["d", "W"]);
        assert_eq!(e.text(), "bar-baz");
    }

    #[test]
    fn dW_on_last_WORD_consumes_the_line() {
        // `dW` from the last char deletes the char to the end of
        // line; the blank before it survives.
        let e = norm("aa bb cc dd", 0, 9, &["d", "W"]); // cursor on 'd'
        assert_eq!(e.text(), "aa bb cc ");
    }

    #[test]
    fn esc_in_normal_cancels_a_stale_count() {
        // `3` opens a count. `Esc` cancels it. The later `dw`
        // deletes one word, not three.
        let e = norm("a b c d e f", 0, 0, &["3", "esc", "d", "w"]);
        assert_eq!(e.text(), "b c d e f");
    }

    #[test]
    fn stale_count_cannot_make_db_eat_a_line() {
        // The reported case: Esc into normal, a stray digit, then
        // `db`. One word dies, the line survives.
        let e = norm("one two three", 0, 8, &["3", "esc", "d", "b"]);
        assert_eq!(e.text(), "one three");
    }

    #[test]
    fn digit_count_still_works_when_not_cancelled() {
        // Without the Esc in between, the count applies: `3dw`
        // deletes three words.
        let e = norm("a b c d e f", 0, 0, &["3", "d", "w"]);
        assert_eq!(e.text(), "d e f");
    }

    #[test]
    fn g_prefix_esc_cancels_the_pending_g() {
        // `g`, Esc, `g` must not run `gg`.
        let e = norm("l1\nl2\nl3", 2, 0, &["g", "esc", "g"]);
        assert_eq!(e.cursor(), (2, 0));
    }

    #[test]
    fn shift_a_types_uppercase_a_in_insert_mode() {
        // Shift+a types `A` at the caret, like the reference base
        // editor (the old host extension jumped the caret instead).
        let mut e = ed("hello\nworld");
        e.row = 0;
        e.col = 2;
        e.press(Key::Char('A'));
        assert_eq!(e.mode(), Mode::Insert);
        assert_eq!(e.text(), "heAllo\nworld");
    }

    #[test]
    fn shift_a_at_line_end_appends_the_char() {
        let mut e = ed("ab");
        e.col = 2;
        e.press(Key::Char('A'));
        assert_eq!(e.text(), "abA");
        assert_eq!(e.cursor(), (0, 3));
    }

    #[test]
    fn shift_a_types_uppercase_a_in_replace_mode() {
        // Shift+a types `A` at the caret, like the reference base
        // editor (the old host extension jumped the caret instead).
        let mut e = norm("abc", 0, 0, &["R"]);
        assert_eq!(e.mode(), Mode::Replace);
        press(&mut e, &["A"]);
        assert_eq!(e.cursor(), (0, 1), "the overtype steps one char right");
        press(&mut e, &["esc"]);
        assert_eq!(e.text(), "Abc");
    }

    #[test]
    fn clear_current_line_kills_the_line() {
        // The host maps Ctrl-U to this in insert mode.
        let mut e = ed("one\ntwo");
        e.row = 1;
        e.col = 1;
        e.clear_current_line();
        assert_eq!(e.text(), "one\n");
        assert_eq!(e.cursor(), (1, 0));
    }

    // ── command line ────────────────────────────────────────────

    #[test]
    fn command_line_label_shows_prompt() {
        let mut e = norm("abc abc", 0, 0, &["/"]);
        assert_eq!(e.mode(), Mode::CommandLine);
        press(&mut e, &["a", "b"]);
        assert_eq!(e.command_line_label().as_deref(), Some("/ab\u{2588}"));
        press(&mut e, &["bs"]);
        assert_eq!(e.command_line_label().as_deref(), Some("/a\u{2588}"));
    }

    #[test]
    fn command_line_enter_runs_search() {
        let mut e = norm("abc abc", 0, 0, &["/", "a", "b", "c"]);
        press(&mut e, &["enter"]);
        assert_eq!(e.mode(), Mode::Normal);
        assert_eq!(
            e.cursor(),
            (0, 4),
            "forward search skips the match under the caret"
        );
        press(&mut e, &["n"]);
        assert_eq!(e.cursor(), (0, 0), "n wraps the buffer");
    }

    // ── the pi-vim compat suite (test/vim-compat.test.ts) ─────

    #[test]
    fn compat_w_and_W_skip_indentation_after_crossing_a_line() {
        // From the reference compat suite: `w` and `W` both skip
        // the indentation of the next line.
        let e = norm("foo\n  bar", 0, 0, &["w"]);
        assert_eq!(e.cursor(), (1, 2));
        let e = norm("foo\n  bar", 0, 0, &["W"]);
        assert_eq!(e.cursor(), (1, 2));
    }

    #[test]
    fn compat_word_movement_treats_unicode_letters_as_a_word() {
        let e = norm("caf\u{e9} next", 0, 0, &["w"]);
        assert_eq!(e.cursor(), (0, 5));
    }

    #[test]
    fn compat_cw_behaves_as_ce_and_leaves_following_whitespace() {
        let mut e = norm("foo bar", 0, 0, &["c", "w"]);
        press(&mut e, &["X", "esc"]);
        assert_eq!(e.text(), "X bar");
    }

    #[test]
    fn compat_single_dw_at_end_of_line_preserves_newline() {
        let e = norm("foo\n  bar baz", 0, 0, &["d", "w"]);
        assert_eq!(e.text(), "\n  bar baz");
    }

    #[test]
    fn compat_operator_and_motion_counts_multiply() {
        let e = norm(
            "one two three four five six seven",
            0,
            0,
            &["2", "d", "3", "w"],
        );
        assert_eq!(e.text(), "seven");
    }

    #[test]
    fn compat_O_opens_an_auto_indented_line_without_replacing_the_buffer() {
        let mut e = norm("  foo\nbar", 0, 1, &["O"]);
        press(&mut e, &["X", "esc"]);
        assert_eq!(e.text(), "  X\n  foo\nbar");
        assert_eq!(e.cursor(), (0, 2));
    }

    #[test]
    fn compat_counted_O_repeats_the_inserted_line_like_vim() {
        let mut e = norm("  foo\nbar", 0, 0, &["3", "O"]);
        press(&mut e, &["X", "esc"]);
        assert_eq!(e.text(), "  X\n  X\n  X\n  foo\nbar");
        assert_eq!(e.cursor(), (2, 2));
    }

    #[test]
    fn command_line_ctrl_u_clears_input() {
        let mut e = norm("abc", 0, 0, &["/", "z", "z"]);
        assert_eq!(e.command_line_label().as_deref(), Some("/zz\u{2588}"));
        press(&mut e, &["ctrl-u"]);
        assert_eq!(e.command_line_label().as_deref(), Some("/\u{2588}"));
        press(&mut e, &["esc"]);
        assert_eq!(e.mode(), Mode::Normal);
    }

    // ── line wrapping in the input area ────────────

    /// The rendered text of each display row (for assertions).
    fn row_texts(rows: &[EditorRow]) -> Vec<&str> {
        rows.iter().map(|r| r.text.as_str()).collect()
    }

    #[test]
    fn wrap_row_hard_breaks_at_the_width() {
        assert_eq!(wrap_row("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap_row("abc", 4), vec!["abc"]);
    }

    #[test]
    fn wrap_row_never_overflows_the_width() {
        // No fragment is ever longer than the box, even for a long
        // run of spaces or a long word: the line is sliced every
        // `width` characters.
        for piece in wrap_row("alpha beta gamma", 8) {
            assert!(piece.chars().count() <= 8, "overflow `{piece}`");
        }
        assert_eq!(wrap_row(&"x".repeat(9), 8), vec!["xxxxxxxx", "x"]);
        // Wrapping is lossless: joining the rows reproduces the line.
        let joined: String = wrap_row("the quick brown fox jumps over the lazy dog", 12).join("");
        assert_eq!(joined, "the quick brown fox jumps over the lazy dog");
    }

    #[test]
    fn wrap_row_keeps_empty_lines_empty() {
        assert_eq!(wrap_row("", 80), vec![""]);
    }

    #[test]
    fn display_row_counts_wrapped_lines() {
        let mut e = Editor::new();
        e.set_text("hello world\nfoo");
        // "hello world" is 11 chars: two rows at 10 columns, one at 80.
        assert_eq!(e.display_row_count(10), 3); // 2 + 1
        assert_eq!(e.display_row_count(80), 2); // 1 + 1
    }

    #[test]
    fn display_rows_wrap_and_window_from_the_scroll_row() {
        let mut e = Editor::new();
        e.set_text("one two three four five\nx");
        // At width 8 "one two three four five" (22 chars) slices into
        // three rows; "x" is one more.
        assert_eq!(e.display_row_count(8), 4);
        assert_eq!(
            row_texts(&e.display_rows(0, 4, 8)),
            vec!["one two ", "three fo", "ur five", "x"]
        );
        // Scrolled two display rows down, a two-row window shows the tail.
        assert_eq!(row_texts(&e.display_rows(2, 2, 8)), vec!["ur five", "x"]);
    }

    #[test]
    fn cursor_display_maps_the_caret_to_the_wrapped_row() {
        // "aaaa bbbb cccc" is 14 chars; it slices at 8 into
        // "aaaa bbb" (0..=7) / "b cccc" (8..=13). "dddd" is a third row.
        let e = norm("aaaa bbbb cccc\ndddd", 0, 0, &[]);
        assert_eq!(e.display_row_count(8), 3); // 2 + 1

        // Document col 4 is the 'b' of the 2nd "aaaa" run; it sits on the
        // first display row (chars 0..=7), at display column 4.
        let at = norm("aaaa bbbb cccc\ndddd", 0, 4, &[]);
        assert_eq!(at.cursor(), (0, 4));
        assert_eq!(at.cursor_display(8), (0, 4));

        // Col 8 begins the 2nd display row; the caret sits at the start
        // of that row, display column 0.
        let at = norm("aaaa bbbb cccc\ndddd", 0, 8, &[]);
        assert_eq!(at.cursor_display(8), (1, 0));

        // Col 11 lands mid-second-row at display column 3.
        let at = norm("aaaa bbbb cccc\ndddd", 0, 11, &[]);
        assert_eq!(at.cursor_display(8), (1, 3));

        // End-of-line (col 14, one past the last char, 14 total) sits just
        // past the 2nd row's text: display column 6.
        let at = norm("aaaa bbbb cccc\ndddd", 0, 14, &[]);
        assert_eq!(at.cursor(), (0, 14));
        assert_eq!(at.cursor_display(8), (1, 6));
    }

    // ── `@` token detection (docs/tui-file-picker.md section 5) ────

    /// Insert-mode editor with the cursor at the end of the line.
    fn ed_at_end(text: &str) -> Editor {
        let mut e = ed(text);
        e.col = e.lines[e.row].len();
        e
    }

    #[test]
    fn at_token_preceded_by_only_whitespace_is_detected() {
        // "   @src/m" — the `@` at col 3 is preceded only by spaces,
        // so it is a valid trigger. The query is the text up to the
        // caret.
        let e = ed_at_end("   @src/m");
        assert_eq!(e.at_token_info(), Some((3, "src/m".into())));
    }

    #[test]
    fn at_token_at_line_start_is_detected() {
        let e = ed_at_end("@file.txt");
        assert_eq!(e.at_token_info(), Some((0, "file.txt".into())));
    }

    #[test]
    fn at_token_inside_a_word_is_not_a_token() {
        // The `@` at col 3 follows a word char, so it is not at a
        // word boundary: no token.
        let e = ed_at_end("foo@bar");
        assert_eq!(e.at_token_info(), None);
    }

    #[test]
    fn at_token_after_word_chars_is_not_detected() {
        // "hello@src/m" — the `@` at col 5 is glued to "hello" with no
        // separator, so it is part of a larger token: no trigger.
        let e = ed_at_end("hello@src/m");
        assert_eq!(e.at_token_info(), None);
    }

    #[test]
    fn at_token_needs_insert_mode() {
        let mut e = ed_at_end("@src/main.rs");
        e.mode = Mode::Normal;
        assert_eq!(e.at_token_info(), None);
    }

    #[test]
    fn at_token_query_is_the_text_between_at_and_caret() {
        // The caret mid-token: the query stops at the caret, not the
        // end of the line. "  @src/xyz tail" with caret at col 6:
        // the @ is at col 2, the caret sits just past "src".
        let mut e = ed("  @src/xyz tail");
        e.col = 6;
        assert_eq!(e.at_token_info(), Some((2, "src".into())));
    }

    #[test]
    fn at_token_finds_last_at_before_caret() {
        // Two `@` tokens on the line: the nearest one to the caret wins.
        // Both are valid: the first is at start-of-line, the second is
        // preceded by a space.
        let mut e = ed("@a @b");
        e.col = 5; // caret past the second token
        assert_eq!(e.at_token_info(), Some((3, "b".into())));
        // The caret just past the first token: the first token is at
        // col 0 (start of line) → valid.
        e.col = 2;
        assert_eq!(e.at_token_info(), Some((0, "a".into())));
    }

    #[test]
    fn at_token_glued_to_word_is_not_detected() {
        // "foo@b" — the `@` at col 3 is glued to "foo" (no whitespace
        // before it), so it is part of a larger token: not a trigger.
        let e = ed_at_end("foo@b");
        assert_eq!(e.at_token_info(), None);
    }

    #[test]
    fn replace_at_token_swaps_in_the_value() {
        let mut e = ed_at_end("hello @src/m");
        e.replace_at_token(6, "docs/guide.md");
        assert_eq!(e.lines[e.row], "hello docs/guide.md");
        assert_eq!(e.col, 6 + "docs/guide.md".len(), "caret past the path");
    }

    #[test]
    fn replace_at_token_keeps_text_after_caret() {
        let mut e = ed("@a and more");
        e.col = 2; // caret just past the token "a"
        e.replace_at_token(0, "src/main.rs");
        assert_eq!(e.lines[0], "src/main.rs and more");
        assert_eq!(e.col, "src/main.rs".len());
    }

    #[test]
    fn replace_at_token_pushes_undo() {
        let mut e = ed_at_end("@x");
        let before = e.undo_stack.len();
        e.replace_at_token(0, "path/y.md");
        assert_eq!(e.undo_stack.len(), before + 1, "the swap is undoable");
    }
}
