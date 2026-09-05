//! The conversation browse mode and the position bar
//! (docs/tui-conversation-browsing.md).
//!
//! `Browse` is the state machine behind the double-`s` overlay: the
//! cursor over the rendered transcript, the line-number gutter, the
//! scrolloff view following, and the stage-2 regex search. The host
//! routes keys through [`Browse::key`] while the mode holds
//! (`app.rs` owns the arm window and the host-role keys).
//!
//! The pure helpers here are the section-3 bar geometry, the section
//! 4.3 gutter numbering, and the section 4.5 scrolloff follow. They
//! take plain integers so the conformance tests (section 9) drive
//! them directly.

use std::collections::HashSet;

use regex::Regex;

use crate::app::Key;

/// The `scrolloff` margin of the machine's neovim (section 6.1):
/// at least three lines above and below the cursor.
pub const SCROLLOFF: usize = 3;
/// The count cap of the key table (section 4.4).
pub const COUNT_CAP: u32 = 99_999;
/// The one-line status hint of the key table (section 4.4).
pub const BROWSE_HINT: &str = "browse: gg top, G end, :N line, ss leave";

/// One search direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Forward,
    Backward,
}

/// A remembered view for the `jumpoptions = "stack,view"` restore
/// (section 7.3): the view state held before a forward jump.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SavedView {
    pub scroll: usize,
    pub line: usize,
    pub col: usize,
}

/// The open command line of the browse mode.
pub enum Typing {
    /// The `:N` goto line: digits plus an accepted `j` / `k` suffix.
    Goto { digits: String },
    /// The `/` or `?` pattern input (section 7.3).
    Search {
        dir: Dir,
        input: String,
        /// The cursor position when the search opened: `incsearch`
        /// jumps from here, not from the live cursor.
        origin: (usize, usize),
    },
}

/// The outcome of one command-line key.
enum TypeKeyOutcome {
    /// Consumed, nothing to show.
    Handled,
    /// Consumed, with the hint to show (`line: 1..total`, the regex
    /// error).
    Hint(String),
    /// Closed the line: the key must act in the normal state.
    Pass,
}

/// The search state (section 7.3). The last valid compiled pattern
/// survives an invalid input; the `active` flag is the highlight bit
/// (an `Esc` clears it, leaving the pattern for a repeat).
pub struct Search {
    pattern: Option<String>,
    re: Option<Regex>,
    active: bool,
    forward: bool,
    /// The current match, in transcript line coordinates.
    active_match: Option<(usize, usize)>,
    /// The match-line cache for the renderer, with the total and the
    /// pattern version it was built from.
    hl: HashSet<usize>,
    hl_total: usize,
    hl_version: u64,
    re_version: u64,
}

impl Default for Search {
    fn default() -> Self {
        Self {
            pattern: None,
            re: None,
            active: false,
            forward: true,
            active_match: None,
            hl: HashSet::new(),
            hl_total: 0,
            hl_version: 0,
            re_version: 0,
        }
    }
}

/// The browse state machine (section 4). The cursor is `(line, col)`:
/// `line` is a zero-based transcript line index, `col` a zero-based
/// character position within that line. Every move clamps `col` to
/// the line length (section 4.1).
#[derive(Default)]
pub struct Browse {
    active: bool,
    /// Entry is pending a render pass: the cursor lands on the first
    /// visible line, col 0, and the view does not move (section 4.2).
    pending_entry: bool,
    line: usize,
    col: usize,
    /// The count prefix of the next motion, capped at [`COUNT_CAP`].
    /// A bare motion (no count typed) counts as 1; a typed `0`
    /// counts as a no-op motion (section 4.4).
    pending: u32,
    /// A count digit was typed: `0<key>` is then a no-op motion,
    /// while a bare key counts as 1.
    has_count: bool,
    /// A `g` awaiting its second `g`.
    pending_g: bool,
    typing: Option<Typing>,
    search: Search,
    /// The last search move, for the `N` view restore (section 7.3).
    last_move: Option<Dir>,
    saved_view: Option<SavedView>,
    last_total: usize,
    last_h: usize,
}

/// The view the browse handler moves: the rendered transcript, its
/// height, the scroll offset, and the half-page distance.
pub struct View<'a> {
    /// The total rendered transcript lines.
    pub total: usize,
    /// The visible view height.
    pub h: usize,
    /// The scroll offset, written by the view motions.
    pub scroll: &'a mut usize,
    /// The half-page scroll distance of `Ctrl+U` / `Ctrl+D`.
    pub half: usize,
    /// The rendered transcript line texts, oldest first. Search runs
    /// over these; the renderer builds them each frame.
    pub texts: &'a [String],
}

impl Browse {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn active(&self) -> bool {
        self.active
    }

    /// The cursor in transcript coordinates, `(line, col)`.
    pub fn line_col(&self) -> (usize, usize) {
        (self.line, self.col)
    }

    /// The command-line prompt for the input box title
    /// (`:42`, `/pat`, `?pat`), `None` outside the command line.
    pub fn prompt(&self) -> Option<String> {
        match &self.typing {
            Some(Typing::Goto { digits }) => Some(format!(":{digits}")),
            Some(Typing::Search { dir, input, .. }) => {
                let p = match dir {
                    Dir::Forward => '/',
                    Dir::Backward => '?',
                };
                Some(format!("{p}{input}"))
            }
            None => None,
        }
    }

    /// The highlight state for the renderer: the match-line cache
    /// refreshed against the current total, and the accent line.
    /// The cache is one pass over the texts per total or pattern
    /// change, not per frame.
    pub fn highlight_lines(
        &mut self,
        total: usize,
        texts: &[String],
    ) -> (&HashSet<usize>, Option<(usize, usize)>) {
        if self.search.active
            && (self.search.hl_total != total || self.search.hl_version != self.search.re_version)
        {
            self.search.hl = self.match_lines(texts);
            self.search.hl_total = total;
            self.search.hl_version = self.search.re_version;
        }
        (
            &self.search.hl,
            self.search.active_match.filter(|_| self.search.active),
        )
    }

    /// The active match position (the accent line), if any. Test
    /// only: the renderer reads it through `App::browse_highlight`.
    #[allow(dead_code)]
    pub fn active_match(&self) -> Option<(usize, usize)> {
        self.search.active_match.filter(|_| self.search.active)
    }

    /// The last valid pattern, redisplayed by the host. Test only:
    /// the conformance rows of section 9 read it.
    #[allow(dead_code)]
    pub fn last_pattern(&self) -> Option<&str> {
        self.search.pattern.as_deref()
    }

    /// The command line is open: the hardware caret parks on the
    /// prompt, like the editor's search line.
    pub fn typing(&self) -> bool {
        self.typing.is_some()
    }

    // ── entry and exit (section 4.2) ─────────────────────────────

    /// Enter browse mode. The cursor resolves to the first visible
    /// line at the next [`Browse::sync`] (the view does not move).
    pub fn enter(&mut self) {
        self.active = true;
        self.pending_entry = true;
        self.line = 0;
        self.col = 0;
        self.pending = 0;
        self.has_count = false;
        self.pending_g = false;
        self.typing = None;
        self.search.active = false;
        self.search.active_match = None;
        self.last_move = None;
        self.saved_view = None;
    }

