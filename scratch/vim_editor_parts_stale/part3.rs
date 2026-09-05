
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
                                &self.lines, cursor, count, c, &mut self.last_char_search,
                            );
                            self.run_motion(res);
                        }
                        'F' => {
                            let res = find_char_backward(
                                &self.lines, cursor, count, c, &mut self.last_char_search,
                            );
                            self.run_motion(res);
                        }
                        't' => {
                            let res = till_char_forward(
                                &self.lines, cursor, count, c, &mut self.last_char_search,
                            );
                            self.run_motion(res);
                        }
                        _ => {
                            let res = till_char_backward(
                                &self.lines, cursor, count, c, &mut self.last_char_search,
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
                // Vim: l stops on the last character; it never
                // wraps to the next line (the compat fix).
                let len = line_len(&self.lines, cursor.0);
                let target = if len == 0 {
                    0
                } else {
                    (cursor.1.min(len - 1) + motion_count as usize).min(len - 1)
                };
                let res = MotionResult {
                    pos: (cursor.0, target),
                    linewise: false,
                    inclusive: true,
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
                let res = match c {
                    'b' => word_backward(&self.lines, cursor, motion_count),
                    'e' => word_end(&self.lines, cursor, motion_count),
                    'W' => WORD_backward(&self.lines, cursor, motion_count),
                    'B' => WORD_backward(&self.lines, cursor, motion_count),
                    _ => WORD_end(&self.lines, cursor, motion_count),
                };
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
                let n = if count_explicit { count } else { self.lines.len() as u32 };
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
                        // Plain normal mode: Esc is a no-op.
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
            && chars_of(&self.lines, cursor.0)
                [cursor.1..]
                .iter()
                .any(|c| !c.is_whitespace())
        {
            res = MotionResult {
                pos: (
                    cursor.0,
                    line_len(&self.lines, cursor.0).saturating_sub(1),
                ),
                linewise: false,
                inclusive: true,
            };
        }
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
        for i in self.col..end {
            chs[i] = c;
        }
        self.lines[self.row] = chs.into_iter().collect();
        self.col = end - 1;
    }

    // ── join lines (the pi-vim `J`) ─────────────────────────────

    /// `J` joins the next `count` lines: a single space between
    /// non-empty lines, and no leading blanks on the joined line.
    fn join_lines(&mut self, count: u32) {
        self.begin_change_recording('J', count);
        let join_count = (count.max(1) as usize)
            .min(self.lines.len().saturating_sub(self.row + 1));
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
            self.col = join_col.min(final_len.saturating_sub(1).max(0));
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
            for i in self.col..end {
                chs[i] = if chs[i].is_lowercase() {
                    chs[i].to_uppercase().next().unwrap_or(chs[i])
                } else {
                    chs[i].to_lowercase().next().unwrap_or(chs[i])
                };
            }
            self.lines[self.row] = chs.into_iter().collect();
            let new_len = line_len(&self.lines, self.row);
            self.col = end.min(new_len.saturating_sub(1).max(0));
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
                        .min(line_len(&self.lines, self.row).saturating_sub(1).max(0))
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
                        .min(new_len.saturating_sub(1).max(0));
                } else {
                    let paste_lines: Vec<&str> = text.split('\n').collect();
                    let before_s: String = chs[..insert_col].iter().collect();
                    let after_s: String = chs[insert_col..].iter().collect();
                    let mut here: Vec<String> = vec![before_s + paste_lines[0]];
                    for mid in &paste_lines[1..paste_lines.len() - 1] {
                        here.push(mid.to_string());
                    }
                    here.push(
                        paste_lines[paste_lines.len() - 1].to_string() + &after_s,
                    );
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
    fn replay_last_change(&mut self, count_override: u32) {
        let rec = match self.last_change.clone() {
            Some(r) => r,
            None => return,
        };
        self.is_replaying = true;
        let n = if count_override > 0 { count_override } else { rec.count };
        self.count = n;
        self.count_started = n > 0;
        for k in rec.keys.iter() {
            self.normal_press(Key::Char(*k));
        }
        if rec.entered_insert {
            let types = if count_override > 0 { count_override as usize } else { 1 };
            let was_replace = self.mode == Mode::Replace;
            let text = rec.inserted_text.clone();
            for _ in 0..types {
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
                }
            }
        };
        let mut pos = (self.row, self.col);
        for _ in 0..count.max(1) {
            match find_next_match(&self.lines, pos, &pattern, forward) {
                Some(m) => pos = m,
                None => break,
            }
        }
        MotionResult { pos, linewise: false, inclusive: false }
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
                }
            }
        };
        self.last_search_pattern = Some(word.clone());
        self.last_search_forward = forward;
        match find_next_match(&self.lines, (self.row, self.col), &word, forward) {
            Some(m) => MotionResult { pos: m, linewise: false, inclusive: false },
            None => MotionResult {
                pos: (self.row, self.col),
                linewise: false,
                inclusive: false,
            },
        }
    }
}
