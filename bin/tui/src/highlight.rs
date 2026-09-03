//! Syntax highlighting for transcript content.
//!
//! Implements the fourth request of
//! `docs/tui_feature_requests_from_human.md`: syntax highlighting for
//! tool results where possible, and markdown syntax highlighting with
//! proper rendering for message `content`.
//!
//! These are pure functions: text in, styled segments out. No I/O, no
//! log vocabulary, no decision logic. The TUI does not render markdown
//! structurally; it colors the syntax so the structure reads on the
//! terminal.

use ratatui::style::{Modifier, Style};

/// One styled piece of text, ready for the word-wraper.
pub type Seg = (Style, String);

// ── palette ────────────────────────────────────────────────────
// Every color is 16-color safe so the highlighting survives a dim
// terminal.

// ── markdown ───────────────────────────────────────────────────

fn is_fence_delim(t: &str) -> bool {
    t.starts_with("```") || t.starts_with("~~~")
}

/// A heading: one to six `#`, then a space or end of line.
fn is_heading(t: &str) -> bool {
    let mut hashes = 0usize;
    for c in t.chars() {
        if c == '#' {
            hashes += 1;
            continue;
        }
        break;
    }
    (1..=6).contains(&hashes) && (t[hashes..].starts_with(' ') || t[hashes..].is_empty())
}

/// A list marker: `-`, `+`, or `*` followed by a space or end of
/// line, or a number plus `.` followed by a space or end of line.
/// Returns the marker and the rest of the line (including its
/// leading space, so the indent is preserved).
fn list_split(t: &str) -> Option<(String, &str)> {
    if let Some(r) = t.strip_prefix(['-', '+']) {
        if r.is_empty() || r.starts_with(' ') {
            return Some(("-".to_string(), r));
        }
        return None;
    }
    if let Some(r) = t.strip_prefix('*') {
        if r.is_empty() || r.starts_with(' ') {
            return Some(("*".to_string(), r));
        }
        return None;
    }
    let b = t.as_bytes();
    let mut i = 0usize;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i > 0 && i < b.len() && b[i] == b'.' {
        let after = &t[i + 1..];
        if after.is_empty() || after.starts_with(' ') {
            return Some((t[..i + 1].to_string(), after));
        }
    }
    None
}

enum Tok {
    Code,
    Bold,
    Italic,
    Link { j: usize },
}

/// The inline token at `i`, if any: the token kind and the exclusive
/// end index in `cs`. `Tok::Link` also carries `j`, the index of the
/// `]` that separates text from url.
fn next_token(cs: &[char], i: usize) -> Option<(Tok, usize)> {
    let n = cs.len();
    match cs[i] {
        '`' => cs[i + 1..]
            .iter()
            .position(|&ch| ch == '`')
            .map(|rel| (Tok::Code, i + 2 + rel)),
        '*' if i + 1 < n && cs[i + 1] == '*' => {
            // **bold**: close at the next `**`.
            cs[i + 2..]
                .iter()
                .position(|&ch| ch == '*')
                .filter(|&rel| {
                    let j = i + 2 + rel;
                    j + 1 < n && cs[j + 1] == '*'
                })
                .map(|rel| (Tok::Bold, i + 2 + rel + 2))
        }
        '*' => {
            // *italic*: close at the next lone `*`.
            cs[i + 1..]
                .iter()
                .position(|&ch| ch == '*')
                .filter(|&rel| {
                    let j = i + 1 + rel;
                    j + 1 >= n || cs[j + 1] != '*'
                })
                .map(|rel| (Tok::Italic, i + 1 + rel + 1))
        }
        '[' => {
            // [text](url)
            let j_rel = cs[i + 1..].iter().position(|&ch| ch == ']')?;
            let j = i + 1 + j_rel;
            if j + 1 >= n || cs[j + 1] != '(' {
                return None;
            }
            let p_rel = cs[j + 2..].iter().position(|&ch| ch == ')')?;
            Some((Tok::Link { j }, j + 2 + p_rel + 1))
        }
        _ => None,
    }
}

fn seg(cs: &[char], from: usize, to: usize) -> String {
    cs[from..to].iter().collect()
}

// ── presentation pass (docs/tui-markdown-render.md) ──────────
//
// The marker-free render: the raw-token functions above keep the
// markers for the extension transform path. This pass drops the
// marker text and keeps the style. The list bullet stays a visible
// bullet; the `#`, `>` runs and the emphasis stars drop; the
// backticks drop; the link text shows and its URL stays dimmed;
// the table rows draw as a box-drawing grid; the fence marker
// lines dim and the fence content stays literal.

use crate::color::{Palette, Role};

/// One hard line of message content as presentation segments:
/// the markers out, the styles in. `fence` carries the fenced-code
/// state across hard lines, like [`markdown_line`]. Palette colors
/// replace the 16-color styles; the plain runs stay at the default
/// style so the caller's `with_plain_base` pass paints them.
pub fn md_line(line: &str, fence: &mut bool, palette: &Palette) -> Vec<Seg> {
    let t = line.trim_start();
    if *fence {
        if is_fence_delim(t) {
            *fence = false;
            return fence_line_p(t, palette);
        }
        return vec![(
            palette.style(Role::Code, Modifier::empty()),
            line.to_string(),
        )];
    }
    if is_fence_delim(t) {
        *fence = true;
        return fence_line_p(t, palette);
    }
    if is_heading(t) {
        // The `#` run drops; the heading style stays.
        let text = t.trim_start_matches('#').trim_start().to_string();
        let style = palette.style(Role::Heading, Modifier::BOLD);
        return vec![(style, text)];
    }
    if t.starts_with('>') {
        // The `>` marker drops; the quote style stays.
        let text = t.trim_start_matches('>').trim_start().to_string();
        let style = palette.style(Role::Quote, Modifier::DIM);
        return vec![(style, text)];
    }
    if let Some((marker, rest)) = list_split(t) {
        // The bullet stays a visible bullet (docs/tui-markdown-
        // render.md section 3).
        let style = palette.style(Role::List, Modifier::BOLD);
        let mut segs: Vec<Seg> = vec![(style, marker)];
        segs.extend(inline_segments_p(rest, palette));
        return segs;
    }
    inline_segments_p(line, palette)
}