    /// Leave browse mode: the view stays where browse put it
    /// (section 4.2), and the highlight drops (section 7.3).
    pub fn exit(&mut self) {
        self.active = false;
        self.pending_entry = false;
        self.pending = 0;
        self.has_count = false;
        self.pending_g = false;
        self.typing = None;
        self.search.active = false;
        self.search.active_match = None;
        self.last_move = None;
        self.saved_view = None;
    }

    /// A session switch resets the browse state with the scroll
    /// (section 4.7): everything, including the last pattern.
    pub fn reset(&mut self, scroll: &mut usize) {
        *scroll = 0;
        self.exit();
        self.search.pattern = None;
        self.search.re = None;
        self.search.re_version = 0;
        self.search.hl.clear();
        self.last_total = 0;
        self.last_h = 0;
    }

    /// The renderer's layout sync (sections 4.6 and 4.7): when the
    /// total or the height changes under a held cursor, the cursor
    /// clamps to the new total and the view re-centers on it with
    /// the scrolloff margins. A pending entry resolves here instead.
    /// A pure tail growth (a rendered event landed, `grew = true`)
    /// keeps the view put: no auto-follow (section 4.6). A growth
    /// without a new event is a pane rewrap and re-centers
    /// (section 4.7).
    pub fn sync(&mut self, total: usize, h: usize, scroll: &mut usize, grew: bool) {
        let changed = total != self.last_total || h != self.last_h;
        let pure_growth = total > self.last_total && grew && h == self.last_h;
        self.last_total = total;
        self.last_h = h;
        if total == 0 {
            self.line = 0;
            self.col = 0;
            *scroll = 0;
            self.pending_entry = false;
            return;
        }
        // The cursor pins to its line number (section 4.6): a cap
        // drop shifts the lines, the number keeps pointing, the
        // fold / thinking toggle clamps to the new total.
        let clamped = self.line >= total;
        self.line = self.line.min(total - 1);
        if self.pending_entry {
            // Entry: the cursor is the first visible line, col 0,
            // and the view does not move (section 4.2).
            let start = total.saturating_sub(*scroll + h);
            *scroll = (*scroll).min(total.saturating_sub(h));
            self.line = start.min(total - 1);
            self.col = 0;
            self.pending_entry = false;
        } else if changed && (!pure_growth || clamped) {
            // A shrink, a height change, a rewrap growth, or a
            // clamped cursor re-centers with the scrolloff margins
            // (section 4.7).
            *scroll = follow_view(total, h, self.line, *scroll);
        }
    }

    /// Clamp the cursor column to the rendered line length
    /// (section 4.1). The col may sit one past the last character
    /// (the blank block cell at line end).
    pub fn clamp_col(&mut self, line_len: usize) {
        self.col = self.col.min(line_len);
    }

    // ── the key table (sections 4.4 and 7.3) ─────────────────────

    /// One browse-owned key. Returns the flash hint to show, if any.
    /// The host keys (`Ctrl+C` / `Ctrl+R` / the toggles / `Tab` /
    /// the quit gate) never reach here: the host routes them
    /// (`app.rs`), and the double-`s` exit arm is the host's too.
    pub fn key(&mut self, key: Key, v: &mut View) -> Option<String> {
        if !self.active {
            return None;
        }
        if let Some(t) = self.typing.take() {
            match self.typing_key(key, t, v) {
                TypeKeyOutcome::Handled => return None,
                TypeKeyOutcome::Hint(h) => return Some(h),
                // A structural key closed the line: act on it in the
                // normal state, with the count cleared.
                TypeKeyOutcome::Pass => {
                    self.pending = 0;
                    return self.normal_key(key, v);
                }
            }
        }
        self.normal_key(key, v)
    }

    /// One key while the command line is open.
    fn typing_key(&mut self, key: Key, t: Typing, v: &mut View) -> TypeKeyOutcome {
        match t {
            Typing::Goto { mut digits } => match key {
                Key::Char(c) if c.is_ascii_digit() => {
                    digits.push(c);
                    self.typing = Some(Typing::Goto { digits });
                    TypeKeyOutcome::Handled
                }
                // The `j` / `k` suffix is accepted and ignored: the
                // ex form already names the line (section 4.4).
                Key::Char('j') | Key::Char('k') => {
                    self.typing = Some(Typing::Goto { digits });
                    TypeKeyOutcome::Handled
                }
                // Anything but a line number: no move, the hint
                // names the range (section 4.4).
                Key::Char(_) => {
                    self.typing = None;
                    TypeKeyOutcome::Hint(Self::goto_hint(v.total))
                }
                Key::Enter => {
                    // A `u64` parse keeps a huge `:N` clamping to
                    // the last line instead of reading as 0.
                    let n: u64 = digits.trim().parse().unwrap_or(u64::MAX);
                    self.typing = None;
                    if n == 0 || v.total == 0 {
                        return TypeKeyOutcome::Hint(Self::goto_hint(v.total));
                    }
                    self.line = (n as usize - 1).min(v.total - 1);
                    let len = v.texts.get(self.line).map(|t| line_len(t)).unwrap_or(0);
                    self.col = self.col.min(len);
                    *v.scroll = follow_view(v.total, v.h, self.line, *v.scroll);
                    TypeKeyOutcome::Handled
                }
                Key::Backspace => {
                    digits.pop();
                    self.typing = Some(Typing::Goto { digits });
                    TypeKeyOutcome::Handled
                }
                Key::Esc => {
                    // Esc cancels the line and clears the active
                    // highlight; it never leaves browse.
                    self.search.active = false;
                    self.search.active_match = None;
                    TypeKeyOutcome::Handled
                }
                // Any other key closes the line and acts.
                _ => TypeKeyOutcome::Pass,
            },
            Typing::Search { dir, mut input, origin } => match key {
                Key::Char(c) => {
                    input.push(c);
                    self.search.forward = dir == Dir::Forward;
                    // `incsearch`: each keystroke re-matches, and the
                    // cursor jumps to the first match in the search
                    // direction (section 7.3).
                    let outcome = match compile_smart(&input) {
                        Ok(re) => {
                            self.search.re = Some(re);
                            self.search.re_version += 1;
                            self.search.pattern = Some(input.clone());
                            self.search.active = true;
                            if let Some(m) = next_match(
                                v.texts,
                                self.search.re.as_ref().unwrap(),
                                origin,
                                dir,
                            ) {
                                self.search.active_match = Some(m);
                                self.line = m.0;
                                self.col = m.1;
                                *v.scroll = follow_view(v.total, v.h, self.line, *v.scroll);
                            }
                            TypeKeyOutcome::Handled
                        }
                        Err(e) => {
                            // An invalid pattern keeps the last valid
                            // one; the command line hints the error
                            // (section 7.3).
                            TypeKeyOutcome::Hint(format!("regex: {e}"))
                        }
                    };
                    self.typing = Some(Typing::Search { dir, input, origin });
                    outcome
                }
                Key::Backspace => {
                    if input.pop().is_none() {
                        // A backspace on the empty input cancels.
                        return TypeKeyOutcome::Handled;
                    }
                    self.typing = Some(Typing::Search { dir, input, origin });
                    TypeKeyOutcome::Handled
                }
                Key::Enter => {
                    // Commit: the pattern is live, and the view
                    // centers on the match (the machine's `zz`).
                    if input.is_empty() {
                        return TypeKeyOutcome::Handled;
                    }
                    if let Ok(re) = compile_smart(&input) {
                        self.search.re = Some(re);
                        self.search.re_version += 1;
                        self.search.pattern = Some(input);
                        self.search.active = true;
                        self.last_move = Some(dir);
                        // The view state before the jump is the
                        // restore point of a backward `N`.
                        self.saved_view = Some(SavedView {
                            scroll: *v.scroll,
                            line: self.line,
                            col: self.col,
                        });
                        if let Some(m) = next_match(v.texts, self.search.re.as_ref().unwrap(), origin, dir) {
                            self.search.active_match = Some(m);
                            self.line = m.0;
                            self.col = m.1;
                            center_view(v, self.line);
                        }
                    }
                    TypeKeyOutcome::Handled
                }
                Key::Esc => {
                    // Esc clears the active highlight (section 6.2).
                    self.search.active = false;
                    self.search.active_match = None;
                    TypeKeyOutcome::Handled
                }
                _ => TypeKeyOutcome::Pass,
            },
        }
    }

