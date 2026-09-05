
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
                            &self.lines, cursor, count, c, &mut self.last_char_search,
                        ),
                        'F' => find_char_backward(
                            &self.lines, cursor, count, c, &mut self.last_char_search,
                        ),
                        't' => till_char_forward(
                            &self.lines, cursor, count, c, &mut self.last_char_search,
                        ),
                        _ => till_char_backward(
                            &self.lines, cursor, count, c, &mut self.last_char_search,
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
        let (start, end) = if anchor < cursor
            || (anchor.0 == cursor.0 && anchor.1 <= cursor.1)
        {
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
                let paste_lines: Vec<String> =
                    reg.text.split('\n').map(str::to_string).collect();
                if reg.linewise {
                    if range.linewise {
                        new_lines.splice(pos.0..pos.0, paste_lines.clone());
                    } else {
                        new_lines.splice(pos.0 + 1..pos.0 + 1, paste_lines.clone());
                    }
                    self.push_undo();
                    self.lines = new_lines;
                    let target_line = if range.linewise {
                        pos.0
                    } else {
                        pos.0 + 1
                    };
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
                            .min(line_len(&self.lines, self.row).saturating_sub(1).max(0));
                    } else {
                        let before_s: String = lchs[..cut].iter().collect();
                        let after_s: String = lchs[cut..].iter().collect();
                        let mut merged: Vec<String> =
                            vec![before_s + paste_lines[0]];
                        for mid in &paste_lines[1..paste_lines.len() - 1] {
                            merged.push(mid.clone());
                        }
                        merged.push(
                            paste_lines[paste_lines.len() - 1].to_string() + &after_s,
                        );
                        self.push_undo();
                        new_lines.splice(pos.0..pos.0 + 1, merged);
                        self.lines = new_lines;
                        let last_idx = (pos.0 + paste_lines.len() - 1)
                            .min(self.lines.len().saturating_sub(1));
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
            self.lines[range.start.0] = toggle_case_line(
                &fchs[range.start.1..].iter().collect::<String>(),
            );
            for ln in range.start.0 + 1..range.end.0 {
                self.lines[ln] = toggle_case_line(&self.lines[ln]);
            }
            let last = self.lines[range.end.0].clone();
            let lchs: Vec<char> = last.chars().collect();
            let head: String = lchs[..range.end.1 + 1].iter().collect();
            let tail: String = lchs[range.end.1 + 1..].iter().collect();
            self.lines[range.end.0] =
                format!("{}{}", toggle_case_line(&head), tail);
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
                self.row = (self.row + count.max(1) as usize)
                    .min(self.lines.len().saturating_sub(1));
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
                    'b' | 'B' => word_backward(&self.lines, cursor, count),
                    'e' => word_end(&self.lines, cursor, count),
                    'W' => WORD_backward(&self.lines, cursor, count),
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
                let res = self.search_repeat(count, if forward {
                    self.last_search_forward
                } else {
                    !self.last_search_forward
                });
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