/// The fence marker line of the presentation pass: the delimiter
/// and the language tag dimmed (the code content keeps the Code
/// role, literal).
fn fence_line_p(t: &str, palette: &Palette) -> Vec<Seg> {
    let delim = if t.starts_with("```") { "```" } else { "~~~" };
    let style = palette.style(Role::Fence, Modifier::DIM);
    let mut out = vec![(style, delim.to_string())];
    let rest = t[delim.len()..].trim_start();
    if !rest.is_empty() {
        out.push((style, rest.to_string()));
    }
    out
}

/// The inline tokens of the presentation pass: the markers out,
/// the styles in. `` `code` `` shows the word in the inline-code
/// style without the backticks; `**b**` shows bold without the
/// stars; `*i*` shows underlined without the stars; `[text](url)`
/// shows the link text and the dimmed URL.
pub fn inline_segments_p(line: &str, palette: &Palette) -> Vec<Seg> {
    let cs: Vec<char> = line.chars().collect();
    let n = cs.len();
    let mut out: Vec<Seg> = Vec::new();
    let mut plain = String::new();
    let mut i = 0usize;
    while i < n {
        let c = cs[i];
        if let Some((tok, end)) = next_token(&cs, i) {
            if !plain.is_empty() {
                out.push((Style::default(), std::mem::take(&mut plain)));
            }
            match tok {
                Tok::Code => {
                    let inner: String = cs[i + 1..end - 1].iter().collect();
                    out.push((palette.style(Role::InlineCode, Modifier::empty()), inner));
                }
                Tok::Bold => {
                    let inner: String = cs[i + 2..end - 2].iter().collect();
                    out.push((Style::default().add_modifier(Modifier::BOLD), inner));
                }
                Tok::Italic => {
                    let inner: String = cs[i + 1..end - 1].iter().collect();
                    out.push((Style::default().add_modifier(Modifier::UNDERLINED), inner));
                }
                Tok::Link { j } => {
                    let text: String = cs[i + 1..j].iter().collect();
                    // The parens of the marker-free URL stay off:
                    // j + 2 is the first URL char, end - 1 the `)`.
                    let url: String = cs[j + 2..end - 1].iter().collect();
                    out.push((palette.style(Role::Link, Modifier::UNDERLINED), text));
                    out.push((palette.style(Role::LinkUrl, Modifier::DIM), url));
                }
            }
            i = end;
            continue;
        }
        plain.push(c);
        i += 1;
    }
    if !plain.is_empty() {
        out.push((Style::default(), plain));
    }
    out
}

// ── json ───────────────────────────────────────────────────────

/// True when `text` is a complete JSON document. Gate for JSON
/// highlighting of tool result text.
pub fn looks_like_json(text: &str) -> bool {
    let t = text.trim();
    (t.starts_with('{') || t.starts_with('['))
        && serde_json::from_str::<serde_json::Value>(t).is_ok()
}

/// The JSON token walk: one hard line as styled segments, the
/// colors lowered through the palette roles. The same token split as
/// the pi highlight.js JSON scope mapping (docs/tui-color-pi-
/// alignment.md): a string followed by `:` is a key, the `attr`
/// scope, colored through `SyntaxVariable`; string values through
/// `SyntaxString`; numbers, and the `true` / `false` / `null`
/// literals (the `literal` scope), through `SyntaxNumber`; the
/// structural punctuation through `SyntaxPunctuation`. The colors
/// carry no extra modifiers, like the pi token colors. Used when
/// the result body of a read or unknown tool is a JSON document.
pub fn json_line_p(line: &str, palette: &Palette) -> Vec<Seg> {
    let cs: Vec<char> = line.chars().collect();
    let n = cs.len();
    let style = |role: Role, mods: Modifier| palette.style(role, mods);
    let mut out: Vec<Seg> = Vec::new();
    let mut plain = String::new();
    let mut i = 0usize;
    while i < n {
        let c = cs[i];
        if c == '"' {
            let mut j = i + 1;
            let mut closed = false;
            while j < n {
                if cs[j] == '\\' {
                    j += 2;
                    continue;
                }
                if cs[j] == '"' {
                    closed = true;
                    break;
                }
                j += 1;
            }
            if !plain.is_empty() {
                out.push((Style::default(), std::mem::take(&mut plain)));
            }
            let end = if closed { j + 1 } else { n };
            let st = if closed {
                let mut k = j + 1;
                while k < n && (cs[k] == ' ' || cs[k] == '\t') {
                    k += 1;
                }
                if k < n && cs[k] == ':' {
                    style(Role::SyntaxVariable, Modifier::empty())
                } else {
                    style(Role::SyntaxString, Modifier::empty())
                }
            } else {
                style(Role::SyntaxString, Modifier::empty())
            };
            out.push((st, seg(&cs, i, end)));
            i = end;
            continue;
        }
        if c.is_ascii_digit() || (c == '-' && i + 1 < n && cs[i + 1].is_ascii_digit()) {
            let mut j = i;
            while j < n && (cs[j].is_ascii_digit() || matches!(cs[j], '.' | '+' | '-' | 'e' | 'E'))
            {
                j += 1;
            }
            if !plain.is_empty() {
                out.push((Style::default(), std::mem::take(&mut plain)));
            }
            out.push((style(Role::SyntaxNumber, Modifier::empty()), seg(&cs, i, j)));
            i = j;
            continue;
        }
        if c == 't' && line[i..].starts_with("true") {
            if !plain.is_empty() {
                out.push((Style::default(), std::mem::take(&mut plain)));
            }
            out.push((style(Role::SyntaxNumber, Modifier::empty()), "true".to_string()));
            i += 4;
            continue;
        }
        if c == 'f' && line[i..].starts_with("false") {
            if !plain.is_empty() {
                out.push((Style::default(), std::mem::take(&mut plain)));
            }
            out.push((
                style(Role::SyntaxNumber, Modifier::empty()),
                "false".to_string(),
            ));
            i += 5;
            continue;
        }
        if c == 'n' && line[i..].starts_with("null") {
            if !plain.is_empty() {
                out.push((Style::default(), std::mem::take(&mut plain)));
            }
            out.push((style(Role::SyntaxNumber, Modifier::empty()), "null".to_string()));
            i += 4;
            continue;
        }
        if matches!(c, '{' | '}' | '[' | ']' | ',' | ':') {
            if !plain.is_empty() {
                out.push((Style::default(), std::mem::take(&mut plain)));
            }
            out.push((style(Role::SyntaxPunctuation, Modifier::empty()), c.to_string()));
        } else {
            plain.push(c);
        }
        i += 1;
    }
    if !plain.is_empty() {
        out.push((Style::default(), plain));
    }
    out
}