    /// The `:N` command-line hint (section 4.4): the valid range.
    fn goto_hint(total: usize) -> String {
        format!("line: 1..{total}")
    }

    /// The motion count of the pending digits: a bare motion counts
    /// as 1, a typed `0` counts as a no-op (section 4.4). Clears the
    /// count either way.
    fn motion_count(&mut self) -> u32 {
        let n = if self.has_count { self.pending } else { 1 };
        self.pending = 0;
        self.has_count = false;
        n
    }

    /// One key in the browse normal state (the command line closed).
    fn normal_key(&mut self, key: Key, v: &mut View) -> Option<String> {
        match key {
            Key::Char(c) => self.char_key(c, v),
            Key::Esc => {
                // Cancel a pending count; clear the active search
                // highlight; never leave browse mode (section 4.4).
                self.pending = 0;
                self.has_count = false;
                self.pending_g = false;
                self.search.active = false;
                self.search.active_match = None;
                None
            }
            Key::CtrlU => {
                // Half a page up; the stranded cursor lands on the
                // new top line (section 4.4).
                self.view_move(v, true, v.half);
                None
            }
            Key::CtrlD => {
                self.view_move(v, false, v.half);
                None
            }
            Key::Wheel(d) => {
                // The wheel moves the view first (three lines, the
                // host distance); the stranded cursor follows.
                self.view_move(v, d < 0, 3);
                None
            }
            // The unbound keys: a no-op, the status hint names the
            // one line of the key table (section 4.4).
            _ => Some(BROWSE_HINT.to_string()),
        }
    }

    /// The character keys of the normal state.
    fn char_key(&mut self, c: char, v: &mut View) -> Option<String> {
        if c.is_ascii_digit() {
            // Counts prefix the motions, capped at 99999. A count of
            // 0 is a no-op motion (section 4.4).
            self.pending = (self.pending * 10 + (c as u32 - '0' as u32)).min(COUNT_CAP);
            self.has_count = true;
            return None;
        }
        match c {
            'j' => {
                let n = self.motion_count() as usize;
                if n > 0 && v.total > 0 {
                    self.line = (self.line + n).min(v.total - 1);
                    self.clamp_to_line(v);
                    *v.scroll = follow_view(v.total, v.h, self.line, *v.scroll);
                }
                None
            }
            'k' => {
                let n = self.motion_count() as usize;
                if n > 0 && v.total > 0 {
                    self.line = self.line.saturating_sub(n);
                    self.clamp_to_line(v);
                    *v.scroll = follow_view(v.total, v.h, self.line, *v.scroll);
                }
                None
            }
            'h' => {
                let n = self.motion_count() as usize;
                if n > 0 {
                    self.col = self.col.saturating_sub(n);
                }
                None
            }
            'l' => {
                let n = self.motion_count() as usize;
                if n > 0 {
                    // The ceiling is the line end: the col may rest
                    // one past the last character (section 4.1).
                    let len = v.texts.get(self.line).map(|t| line_len(t)).unwrap_or(0);
                    self.col = (self.col + n).min(len);
                }
                None
            }
            'g' => {
                if self.pending_g {
                    // `gg`: the cursor to line 1, the view to the
                    // top; a count takes line n (section 4.4).
                    let n = self.motion_count() as usize;
                    self.pending_g = false;
                    if v.total > 0 && n > 0 {
                        self.line = (n - 1).min(v.total - 1);
                        self.clamp_to_line(v);
                        // Seed the view at the top; the scrolloff
                        // margins apply (section 4.5).
                        *v.scroll = follow_view(
                            v.total,
                            v.h,
                            self.line,
                            v.total.saturating_sub(v.h),
                        );
                    }
                } else {
                    self.pending_g = true;
                }
                None
            }
            'G' => {
                // The cursor to the last line, the view to the tail;
                // a count takes line n (section 4.4). A typed `0`
                // is a no-op motion.
                let n = self.pending as usize;
                let has = self.has_count;
                self.pending = 0;
                self.has_count = false;
                if v.total > 0 {
                    if has && n > 0 {
                        self.line = (n - 1).min(v.total - 1);
                    } else if !has {
                        self.line = v.total - 1;
                    }
                    if !has || n > 0 {
                        self.clamp_to_line(v);
                        *v.scroll = follow_view(v.total, v.h, self.line, 0);
                    }
                }
                None
            }
            ':' => {
                // The ex form goto line (section 4.4).
                self.pending = 0;
                self.has_count = false;
                self.pending_g = false;
                self.typing = Some(Typing::Goto { digits: String::new() });
                None
            }
            '/' => {
                self.open_search(v, Dir::Forward);
                None
            }
            '?' => {
                self.open_search(v, Dir::Backward);
                None
            }
            'n' => {
                self.search_step(v, Dir::Forward);
                None
            }
            'N' => {
                self.search_step(v, Dir::Backward);
                None
            }
            '*' => {
                self.search_word(v, Dir::Forward);
                None
            }
            '#' => {
                self.search_word(v, Dir::Backward);
                None
            }
            _ => Some(BROWSE_HINT.to_string()),
        }
    }

    /// Clamps the cursor col to the current line's rendered length.
    fn clamp_to_line(&mut self, v: &View) {
        let len = v.texts.get(self.line).map(|t| line_len(t)).unwrap_or(0);
        self.col = self.col.min(len);
    }

