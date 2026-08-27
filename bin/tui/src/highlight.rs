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

use ratatui::style::{Color, Modifier, Style};

/// One styled piece of text, ready for the word-wraper.
pub type Seg = (Style, String);

// ── palette ────────────────────────────────────────────────────
// Every color is 16-color safe so the highlighting survives a dim
// terminal.

/// Fenced-code content.
pub fn code_style() -> Style {
    Style::default().fg(Color::Green)
}
/// A ``` / ~~~ fence line, including its language tag.
pub fn fence_style() -> Style {
    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
}
/// A markdown heading line.
pub fn heading_style() -> Style {
    Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD)
}
/// A blockquote line.
pub fn quote_style() -> Style {
    Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM)
}
/// A list marker (`-`, `*`, `+`, `1.`).
pub fn list_style() -> Style {
    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
}
/// An inline `code` span, backticks included.
pub fn inline_code_style() -> Style {
    Style::default().fg(Color::Yellow)
}
/// A **bold** span, stars included.
pub fn bold_style() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}
/// A *italic* span, stars included.
pub fn italic_style() -> Style {
    Style::default().add_modifier(Modifier::UNDERLINED)
}
/// The `[text]` part of a markdown link.
pub fn link_style() -> Style {
    Style::default().fg(Color::Cyan).add_modifier(Modifier::UNDERLINED)
}
/// The `(url)` part of a markdown link.
pub fn link_url_style() -> Style {
    Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM)
}
/// A JSON object key (the string before the `:`).
pub fn json_key_style() -> Style {
    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
}
/// A JSON string value.
pub fn json_string_style() -> Style {
    Style::default().fg(Color::Yellow)
}
/// A JSON number.
pub fn json_number_style() -> Style {
    Style::default().fg(Color::Red)
}
/// `true` / `false`.
pub fn json_literal_style() -> Style {
    Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD)
}
/// `null`.
pub fn json_null_style() -> Style {
    Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM)
}
/// JSON structural punctuation (`{ } [ ] , :`).
pub fn json_punct_style() -> Style {
    Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM)
}

// ── markdown ───────────────────────────────────────────────────

/// One hard line of markdown-like text, as styled segments.
///
/// `fence` carries the fenced-code-block state across hard lines:
/// `true` while inside a ``` or ~~~ block. Recognized syntax:
/// fence delims (with optional language tag), headings (`#`..`######`),
/// blockquotes (`>`), list markers (`-`/`*`/`+`/`N.`), and inline
/// `` `code` ``, `**bold**`, `*italic*`, `[text](url)`.
///
/// Everything unrecognized stays plain: a log view must never lose
/// text, only add color.
pub fn markdown_line(line: &str, fence: &mut bool) -> Vec<Seg> {
    let t = line.trim_start();
    if *fence {
        if is_fence_delim(t) {
            *fence = false;
            return fence_line(t);
        }
        return vec![(code_style(), line.to_string())];
    }
    if is_fence_delim(t) {
        *fence = true;
        return fence_line(t);
    }
    if is_heading(t) {
        return vec![(heading_style(), line.to_string())];
    }
    if t.starts_with('>') {
        return vec![(quote_style(), line.to_string())];
    }
    if let Some((marker, rest)) = list_split(t) {
        let mut segs: Vec<Seg> = vec![(list_style(), marker)];
        segs.extend(inline_segments(rest));
        return segs;
    }
    inline_segments(line)
}

fn is_fence_delim(t: &str) -> bool {
    t.starts_with("```") || t.starts_with("~~~")
}