// ── generic code preview highlighting ────────────────────────────
//
// The picker preview pane (docs/tui-file-picker.md, section 9:
// "Preview depth: plain text on day 0. Code highlight is a later
// add.") colors file content by detected language: string and number
// literals everywhere, line and block comments, and a small keyword
// set per language family. JSON delegates to [`json_line_p`] and
// markdown to [`md_line`]. Unknown files stay plain text.

/// Detect the highlight language from a file path, by extension (and
/// a few filename special cases). `None` means no known language:
/// the preview shows plain text.
pub fn language_from_path(path: &str) -> Option<&'static str> {
    let p = std::path::Path::new(path);
    let file_name = p.file_name().and_then(|f| f.to_str()).unwrap_or("");
    let file_name_lc = file_name.to_ascii_lowercase();
    if file_name_lc == "dockerfile" {
        return Some("dockerfile");
    }
    if file_name_lc == "makefile" || file_name_lc == "gnumakefile" {
        return Some("make");
    }
    let ext = p.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "rs" => "rust",
        "py" | "pyi" => "python",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" => "cpp",
        "go" => "go",
        "java" => "java",
        "js" | "mjs" | "cjs" => "javascript",
        "ts" | "tsx" | "jsx" => "typescript",
        "sh" | "bash" | "zsh" | "ksh" | "fish" => "shell",
        "json" => "json",
        "md" | "markdown" | "mdx" => "markdown",
        "yaml" | "yml" | "toml" | "ini" | "cfg" | "conf" | "env" | "properties" => "config",
        "html" | "htm" | "xml" => "html",
        "css" | "scss" | "sass" => "css",
        "sql" => "sql",
        "lua" => "lua",
        "rb" | "rake" => "ruby",
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        "scala" | "sc" => "scala",
        "ex" | "exs" => "elixir",
        "zig" => "zig",
        "nim" => "nim",
        "pl" | "pm" | "t" => "perl",
        "r" | "R" => "r",
        "m" | "mm" => "objc",
        "vb" => "vb",
        "mk" | "make" => "make",
        _ => return None,
    })
}

/// A stateful per-line code highlighter for the picker preview pane.
///
/// State (block-comment position, markdown fence) carries across
/// lines, so one instance is created per file read and fed the
/// lines in order.
pub struct CodeHighlighter {
    in_block_comment: bool,
    md_fence: bool,
}

impl CodeHighlighter {
    pub fn new() -> Self {
        Self {
            in_block_comment: false,
            md_fence: false,
        }
    }

    /// Highlight one hard line. `lang` is from
    /// [`language_from_path`]; `None` (unknown file type) stays
    /// plain text.
    pub fn line(&mut self, line: &str, lang: Option<&str>, palette: &Palette) -> Vec<Seg> {
        let lang = match lang {
            Some(l) => l,
            None => return vec![(Style::default(), line.to_string())],
        };
        match lang {
            "json" => json_line_p(line, palette),
            "markdown" => md_line(line, &mut self.md_fence, palette),
            _ => self.code_line(line, lang, palette),
        }
    }

    /// The generic code tokenizer: string literals, number literals,
    /// line comments, `/* */` block comments, and a per-language
    /// keyword set. Plain runs come out with the default style so
    /// the caller's plain-base pass can paint them. All indexing is
    /// in character space (the `cs` vec), never byte offsets into
    /// the raw line, so multibyte text cannot desync the indexes.
    fn code_line(&mut self, line: &str, lang: &str, palette: &Palette) -> Vec<Seg> {
        let style = |role: Role, mods: Modifier| palette.style(role, mods);
        let cs: Vec<char> = line.chars().collect();
        let n = cs.len();
        let mut out: Vec<Seg> = Vec::new();
        let mut plain = String::new();
        let mut i = 0usize;

        let kws = keywords_for(lang);
        let lc_prefixes = line_comment_prefixes(lang);
        let block_comment = has_block_comment(lang);
        let lc_chars: Vec<Vec<char>> =
            lc_prefixes.iter().map(|p| p.chars().collect()).collect();

        // Resume a block comment opened on a previous line.
        if self.in_block_comment {
            match find_char_seq(&cs, 0, "*/") {
                Some(pos) => {
                    self.in_block_comment = false;
                    let end = pos + 2;
                    out.push((
                        style(Role::SyntaxComment, Modifier::DIM),
                        seg(&cs, 0, end),
                    ));
                    i = end;
                }
                None => {
                    return vec![(
                        style(Role::SyntaxComment, Modifier::DIM),
                        line.to_string(),
                    )];
                }
            }
        }

        let flush_plain = |out: &mut Vec<Seg>, plain: &mut String| {
            if !plain.is_empty() {
                out.push((Style::default(), std::mem::take(plain)));
            }
        };

        while i < n {
            let c = cs[i];

            // String / char literal (quotes and backslash escapes).
            if c == '"' || c == '\'' {
                let quote = c;
                let mut j = i + 1;
                while j < n {
                    if cs[j] == '\\' && j + 1 < n {
                        j += 2;
                        continue;
                    }
                    if cs[j] == quote {
                        j += 1;
                        break;
                    }
                    j += 1;
                }
                flush_plain(&mut out, &mut plain);
                out.push((
                    style(Role::SyntaxString, Modifier::empty()),
                    seg(&cs, i, j),
                ));
                i = j;
                continue;
            }

            // Line comment: everything to end of line.
            if lc_chars.iter().any(|p| cs[i..].starts_with(p.as_slice())) {
                flush_plain(&mut out, &mut plain);
                out.push((
                    style(Role::SyntaxComment, Modifier::DIM),
                    seg(&cs, i, n),
                ));
                return out;
            }

            // Block comment; may run past the end of this line.
            if block_comment && n - i >= 2 && cs[i] == '/' && cs[i + 1] == '*' {
                flush_plain(&mut out, &mut plain);
                match find_char_seq(&cs, i + 2, "*/") {
                    Some(pos) => {
                        let end = pos + 2;
                        out.push((
                            style(Role::SyntaxComment, Modifier::DIM),
                            seg(&cs, i, end),
                        ));
                        i = end;
                    }
                    None => {
                        out.push((
                            style(Role::SyntaxComment, Modifier::DIM),
                            seg(&cs, i, n),
                        ));
                        self.in_block_comment = true;
                        return out;
                    }
                }
                continue;
            }

            // Number literal.
            if c.is_ascii_digit() {
                let mut j = i + 1;
                while j < n
                    && (cs[j].is_ascii_digit()
                        || matches!(
                            cs[j], '.' | 'x' | 'X' | 'o' | 'O' | 'b' | 'B' | 'e' | 'E' | 'a'
                                | 'f' | 'A' | 'F' | '_'
                        ))
                {
                    j += 1;
                }
                flush_plain(&mut out, &mut plain);
                out.push((
                    style(Role::SyntaxNumber, Modifier::empty()),
                    seg(&cs, i, j),
                ));
                i = j;
                continue;
            }

            // Identifier / keyword.
            if c.is_ascii_alphabetic() || c == '_' {
                let mut j = i + 1;
                while j < n && (cs[j].is_ascii_alphanumeric() || cs[j] == '_') {
                    j += 1;
                }
                let word: String = cs[i..j].iter().collect();
                if kws.contains(&word.as_str()) {
                    flush_plain(&mut out, &mut plain);
                    out.push((
                        style(Role::SyntaxKeyword, Modifier::empty()),
                        word,
                    ));
                    i = j;
                    continue;
                }
            }

            plain.push(c);
            i += 1;
        }
        flush_plain(&mut out, &mut plain);
        out
    }
}