    /// The view scroll with the stranded-cursor rule (section 4.4
    /// and 4.5): the view moves first. A cursor at the moving edge
    /// band, or stranded outside the new view, jumps to the new
    /// edge line (the top for a scroll up, the bottom for a scroll
    /// down).
    fn view_move(&mut self, v: &mut View, up: bool, dist: usize) {
        if v.total == 0 {
            return;
        }
        let old_start = v.total.saturating_sub(*v.scroll + v.h);
        if up {
            *v.scroll = v.scroll.saturating_add(dist);
        } else {
            *v.scroll = v.scroll.saturating_sub(dist);
        }
        let start = v.total.saturating_sub(*v.scroll + v.h).min(v.total.saturating_sub(v.h));
        let bottom = start.saturating_add(v.h - 1).min(v.total - 1);
        if up {
            // The top edge moved up: a cursor at the old top band,
            // or stranded below the new view, lands on the new top.
            if self.line.saturating_sub(old_start) <= SCROLLOFF || self.line > bottom {
                self.line = start.min(v.total - 1);
            }
        } else {
            // The bottom edge moved down: a cursor at the old
            // bottom band, or stranded above the new view, lands on
            // the new bottom.
            if bottom.saturating_sub(self.line) <= SCROLLOFF || self.line < start {
                self.line = bottom;
            }
        }
    }

    fn open_search(&mut self, v: &View, dir: Dir) {
        // The origin is the cursor position: `incsearch` jumps from
        // here (section 7.3). The saved view is the `N` restore
        // point of a forward jump.
        self.pending = 0;
        self.has_count = false;
        self.pending_g = false;
        self.saved_view = Some(SavedView {
            scroll: *v.scroll,
            line: self.line,
            col: self.col,
        });
        self.typing = Some(Typing::Search {
            dir,
            input: String::new(),
            origin: (self.line, self.col),
        });
    }

    /// The `n` / `N` step, counted and wrapping the ends
    /// (section 7.3, the `wrapscan` match). The view centers on the
    /// match (the machine's `zz` remap, section 6.2), except a
    /// backward `N` after a forward jump, which restores the saved
    /// view (the `jumpoptions` pair, section 7.3).
    fn search_step(&mut self, v: &mut View, dir: Dir) {
        let re = match self.search.re.clone() {
            Some(r) => r,
            None => return,
        };
        let count = self.motion_count();
        let was_forward = self.last_move == Some(Dir::Forward);
        if count == 0 {
            // A typed `0` is a no-op motion (section 4.4).
            return;
        }
        // A forward jump remembers its view for a backward `N`.
        if dir == Dir::Forward {
            self.saved_view = Some(SavedView {
                scroll: *v.scroll,
                line: self.line,
                col: self.col,
            });
        }
        let mut pos = (self.line, self.col);
        let mut found = false;
        for _ in 0..count {
            match next_match(v.texts, &re, pos, dir) {
                Some(m) => {
                    pos = m;
                    found = true;
                }
                None => break,
            }
        }
        if !found {
            return;
        }
        self.line = pos.0;
        self.col = pos.1;
        self.last_move = Some(dir);
        self.search.active = true;
        self.search.active_match = Some(pos);
        if dir == Dir::Backward && was_forward {
            // The restore case: one remembered pair (section 7.3).
            if let Some(sv) = self.saved_view.take() {
                *v.scroll = sv.scroll;
            } else {
                center_view(v, self.line);
            }
        } else {
            center_view(v, self.line);
        }
    }

    /// The `*` / `#` word search (section 7.3): the word under the
    /// cursor, escaped into a pattern, searched forward / backward.
    fn search_word(&mut self, v: &mut View, dir: Dir) {
        let text = match v.texts.get(self.line) {
            Some(t) => t,
            None => return,
        };
        let (word, start, end, in_word) = match word_under_cursor(text, self.col, dir) {
            Some(w) => w,
            None => return,
        };
        let _ = end;
        let escaped = regex::escape(&word);
        let re = match compile_smart(&escaped) {
            Ok(r) => r,
            Err(_) => return,
        };
        self.search.pattern = Some(escaped);
        self.search.re = Some(re.clone());
        self.search.re_version += 1;
        self.search.forward = dir == Dir::Forward;
        self.search.active = true;
        // A forward word search remembers its view, like `n`.
        let was_forward = self.last_move == Some(Dir::Forward);
        if dir == Dir::Forward {
            self.saved_view = Some(SavedView {
                scroll: *v.scroll,
                line: self.line,
                col: self.col,
            });
        }
        // The origin skips the occurrence the cursor sits on: in a
        // word the word start, off a word the cursor itself.
        let origin = if in_word {
            (self.line, start)
        } else {
            (self.line, self.col)
        };
        let m = match next_match(v.texts, &re, origin, dir) {
            Some(m) => m,
            None => return,
        };
        self.line = m.0;
        self.col = m.1;
        self.last_move = Some(dir);
        self.search.active_match = Some(m);
        if dir == Dir::Backward && was_forward {
            if let Some(sv) = self.saved_view.take() {
                *v.scroll = sv.scroll;
            } else {
                center_view(v, self.line);
            }
        } else {
            center_view(v, self.line);
        }
    }

    /// Every match of the current pattern, in transcript line
    /// coordinates. The renderer's highlight runs over these.
    pub fn match_lines(&self, texts: &[String]) -> HashSet<usize> {
        let re = match &self.search.re {
            Some(r) => r,
            None => return HashSet::new(),
        };
        let mut out = HashSet::new();
        for (i, t) in texts.iter().enumerate() {
            if re.is_match(t) {
                out.insert(i);
            }
        }
        out
    }
}

/// The rendered length of a transcript line, in characters. The
/// cursor col is a character position (section 4.1).
fn line_len(t: &str) -> usize {
    t.chars().count()
}

/// The scrolloff view follow (section 4.5): the smallest scroll
/// move that keeps `SCROLLOFF` lines above and below the cursor.
/// When a log edge sits closer, the view pins to that edge. A view
/// too short for both margins centers on the cursor.
pub fn follow_view(total: usize, h: usize, line: usize, scroll: usize) -> usize {
    if total == 0 || h == 0 {
        return 0;
    }
    if total <= h {
        return 0;
    }
    let line = line.min(total - 1);
    let cur = total.saturating_sub(scroll + h);
    let mut t = cur;
    // The top margin: at least SCROLLOFF lines above the cursor.
    if line < t + SCROLLOFF {
        t = line.saturating_sub(SCROLLOFF);
    }
    // The bottom margin: at least SCROLLOFF lines below the cursor.
    if t.saturating_add(h).saturating_sub(1) < line + SCROLLOFF {
        t = line.saturating_add(SCROLLOFF + 1).saturating_sub(h);
    }
    // A view shorter than 2 * margin + 2 cannot hold both margins:
    // the cursor centers.
    if h < 2 * SCROLLOFF + 2 {
        t = line.saturating_sub(h / 2);
    }
    t = t.min(total - h);
    total - t - h
}

/// Center the view on the line (the machine's `zz`, section 6.2).
fn center_view(v: &mut View, line: usize) {
    if v.total == 0 || v.h == 0 {
        return;
    }
    if v.total <= v.h {
        *v.scroll = 0;
        return;
    }
    let t = line.saturating_sub(v.h / 2).min(v.total - v.h);
    *v.scroll = v.total - t - v.h;
}