/// The fence line itself: the delimiter, then the optional language
/// tag (```` ```python ````).
fn fence_line(t: &str) -> Vec<Seg> {
    let delim = if t.starts_with("```") { "```" } else { "~~~" };
    let mut out = vec![(fence_style(), delim.to_string())];
    let rest = t[delim.len()..].trim_start();
    if !rest.is_empty() {
        out.push((fence_style(), rest.to_string()));
    }
    out
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
    (1..=6).contains(&hashes)
        && (t[hashes..].starts_with(' ') || t[hashes..].is_empty())
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

/// Inline markdown on one hard line that is not a fence, heading,
/// quote, or list marker.
pub fn inline_segments(line: &str) -> Vec<Seg> {
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
                Tok::Code => out.push((inline_code_style(), seg(&cs, i, end))),
                Tok::Bold => out.push((bold_style(), seg(&cs, i, end))),
                Tok::Italic => out.push((italic_style(), seg(&cs, i, end))),
                Tok::Link { j } => {
                    let text: String = cs[i + 1..j].iter().collect();
                    let url: String = cs[j + 1..end].iter().collect();
                    out.push((link_style(), format!("[{text}]")));
                    out.push((link_url_style(), url));
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

// ── json ───────────────────────────────────────────────────────

/// True when `text` is a complete JSON document. Gate for JSON
/// highlighting of tool result text.
pub fn looks_like_json(text: &str) -> bool {
    let t = text.trim();
    (t.starts_with('{') || t.starts_with('['))
        && serde_json::from_str::<serde_json::Value>(t).is_ok()
}

/// One hard line of JSON text, as styled segments.
///
/// A JSON string can never contain a raw newline, so per-line
/// processing is lossless. A string followed by `:` is a key; the
/// same string elsewhere is a value.
pub fn json_line(line: &str) -> Vec<Seg> {
    let cs: Vec<char> = line.chars().collect();
    let n = cs.len();
    let mut out: Vec<Seg> = Vec::new();
    let mut plain = String::new();
    let mut i = 0usize;
    while i < n {
        let c = cs[i];
        if c == '"' {
            // Find the closing quote; `\` escapes one char.
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
            let style = if closed {
                // A string whose next non-blank char is `:` is a key.
                let mut k = j + 1;
                while k < n && (cs[k] == ' ' || cs[k] == '\t') {
                    k += 1;
                }
                if k < n && cs[k] == ':' {
                    json_key_style()
                } else {
                    json_string_style()
                }
            } else {
                json_string_style()
            };
            out.push((style, seg(&cs, i, end)));
            i = end;
            continue;
        }
        if c.is_ascii_digit() || (c == '-' && i + 1 < n && cs[i + 1].is_ascii_digit()) {
            let mut j = i;
            while j < n
                && (cs[j].is_ascii_digit() || matches!(cs[j], '.' | '+' | '-' | 'e' | 'E'))
            {
                j += 1;
            }
            if !plain.is_empty() {
                out.push((Style::default(), std::mem::take(&mut plain)));
            }
            out.push((json_number_style(), seg(&cs, i, j)));
            i = j;
            continue;
        }
        if c == 't' && line[i..].starts_with("true") {
            if !plain.is_empty() {
                out.push((Style::default(), std::mem::take(&mut plain)));
            }
            out.push((json_literal_style(), "true".to_string()));
            i += 4;
            continue;
        }
        if c == 'f' && line[i..].starts_with("false") {
            if !plain.is_empty() {
                out.push((Style::default(), std::mem::take(&mut plain)));
            }
            out.push((json_literal_style(), "false".to_string()));
            i += 5;
            continue;
        }
        if c == 'n' && line[i..].starts_with("null") {
            if !plain.is_empty() {
                out.push((Style::default(), std::mem::take(&mut plain)));
            }
            out.push((json_null_style(), "null".to_string()));
            i += 4;
            continue;
        }
        if matches!(c, '{' | '}' | '[' | ']' | ',' | ':') {
            if !plain.is_empty() {
                out.push((Style::default(), std::mem::take(&mut plain)));
            }
            out.push((json_punct_style(), c.to_string()));
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
        let mut fence = false;
        let open = markdown_line("```python", &mut fence);
        assert!(fence, "a fence opens the block");
        assert!(open
            .iter()
            .any(|(s, t)| *t == "```" && *s == fence_style())
            && open.iter().any(|(s, t)| *t == "python" && *s == fence_style()),
            "{open:?}");
        let inside = markdown_line("x = 1", &mut fence);
        assert!(fence, "the block stays open");
        assert_eq!(inside, vec![(code_style(), "x = 1".to_string())], "{inside:?}");
        let close = markdown_line("```", &mut fence);
        assert!(!fence, "a fence closes the block");
        assert!(close.iter().all(|(s, _)| *s == fence_style()), "{close:?}");
    }

    #[test]
    fn headings_lists_and_quotes_are_styled() {
        let mut fence = false;
        let h = markdown_line("# Title", &mut fence);
        assert_eq!(h, vec![(heading_style(), "# Title".to_string())], "{h:?}");
        let h7 = markdown_line("####### seven", &mut fence);
        assert_eq!(h7.len(), 1, "seven hashes are not a heading: {h7:?}");
        let q = markdown_line("> quoted", &mut fence);
        assert_eq!(q, vec![(quote_style(), "> quoted".to_string())], "{q:?}");
        let ul = markdown_line("- item", &mut fence);
        assert_eq!(
            ul,
            vec![
                (list_style(), "-".to_string()),
                (Style::default(), " item".to_string())
            ],
            "{ul:?}"
        );
        let ol = markdown_line("3. third", &mut fence);
        assert_eq!(
            ol,
            vec![
                (list_style(), "3.".to_string()),
                (Style::default(), " third".to_string())
            ],
            "{ol:?}"
        );
        // A lone dash that is not a marker stays plain.
        let plain = markdown_line("--verbose", &mut fence);
        assert_eq!(joined(&plain), "--verbose");
        assert!(plain.iter().all(|(s, _)| *s == Style::default()), "{plain:?}");
    }

    #[test]
    fn inline_tokens_are_styled_and_text_survives() {
        let s = markdown_line("a `code` **b** *i* [t](u) tail", &mut false);
        assert_eq!(joined(&s), "a `code` **b** *i* [t](u) tail", "no text is lost");
        let texts: Vec<&str> = s.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(
            texts,
            vec!["a ", "`code`", " ", "**b**", " ", "*i*", " ", "[t]", "(u)", " tail"],
            "{s:?}"
        );
        let st = |i: usize| s[i].0;
        assert_eq!(st(1), inline_code_style());
        assert_eq!(st(3), bold_style());
        assert_eq!(st(5), italic_style());
        assert_eq!(st(7), link_style());
        assert_eq!(st(8), link_url_style());
        assert_eq!(st(0), Style::default());
        assert_eq!(st(9), Style::default());
    }

    #[test]
    fn unmatched_tokens_stay_plain() {
        // The text must survive: unmatched markers stay literal, a
        // stray `**` never becomes a bold span, and the lone `*` pair
        // reads as italic (`*c*`).
        let s = inline_segments("a `b **c* [d] (e)");
        assert_eq!(joined(&s), "a `b **c* [d] (e)", "unmatched markers never eat text");
        let texts: Vec<&str> = s.iter().map(|(_, t)| t.as_str()).collect();
        let styles: Vec<Style> = s.iter().map(|(st, _)| *st).collect();
        assert_eq!(texts, vec!["a `b *", "*c*", " [d] (e)"], "{s:?}");
        assert_eq!(styles, vec![Style::default(), italic_style(), Style::default()]);
        assert!(!s.iter().any(|(st, _)| *st == bold_style()), "no bold from **");
    }

    #[test]
    fn json_line_styles_keys_strings_numbers_literals() {
        let s = json_line(r#"{"a":1,"s":"x","t":true,"n":null}"#);
        let texts: Vec<&str> = s.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "{", "\"a\"", ":", "1", ",", "\"s\"", ":", "\"x\"", ",", "\"t\"", ":", "true",
                ",", "\"n\"", ":", "null", "}"
            ],
            "{s:?}"
        );
        let st = |i: usize| s[i].0;
        assert_eq!(st(1), json_key_style(), "strings before : are keys");
        assert_eq!(st(5), json_key_style());
        assert_eq!(st(9), json_key_style());
        assert_eq!(st(13), json_key_style());
        assert_eq!(st(7), json_string_style(), "strings after : are values");
        assert_eq!(st(3), json_number_style());
        assert_eq!(st(11), json_literal_style());
        assert_eq!(st(15), json_null_style());
        assert_eq!(st(0), json_punct_style());
        assert_eq!(st(2), json_punct_style());
    }

    #[test]
    fn json_number_with_sign_and_exponent() {
        let s = json_line("-3.25e-4, 10, +2");
        let texts: Vec<&str> = s.iter().map(|(_, t)| t.as_str()).collect();
        // A leading `+` is not a JSON number start: it stays plain.
        assert_eq!(
            texts,
            vec!["-3.25e-4", ",", " ", "10", ",", " +", "2"],
            "a leading + is not a JSON number start: it stays plain"
        );
        let st = |i: usize| s[i].0;
        assert_eq!(st(0), json_number_style());
        assert_eq!(st(1), json_punct_style());
        assert_eq!(st(3), json_number_style());
        assert_eq!(st(6), json_number_style());
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
        let s = json_line(r#"{"k":"a\"b\n","m":"c"}"#);
        let texts: Vec<&str> = s.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "{", "\"k\"", ":", r#""a\"b\n""#, ",", "\"m\"", ":", r#""c""#, "}"
            ],
            "{s:?}"
        );
    }
}