/// Find the character run `needle` at or after `from` in `cs`, in
/// character space. The result is a char index, safe to feed into
/// [`seg`] on lines with multibyte characters.
fn find_char_seq(cs: &[char], from: usize, needle: &str) -> Option<usize> {
    let nc: Vec<char> = needle.chars().collect();
    let n = cs.len();
    if nc.is_empty() || nc.len() > n || from >= n {
        return None;
    }
    let limit = n - nc.len() + 1;
    for i in from..limit {
        if cs[i..i + nc.len()] == nc[..] {
            return Some(i);
        }
    }
    None
}

impl Default for CodeHighlighter {
    fn default() -> Self {
        Self::new()
    }
}

/// Line-comment prefixes per language (consumed to end of line).
fn line_comment_prefixes(lang: &str) -> &'static [&'static str] {
    match lang {
        "c" | "cpp" | "rust" | "go" | "java" | "javascript" | "typescript"
        | "swift" | "kotlin" | "scala" | "zig" => &["//"],
        "python" | "ruby" | "shell" | "make" | "dockerfile" | "config" | "r" => &["#"],
        "sql" | "lua" | "haskell" | "perl" => &["--"],
        "nim" => &["#", ";"],
        "html" | "xml" => &["<!--"],
        _ => &[],
    }
}

/// Languages with `/* ... */` block comments.
fn has_block_comment(lang: &str) -> bool {
    matches!(
        lang,
        "c" | "cpp" | "rust" | "go" | "java" | "javascript" | "typescript"
            | "swift" | "kotlin" | "scala" | "css" | "zig"
    )
}

/// A small keyword set per language family. Unlisted languages get
/// the C-family list (the common case among supported code
/// languages); the empty list means no keyword coloring.
fn keywords_for(lang: &str) -> &'static [&'static str] {
    const C_FAMILY: &[&str] = &[
        "auto", "break", "case", "catch", "const", "continue", "default",
        "delete", "do", "else", "enum", "extern", "false", "final",
        "finally", "for", "goto", "if", "implements", "import", "in",
        "interface", "new", "null", "override", "package", "private",
        "protected", "public", "return", "static", "struct", "super",
        "switch", "this", "throw", "true", "try", "typedef", "union",
        "unsigned", "using", "virtual", "while",
    ];
    match lang {
        "rust" => &[
            "async", "await", "break", "const", "continue", "dyn", "else",
            "enum", "false", "fn", "for", "if", "impl", "let", "loop",
            "match", "mod", "move", "mut", "pub", "ref", "return", "self",
            "static", "struct", "super", "trait", "true", "type", "use",
            "where", "while",
        ],
        "python" => &[
            "False", "True", "None", "and", "as", "assert", "async",
            "await", "break", "class", "continue", "def", "del", "elif",
            "else", "except", "finally", "for", "from", "global", "if",
            "import", "in", "is", "lambda", "nonlocal", "not", "or",
            "pass", "raise", "return", "while", "with", "yield",
        ],
        "go" => &[
            "break", "case", "chan", "const", "continue", "defer", "else",
            "fallthrough", "func", "go", "goto", "if", "import",
            "interface", "map", "package", "range", "return", "select",
            "struct", "switch", "type", "var", "true", "false", "nil",
            "iota",
        ],
        "javascript" | "typescript" => &[
            "async", "await", "break", "case", "catch", "class", "const",
            "continue", "debugger", "default", "delete", "do", "else",
            "export", "extends", "false", "finally", "for", "function",
            "if", "import", "in", "instanceof", "interface", "let", "new",
            "null", "of", "package", "private", "protected", "public",
            "readonly", "return", "static", "super", "switch", "this",
            "throw", "true", "try", "type", "typeof", "var", "void",
            "while", "with", "yield",
        ],
        "shell" | "make" | "dockerfile" => &[
            "if", "then", "else", "elif", "fi", "for", "while", "do",
            "done", "case", "esac", "function", "return", "exit", "local",
            "export", "declare", "set", "unset", "true", "false",
            "FROM", "RUN", "CMD", "ENTRYPOINT", "COPY", "ADD", "WORKDIR",
            "EXPOSE", "ENV", "ARG", "VOLUME", "USER", "ONBUILD", "all",
            "include", "override", "ifdef", "ifndef", "ifeq", "ifneq",
            "endif", "define", "endef",
        ],
        "sql" => &[
            "SELECT", "FROM", "WHERE", "INSERT", "UPDATE", "DELETE",
            "CREATE", "DROP", "ALTER", "TABLE", "JOIN", "LEFT", "RIGHT",
            "INNER", "OUTER", "ON", "AS", "AND", "OR", "NOT", "NULL",
            "IN", "IS", "BY", "ORDER", "GROUP", "HAVING", "LIMIT",
            "UNION", "VALUES", "SET", "INTO",
            "select", "from", "where", "insert", "update", "delete",
            "create", "drop", "alter", "table", "join", "left", "right",
            "inner", "outer", "on", "as", "and", "or", "not", "null",
            "in", "is", "by", "order", "group", "having", "limit",
            "union", "values", "set", "into",
        ],
        "ruby" => &[
            "def", "end", "class", "module", "return", "if", "elsif",
            "else", "unless", "while", "until", "do", "for", "case",
            "when", "then", "begin", "rescue", "ensure", "raise", "yield",
            "require", "require_relative", "include", "attr_accessor",
            "true", "false", "nil", "self", "super", "new", "puts",
            "print", "puts",
        ],
        "lua" => &[
            "and", "break", "do", "else", "elseif", "end", "false", "for",
            "function", "goto", "if", "in", "local", "nil", "not", "or",
            "repeat", "return", "then", "true", "until", "while",
        ],
        "r" => &[
            "function", "if", "else", "for", "while", "repeat", "break",
            "next", "return", "NULL", "TRUE", "FALSE", "library", "require",
            "in",
        ],
        "perl" => &[
            "if", "elsif", "else", "unless", "while", "until", "for",
            "foreach", "do", "done", "my", "our", "local", "use", "no",
            "require", "print", "printf", "sub", "return", "die", "warn",
            "undef", "defined", "and", "or", "not", "BEGIN", "END",
        ],
        "elixir" => &[
            "def", "defmodule", "defp", "defmacro", "do", "end", "if",
            "else", "case", "when", "fn", "fn", "use", "import", "require",
            "alias", "with", "try", "rescue", "catch", "raise", "throw",
            "true", "false", "nil",
        ],
        "html" | "xml" | "css" | "config" => &[],
        _ => C_FAMILY,
    }
}

