
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
                    let copies =
                        vec![self.lines[self.row].clone();
                            self.open_line_repeat_count as usize - 1];
                    self.push_undo();
                    self.lines.splice(
                        self.row + 1..self.row + 1,
                        copies.iter().cloned(),
                    );
                    self.row += self.open_line_repeat_count as usize - 1;
                    self.col = self.col.saturating_sub(1);
                    self.open_line_repeat_count = 1;
                } else if self.col > 0 {
                    // Vim steps the cursor back one on Esc; at
                    // column 0 it stays (the pi-vim rule).
                    self.col -= 1;
                }
                self.finalize_change_recording();
                None
            }
            Key::CtrlC => {
                self.mode = Mode::Normal;
                self.finalize_change_recording();
                None
            }
            Key::Backspace => {
                if self.is_recording_insert() {
                    self.record_insert_backspace();
                }
                if self.col > 0 || self.row > 0 {
                    self.push_undo();
                    self.back_one();
                }
                None
            }
            Key::Delete => {
                let len = line_len(&self.lines, self.row);
                if self.col < len {
                    self.push_undo();
                    self.delete_char_at(self.col);
                }
                None
            }
            Key::Char(c) => {
                if self.is_recording_insert() {
                    self.record_insert_text(c);
                }
                self.push_undo();
                self.insert_char(c);
                None
            }
            Key::Enter => {
                if self.is_recording_insert() {
                    self.record_insert_text('\n');
                }
                self.push_undo();
                self.insert_char('\n');
                None
            }
            Key::Left => {
                self.col = self.col.saturating_sub(1);
                None
            }
            Key::Right => {
                let max = line_len(&self.lines, self.row);
                self.col = (self.col + 1).min(max);
                None
            }
            Key::Up => {
                self.row = self.row.saturating_sub(1);
                self.clamp_col();
                None
            }
            Key::Down => {
                self.row = (self.row + 1).min(self.lines.len().saturating_sub(1));
                self.clamp_col();
                None
            }
            Key::Home => {
                self.col = 0;
                None
            }
            Key::End => {
                self.col = line_len(&self.lines, self.row);
                None
            }
            _ => {
                return Some(
                    "insert mode: type · Ctrl-J newline · Esc normal".to_string(),
                )
            }
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
                None
            }
            Key::CtrlC => {
                self.mode = Mode::Normal;
                self.finalize_change_recording();
                self.replaced_chars.clear();
                None
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
                            chs.remove(col.min(chs.len().saturating_sub(1).max(0)));
                        }
                    }
                    self.lines[self.row] = chs.into_iter().collect();
                    self.col = col;
                    if self.is_recording_insert() {
                        self.record_insert_backspace();
                    }
                }
                None
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
                None
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
                None
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
        if let Some(mut r) = self.current_recording.take() {
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
