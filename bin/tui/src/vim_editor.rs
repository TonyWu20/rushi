//! The multi-line message editor with native vim modal input.
//!
//! The input area is a textarea (the draft is a `Vec` of lines) plus a
//! modal key state machine, following the `pi-vim` extension of
//! pi-config (`vim-modal.ts`: the mode labels, operator-pending as
//! "normal mode plus a pending operator", and the motion and
//! operator set):
//!
//! - **normal**: `h j k l`, `w b e`, `0 $`, `gg G`, `x X` (the
//!   `X` takes a count), `d c y` + motion (`dd cc`, `3dd`),
//!   `D C` (to end of line), `yy` (line yank), `p P` (with a
//!   count), `i a I A o O`, `r` (via insert: `r` is typed as-is
//!   through the `a` form; `R` is the overwrite mode), `v V`,
//!   `Esc` cancels a pending operator
//! - **insert**: chars append, `Ctrl-J` (normalized to `Enter` by
//!   the host) inserts a hard newline (multi-line input), `Backspace`
//!   joins lines at column 0, `Esc` returns to normal
//! - **replace**: entered with `R` in normal mode: each typed char
//!   overwrites the character under the cursor (the last character
//!   of a line is overwritten, not appended); `Esc` returns to
//!   normal
//! - **visual / visual-line**: `v` char-wise, `V` line-wise; the
//!   mark is the other end of the selection, motions move the
//!   cursor against the mark; `d x c y p P` act on the selection;
//!   `Esc` or `v` leaves visual
//!
//! Counts prefix operators and motions: `3dd`, `2w`, `3c`.
//! The yank buffer is a single slot; a delete sets it too, like
//! vim.
//!
//! The editor owns no key mapping and no I/O: `press` takes an
//! already-normalized [`crate::app::Key`] plus a hint callback, so
//! the state machine stays testable in isolation.

use crate::app::Key;

/// The modal states. `operator-pending` is normal mode with a
/// pending operator (the pi-vim `vimState.mode` convention). The
/// idle state is insert: the composer starts in typing mode, and
/// `Esc` drops to normal for motions (the pi-vim default, where
/// `pi-vim` starts in insert mode).
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
        }
    }
}

/// A pending operator or motion awaiting its second key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pending {
    /// `d`, `c`, or `y` awaiting a motion.
    Operator(char),
    /// `r` awaiting the replacement character.
    Replace,
}

/// The multi-line textarea plus the modal state. The invariant is
/// at least one line; `new` holds it.
#[derive(Debug, Clone)]
pub struct Editor {
    pub lines: Vec<String>,
    /// The cursor line index (0-based).
    pub row: usize,
    /// The cursor column, in characters, within `lines[row]`.
    pub col: usize,
    pub mode: Mode,
    /// Set while an operator or a counted motion waits for its key.
    pub pending: Option<Pending>,
    /// The visual-mode mark (the other end of the selection).
    mark: Option<(usize, usize)>,
    /// The yank buffer; deletes set it too, like vim.
    yank: Option<Vec<String>>,
    /// The pending count for an operator or motion.
    count: u32,
    count_seen: bool,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

impl Editor {
    /// A fresh editor: one empty line, idle in insert mode (the
    /// composer's typing mode; `Esc` drops to normal).
    pub fn new() -> Self {
        Self {
            lines: vec![String::new()],
            row: 0,
            col: 0,
            mode: Mode::Insert,
            pending: None,
            mark: None,
            yank: None,
            count: 0,
            count_seen: false,
        }
    }