/// Highlight a whole text body as styled hard lines. Shared by the
/// picker preview pane and the tool-result renderer. `lang` is from
/// [`language_from_path`]; `None` keeps every line plain. Returns
/// one `Vec<Seg>` per hard line, ready for the word-wraper.
pub fn highlight_text_lines(text: &str, lang: Option<&str>, palette: &Palette) -> Vec<Vec<Seg>> {
    let mut hl = CodeHighlighter::new();
    text.lines()
        .map(|l| hl.line(l, lang, palette))
        .collect()
}

// ── the grid table (docs/tui-markdown-render.md section 1) ────

/// True when the hard line is a table row: a `|`-separated run with
/// at least two cells. The separator row (`|---|---|`) counts: it
/// marks the header row as the table's first row.
pub fn is_table_row(line: &str) -> bool {
    let t = line.trim();
    if !t.starts_with('|') || t.matches('|').count() < 2 {
        return false;
    }
    let inner = t.trim_start_matches('|').trim_end_matches('|');
    inner.split('|').count() >= 2
}

/// The cells of one table row: the `|`-separated run split at the
/// pipes. The outer pipes drop; the cells keep their padding
/// trimmed.
pub fn table_cells(line: &str) -> Vec<String> {
    let t = line.trim();
    let inner = t.trim_start_matches('|').trim_end_matches('|');
    inner.split('|').map(|c| c.trim().to_string()).collect()
}

/// True when the row is the `|---|---|` separator: every cell is a
/// run of dashes (or empty).
pub fn is_table_separator(line: &str) -> bool {
    if !is_table_row(line) {
        return false;
    }
    table_cells(line)
        .iter()
        .all(|c| c.is_empty() || c.chars().all(|c| c == '-'))
}

/// The grid table of one table block: the rows are the `|`-separated
/// lines in order; a separator row after the header row drops from
/// the grid, and the header row styles bold. The box-drawing grid
/// fits `width` columns: the column width takes the content width
/// capped at the even share, the overflow elides with a trailing
/// ellipsis (the narrow-pane rule of docs/tui-markdown-render.md
/// section 3). Each returned inner `Vec` is one grid row of styled
/// segments, in cell order.
pub fn table_grid(rows: &[String], width: usize, palette: &Palette) -> Vec<Vec<(Style, String)>> {
    let has_sep = rows.get(1).map(|r| is_table_separator(r)).unwrap_or(false);
    let mut cells_rows: Vec<Vec<String>> = Vec::new();
    let mut header_index: Option<usize> = None;
    for (i, r) in rows.iter().enumerate() {
        if i == 1 && has_sep {
            continue;
        }
        if i == 0 && has_sep {
            header_index = Some(0);
        }
        cells_rows.push(table_cells(r));
    }
    if cells_rows.is_empty() {
        return Vec::new();
    }
    let ncols = cells_rows.iter().map(|c| c.len()).max().unwrap_or(0);
    if ncols == 0 {
        return Vec::new();
    }
    // The column widths: the content cap, then the narrow share.
    // The grid owns `ncols` verticals, the two outer borders, and
    // `ncols` padding cells: the content columns split the rest.
    let avail = width
        .saturating_sub(2)
        .saturating_sub(ncols)
        .saturating_sub(2 * ncols);
    let share = (avail / ncols).max(1);
    let widths: Vec<usize> = (0..ncols)
        .map(|c| {
            let content = cells_rows
                .iter()
                .map(|row| row.get(c).map(|s| s.chars().count()).unwrap_or(0))
                .max()
                .unwrap_or(0);
            content.min(share)
        })
        .collect();

    let border = palette.style(Role::Hint, Modifier::DIM);
    let plain = palette.style(Role::PlainText, Modifier::empty());
    let header_style = palette.style(Role::PlainText, Modifier::BOLD);

    let clamp = |s: &str, w: usize| -> String {
        let chars: Vec<char> = s.chars().collect();
        if chars.len() <= w {
            return s.to_string();
        }
        if w == 0 {
            return String::new();
        }
        let mut out: String = chars[..w - 1].iter().collect();
        out.push('…');
        out
    };

    let mut out: Vec<Vec<(Style, String)>> = Vec::new();
    out.push(grid_border('┌', '┐', '┬', '─', &widths, &border));
    for (ri, row) in cells_rows.iter().enumerate() {
        let is_header = header_index == Some(ri);
        let mut cells_out: Vec<(Style, String)> = Vec::new();
        cells_out.push((border, "│".to_string()));
        for (c, &w) in widths.iter().enumerate().take(ncols) {
            let cell = row.get(c).cloned().unwrap_or_default();
            // Pad to the column width: every row's verticals must land
            // on the same columns as the border rows (a shorter cell
            // renders at the column's full width, left-aligned).
            let text = format!(" {:<w$} ", clamp(&cell, w));
            let st = if is_header { &header_style } else { &plain };
            cells_out.push((*st, text));
            if c + 1 < ncols {
                cells_out.push((border, "│".to_string()));
            }
        }
        cells_out.push((border, "│".to_string()));
        out.push(cells_out);
        if ri + 1 < cells_rows.len() {
            out.push(grid_border('├', '┤', '┼', '─', &widths, &border));
        }
    }
    out.push(grid_border('└', '┘', '┴', '─', &widths, &border));
    out
}