/// The position bar geometry (section 3). The track is the pane
/// height `h`; one cell maps to `total / h` log lines, rounded up.
/// The thumb bottom sits `scroll` lines above the track bottom, and
/// the tail marker anchors the latest position. The cursor marker
/// is the browse-mode addition.
pub struct BarGeom {
    /// The top track cell of the thumb, 0-based from the top.
    pub thumb_top: usize,
    /// The thumb height in cells.
    pub thumb_h: usize,
    /// The tail marker cell (the track bottom).
    pub tail_cell: usize,
    /// The cursor marker cell, browse mode only.
    pub cursor_cell: Option<usize>,
}

pub fn bar_geometry(total: usize, h: usize, scroll: usize, cursor: Option<usize>) -> Option<BarGeom> {
    if total == 0 || h == 0 {
        return None;
    }
    // One track cell maps to `total / h` lines, rounded up, at least
    // one (section 3).
    let step = total.div_ceil(h);
    // The thumb height is `max(1, h * h / total)`, capped at the
    // track (section 3). A total that fits the view spans it.
    let thumb_h = (h * h / total).max(1).min(h);
    // The window is the last `h` lines after skipping `scroll`; its
    // bottom sits `scroll` lines above the track bottom.
    let s = scroll.min(total - 1);
    let bottom_line = total - 1 - s;
    let thumb_bottom = bottom_line / step;
    let thumb_top = thumb_bottom.saturating_sub(thumb_h - 1);
    Some(BarGeom {
        thumb_top,
        thumb_h,
        tail_cell: h - 1,
        cursor_cell: cursor.map(|c| c.min(total - 1) / step),
    })
}

/// The gutter width of section 4.3: the digit count of `total` plus
/// one trailing space, like neovim.
pub fn gutter_width(total: usize) -> usize {
    if total == 0 {
        return 1;
    }
    total.to_string().len() + 1
}

/// The gutter number of a visible line (section 4.3): the absolute
/// line number at the cursor, the relative distance elsewhere. Lines
/// are zero-based; the absolute number is the line plus one.
pub fn gutter_number(cursor: usize, line: usize) -> u32 {
    if line == cursor {
        (line + 1) as u32
    } else if line > cursor {
        (line - cursor) as u32
    } else {
        (cursor - line) as u32
    }
}

/// The `ignorecase` + `smartcase` compile (section 7.3): a pattern
/// without an uppercase compiles case-blind, with one case-sensitive.
fn compile_smart(pat: &str) -> Result<Regex, regex::Error> {
    let case_blind = !pat.chars().any(|c| c.is_uppercase());
    if case_blind {
        Regex::new(&format!("(?i){pat}"))
    } else {
        Regex::new(pat)
    }
}

/// The next match strictly after / before `origin`, in `dir`. The
/// match wraps the ends; a pattern with no match at all yields
/// `None` (the motion is a no-op). `origin` is exclusive: a match
/// starting at the origin position is skipped, so a repeat never
/// re-finds the match the cursor sits on.
fn next_match(
    texts: &[String],
    re: &Regex,
    origin: (usize, usize),
    dir: Dir,
) -> Option<(usize, usize)> {
    let mut matches: Vec<(usize, usize)> = Vec::new();
    for (i, t) in texts.iter().enumerate() {
        for m in re.find_iter(t) {
            matches.push((i, m.start()));
        }
    }
    if matches.is_empty() {
        return None;
    }
    match dir {
        Dir::Forward => {
            for m in &matches {
                if m.0 > origin.0 || (m.0 == origin.0 && m.1 > origin.1) {
                    return Some(*m);
                }
            }
            Some(matches[0])
        }
        Dir::Backward => {
            for m in matches.iter().rev() {
                if m.0 < origin.0 || (m.0 == origin.0 && m.1 < origin.1) {
                    return Some(*m);
                }
            }
            Some(*matches.last().unwrap())
        }
    }
}

/// The word under the cursor: the longest alphanumeric-plus-
/// underscore run (section 7.3). On a non-word character the search
/// takes the next word forward, or the previous word backward.
/// Returns `(word, start, end, in_word)`, the end exclusive, and
/// `in_word` says the cursor character is a word character.
fn word_under_cursor(
    text: &str,
    col: usize,
    dir: Dir,
) -> Option<(String, usize, usize, bool)> {
    let chars: Vec<char> = text.chars().collect();
    let col = col.min(chars.len());
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    // The cursor sits in a word: extend to its run.
    if col > 0 && is_word(chars[col]) {
        let mut s = col;
        while s > 0 && is_word(chars[s - 1]) {
            s -= 1;
        }
        let mut e = col + 1;
        while e < chars.len() && is_word(chars[e]) {
            e += 1;
        }
        return Some((chars[s..e].iter().collect(), s, e, true));
    }
    match dir {
        Dir::Forward => {
            let mut i = col;
            while i < chars.len() && !is_word(chars[i]) {
                i += 1;
            }
            if i >= chars.len() {
                return None;
            }
            let s = i;
            while i < chars.len() && is_word(chars[i]) {
                i += 1;
            }
            Some((chars[s..i].iter().collect(), s, i, false))
        }
        Dir::Backward => {
            let mut i = col;
            while i > 0 && !is_word(chars[i - 1]) {
                i -= 1;
            }
            if i == 0 {
                return None;
            }
            let e = i;
            while i > 0 && is_word(chars[i - 1]) {
                i -= 1;
            }
            Some((chars[i..e].iter().collect(), i, e, false))
        }
    }
}

// ── conformance tests (section 9, the state-machine rows) ─────────

#[cfg(test)]
mod tests {
    use super::*;

    /// A bare view: `total` lines, one text each.
    fn lines(total: usize) -> Vec<String> {
        (0..total).map(|i| format!("line {i}")).collect()
    }

    fn v<'a>(texts: &'a [String], h: usize, scroll: &'a mut usize) -> View<'a> {
        View {
            total: texts.len(),
            h,
            scroll,
            half: h.saturating_sub(1) / 2,
            texts,
        }
    }

    /// A browse session on `total` lines, entered with the cursor on
    /// the first visible line of the scroll-0 tail view.
    fn entered(texts: &[String], h: usize, scroll: &mut usize) -> Browse {
        let mut b = Browse::new();
        b.enter();
        b.sync(texts.len(), h, scroll, false);
        b
    }

    // The `:N` rows of section 9.
    #[test]
    fn goto_line_42_of_100() {
        let texts = lines(100);
        let mut scroll = 0usize;
        let mut b = entered(&texts, 24, &mut scroll);
        let mut view = v(&texts, 24, &mut scroll);
        assert!(b.key(Key::Char(':'), &mut view).is_none());
        assert!(b.key(Key::Char('4'), &mut view).is_none());
        assert!(b.key(Key::Char('2'), &mut view).is_none());
        assert!(b.key(Key::Enter, &mut view).is_none());
        assert_eq!(b.line_col().0, 41, ":42 lands on line 42");
    }

