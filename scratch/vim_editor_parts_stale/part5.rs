
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
                if !self.search_input.is_empty() {
                    self.last_search_pattern = Some(std::mem::take(&mut self.search_input));
                }
                self.search_active = false;
                if let Some(pat) = self.last_search_pattern.clone() {
                    let forward = self.last_search_forward;
                    if let Some(m) = find_next_match(&self.lines, (self.row, self.col), &pat, forward)
                    {
                        self.go_to(m);
                    }
                }
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
    MotionResult { pos, linewise: false, inclusive: false }
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
    MotionResult { pos, linewise: false, inclusive: false }
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
    MotionResult { pos, linewise: false, inclusive: true }
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

/// `W` — the start of the next WORD (blank-delimited).
fn WORD_forward(lines: &[String], cursor: (usize, usize), count: u32) -> MotionResult {
    let mut pos = cursor;
    for _ in 0..count.max(1) {
        pos = next_WORD_start(lines, pos.0, pos.1);
    }
    MotionResult { pos, linewise: false, inclusive: false }
}

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

/// `B` — the start of the previous WORD.
fn WORD_backward(lines: &[String], cursor: (usize, usize), count: u32) -> MotionResult {
    let mut pos = cursor;
    for _ in 0..count.max(1) {
        pos = prev_WORD_start(lines, pos.0, pos.1);
    }
    MotionResult { pos, linewise: false, inclusive: false }
}

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

/// `E` — the end of the current / next WORD (inclusive motion).
fn WORD_end(lines: &[String], cursor: (usize, usize), count: u32) -> MotionResult {
    let mut pos = cursor;
    for _ in 0..count.max(1) {
        pos = next_WORD_end(lines, pos.0, pos.1);
    }
    MotionResult { pos, linewise: false, inclusive: true }
}

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
fn first_nonblank_motion(
    lines: &[String],
    cursor: (usize, usize),
    _count: u32,
) -> MotionResult {
    MotionResult {
        pos: (cursor.0, first_nonblank(&lines[cursor.0])),
        linewise: false,
        inclusive: false,
    }
}

/// `0` — the start of the line.
fn line_start(_lines: &[String], cursor: (usize, usize), _count: u32) -> MotionResult {
    MotionResult { pos: (cursor.0, 0), linewise: false, inclusive: false }
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
        return MotionResult { pos: cursor, linewise: false, inclusive: true };
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
        return MotionResult { pos: cursor, linewise: false, inclusive: true };
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

fn paragraph_backward(
    lines: &[String],
    cursor: (usize, usize),
    count: u32,
) -> MotionResult {
    let mut line = cursor.0;
    for _ in 0..count.max(1) {
        while line > 0 && is_blank_line(&lines[line]) {
            line -= 1;
        }
        while line > 0 && !is_blank_line(&lines[line]) {
            line -= 1;
        }
    }
    MotionResult { pos: (line, 0), linewise: true, inclusive: false }
}

fn paragraph_forward(
    lines: &[String],
    cursor: (usize, usize),
    count: u32,
) -> MotionResult {
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
    MotionResult { pos: (line, 0), linewise: true, inclusive: false }
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
    BRACKET_PAIRS
        .iter()
        .find(|(o, _)| *o == c)
        .map(|(_, m)| *m)
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
        while line >= 0 {
            let lt = chars_of(lines, line);
            while (col as i64) >= 0 {
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
                col = col.saturating_sub(1);
                if col == 0 {
                    break;
                }
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
    let first = chars_of(lines, r.start.0)[r.start.1..].iter().collect::<String>();
    let mut out = vec![first];
    for i in r.start.0 + 1..r.end.0 {
        out.push(lines[i].clone());
    }
    let last_s = chars_of(lines, r.end.0);
    let end_col = if r.inclusive { r.end.1 + 1 } else { r.end.1 };
    out.push(last_s[..end_col.min(last_s.len())].iter().collect());
    out.join("\n")
}

/// Delete a range and return the new lines plus the cursor
/// position.
fn delete_range(
    lines: &[String],
    r: &OpRange,
) -> (Vec<String>, (usize, usize)) {
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
        let col = r.start.1.min(result_len.saturating_sub(1).max(0));
        return (new_lines, (r.start.0, col));
    }
    let first: Vec<char> = lines[r.start.0].chars().collect();
    let last: Vec<char> = lines[r.end.0].chars().collect();
    let end_col = if r.inclusive { r.end.1 + 1 } else { r.end.1 };
    let merged: String = first[r.start.1..]
        .iter()
        .collect::<String>()
        + &last[end_col.min(last.len())..].iter().collect::<String>();
    new_lines.splice(r.start.0..=r.end.0, std::iter::once(merged));
    if new_lines.is_empty() {
        new_lines.push(String::new());
    }
    let col = r.start.1.min(
        line_len(&new_lines, r.start.0).saturating_sub(1).max(0),
    );
    (new_lines, (r.start.0, col))
}

/// Indent the lines of a range by two spaces (the `>` operator).
fn indent_range(
    lines: &[String],
    r: &OpRange,
) -> (Vec<String>, (usize, usize)) {
    let mut nl = lines.to_vec();
    for i in r.start.0..=r.end.0 {
        if !nl[i].is_empty() {
            nl[i] = format!("  {}", nl[i]);
        }
    }
    let cursor = (
        r.start.0,
        first_nonblank(&nl[r.start.0.min(nl.len() - 1)]),
    );
    (nl, cursor)
}

/// Dedent the lines of a range (the `<` operator): remove up to
/// two leading spaces, or one leading tab.
fn dedent_range(
    lines: &[String],
    r: &OpRange,
) -> (Vec<String>, (usize, usize)) {
    let mut nl = lines.to_vec();
    for i in r.start.0..=r.end.0 {
        let chs: Vec<char> = nl[i].chars().collect();
        let mut removed = 0;
        while removed < 2 && removed < chs.len() && chs[removed] == ' ' {
            removed += 1;
        }
        if removed == 0 && chs.first() == Some(&'\t') {
            removed = 1;
        }
        nl[i] = chs[removed..].iter().collect();
    }
    let cursor = (
        r.start.0,
        first_nonblank(&nl[r.start.0.min(nl.len() - 1)]),
    );
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
            (
                lines.to_vec(),
                (r.start.0, r.start.1),
                false,
            )
        }
        '>' => indent_range(lines, r),
        '<' => dedent_range(lines, r),
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
                let sep = if existing.linewise || linewise { "\n" } else { "" };
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
                let sep = if existing.linewise || linewise { "\n" } else { "" };
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
                if let Some(prev) = registers.get(&(i - 1) as char) {
                    registers.insert(i as char, prev.clone());
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

fn range_on_line(
    row: usize,
    start: usize,
    end: usize,
) -> OpRange {
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
        while start > 0
            && !is_obj_word_char(chs[start - 1])
            && !is_blank_char(chs[start - 1])
        {
            start -= 1;
        }
        while end + 1 < chs.len()
            && !is_obj_word_char(chs[end + 1])
            && !is_blank_char(chs[end + 1])
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
        '"' => Some(if inner { inner_double_quote } else { a_double_quote }),
        '\'' => Some(if inner { inner_single_quote } else { a_single_quote }),
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
        let start_col = if ln == open_line {
            open_col + 1
        } else {
            0
        };
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