/// One grid border row: the left corner, one `─` run per column
/// (the column width plus the two padding cells), the join per
/// gap, the right corner. The cells carry the border style.
fn grid_border(
    left: char,
    right: char,
    join: char,
    run: char,
    widths: &[usize],
    style: &Style,
) -> Vec<(Style, String)> {
    let mut out: Vec<(Style, String)> = Vec::new();
    out.push((*style, left.to_string()));
    for (i, w) in widths.iter().enumerate() {
        out.push((
            *style,
            std::iter::repeat_n(run, w + 2).collect::<String>(),
        ));
        if i + 1 < widths.len() {
            out.push((*style, join.to_string()));
        }
    }
    out.push((*style, right.to_string()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The plain-joined text of segments; the source must survive
    /// highlighting losslessly.
    fn joined(segs: &[Seg]) -> String {
        segs.iter().map(|(_, s)| s.as_str()).collect()
    }

    #[test]
    fn fence_state_toggles_across_lines() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let fence_st = p.style(crate::color::Role::Fence, Modifier::DIM);
        let code = p.style(crate::color::Role::Code, Modifier::empty());
        let mut fence = false;
        let open = md_line("```python", &mut fence, &p);
        assert!(fence, "a fence opens the block");
        assert!(
            open.iter().any(|(s, t)| *t == "```" && *s == fence_st)
                && open
                    .iter()
                    .any(|(s, t)| *t == "python" && *s == fence_st),
            "{open:?}"
        );
        let inside = md_line("x = 1", &mut fence, &p);
        assert!(fence, "the block stays open");
        assert_eq!(inside, vec![(code, "x = 1".to_string())], "{inside:?}");
        let close = md_line("```", &mut fence, &p);
        assert!(!fence, "a fence closes the block");
        assert!(close.iter().all(|(s, _)| *s == fence_st), "{close:?}");
    }

    #[test]
    fn headings_lists_and_quotes_are_styled() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let head = p.style(crate::color::Role::Heading, Modifier::BOLD);
        let quote = p.style(crate::color::Role::Quote, Modifier::DIM);
        let list = p.style(crate::color::Role::List, Modifier::BOLD);
        let mut fence = false;
        let h = md_line("# Title", &mut fence, &p);
        assert_eq!(h, vec![(head, "Title".to_string())], "{h:?}");
        let h7 = md_line("####### seven", &mut fence, &p);
        assert_eq!(h7.len(), 1, "seven hashes are not a heading: {h7:?}");
        let q = md_line("> quoted", &mut fence, &p);
        assert_eq!(q, vec![(quote, "quoted".to_string())], "{q:?}");
        let ul = md_line("- item", &mut fence, &p);
        assert_eq!(
            ul,
            vec![
                (list, "-".to_string()),
                (Style::default(), " item".to_string())
            ],
            "{ul:?}"
        );
        let ol = md_line("3. third", &mut fence, &p);
        assert_eq!(
            ol,
            vec![
                (list, "3.".to_string()),
                (Style::default(), " third".to_string())
            ],
            "{ol:?}"
        );
        // A lone dash that is not a marker stays plain.
        let plain = md_line("--verbose", &mut fence, &p);
        assert_eq!(joined(&plain), "--verbose");
        assert!(
            plain.iter().all(|(s, _)| *s == Style::default()),
            "{plain:?}"
        );
    }

    #[test]
    fn inline_tokens_are_styled_and_text_survives() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let s = inline_segments_p("a `code` **b** *i* [t](u) tail", &p);
        // The marker-free pass drops the marker characters; the
        // words survive.
        assert_eq!(joined(&s), "a code b i tu tail", "no word is lost");
        let texts: Vec<&str> = s.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(
            texts,
            vec!["a ", "code", " ", "b", " ", "i", " ", "t", "u", " tail"],
            "{s:?}"
        );
        let st = |i: usize| s[i].0;
        let inline_code = p.style(crate::color::Role::InlineCode, Modifier::empty());
        let link = p.style(crate::color::Role::Link, Modifier::UNDERLINED);
        let link_url = p.style(crate::color::Role::LinkUrl, Modifier::DIM);
        assert_eq!(st(1), inline_code);
        assert_eq!(st(3), Style::default().add_modifier(Modifier::BOLD));
        assert_eq!(st(5), Style::default().add_modifier(Modifier::UNDERLINED));
        assert_eq!(st(7), link);
        assert_eq!(st(8), link_url);
        assert_eq!(st(0), Style::default());
        assert_eq!(st(9), Style::default());
    }

    #[test]
    fn unmatched_tokens_stay_plain() {
        // The words must survive: unmatched markers stay literal, a
        // stray `**` never becomes a bold span, and the lone `*` pair
        // reads as italic.
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let s = inline_segments_p("a `b **c* [d] (e)", &p);
        let texts: Vec<&str> = s.iter().map(|(_, t)| t.as_str()).collect();
        let styles: Vec<Style> = s.iter().map(|(st, _)| *st).collect();
        assert_eq!(texts, vec!["a `b *", "c", " [d] (e)"], "{s:?}");
        assert_eq!(
            styles,
            vec![
                Style::default(),
                Style::default().add_modifier(Modifier::UNDERLINED),
                Style::default()
            ]
        );
        assert!(
            !s.iter().any(|(st, _)| st.add_modifier.contains(Modifier::BOLD)),
            "no bold from **"
        );
    }

    #[test]
    fn json_line_styles_keys_strings_numbers_literals() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let s = json_line_p("{\"a\":1,\"s\":\"x\",\"t\":true,\"n\":null}", &p);
        let texts: Vec<&str> = s.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "{", "\"a\"", ":", "1", ",", "\"s\"", ":", "\"x\"", ",", "\"t\"", ":", "true", ",",
                "\"n\"", ":", "null", "}"
            ],
            "{s:?}"
        );
        let st = |i: usize| s[i].0;
        let key = p.style(crate::color::Role::SyntaxVariable, Modifier::empty());
        let string = p.style(crate::color::Role::SyntaxString, Modifier::empty());
        let num = p.style(crate::color::Role::SyntaxNumber, Modifier::empty());
        // The literals color through the number role, like the pi
        // `literal` scope mapping: `null` is no longer a dim token.
        assert_eq!(st(1), key, "strings before : are keys");
        assert_eq!(st(5), key);
        assert_eq!(st(9), key);
        assert_eq!(st(13), key);
        assert_eq!(st(7), string, "strings after : are values");
        assert_eq!(st(3), num);
        assert_eq!(st(11), num, "true is a literal");
        assert_eq!(st(15), num, "null is a literal");
    }

    #[test]
    fn json_number_with_sign_and_exponent() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let s = json_line_p("-3.25e-4, 10, +2", &p);
        let texts: Vec<&str> = s.iter().map(|(_, t)| t.as_str()).collect();
        // A leading `+` is not a JSON number start: it stays plain.
        assert_eq!(
            texts,
            vec!["-3.25e-4", ",", " ", "10", ",", " +", "2"],
            "a leading + is not a JSON number start: it stays plain"
        );
        let st = |i: usize| s[i].0;
        let num = p.style(crate::color::Role::SyntaxNumber, Modifier::empty());
        let punct = p.style(crate::color::Role::SyntaxPunctuation, Modifier::empty());
        assert_eq!(st(0), num);
        assert_eq!(st(1), punct);
        assert_eq!(st(3), num);
        assert_eq!(st(6), num);
    }

    #[test]
    fn looks_like_json_only_accepts_complete_documents() {
        assert!(looks_like_json("{\"a\":1}"));
        assert!(looks_like_json("[1, 2]"));
        assert!(looks_like_json("  {\"a\":1}  "));
        assert!(!looks_like_json("{\"a\":"));
        assert!(!looks_like_json("no"));
        assert!(!looks_like_json("1"));
        assert!(!looks_like_json(""));
    }

    #[test]
    fn json_escapes_do_not_break_the_token() {
        let p = crate::color::Palette::builtin(crate::color::Level::Rgb);
        let s = json_line_p("{\"k\":\"a\\\"b\\n\",\"m\":\"c\"}", &p);
        let texts: Vec<&str> = s.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "{",
                "\"k\"",
                ":",
                "\"a\\\"b\\n\"",
                ",",
                "\"m\"",
                ":",
                "\"c\"",
                "}"
            ],
            "{s:?}"
        );
    }

    #[test]
    fn table_grid_rows_keep_fixed_column_widths() {
        // Every row (cell rows and border rows) must land its verticals
        // on the same columns: a shorter cell pads to the column width,
        // so no row is narrower than the borders.
        let palette = Palette::builtin(crate::color::Level::Rgb);
        let rows = vec![
            "| Name  | Description       |".to_string(),
            "|-------|-----------------|".to_string(),
            "| a     | b               |".to_string(),
            "| longer| a much longer text here |".to_string(),
        ];
        let grid = table_grid(&rows, 80, &palette);
        assert!(grid.len() >= 5, "borders plus rows: {grid:?}");
        let widths: Vec<usize> = grid
            .iter()
            .map(|r| r.iter().map(|(_, s)| s.chars().count()).sum::<usize>())
            .collect();
        let first = widths[0];
        assert!(
            widths.iter().all(|w| *w == first),
            "every grid row has the same width: {widths:?}"
        );
        // The middle separator line and the cell rows align: the
        // verticals of one cell row sit at the border's column joins.
        let row_text: String = grid[1].iter().map(|(_, s)| s.as_str()).collect();
        assert!(row_text.starts_with('│'), "cell row: {row_text:?}");
        assert!(row_text.ends_with('│'), "cell row: {row_text:?}");
        // The clamped column: the long cell elides when the pane is
        // narrow, the short cell pads to the column width.
        let row3: String = grid[3].iter().map(|(_, s)| s.as_str()).collect();
        assert!(row3.contains("a"), "short cell row: {row3:?}");
        let row5: String = grid[5].iter().map(|(_, s)| s.as_str()).collect();
        assert!(row5.contains("longer"), "wide cell row: {row5:?}");
    }

    #[test]
    fn table_grid_clamps_and_elides_on_a_narrow_pane() {
        let palette = Palette::builtin(crate::color::Level::Rgb);
        let rows = vec![
            "| Name | Description |".to_string(),
            "|------|-------------|".to_string(),
            "| abc  | a very long description that must elide |".to_string(),
        ];
        let grid = table_grid(&rows, 30, &palette);
        let widths: Vec<usize> = grid
            .iter()
            .map(|r| r.iter().map(|(_, s)| s.chars().count()).sum::<usize>())
            .collect();
        assert!(widths.iter().all(|w| *w == widths[0]), "{widths:?}");
        let last: String = grid.last().unwrap().iter().map(|(_, s)| s.as_str()).collect();
        let cell: String = grid[3].iter().map(|(_, s)| s.as_str()).collect();
        assert!(cell.contains('…'), "the elided cell: {cell:?}");
        assert!(last.contains('└'), "{last:?}");
    }

    // ── code-preview highlighter tests ─────────────────────────────

    #[test]
    fn language_from_path_maps_known_extensions() {
        assert_eq!(language_from_path("/repo/src/main.rs"), Some("rust"));
        assert_eq!(language_from_path("x.json"), Some("json"));
        assert_eq!(language_from_path("a/b.md"), Some("markdown"));
        assert_eq!(language_from_path("script.py"), Some("python"));
        assert_eq!(language_from_path("app.go"), Some("go"));
        assert_eq!(language_from_path("comp.ts"), Some("typescript"));
        assert_eq!(language_from_path("run.sh"), Some("shell"));
        assert_eq!(language_from_path("Dockerfile"), Some("dockerfile"));
        assert_eq!(language_from_path("Makefile"), Some("make"));
        // Unknown / no extension → None → plain text.
        assert_eq!(language_from_path("data.csv"), None);
        assert_eq!(language_from_path("notes.txt"), None);
        assert_eq!(language_from_path("/noext"), None);
    }

    #[test]
    fn code_highlighter_rust_line() {
        let p = Palette::builtin(crate::color::Level::Rgb);
        let mut hl = CodeHighlighter::new();
        let s = hl.line("let x: u32 = 42; // note", Some("rust"), &p);
        assert_eq!(joined(&s), "let x: u32 = 42; // note");
        let kw = p.style(Role::SyntaxKeyword, Modifier::empty());
        let num = p.style(Role::SyntaxNumber, Modifier::empty());
        let cmt = p.style(Role::SyntaxComment, Modifier::DIM);
        assert!(s.iter().any(|(st, t)| t == "let" && *st == kw), "{s:?}");
        assert!(s.iter().any(|(st, t)| t == "42" && *st == num), "{s:?}");
        assert!(s.iter().any(|(st, t)| t == "// note" && *st == cmt), "{s:?}");
    }

    #[test]
    fn code_highlighter_python_comment_and_string() {
        let p = Palette::builtin(crate::color::Level::Rgb);
        let mut hl = CodeHighlighter::new();
        let s = hl.line("x = \"hi\"  # greet", Some("python"), &p);
        assert_eq!(joined(&s), "x = \"hi\"  # greet");
        let string = p.style(Role::SyntaxString, Modifier::empty());
        let cmt = p.style(Role::SyntaxComment, Modifier::DIM);
        assert!(s.iter().any(|(st, t)| t == "\"hi\"" && *st == string), "{s:?}");
        assert!(s.iter().any(|(st, t)| t == "# greet" && *st == cmt), "{s:?}");
    }

    #[test]
    fn code_highlighter_block_comment_spans_lines() {
        let p = Palette::builtin(crate::color::Level::Rgb);
        let mut hl = CodeHighlighter::new();
        let l1 = hl.line("/* start", Some("c"), &p);
        let cmt = p.style(Role::SyntaxComment, Modifier::DIM);
        assert!(l1.iter().all(|(st, _)| *st == cmt), "{l1:?}");
        let l2 = hl.line("middle", Some("c"), &p);
        assert!(l2.iter().all(|(st, _)| *st == cmt), "{l2:?}");
        let l3 = hl.line("end */ code", Some("c"), &p);
        assert!(l3.iter().any(|(st, t)| t == "end */" && *st == cmt), "{l3:?}");
        // The rest after the block comment ends is plain.
        assert!(l3.iter().any(|(st, t)| t == " code" && *st == Style::default()), "{l3:?}");
    }

    #[test]
    fn code_highlighter_json_delegates() {
        let p = Palette::builtin(crate::color::Level::Rgb);
        let mut hl = CodeHighlighter::new();
        let s = hl.line("{\"a\": 1}", Some("json"), &p);
        let key = p.style(Role::SyntaxVariable, Modifier::empty());
        assert!(s.iter().any(|(st, t)| t == "\"a\"" && *st == key), "{s:?}");
    }

    #[test]
    fn code_highlighter_unknown_lang_is_plain() {
        let p = Palette::builtin(crate::color::Level::Rgb);
        let mut hl = CodeHighlighter::new();
        let s = hl.line("hello \"world\" 42", None, &p);
        // No known language → single plain segment, default style.
        assert_eq!(s.len(), 1, "{s:?}");
        assert_eq!(s[0].0, Style::default());
        assert_eq!(s[0].1, "hello \"world\" 42");
    }

    #[test]
    fn code_highlighter_survives_multibyte_lines() {
        // The tokenizer works in char space: a comment that starts
        // after a multibyte run must not desync byte/char indexes.
        let p = Palette::builtin(crate::color::Level::Rgb);
        let mut hl = CodeHighlighter::new();
        let s = hl.line("let ä = 1; // naïve ☃", Some("rust"), &p);
        assert_eq!(joined(&s), "let ä = 1; // naïve ☃");
        let cmt = p.style(Role::SyntaxComment, Modifier::DIM);
        assert!(
            s.iter().any(|(st, t)| t == "// naïve ☃" && *st == cmt),
            "{s:?}"
        );
    }

    #[test]
    fn code_highlighter_number_suffixes_and_hex() {
        let p = Palette::builtin(crate::color::Level::Rgb);
        let mut hl = CodeHighlighter::new();
        let s = hl.line("let a = 0xFF; let b = 1_000;", Some("rust"), &p);
        assert_eq!(joined(&s), "let a = 0xFF; let b = 1_000;");
        let num = p.style(Role::SyntaxNumber, Modifier::empty());
        let texts: Vec<&str> = s.iter().map(|(_, t)| t.as_str()).collect();
        assert!(
            texts.contains(&"0xFF") && texts.contains(&"1_000"),
            "hex and underscore literals stay one token: {texts:?}"
        );
        assert!(s.iter().any(|(st, t)| t == "0xFF" && *st == num));
        assert!(s.iter().any(|(st, t)| t == "1_000" && *st == num));
    }

    #[test]
    fn highlight_text_lines_shared_entry_point() {
        let p = Palette::builtin(crate::color::Level::Rgb);
        let rows = highlight_text_lines("fn main() {}\nlet x = 2;", Some("rust"), &p);
        assert_eq!(rows.len(), 2, "one styled row per hard line");
        let kw = p.style(Role::SyntaxKeyword, Modifier::empty());
        assert!(rows[0].iter().any(|(st, t)| t == "fn" && *st == kw));
        let num = p.style(Role::SyntaxNumber, Modifier::empty());
        assert!(rows[1].iter().any(|(st, t)| t == "2" && *st == num));
        // No language: lines come back as single plain runs.
        let plain = highlight_text_lines("just text", None, &p);
        assert_eq!(plain, vec![vec![(Style::default(), "just text".to_string())]]);
    }

    #[test]
    fn code_highlighter_markdown_fence_state() {
        let p = Palette::builtin(crate::color::Level::Rgb);
        let mut hl = CodeHighlighter::new();
        let a = hl.line("```python", Some("markdown"), &p);
        let fence = p.style(Role::Fence, Modifier::DIM);
        assert!(a.iter().any(|(st, t)| t == "```" && *st == fence), "{a:?}");
        let b = hl.line("x=1", Some("markdown"), &p);
        let code = p.style(Role::Code, Modifier::empty());
        assert_eq!(b, vec![(code, "x=1".to_string())], "{b:?}");
    }
}