    #[test]
    fn goto_clamps_to_the_last_line() {
        let texts = lines(100);
        let mut scroll = 0usize;
        let mut b = entered(&texts, 24, &mut scroll);
        let mut view = v(&texts, 24, &mut scroll);
        assert!(b.key(Key::Char(':'), &mut view).is_none());
        for c in ['9', '9', '9'] {
            b.key(Key::Char(c), &mut view);
        }
        assert!(b.key(Key::Enter, &mut view).is_none());
        assert_eq!(b.line_col().0, 99, ":999 clamps to line 100");
    }

    #[test]
    fn goto_suffixes_are_ignored() {
        let texts = lines(100);
        let mut scroll = 0usize;
        let mut b = entered(&texts, 24, &mut scroll);
        let mut view = v(&texts, 24, &mut scroll);
        for c in [':', '4', '2', 'j'] {
            b.key(Key::Char(c), &mut view);
        }
        assert!(b.key(Key::Enter, &mut view).is_none());
        assert_eq!(b.line_col().0, 41, ":42j ignores the suffix");
    }

    #[test]
    fn goto_zero_hints_and_moves_nothing() {
        let texts = lines(100);
        let mut scroll = 0usize;
        let mut b = entered(&texts, 24, &mut scroll);
        let line0 = b.line_col().0;
        let mut view = v(&texts, 24, &mut scroll);
        for c in [':', '0'] {
            b.key(Key::Char(c), &mut view);
        }
        let hint = b.key(Key::Enter, &mut view);
        assert_eq!(hint.as_deref(), Some("line: 1..100"), ":0 hints the range");
        assert_eq!(b.line_col().0, line0, ":0 moves nothing");
    }

    #[test]
    fn goto_non_number_hints_and_moves_nothing() {
        let texts = lines(100);
        let mut scroll = 0usize;
        let mut b = entered(&texts, 24, &mut scroll);
        let line0 = b.line_col().0;
        let mut view = v(&texts, 24, &mut scroll);
        b.key(Key::Char(':'), &mut view);
        let hint = b.key(Key::Char('a'), &mut view);
        assert_eq!(hint.as_deref(), Some("line: 1..100"));
        assert_eq!(b.line_col().0, line0, "a non-number moves nothing");
    }

    #[test]
    fn goto_huge_number_clamps_to_the_last_line() {
        let texts = lines(100);
        let mut scroll = 0usize;
        let mut b = entered(&texts, 24, &mut scroll);
        let mut view = v(&texts, 24, &mut scroll);
        b.key(Key::Char(':'), &mut view);
        for c in ['1', '2', '3', '4', '5', '6', '7', '8', '9', '0'] {
            b.key(Key::Char(c), &mut view);
        }
        assert!(b.key(Key::Enter, &mut view).is_none());
        assert_eq!(b.line_col().0, 99, "a huge :N clamps to the last line");
    }

    // The `j` / `k` rows of section 9.
    #[test]
    fn k_at_line_one_does_not_move() {
        let texts = lines(100);
        let mut scroll = 0usize;
        let mut b = entered(&texts, 24, &mut scroll);
        b.line = 0;
        let mut view = v(&texts, 24, &mut scroll);
        assert!(b.key(Key::Char('k'), &mut view).is_none());
        assert_eq!(b.line_col().0, 0, "the floor holds");
    }

    #[test]
    fn counted_j_from_line_3() {
        let texts = lines(100);
        let mut scroll = 0usize;
        let mut b = entered(&texts, 24, &mut scroll);
        b.line = 2;
        let mut view = v(&texts, 24, &mut scroll);
        b.key(Key::Char('5'), &mut view);
        assert!(b.key(Key::Char('j'), &mut view).is_none());
        assert_eq!(b.line_col().0, 7, "5j from line 3 lands on line 8");
    }

    #[test]
    fn zero_count_is_a_no_op() {
        let texts = lines(100);
        let mut scroll = 0usize;
        let mut b = entered(&texts, 24, &mut scroll);
        b.line = 5;
        let mut view = v(&texts, 24, &mut scroll);
        b.key(Key::Char('0'), &mut view);
        assert!(b.key(Key::Char('j'), &mut view).is_none());
        assert_eq!(b.line_col().0, 5, "0j does not move");
    }

    // The `gg` / `G` rows of section 9.
    #[test]
    fn gg_lands_on_line_one_at_the_top() {
        let texts = lines(100);
        let mut scroll = 76usize; // the view sits mid-log
        let mut b = entered(&texts, 24, &mut scroll);
        b.line = 39;
        let mut view = v(&texts, 24, &mut scroll);
        b.key(Key::Char('g'), &mut view);
        assert!(b.key(Key::Char('g'), &mut view).is_none());
        assert_eq!(b.line_col().0, 0, "gg lands on line 1");
        assert_eq!(scroll, 76, "the view is at the top (the scroll covers the log)");
    }

    #[test]
    fn counted_gg_lands_on_line_5() {
        let texts = lines(100);
        let mut scroll = 76usize;
        let mut b = entered(&texts, 24, &mut scroll);
        b.line = 39;
        let mut view = v(&texts, 24, &mut scroll);
        b.key(Key::Char('5'), &mut view);
        b.key(Key::Char('g'), &mut view);
        assert!(b.key(Key::Char('g'), &mut view).is_none());
        assert_eq!(b.line_col().0, 4, "5gg lands on line 5");
    }

    #[test]
    fn g_lands_on_the_last_line_at_the_tail() {
        let texts = lines(100);
        let mut scroll = 0usize;
        let mut b = entered(&texts, 24, &mut scroll);
        b.line = 39;
        let mut view = v(&texts, 24, &mut scroll);
        assert!(b.key(Key::Char('G'), &mut view).is_none());
        assert_eq!(b.line_col().0, 99, "G lands on the last line");
        assert_eq!(scroll, 0, "the view is at the tail");
    }

    // The unbound-key row of section 9.
    #[test]
    fn unbound_key_hints_and_moves_nothing() {
        let texts = lines(100);
        let mut scroll = 0usize;
        let mut b = entered(&texts, 24, &mut scroll);
        let start = b.line_col();
        let mut view = v(&texts, 24, &mut scroll);
        let hint = b.key(Key::Char('x'), &mut view);
        assert_eq!(hint.as_deref(), Some(BROWSE_HINT));
        assert_eq!(b.line_col(), start, "no state change");
    }

    // The `Ctrl+U` strand row of section 9.
    #[test]
    fn ctrl_u_strands_the_cursor_to_the_new_top() {
        let texts = lines(100);
        let mut scroll = 50usize;
        let mut b = entered(&texts, 24, &mut scroll);
        // The cursor sits on the view top line.
        let start = 100 - 50 - 24;
        b.line = start;
        let mut view = v(&texts, 24, &mut scroll);
        assert!(b.key(Key::CtrlU, &mut view).is_none());
        assert_eq!(scroll, 50 + 11, "the view moves up half a page");
        let new_start = 100 - scroll - 24;
        assert_eq!(b.line_col().0, new_start, "the cursor lands on the new top line");
    }