    /// The draft text: lines joined with `\n`. The first line
    /// carries no leading separator, matching the old one-line
    /// draft.
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// Load a whole draft. The cursor is clamped to the new text.
    pub fn set_text(&mut self, text: &str) {
        let mut ls: Vec<String> = text.lines().map(str::to_string).collect();
        if ls.is_empty() {
            ls = vec![String::new()];
        }
        self.lines = ls;
        if self.row >= self.lines.len() {
            self.row = self.lines.len() - 1;
        }
        self.clamp_col();
        self.pending = None;
        self.mark = None;
        self.count = 0;
        self.count_seen = false;
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

    /// The visible lines at `scroll` (the index of the first visible
    /// line), at most `height` rows.
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

    /// A pending operator or motion label, for the status row
    /// (`[d-PENDING]`), mirroring the pi-vim format.
    pub fn pending_label(&self) -> Option<String> {
        match self.pending {
            Some(Pending::Operator(o)) => Some(format!("[{o}-PENDING]")),
            Some(Pending::Replace) => Some("[r-PENDING]".to_string()),
            _ => None,
        }
    }

    // ── key entry point ─────────────────────────────────────────

    /// Handle one key. Returns a one-line hint for a key that
    /// changed nothing (no-op in the current mode, an unknown
    /// operator target); the caller flashes it.
    pub fn press(&mut self, key_in: Key) -> Option<String> {
        // `CtrlJ` is the multi-line newline key: the host maps it here
        // so every mode's `Enter` arm applies (insert splits the line,
        // normal moves down, replace overwrites a newline).
        let key = match key_in {
            Key::CtrlJ => Key::Enter,
            k => k,
        };
        match self.mode {
            Mode::Insert => self.insert_press(key),
            Mode::Replace => self.replace_press(key),
            Mode::Visual | Mode::VisualLine => self.visual_press(key),
            Mode::Normal => self.normal_press(key),
        }
    }

    // ── insert / replace ────────────────────────────────────────

    fn insert_press(&mut self, key: Key) -> Option<String> {
        match key {
            Key::Char(c) => self.insert_char(c),
            Key::Enter => self.insert_char('\n'),
            Key::Backspace => {
                if self.col > 0 {
                    self.back_one();
                } else if self.row > 0 {
                    let prev = self.lines.remove(self.row - 1);
                    let cur = self.lines[self.row - 1].clone();
                    self.lines[self.row - 1] = format!("{}{}", prev, cur);
                    self.row -= 1;
                    self.col = self.lines[self.row].chars().count();
                }
            }
            Key::Delete => {
                let mut chars: Vec<char> = self.lines[self.row].chars().collect();
                if self.col < chars.len() {
                    chars.remove(self.col);
                    self.lines[self.row] = chars.into_iter().collect();
                }
            }
            Key::Left => {
                self.col = self.col.saturating_sub(1);
            }
            Key::Right => {
                let max = self.lines[self.row].chars().count();
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
            Key::Home => self.col = 0,
            Key::End => self.clamp_col(),
            Key::Esc => {
                self.mode = Mode::Normal;
                // Vim drops the cursor back one when insert ends.
                self.col = self.col.saturating_sub(1);
                self.clamp_col();
            }
            _ => return Some("insert mode: Ctrl-J new line, Esc normal, Enter send".to_string()),
        }
        None
    }

    fn replace_press(&mut self, key: Key) -> Option<String> {
        match key {
            Key::Char(c) => {
                let mut chars: Vec<char> = self.lines[self.row].chars().collect();
                if self.col < chars.len() {
                    chars[self.col] = c;
                    self.lines[self.row] = chars.into_iter().collect();
                    // Advance like a terminal overwrite.
                    self.col += 1;
                }
            }
            Key::Enter => self.insert_char('\n'),
            Key::Backspace => {
                let mut chars: Vec<char> = self.lines[self.row].chars().collect();
                if self.col < chars.len() {
                    chars.remove(self.col);
                    self.lines[self.row] = chars.into_iter().collect();
                } else {
                    self.back_one();
                }
            }
            Key::Delete => {
                let mut chars: Vec<char> = self.lines[self.row].chars().collect();
                if self.col < chars.len() {
                    chars.remove(self.col);
                    self.lines[self.row] = chars.into_iter().collect();
                }
            }
            Key::Esc => {
                self.mode = Mode::Normal;
            }
            _ => return Some("replace mode: type overwrites, Esc ends".to_string()),
        }
        None
    }

    // ── normal mode ─────────────────────────────────────────────

    fn normal_press(&mut self, key: Key) -> Option<String> {
        if let Some(p) = self.pending {
            if key == Key::Esc {
                // Esc cancels the pending operator / counted motion.
                self.reset_pending();
                return Some("operator cancelled".to_string());
            }
            return self.pending_press(p, key);
        }
        match key {
            Key::Char(d @ ('1'..='9')) => {
                self.count = self
                    .count
                    .saturating_mul(10)
                    .saturating_add(d as u32 - '0' as u32);
                self.count_seen = true;
            }
            Key::Char('x') => self.delete_chars(usize::max(1, self.count as usize)),
            Key::Char('X') => self.delete_backward_chars(usize::max(1, self.count as usize)),
            Key::Char('d') | Key::Char('c') => {
                self.pending = Some(Pending::Operator(key_char(&key)));
            }
            Key::Char('D') => self.op_char('d', '$', None),
            Key::Char('C') => self.op_char('c', '$', None),
            Key::Char('y') => self.pending = Some(Pending::Operator('y')),
            Key::Char('f') => self.yank_lines(usize::max(1, self.count as usize)),
            Key::Char('p') => self.paste(false),
            Key::Char('P') => self.paste(true),
            Key::Char('R') => self.mode = Mode::Replace,
            // The command keys must match before the generic motion
            // arm: `a`, `i`, `o`, `o`-family are commands, not
            // motions.
            Key::Char('i') => self.mode = Mode::Insert,
            Key::Char('a') => {
                let max = self.lines[self.row].chars().count();
                self.col = (self.col + 1).min(max);
                self.mode = Mode::Insert;
            }
            Key::Char('I') => {
                self.col = first_nonblank(&self.lines[self.row]);
                self.mode = Mode::Insert;
            }
            Key::Char('A') => {
                self.col = self.lines[self.row].chars().count();
                self.mode = Mode::Insert;
            }
            Key::Char('o') | Key::Char('O') => {
                // Both insert a blank line: `o` below, `O` above.
                let below = matches!(key, Key::Char('o'));
                let pos = if below {
                    self.row + 1
                } else {
                    self.row
                };
                self.lines.insert(pos, String::new());
                self.row = pos;
                self.col = 0;
                self.mode = Mode::Insert;
            }
            Key::Char('s') => {
                // `s` = `ci` (change one character): replace the
                // character under the cursor and enter insert mode.
                let mut chars: Vec<char> = self.lines[self.row].chars().collect();
                if self.col < chars.len() {
                    chars.remove(self.col);
                    self.lines[self.row] = chars.into_iter().collect();
                }
                self.mode = Mode::Insert;
            }
            Key::Char('S') => {
                // `S` = `CC`: clear the whole line, then insert.
                self.lines[self.row].clear();
                self.col = 0;
                self.mode = Mode::Insert;
            }
            Key::Char('r') => {
                // `r` = replace one character: remember the replace
                // operator awaiting its character.
                self.pending = Some(Pending::Replace);
            }
            Key::Char('v') => {
                self.mark = Some((self.row, self.col));
                self.mode = Mode::Visual;
            }
            Key::Char('V') => {
                self.mark = Some((self.row, 0));
                self.mode = Mode::VisualLine;
            }
            Key::Char('G') => {
                self.go_to((self.lines.len().saturating_sub(1), self.col));
            }
            Key::Char(m) => {
                // A counted motion (`3w`) applies the count to this one
                // motion and is done; `motion_target` folds in the open
                // count. No pending state is left, so the next key is a
                // fresh command.
                self.go_to(self.motion_target(m));
            }
            Key::Enter => self.go_to(self.motion_target('j')),
            Key::Backspace | Key::Up => self.go_to(self.motion_target('k')),
            Key::Down => self.go_to(self.motion_target('j')),
            Key::Left => self.go_to(self.motion_target('h')),
            Key::Right => self.go_to(self.motion_target('l')),
            Key::Home => self.go_to((self.row, 0)),
            Key::End => self.clamp_col_end(),
            Key::Esc => {
                // Plain normal mode: Esc is a no-op (cancel is the
                // pending state's only use).
            }
            _ => return Some("normal: hjkl wbe 0$ gg G x X D C dd cc yy p P v V i a I A o O R".to_string()),
        }
        // A digit keeps the open count for the next key; anything else
        // closes it.
        if self.pending.is_none() && !matches!(key, Key::Char('1'..='9')) {
            self.count = 0;
            self.count_seen = false;
        }
        None
    }

    /// The second key of a pending operator or counted motion.
    fn pending_press(&mut self, p: Pending, key: Key) -> Option<String> {
        match p {
            Pending::Replace => match key {
                Key::Char(c) => {
                    let mut chars: Vec<char> = self.lines[self.row].chars().collect();
                    if self.col < chars.len() {
                        chars[self.col] = c;
                        self.lines[self.row] = chars.into_iter().collect();
                    }
                    self.reset_pending();
                    None
                }
                _ => {
                    self.reset_pending();
                    Some("replace operator cancelled".to_string())
                }
            },
            Pending::Operator(op) => {
                if let Key::Char(d @ ('1'..='9')) = key {
                    self.count = self
                        .count
                        .saturating_mul(10)
                        .saturating_add(d as u32 - '0' as u32);
                    return None;
                }
                let motion = match key {
                    Key::Char(c) => c,
                    Key::Enter => 'j',
                    Key::Backspace | Key::Up => 'k',
                    Key::Down => 'j',
                    Key::Left => 'h',
                    Key::Right => 'l',
                    Key::Esc => {
                        self.reset_pending();
                        return Some("operator cancelled".to_string());
                    }
                    _ => {
                        self.reset_pending();
                        return Some(
                            "operator awaiting a motion: h l j k w b e 0 $".to_string(),
                        );
                    }
                };
                // `yy` (and `n yy`): the second `y` is the line
                // motion, which has no plain key of its own.
                if op == 'y' && motion == 'y' {
                    self.yank_lines(usize::max(1, self.count as usize));
                    self.reset_pending();
                    return None;
                }
                let target = self.apply_motion(motion, usize::max(1, self.count as usize));
                self.op_char(op, motion, Some(target));
                self.reset_pending();
                None
            }
        }
    }

    fn reset_pending(&mut self) {
        self.pending = None;
        self.count = 0;
        self.count_seen = false;
    }

    // ── visual modes ────────────────────────────────────────────

    fn visual_press(&mut self, key: Key) -> Option<String> {
        match key {
            Key::Esc | Key::Char('v') => {
                self.mark = None;
                self.mode = Mode::Normal;
            }
            Key::Char('d') | Key::Char('x') => self.visual_delete(),
            Key::Char('c') => {
                let (start, _) = self.visual_range();
                self.visual_delete();
                self.row = start.0;
                self.col = start.1;
                self.mode = Mode::Insert;
            }
            Key::Char('y') => self.visual_yank(),
            Key::Char('p') => self.paste(false),
            Key::Char('P') => self.paste(true),
            Key::Char(d @ ('1'..='9')) => {
                self.count = self
                    .count
                    .saturating_mul(10)
                    .saturating_add(d as u32 - '0' as u32);
            }
            Key::Char(m) => self.visual_motion(m),
            Key::Enter => self.visual_motion('j'),
            Key::Backspace | Key::Up => self.visual_motion('k'),
            Key::Down => self.visual_motion('j'),
            Key::Left => self.visual_motion('h'),
            Key::Right => self.visual_motion('l'),
            Key::Home => self.col = 0,
            Key::End => self.clamp_col_end(),
            _ => return Some("visual: d x c y p P, motions move the cursor, Esc ends".to_string()),
        }
        None
    }

    /// The visual range as an ordered pair of (row, col) ends.
    /// Line-wise covers whole lines; char-wise is the mark..cursor
    /// span, *inclusive* of the cursor character (the cursor side is
    /// extended by one so `span_text`'s exclusive end covers it).
    fn visual_range(&self) -> ((usize, usize), (usize, usize)) {
        let (mr, mc) = self.mark.unwrap_or((self.row, self.col));
        let cc = if self.mode == Mode::VisualLine {
            self.lines[self.row].chars().count()
        } else {
            self.col + 1
        };
        let a = (mr, mc);
        let b = (self.row, cc);
        if a <= b {
            (a, b)
        } else {
            (b, a)
        }
    }

    fn visual_delete(&mut self) {
        let (start, end) = self.visual_range();
        let yanked = self.span_text(start, end);
        self.replace_span(start, end, "");
        self.yank = Some(yanked);
        self.mark = None;
        self.mode = Mode::Normal;
        self.row = self.row.min(self.lines.len().saturating_sub(1));
        self.clamp_col();
    }

    fn visual_yank(&mut self) {
        let (start, end) = self.visual_range();
        self.yank = Some(self.span_text(start, end));
    }

    /// A motion inside visual mode: line-wise motions move whole
    /// lines with a count, char-wise motions move the cursor.
    fn visual_motion(&mut self, m: char) {
        let n = usize::max(1, self.count as usize);
        if self.mode == Mode::VisualLine {
            if m == 'j' {
                self.row = (self.row + n).min(self.lines.len().saturating_sub(1));
            } else if m == 'k' {
                self.row = self.row.saturating_sub(n);
            } else {
                let target = self.apply_motion(m, 1);
                self.row = target.0;
                self.col = 0;
            }
        } else {
            let target = self.apply_motion(m, n);
            self.go_to(target);
        }
        self.count = 0;
    }

    // ── operators ───────────────────────────────────────────────

    /// Apply the operator `op` to the motion `motion`; `target` is
    /// the precomputed end when the pending path already moved.
    fn op_char(&mut self, op: char, motion: char, target: Option<(usize, usize)>) {
        let n = usize::max(1, self.count as usize);
        // The line motion (`dd`, `cc`, `3dd`): remove `n` whole lines
        // from the cursor line. The delete also yanks the lines (the
        // delete sets the yank buffer, like vim).
        if motion == 'd' || motion == 'c' {
            let count = n;
            let first = self.row;
            let last = (first + count - 1).min(self.lines.len().saturating_sub(1));
            let removed: Vec<String> = self.lines[first..=last].to_vec();
            self.lines.splice(first..=last, std::iter::empty());
            if self.lines.is_empty() {
                self.lines.push(String::new());
            }
            self.row = first.min(self.lines.len().saturating_sub(1));
            self.col = 0;
            if op == 'c' {
                // `cc` leaves the (now empty) line in insert mode.
                if first < self.lines.len() {
                    self.lines[first].clear();
                }
                self.row = first;
                self.mode = Mode::Insert;
            } else {
                self.yank = Some(removed);
            }
            self.count = 0;
            self.count_seen = false;
            return;
        }
        let target = target.unwrap_or_else(|| self.apply_motion(motion, n));
        let (r0, c0) = (self.row, self.col);
        let (r1, c1) = target;
        // The ordered span the operator covers.
        let mut start = (r0, c0);
        let mut end = (r1, c1);
        if motion == '$' || motion == 'e' {
            // The operator extends to the end of the word / line.
            end = (r0, self.lines[r0].chars().count());
        }
        if r0 == r1 && c1 < c0 {
            (start, end) = (end, start);
        }
        match op {
            'd' => {
                let yanked = self.span_text(start, end);
                self.replace_span(start, end, "");
                self.yank = Some(yanked);
                self.row = start.0.min(self.lines.len().saturating_sub(1));
                self.col = start.1;
                self.clamp_col();
            }
            'y' => {
                self.yank = Some(self.span_text(start, end));
            }
            _ => {
                // Change: delete the span, then insert at its start.
                self.replace_span(start, end, "");
                self.row = start.0;
                self.col = start.1;
                self.clamp_col();
                self.mode = Mode::Insert;
            }
        }
        self.count = 0;
        self.count_seen = false;
    }

    /// Delete `n` characters from the cursor on (`x`).
    fn delete_chars(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        let mut chars: Vec<char> = self.lines[self.row].chars().collect();
        if self.col < chars.len() {
            let take = n.min(chars.len() - self.col);
            let mut cut = Vec::new();
            chars.drain(self.col..self.col + take).for_each(|c| cut.push(c));
            self.yank = Some(vec![cut.into_iter().collect()]);
            self.lines[self.row] = chars.into_iter().collect();
            self.clamp_col();
        }
    }

    /// Delete `n` characters back from the cursor (`X`).
    fn delete_backward_chars(&mut self, n: usize) {
        for _ in 0..n {
            self.back_one();
        }
    }

    /// Yank `n` line(s) from the cursor line (`yy` / `f` form).
    fn yank_lines(&mut self, n: usize) {
        let take = n.min(self.lines.len().saturating_sub(self.row));
        self.yank = Some(self.lines[self.row..self.row + take].to_vec());
    }

    /// Paste the yank buffer: after the cursor line (`p`), before it
    /// (`P`). A line buffer pastes `n` copies.
    fn paste(&mut self, before: bool) {
        let Some(buf) = self.yank.clone() else {
            return;
        };
        if buf.is_empty() && buf.first().is_some_and(|l| l.is_empty()) {
            // An empty yank: paste one empty line.
        }
        let n = usize::max(1, self.count as usize);
        let mut to_insert: Vec<String> = Vec::new();
        for _ in 0..n {
            to_insert.extend(buf.clone());
        }
        if to_insert.is_empty() {
            return;
        }
        let pos = if before {
            self.row
        } else {
            self.row + 1
        };
        self.lines.splice(pos..pos, to_insert);
        self.row = pos.min(self.lines.len().saturating_sub(1));
        self.clamp_col();
        self.count = 0;
    }

    // ── geometry helpers ────────────────────────────────────────

    fn insert_char(&mut self, c: char) {
        let line = self.lines[self.row].clone();
        let prefix: String = line.chars().take(self.col).collect();
        let suffix: String = line.chars().skip(self.col).collect();
        if c == '\n' {
            self.lines[self.row] = prefix;
            self.lines.insert(self.row + 1, suffix);
            self.row += 1;
            self.col = 0;
        } else {
            self.lines[self.row] = format!("{prefix}{c}{suffix}");
            self.col += 1;
        }
    }

    fn back_one(&mut self) {
        if self.col > 0 {
            let mut chars: Vec<char> = self.lines[self.row].chars().collect();
            chars.truncate(self.col);
            self.lines[self.row] = chars.into_iter().collect();
            self.col -= 1;
        } else if self.row > 0 {
            let prev = self.lines.remove(self.row - 1);
            self.lines[self.row - 1] = format!("{}{}", prev, &self.lines[self.row]);
            self.row -= 1;
            self.col = self.lines[self.row].chars().count();
        }
    }

    fn clamp_col(&mut self) {
        if self.row >= self.lines.len() {
            self.row = self.lines.len().saturating_sub(1);
        }
        self.col = self.col.min(self.lines.get(self.row).map(|s| s.chars().count()).unwrap_or(0));
    }

    fn clamp_col_end(&mut self) {
        self.col = self.lines[self.row].chars().count();
    }

    /// Move the cursor to `(row, col)`, clamped to the document.
    fn go_to(&mut self, target: (usize, usize)) {
        self.row = target.0.min(self.lines.len().saturating_sub(1));
        self.col = target
            .1
            .min(self.lines[self.row].chars().count());
    }

    /// The end position of `n` repetitions of `m` from the cursor.
    /// This is the motion geometry; visual mode reuses it.
    fn apply_motion(&self, m: char, n: usize) -> (usize, usize) {
        let mut r = self.row;
        let mut c = self.col;
        let lines = &self.lines;
        let len = lines.len();
        for _ in 0..n.max(1) {
            match m {
                'h' => {
                    if c > 0 {
                        c -= 1;
                    } else if r > 0 {
                        r -= 1;
                        c = lines[r].chars().count().saturating_sub(1);
                    }
                }
                'l' => {
                    let max = lines[r].chars().count();
                    if c < max {
                        c += 1;
                    } else if r < len.saturating_sub(1) {
                        r += 1;
                        c = 0;
                    }
                }
                'j' => {
                    if r < len.saturating_sub(1) {
                        r += 1;
                    }
                    c = c.min(lines[r].chars().count());
                }
                'k' => {
                    r = r.saturating_sub(1);
                    c = c.min(lines[r].chars().count());
                }
                'w' => {
                    let chars: Vec<char> = lines[r].chars().collect();
                    let in_word = |i: usize| i < chars.len() && is_word_char(chars[i]);
                    // From a word, `w` runs to the end of the word
                    // and over its trailing space (the vim rule: it
                    // lands on the first non-blank of the next word);
                    // from a space, `w` lands on the next word start.
                    let mut c2 = c;
                    if in_word(c2) {
                        while c2 + 1 < chars.len() && in_word(c2 + 1) {
                            c2 += 1;
                        }
                        // Vim lands `w` on the *first non-blank* of
                        // the next word (or the end of line when the
                        // rest is blank).
                        let mut i = c2 + 1;
                        while i < chars.len() && !in_word(i) {
                            i += 1;
                        }
                        c2 = if i < chars.len() {
                            i
                        } else {
                            chars.len()
                        };
                    } else {
                        while c2 < chars.len() && !in_word(c2) {
                            c2 += 1;
                        }
                    }
                    if c2 < chars.len() {
                        c = c2;
                    } else if r < len.saturating_sub(1) {
                        r += 1;
                        c = first_nonblank(&lines[r]);
                    }
                }
                'e' => {
                    let chars: Vec<char> = lines[r].chars().collect();
                    let in_word = |i: usize| i < chars.len() && is_word_char(chars[i]);
                    if in_word(c) {
                        while c + 1 < chars.len() && in_word(c + 1) {
                            c += 1;
                        }
                    } else {
                        while c < chars.len() && !in_word(c) {
                            c += 1;
                        }
                        while c + 1 < chars.len() && in_word(c + 1) {
                            c += 1;
                        }
                    }
                }
                'b' => {
                    let chars: Vec<char> = lines[r].chars().collect();
                    let in_word = |i: usize| i < chars.len() && is_word_char(chars[i]);
                    let mut i = c;
                    if in_word(i) {
                        while i > 0 && in_word(i - 1) {
                            i -= 1;
                        }
                    } else {
                        while i > 0 && !in_word(i - 1) {
                            i -= 1;
                        }
                        while i > 0 && in_word(i - 1) {
                            i -= 1;
                        }
                    }
                    c = i;
                }
                '0' => c = 0,
                '$' => c = lines[r].chars().count(),
                'g' => {
                    // `g` begins `gg`: the second key lands here with
                    // the count applied.
                    r = 0;
                    c = c.min(lines[r].chars().count());
                }
                _ => c = c.min(lines[r].chars().count()),
            }
        }
        (r, c)
    }

    /// The plain-key target of a single-keystroke motion with the
    /// current count applied. `g` and `G` have their own forms:
    /// `gg` goes to the top, `G` goes to the bottom.
    fn motion_target(&self, m: char) -> (usize, usize) {
        if m == 'g' {
            return self.apply_motion('g', 1);
        }
        let n = if self.count_seen {
            usize::max(1, self.count as usize)
        } else {
            1
        };
        self.apply_motion(m, n)
    }

    /// Extract the ordered span text between two ends.
    fn span_text(&self, a: (usize, usize), b: (usize, usize)) -> Vec<String> {
        if a.0 == b.0 {
            let s = &self.lines[a.0];
            let chars: Vec<char> = s.chars().collect();
            return vec![chars[a.1..b.1.min(chars.len())].iter().collect()];
        }
        let mut out = Vec::new();
        out.push(self.lines[a.0].chars().skip(a.1).collect());
        for i in a.0 + 1..b.0 {
            out.push(self.lines[i].clone());
        }
        out.push(self.lines[b.0].chars().take(b.1).collect());
        out
    }

    /// Delete the span and insert `repl` in its place. The cursor
    /// lands where the inserted text begins.
    fn replace_span(&mut self, a: (usize, usize), b: (usize, usize), repl: &str) {
        if a.0 == b.0 {
            let chars: Vec<char> = self.lines[a.0].chars().collect();
            let mut newchars: Vec<char> = chars[..a.1].to_vec();
            newchars.extend(repl.chars());
            newchars.extend(chars.iter().skip(b.1.min(chars.len())).cloned());
            self.lines[a.0] = newchars.into_iter().collect();
            self.row = a.0;
            self.col = a.1 + repl.chars().count();
            self.clamp_col();
            return;
        }
        let head: String = self.lines[a.0].chars().take(a.1).collect();
        let tail: String = self.lines[b.0].chars().skip(b.1).collect();
        // The lines `a.0..=b.0` are removed. The replacement lines
        // fill the gap; when nothing is left of the span (a whole-line
        // delete with no replacement) the gap stays empty.
        let mut newlines = self.lines[..a.0].to_vec();
        if !repl.is_empty() {
            newlines.push(repl.to_string());
        }
        let joined = format!("{head}{tail}");
        if !joined.is_empty() {
            newlines.push(joined);
        }
        newlines.extend_from_slice(&self.lines[b.0 + 1..]);
        if newlines.is_empty() {
            newlines.push(String::new());
        }
        let cr = a.0;
        self.lines = newlines;
        self.row = cr.min(self.lines.len().saturating_sub(1));
        self.col = a.1 + repl.len();
        self.clamp_col();
    }
}

fn key_char(k: &Key) -> char {
    match k {
        Key::Char(c) => *c,
        _ => ' ',
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The first non-blank column of a line (the `I` position).
fn first_nonblank(s: &str) -> usize {
    s.char_indices()
        .find(|(_, c)| !c.is_whitespace())
        .map(|(i, _)| i)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;


    fn type_seq(ed: &mut Editor, s: &str) {
        for c in s.chars() {
            let _ = ed.press(Key::Char(c));
        }
    }

    fn line(ed: &Editor) -> String {
        ed.lines[ed.row].clone()
    }

    fn k(ed: &mut Editor, c: char) {
        let _ = ed.press(Key::Char(c));
    }

    /// An editor in **normal mode** holding `text`: the command and
    /// operator tests operate on the normal-mode state machine. The
    /// composer starts in insert (the typing mode), but the vim
    /// commands are exercised from normal mode.
    fn normal(text: &str) -> Editor {
        let mut ed = Editor::new();
        ed.set_text(text);
        ed.mode = Mode::Normal;
        ed
    }

    #[test]
    fn insert_then_send_yields_multiline_text() {
        let mut ed = Editor::new();
        type_seq(&mut ed, "abc");
        let _ = ed.press(Key::Enter);
        type_seq(&mut ed, "def");
        assert_eq!(ed.text(), "abc\ndef");
        assert_eq!(ed.n_lines(), 2);
        assert_eq!(ed.cursor(), (1, 3));
    }

    #[test]
    fn esc_drops_to_normal_and_backs_one() {
        let mut ed = Editor::new();
        type_seq(&mut ed, "abc");
        let _ = ed.press(Key::Esc);
        assert_eq!(ed.mode(), Mode::Normal);
        assert_eq!(ed.cursor(), (0, 2), "the cursor steps back one");
    }

    #[test]
    fn normal_motions_move_the_cursor() {
        let mut ed = normal("hello\nworld");
        k(&mut ed, 'l');
        k(&mut ed, 'l');
        assert_eq!(ed.cursor(), (0, 2));
        k(&mut ed, 'j');
        assert_eq!(ed.cursor(), (1, 2));
        k(&mut ed, 'h');
        k(&mut ed, 'k');
        k(&mut ed, 'l');
        assert_eq!(ed.cursor(), (0, 2));
        k(&mut ed, '$');
        assert_eq!(ed.cursor(), (0, 5));
        k(&mut ed, '0');
        assert_eq!(ed.cursor(), (0, 0));
        k(&mut ed, 'g');
        k(&mut ed, 'g');
        assert_eq!(ed.cursor(), (0, 0), "gg to the top");
        k(&mut ed, 'G');
        assert_eq!(ed.cursor(), (1, 0), "G to the bottom");
    }

    #[test]
    fn word_motions() {
        let mut ed = normal("foo bar baz");
        k(&mut ed, 'w');
        assert_eq!(ed.cursor(), (0, 4), "w to the start of bar");
        k(&mut ed, 'w');
        assert_eq!(ed.cursor(), (0, 8), "w to the start of baz");
        k(&mut ed, 'e');
        assert_eq!(ed.cursor(), (0, 10), "e to the end of baz");
        k(&mut ed, 'b');
        assert_eq!(ed.cursor(), (0, 8), "b back to the word start");
    }

    #[test]
    fn dd_deletes_lines_and_yanks() {
        let mut ed = normal("one\ntwo\nthree");
        k(&mut ed, 'd');
        k(&mut ed, 'd');
        assert_eq!(ed.lines, vec!["two", "three"]);
        assert_eq!(ed.cursor(), (0, 0));
        k(&mut ed, 'p');
        assert_eq!(ed.lines, vec!["two", "one", "three"]);
    }

    #[test]
    fn counted_dd_deletes_multiple_lines() {
        let mut ed = normal("one\ntwo\nthree\nfour");
        for c in "3dd".chars() {
            let _ = ed.press(Key::Char(c));
        }
        assert_eq!(ed.lines, vec!["four"]);
    }

    #[test]
    fn dw_deletes_to_next_word() {
        let mut ed = normal("foo bar");
        k(&mut ed, 'd');
        k(&mut ed, 'w');
        assert_eq!(line(&ed), "bar", "dw deletes the word and its trailing space");
    }

    #[test]
    fn d0_deletes_to_line_start() {
        let mut ed = normal("hello world");
        type_seq(&mut ed, "l");
        k(&mut ed, 'd');
        k(&mut ed, '0');
        assert_eq!(line(&ed), "ello world", "d0 deletes up to the line start");
    }

    #[test]
    fn cc_clears_the_line() {
        let mut ed = normal("abc");
        k(&mut ed, 'c');
        k(&mut ed, 'c');
        assert_eq!(ed.mode(), Mode::Insert);
        assert_eq!(line(&ed), "");
        type_seq(&mut ed, "xyz");
        assert_eq!(ed.text(), "xyz");
    }

    #[test]
    fn c0_changes_to_line_start() {
        let mut ed = normal("hello");
        type_seq(&mut ed, "ll");
        k(&mut ed, 'c');
        k(&mut ed, '0');
        type_seq(&mut ed, "HI ");
        assert_eq!(line(&ed), "HI llo");
    }

    #[test]
    fn x_deletes_under_cursor() {
        let mut ed = normal("abcd");
        type_seq(&mut ed, "l");
        k(&mut ed, 'x');
        assert_eq!(line(&ed), "acd");
    }

    #[test]
    fn xx_deletes_two() {
        let mut ed = normal("abcd");
        k(&mut ed, 'x');
        k(&mut ed, 'x');
        assert_eq!(line(&ed), "cd");
    }

    #[test]
    fn r_replace_mode_overwrites() {
        let mut ed = normal("abc");
        k(&mut ed, 'R');
        assert_eq!(ed.mode(), Mode::Replace);
        type_seq(&mut ed, "XYZ");
        assert_eq!(line(&ed), "XYZ");
        let _ = ed.press(Key::Esc);
        assert_eq!(ed.mode(), Mode::Normal);
    }

    #[test]
    fn visual_line_delete() {
        let mut ed = normal("one\ntwo\nthree");
        k(&mut ed, 'V');
        k(&mut ed, 'j');
        k(&mut ed, 'd');
        assert_eq!(ed.lines, vec!["three"]);
    }

    #[test]
    fn visual_char_delete() {
        let mut ed = normal("abcdef");
        k(&mut ed, 'v');
        k(&mut ed, 'l');
        k(&mut ed, 'd');
        assert_eq!(line(&ed), "cdef", "v then l selects the first two chars, d removes them");
    }

    #[test]
    fn yy_yanks_lines() {
        let mut ed = normal("one\ntwo\nthree");
        k(&mut ed, 'y');
        k(&mut ed, 'y');
        k(&mut ed, 'P');
        assert_eq!(
            ed.lines,
            vec!["one", "one", "two", "three"],
            "yy yanks the cursor line; P inserts it before"
        );
    }

    #[test]
    fn o_and_O_insert_blank_lines() {
        let mut ed = normal("a\nb");
        k(&mut ed, 'o');
        assert_eq!(ed.lines, vec!["a", "", "b"]);
        let _ = ed.press(Key::Esc);
        k(&mut ed, 'k');
        k(&mut ed, 'O');
        assert_eq!(ed.lines, vec!["", "a", "", "b"]);
    }

    #[test]
    fn i_and_a_enter_insert() {
        let mut ed = normal("ab");
        k(&mut ed, 'i');
        assert_eq!(ed.mode(), Mode::Insert);
        assert_eq!(ed.cursor(), (0, 0));
        let mut ed = normal("ab");
        k(&mut ed, 'a');
        assert_eq!(ed.mode(), Mode::Insert);
        assert_eq!(ed.cursor(), (0, 1));
    }

    #[test]
    fn backspace_joins_lines() {
        // Insert mode: Backspace at column 0 joins the previous line
        // (the multi-line composer's behavior).
        let mut ed = Editor::new();
        ed.set_text("ab\ncd");
        ed.row = 1;
        assert_eq!(ed.mode(), Mode::Insert);
        let _ = ed.press(Key::Backspace);
        assert_eq!(ed.text(), "abcd");
    }

    #[test]
    fn display_scrolls_a_long_draft() {
        let ed = normal("a\nb\nc\nd\ne");
        assert_eq!(ed.display(2, 2), vec!["c", "d"]);
        assert_eq!(ed.display(4, 2), vec!["e"], "the scroll clamps");
    }

    #[test]
    fn counted_w_motions() {
        let mut ed = normal("foo bar baz qux");
        for c in "2w".chars() {
            let _ = ed.press(Key::Char(c));
        }
        assert_eq!(ed.cursor(), (0, 8), "2w skips two words");
    }
}