    #[test]
    fn ctrl_d_strands_the_cursor_to_the_new_bottom() {
        let texts = lines(100);
        let mut scroll = 50usize;
        let mut b = entered(&texts, 24, &mut scroll);
        // The cursor sits on the view top line (start = 26).
        b.line = 100 - 50 - 24;
        let mut view = v(&texts, 24, &mut scroll);
        assert!(b.key(Key::CtrlD, &mut view).is_none());
        assert_eq!(scroll, 50 - 11, "the view moves down half a page");
        let new_start = 100usize.saturating_sub(scroll + 24);
        let new_bottom = new_start + 23;
        assert_eq!(
            b.line_col().0, new_bottom,
            "the stranded cursor lands on the new bottom line"
        );
    }

    // The scrolloff row of section 9.
    #[test]
    fn scrolloff_clears_the_margin() {
        let texts = lines(100);
        let mut scroll = 50usize;
        let mut b = entered(&texts, 24, &mut scroll);
        // The view holds lines 26..49; the cursor two lines above
        // the bottom (the bottom margin holds 1 line: violated).
        b.line = 100 - 50 - 24 + 21;
        let mut view = v(&texts, 24, &mut scroll);
        assert!(b.key(Key::Char('j'), &mut view).is_none());
        assert_eq!(
            b.line_col().0,
            100 - 50 - 24 + 22,
            "the cursor moved down one line"
        );
        assert_eq!(
            scroll,
            48,
            "the view scrolled two lines, the minimum to clear the margin"
        );
    }

    // The grow-while-browsing row of section 9.
    #[test]
    fn grow_keeps_the_view_and_pins_the_cursor() {
        let mut texts = lines(100);
        let mut scroll = 0usize;
        let mut b = entered(&texts, 24, &mut scroll);
        let line0 = b.line_col().0;
        // A new event lands: the total rises, the view does not follow.
        texts.push("line 100".into());
        let view = v(&texts, 24, &mut scroll);
        let before = *view.scroll;
        b.sync(101, 24, view.scroll, true);
        assert_eq!(b.line_col().0, line0, "the cursor pins to its line");
        assert_eq!(*view.scroll, before, "the view does not follow the growth");
    }

    // The bar geometry rows of section 9.
    #[test]
    fn bar_geometry_row() {
        let g = bar_geometry(1000, 24, 500, None).unwrap();
        assert_eq!(g.thumb_h, 1, "the thumb height is max(1, 24*24/1000)");
        assert_eq!(g.tail_cell, 23);
        let step = 1000usize.div_ceil(24);
        assert_eq!(
            (1000 - 500 - 1) / step,
            g.thumb_top + g.thumb_h - 1,
            "the thumb bottom cell holds the window bottom line"
        );
    }

    #[test]
    fn bar_fits_the_view_when_the_total_is_small() {
        let g = bar_geometry(10, 24, 0, None).unwrap();
        assert_eq!(
            g.thumb_h, 24,
            "the thumb spans the full track: the log is shorter than the view"
        );
        assert_eq!(g.thumb_top, 0);
    }

    #[test]
    fn bar_cursor_marker_sits_on_the_cursor_line() {
        let g = bar_geometry(100, 24, 0, Some(99)).unwrap();
        let step = 100usize.div_ceil(24);
        assert_eq!(
            g.cursor_cell,
            Some(99 / step),
            "the cursor line maps to its cell"
        );
        assert_eq!(g.tail_cell, 23, "the tail marker anchors the last line");
    }

    // The gutter rows of section 9.
    #[test]
    fn gutter_numbering() {
        let cursor = 49; // line 50, zero-based
        assert_eq!(gutter_number(cursor, 49), 50, "the cursor line shows the absolute number");
        assert_eq!(gutter_number(cursor, 39), 10, "line 40 shows the distance 10");
        assert_eq!(gutter_number(cursor, 59), 10, "line 60 shows the distance 10");
        assert_eq!(gutter_number(cursor, 0), 49, "line 1 shows the distance 49");
    }

    #[test]
    fn gutter_width_is_digits_plus_one() {
        assert_eq!(gutter_width(100), 4, "three digits plus one space");
        assert_eq!(gutter_width(9), 2);
        assert_eq!(gutter_width(1000), 5);
    }

    // The stage-2 rows of section 9.
    #[test]
    fn forward_search_jumps_live_and_highlights() {
        let texts: Vec<String> = vec![
            "alpha".into(),
            "error one".into(),
            "mid".into(),
            "ERROR two".into(),
        ];
        let mut scroll = 0usize;
        let mut b = entered(&texts, 24, &mut scroll);
        let mut view = v(&texts, 24, &mut scroll);
        b.key(Key::Char('/'), &mut view);
        // `incsearch`: the first `e` already lands on the match.
        b.key(Key::Char('e'), &mut view);
        assert_eq!(b.line_col().0, 1, "the live jump lands on the first match");
        for c in ['r', 'r', 'o'] {
            b.key(Key::Char(c), &mut view);
        }
        assert_eq!(b.line_col().0, 1, "the pattern keeps matching");
        assert!(b.key(Key::Enter, &mut view).is_none());
        let (hl, active) = b.highlight_lines(4, &texts);
        assert!(hl.contains(&1) && hl.contains(&3), "both match lines highlight");
        assert_eq!(active, Some((1, 0)), "the current match is the accent line");
    }

    #[test]
    fn smartcase_is_case_sensitive_with_an_uppercase() {
        let texts: Vec<String> = vec!["error".into(), "ERR now".into(), "err".into()];
        let mut scroll = 0usize;
        let mut b = entered(&texts, 24, &mut scroll);
        let mut view = v(&texts, 24, &mut scroll);
        b.key(Key::Char('/'), &mut view);
        for c in ['E', 'R', 'R'] {
            b.key(Key::Char(c), &mut view);
        }
        assert!(b.key(Key::Enter, &mut view).is_none());
        let (hl, _) = b.highlight_lines(3, &texts);
        assert_eq!(*hl, HashSet::from([1]), "only ERR matches");
    }

    #[test]
    fn lowercase_pattern_matches_case_blind() {
        let texts: Vec<String> = vec!["error".into(), "ERR now".into(), "err".into()];
        let mut scroll = 0usize;
        let mut b = entered(&texts, 24, &mut scroll);
        let mut view = v(&texts, 24, &mut scroll);
        b.key(Key::Char('/'), &mut view);
        for c in ['e', 'r', 'r'] {
            b.key(Key::Char(c), &mut view);
        }
        assert!(b.key(Key::Enter, &mut view).is_none());
        let (hl, _) = b.highlight_lines(3, &texts);
        assert_eq!(*hl, HashSet::from([0, 1, 2]), "case-blind matches all");
    }

    #[test]
    fn n_wraps_to_the_first_match_and_centers() {
        let total = 40;
        let texts: Vec<String> = (0..total)
            .map(|i| if i == 10 { "match a".to_string() } else if i == 30 { "match b".to_string() } else { "x".to_string() })
            .collect();
        let mut scroll = 0usize;
        let mut b = entered(&texts, 24, &mut scroll);
        let mut view = v(&texts, 24, &mut scroll);
        b.key(Key::Char('/'), &mut view);
        b.key(Key::Char('m'), &mut view);
        assert!(b.key(Key::Enter, &mut view).is_none());
        // The forward commit lands on the first match after the origin.
        assert_eq!(b.line_col().0, 30, "the commit jumps forward to the match");
        // The next match, forward: the wrap.
        assert!(b.key(Key::Char('n'), &mut view).is_none());
        assert_eq!(b.line_col().0, 10, "n wraps to the earlier match");
    }

    #[test]
    fn n_after_a_forward_jump_restores_the_saved_view() {
        let total = 40;
        let texts: Vec<String> = (0..total)
            .map(|i| if i == 10 { "match a".to_string() } else if i == 30 { "match b".to_string() } else { "x".to_string() })
            .collect();
        let mut scroll = 0usize;
        let mut b = entered(&texts, 24, &mut scroll);
        let mut view = v(&texts, 24, &mut scroll);
        b.key(Key::Char('/'), &mut view);
        b.key(Key::Char('m'), &mut view);
        assert!(b.key(Key::Enter, &mut view).is_none());
        let view_after_commit = *view.scroll;
        assert!(b.key(Key::Char('n'), &mut view).is_none());
        assert_ne!(*view.scroll, view_after_commit, "the forward jump moved the view");
        assert!(b.key(Key::Char('N'), &mut view).is_none());
        assert_eq!(
            *view.scroll,
            view_after_commit,
            "the backward N restores the view held before the forward jump"
        );
        assert_eq!(b.line_col().0, 30, "the cursor returns to the earlier match");
    }

    #[test]
    fn a_bad_pattern_keeps_the_last_valid_one() {
        let texts: Vec<String> = vec!["foo(bar".into(), "foo_bar".into()];
        let mut scroll = 0usize;
        let mut b = entered(&texts, 24, &mut scroll);
        let mut view = v(&texts, 24, &mut scroll);
        b.key(Key::Char('/'), &mut view);
        for c in ['f', 'o', 'o'] {
            b.key(Key::Char(c), &mut view);
        }
        assert!(b.key(Key::Enter, &mut view).is_none());
        // A new, invalid pattern: the last valid pattern stays
        // active, and a repeat still runs it.
        b.key(Key::Char('/'), &mut view);
        b.key(Key::Char('('), &mut view);
        assert!(b.key(Key::Esc, &mut view).is_none());
        assert_eq!(b.last_pattern(), Some("foo"), "the last valid pattern stays");
        assert!(b.key(Key::Char('n'), &mut view).is_none());
        assert!(b.active_match().is_some(), "the repeat re-activates the match");
    }

    #[test]
    fn star_searches_the_word_under_the_cursor() {
        let texts: Vec<String> = vec![
            "the foo_bar sits here".into(),
            "nothing".into(),
            "another foo_bar down".into(),
        ];
        let mut scroll = 0usize;
        let mut b = entered(&texts, 24, &mut scroll);
        // The cursor on the word `foo_bar` of the first line.
        b.line = 0;
        b.col = 4; // on the `f`
        let mut view = v(&texts, 24, &mut scroll);
        assert!(b.key(Key::Char('*'), &mut view).is_none());
        assert_eq!(b.line_col().0, 2, "* searches the escaped word forward");
    }

    #[test]
    fn esc_clears_the_highlight_and_stays_in_browse() {
        let texts: Vec<String> = vec!["error".into(), "ok".into()];
        let mut scroll = 0usize;
        let mut b = entered(&texts, 24, &mut scroll);
        let mut view = v(&texts, 24, &mut scroll);
        b.key(Key::Char('/'), &mut view);
        b.key(Key::Char('e'), &mut view);
        assert!(b.key(Key::Enter, &mut view).is_none());
        assert!(b.key(Key::Esc, &mut view).is_none());
        assert!(b.active(), "Esc never leaves browse mode");
        assert!(b.active_match().is_none(), "Esc clears the active highlight");
        assert_eq!(b.last_pattern(), Some("e"), "the pattern stays for a repeat");
    }

    // The view-follow helper, direct.
    #[test]
    fn follow_view_pins_the_edges() {
        // At the tail, a bottom-margin violation pins the view.
        assert_eq!(follow_view(100, 24, 99, 0), 0);
        // At the top, a top-margin violation pins the view.
        assert_eq!(follow_view(100, 24, 0, 76), 76);
        // A mid view already satisfying both margins moves nothing.
        assert_eq!(follow_view(100, 24, 30, 50), 50);
    }

    #[test]
    fn follow_view_moves_the_minimum() {
        // The cursor three lines above the view bottom: one line
        // of scroll clears the margin.
        assert_eq!(follow_view(100, 24, 47, 50), 49);
    }

    // The `h` / `l` clamp row of section 9.
    #[test]
    fn h_and_l_clamp_at_the_edges() {
        let texts = vec!["abc".to_string()];
        let mut scroll = 0usize;
        let mut b = entered(&texts, 24, &mut scroll);
        b.line = 0;
        b.col = 0;
        let mut view = v(&texts, 24, &mut scroll);
        assert!(b.key(Key::Char('h'), &mut view).is_none());
        assert_eq!(b.col, 0, "`h` at col 0 moves nothing");
        assert!(b.key(Key::Char('l'), &mut view).is_none());
        assert_eq!(b.col, 1);
        for _ in 0..10 {
            b.key(Key::Char('l'), &mut view);
        }
        assert_eq!(b.col, 3, "`l` clamps one past the last character");
    }

    // The `resize while browsing` row of section 9: the rewrap
    // re-centers, the event growth does not follow.
    #[test]
    fn rewrap_growth_recenters_the_view() {
        let texts = lines(100);
        let mut scroll = 50usize;
        let mut b = entered(&texts, 24, &mut scroll);
        b.line = 48;
        let view = v(&texts, 24, &mut scroll);
        let before = *view.scroll;
        // The pane narrows, the wrap reflows, no event lands:
        // `grew = false`, the view re-centers (section 4.7).
        b.sync(120, 24, view.scroll, false);
        assert_ne!(*view.scroll, before, "the rewrap re-centers the view");
        // An event growth keeps the view put (section 4.6).
        let kept = *view.scroll;
        b.sync(121, 24, view.scroll, true);
        assert_eq!(*view.scroll, kept, "the event growth does not follow");
    }

    #[test]
    fn a_height_change_recenters_the_view() {
        // The pane resize drops the height: the view re-centers on
        // the cursor with the scrolloff margins (section 4.7).
        let texts = lines(100);
        let mut scroll = 50usize;
        let mut b = entered(&texts, 24, &mut scroll);
        b.line = 48;
        let view = v(&texts, 16, &mut scroll);
        b.sync(100, 16, view.scroll, false);
        assert_eq!(*view.scroll, 48, "the view re-centers on the cursor");
    }
}
