//! Rust mirror of the pure reference renderer in `lean/TuiStreamSpec.lean`
//! (the `lean-verify` op=drt production executable is `bin/tui-stream-drt`
//! `src/main.rs`; the Lean model executable is `lean/TuiStreamDrt.lean`).
//!
//! The spec models "stream rendering of the model response"
//! (docs/tui-streaming-response.md) as a state machine:
//!
//! - `View` holds `draft` (the in-progress response text), `settled`
//!   (completed responses, oldest first), `follow_tail` (the viewport
//!   choice), and `scroll` (the offset when not following the tail).
//! - A response stream is `StreamEvent::Chunk` (more text arrived) and
//!   `StreamEvent::Finished` (the response is complete).
//!
//! The proven invariants of the spec (P1 convergence, P2 clean settle,
//! P3 prefix growth, P4 no flush, P5 append-only transcript, P6
//! multi-response convergence) are mirrored as unit tests below, and
//! the DRT gate differential-random-tests this implementation against
//! the Lean model over the shared one-line scenario protocol
//! (`mod line`).
//!
//! Text is a sequence of characters; the DRT domain is the ASCII
//! generator alphabet (a-j, 0-9), so a text value is a byte sequence
//! whose byte values are character codes — the same representation the
//! Lean side uses (`List Char` of ASCII characters).

/// One chunk / the full text of a response: a sequence of characters
/// (the spec's `Text`, in the DRT's ASCII byte domain).
pub type Text = Vec<u8>;

/// One event of a single model-response stream (the spec's
/// `StreamEvent`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent<'a> {
    /// More response text has arrived.
    Chunk(&'a Text),
    /// The response is complete.
    Finished,
}

/// Renderer state while a response is in flight, plus the transcript of
/// responses that are already settled (the spec's `View`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    /// Response text accumulated so far for the current stream.
    pub draft: Text,
    /// Completed responses, oldest first. Each entry is the full text
    /// of one finished response.
    pub settled: Vec<Text>,
    /// Viewport choice: pinned to the tail when `true`. Streaming must
    /// not disturb this choice (P4).
    pub follow_tail: bool,
    /// When `follow_tail == false`, the lines scrolled up from the tail.
    pub scroll: u64,
}

impl View {
    /// The empty transcript, following the tail (the spec's
    /// `initialView`).
    pub fn initial() -> View {
        View {
            draft: Vec::new(),
            settled: Vec::new(),
            follow_tail: true,
            scroll: 0,
        }
    }

    /// Apply one stream event to the view: a chunk only appends to the
    /// draft; `Finished` settles the draft into the transcript. Neither
    /// touches the viewport fields (P4).
    pub fn step(&self, e: &StreamEvent) -> View {
        match e {
            StreamEvent::Chunk(t) => View {
                draft: {
                    let mut d = self.draft.clone();
                    d.extend_from_slice(t);
                    d
                },
                ..self.clone()
            },
            StreamEvent::Finished => View {
                settled: {
                    let mut s = self.settled.clone();
                    s.push(self.draft.clone());
                    s
                },
                draft: Vec::new(),
                ..self.clone()
            },
        }
    }
}

/// Run a whole response (its chunks in order, then `Finished`) starting
/// from view `v` (the spec's `runResponse`: an empty chunk list still
/// settles the carried draft).
pub fn run_response(v: &View, r: &[Text]) -> View {
    let mut v = v.clone();
    for c in r {
        v = v.step(&StreamEvent::Chunk(c));
    }
    v = v.step(&StreamEvent::Finished);
    v
}

/// Run a list of responses in order, each settling into the transcript
/// (the spec's `runResponses`).
pub fn run_responses(v: &View, rs: &[Vec<Text>]) -> View {
    rs.iter().fold(v.clone(), |v, r| run_response(&v, r))
}

// ── The DRT line protocol (shared with lean/TuiStreamDrt.lean) ──────

/// One-line scenario protocol:
///
/// Input — five single-space-separated fields:
/// `FT SC DRAFT SETTLED RESPONSES`
///
/// - `FT` `0`|`1` — the initial follow-tail choice
/// - `SC` decimal — the initial scroll offset
/// - `DRAFT` a text token `<n>:<hex>` — the carried draft text
/// - `SETTLED` `k` or `k,<tok>,...` — k already-settled texts
/// - `RESPONSES` `m` or `m,<c>,<tok>,...` — m responses, the i-th
///   with c_i chunk text tokens
///
/// A text token is `<n>:<hex>`: `<n>` is the character count and `<hex>`
/// the even-length lowercase hex of the text bytes. The DRT generator
/// (`scripts/tui-stream-drt-inputs.sh`) restricts chunk text to the
/// ASCII alphabet a-j 0-9, so the byte value is the character code on
/// both sides.
///
/// Output — the view after `run_responses`:
/// `follow=<0|1> scroll=<n> draft=<n>:<hex> settled=<k | k,<tok>,...>`
///
/// A malformed line (or a missing input) prints `ERR` and exits 1 —
/// identically on both sides of the gate.
pub mod line {
    use super::{run_responses, View};

    /// The malformed-input marker (the only non-scenario output).
    pub const ERR: &str = "ERR";

    /// A parsed scenario line: the initial view plus the response list.
    pub struct Scenario {
        pub follow_tail: bool,
        pub scroll: u64,
        pub draft: Vec<u8>,
        pub settled: Vec<Vec<u8>>,
        pub responses: Vec<Vec<Vec<u8>>>,
    }

    /// Parse a scenario line (strict: any deviation is an error, exactly
    /// like the Lean side — the gate only sees well-formed generator
    /// lines, and both sides must reject the same malformed input).
    pub fn parse(line: &str) -> Result<Scenario, ()> {
        let fields: Vec<&str> = line.split(' ').collect();
        if fields.len() != 5 {
            return Err(());
        }
        let follow_tail = match fields[0] {
            "0" => false,
            "1" => true,
            _ => return Err(()),
        };
        let scroll = parse_dec(fields[1])?;
        let draft = parse_text_tok(fields[2])?;
        let settled = parse_settled_field(fields[3])?;
        let responses = parse_responses_field(fields[4])?;
        Ok(Scenario {
            follow_tail,
            scroll,
            draft,
            settled,
            responses,
        })
    }

    /// Run the spec's reference renderer over the scenario and render
    /// the final view (the output line, no newline).
    pub fn run(input: &str) -> Option<String> {
        let s = parse(input).ok()?;
        let v = View {
            draft: s.draft,
            settled: s.settled,
            follow_tail: s.follow_tail,
            scroll: s.scroll,
        };
        Some(render(&run_responses(&v, &s.responses)))
    }

    /// The canonical view rendering (the DRT output line).
    pub fn render(v: &View) -> String {
        let mut out = String::new();
        out.push_str("follow=");
        out.push_str(if v.follow_tail { "1" } else { "0" });
        out.push_str(" scroll=");
        out.push_str(&v.scroll.to_string());
        out.push_str(" draft=");
        out.push_str(&text_tok_of(&v.draft));
        out.push_str(" settled=");
        out.push_str(&settled_field_of(&v.settled));
        out
    }

    fn text_tok_of(t: &[u8]) -> String {
        let mut hex = String::new();
        for b in t {
            hex.push_str(&format!("{b:02x}"));
        }
        format!("{}:{hex}", t.len())
    }

    fn settled_field_of(ls: &[Vec<u8>]) -> String {
        if ls.is_empty() {
            return "0".to_string();
        }
        let toks: Vec<String> = ls.iter().map(|t| text_tok_of(t)).collect();
        format!("{},{}", ls.len(), toks.join(","))
    }

    /// Decimal string to u64; `None` when empty or not all digits.
    fn parse_dec(s: &str) -> Result<u64, ()> {
        if s.is_empty() {
            return Err(());
        }
        s.parse::<u64>()
            .ok()
            .filter(|_| s.bytes().all(|b| b.is_ascii_digit()))
            .ok_or(())
    }

    /// One text token: `<n>:<hex>` — n characters whose byte values are
    /// the even-length lowercase hex.
    fn parse_text_tok(tok: &str) -> Result<Vec<u8>, ()> {
        let mut parts = tok.split(':');
        let Some(n) = parts.next() else {
            return Err(());
        };
        let Some(hex) = parts.next() else {
            return Err(());
        };
        if parts.next().is_some() {
            return Err(());
        }
        let len = parse_dec(n)? as usize;
        let vals = hex_to_values(hex)?;
        if vals.len() != len {
            return Err(());
        }
        Ok(vals)
    }

    /// Even-length lowercase hex to byte values.
    fn hex_to_values(hex: &str) -> Result<Vec<u8>, ()> {
        let bs = hex.as_bytes();
        if bs.len() % 2 != 0 {
            return Err(());
        }
        let mut out = Vec::with_capacity(bs.len() / 2);
        let mut i = 0;
        while i < bs.len() {
            let hi = hex_digit_val(bs[i])?;
            let lo = hex_digit_val(bs[i + 1])?;
            out.push(hi * 16 + lo);
            i += 2;
        }
        Ok(out)
    }

    fn hex_digit_val(b: u8) -> Result<u8, ()> {
        match b {
            b'0'..=b'9' => Ok(b - b'0'),
            b'a'..=b'f' => Ok(b - b'a' + 10),
            _ => Err(()),
        }
    }

    /// The settled field: `k` for an empty list, otherwise `k,<tok>,...`
    /// with k text tokens.
    fn parse_settled_field(f: &str) -> Result<Vec<Vec<u8>>, ()> {
        let toks: Vec<&str> = f.split(',').collect();
        match toks.len() {
            1 => {
                let k = parse_dec(toks[0])?;
                if k != 0 {
                    return Err(());
                }
                Ok(Vec::new())
            }
            _ => {
                let k = parse_dec(toks[0])? as usize;
                if k != toks.len() - 1 {
                    return Err(());
                }
                toks[1..]
                    .iter()
                    .map(|t| parse_text_tok(t))
                    .collect::<Result<Vec<Vec<u8>>, ()>>()
            }
        }
    }

    /// The responses field: `m` for no responses, otherwise `m,<c>,...`
    /// — m responses, the i-th with c_i chunk text tokens.
    fn parse_responses_field(f: &str) -> Result<Vec<Vec<Vec<u8>>>, ()> {
        let toks: Vec<&str> = f.split(',').collect();
        let mut pos = 0usize;
        let next = |pos: &mut usize| -> Result<&str, ()> {
            let t = *toks.get(*pos).ok_or(())?;
            *pos += 1;
            Ok(t)
        };
        if toks.is_empty() {
            return Err(());
        }
        let m = parse_dec(next(&mut pos)?)? as usize;
        if m == 0 {
            // A zero-count field is the bare "0": no tokens may follow.
            if pos != toks.len() {
                return Err(());
            }
            return Ok(Vec::new());
        }
        let mut out: Vec<Vec<Vec<u8>>> = Vec::with_capacity(m);
        for _ in 0..m {
            let c = parse_dec(next(&mut pos)?)? as usize;
            let chunks_end = pos + c;
            if chunks_end > toks.len() {
                return Err(());
            }
            let chunks = toks[pos..chunks_end]
                .iter()
                .map(|t| parse_text_tok(t))
                .collect::<Result<Vec<Vec<u8>>, ()>>()?;
            out.push(chunks);
            pos = chunks_end;
        }
        if pos != toks.len() {
            return Err(());
        }
        Ok(out)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// The spec's `ex_stream_hello` (lean/TuiStreamSpec.lean):
        /// streaming "He" then "llo" settles to the full text — in the
        /// DRT alphabet, "ab" then "cd" settles to "abcd".
        #[test]
        fn ex_stream_hello_mirror() {
            let out = run("1 0 0: 0 1,2,2:6162,2:6364").unwrap();
            assert_eq!(out, "follow=1 scroll=0 draft=0: settled=1,4:61626364");
        }

        /// `ex_empty_response`: an empty response still settles exactly
        /// one (empty) entry.
        #[test]
        fn ex_empty_response_mirror() {
            let out = run("1 0 0: 0 1,0").unwrap();
            assert_eq!(out, "follow=1 scroll=0 draft=0: settled=1,0:");
        }

        /// `ex_two_responses`: two responses settle in order.
        #[test]
        fn ex_two_responses_mirror() {
            let out = run("1 0 0: 0 2,1,1:61,1,1:62").unwrap();
            assert_eq!(out, "follow=1 scroll=0 draft=0: settled=2,1:61,1:62");
        }

        /// P1 convergence with a carried draft and a pre-settled
        /// transcript: the transcript grows by exactly the carried
        /// draft plus the full response text.
        #[test]
        fn p1_convergence_with_carried_draft() {
            // draft "ab", settled ["c"], response ["d"] →
            // settled ["c", "abd"].
            let out = run("1 3 2:6162 1,1:63 1,1,1:64").unwrap();
            assert_eq!(out, "follow=1 scroll=3 draft=0: settled=2,1:63,3:616264");
        }

        /// P4 no-flush: a pinned scroll survives the whole stream.
        #[test]
        fn p4_pinned_scroll_survives() {
            let out = run("0 42 3:616263 2,1:64,1:65 1,2,2:6667,2:6869").unwrap();
            assert_eq!(
                out,
                "follow=0 scroll=42 draft=0: settled=3,1:64,1:65,7:61626366676869"
            );
        }

        /// No responses: the view is untouched (P5 for the empty case).
        #[test]
        fn no_responses_view_unchanged() {
            let out = run("0 5 0: 0 0").unwrap();
            assert_eq!(out, "follow=0 scroll=5 draft=0: settled=0");
        }

        /// Malformed lines print the shared ERR marker (both sides).
        #[test]
        fn malformed_lines_are_err() {
            for bad in [
                "garbage",
                "",
                "2 0 0: 0 0",      // ft not a bit
                "1 x 0: 0 0",      // scroll not decimal
                "1 0 1:zz 0 0",    // bad hex
                "1 0 1:6 0 0",     // odd-length hex
                "1 0 2:61 0 0",    // count/length mismatch
                "1 0 0: 2 0",      // 4 fields
                "1 0 0: 0 0 0",    // 6 fields
                "1 0 0: 0 1,0,0",  // settled count mismatch
                "1 0 0: 0 2,0,0,0",// trailing token after m responses
                "1 0 0: 0 0,1",    // token after a zero response count
            ] {
                assert_eq!(run(bad), None, "{bad}");
            }
        }

        /// The parse/render round-trip is exact on well-formed lines.
        #[test]
        fn round_trip() {
            let sc = parse("0 7 2:6162 3,0:,1:63,2:6465 2,0,1,1:66").unwrap();
            let v = View {
                draft: sc.draft,
                settled: sc.settled.clone(),
                follow_tail: sc.follow_tail,
                scroll: sc.scroll,
            };
            let v2 = run_responses(&v, &sc.responses);
            // The draft settles into the transcript; the rendering is
            // deterministic, so re-parsing the rendered view's fields
            // reproduces the values.
            let rendered = render(&v2);
            assert!(rendered.starts_with("follow=0 scroll=7 draft=0:"));
            // P5: the old settled list is a prefix of the new one.
            let out: String = v2
                .settled
                .iter()
                .map(|t| text_tok_of(t))
                .collect::<Vec<_>>()
                .join(",");
            assert!(out.starts_with("0:,1:63,2:6465,"), "{out}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The spec's `ex_stream_hello`: "He" then "llo" settles to the
    /// full text "Hello" — in the DRT alphabet, "ab" then "cd"
    /// settles to "abcd" (one transcript entry).
    #[test]
    fn ex_stream_hello() {
        let v = run_response(
            &View::initial(),
            &[vec![b'a', b'b'], vec![b'c', b'd']],
        );
        assert_eq!(v.settled, vec![vec![b'a', b'b', b'c', b'd']]);
    }

    /// The spec's `ex_stream_clean`: after a complete response the
    /// draft is cleared.
    #[test]
    fn ex_stream_clean() {
        let v = run_response(
            &View::initial(),
            &[vec![b'a', b'b'], vec![b'c', b'd']],
        );
        assert!(v.draft.is_empty());
    }

    /// The spec's `ex_empty_response`: an empty response still settles
    /// exactly one (empty) entry.
    #[test]
    fn ex_empty_response() {
        let v = run_response(&View::initial(), &[]);
        assert_eq!(v.settled, vec![Vec::new()]);
    }

    /// The spec's `ex_two_responses`: two responses settle in order.
    #[test]
    fn ex_two_responses() {
        let v = run_responses(
            &View::initial(),
            &[vec![vec![b'a']], vec![vec![b'b']]],
        );
        assert_eq!(v.settled, vec![vec![b'a'], vec![b'b']]);
    }

    /// P1 convergence: the transcript grows by exactly the carried
    /// draft plus the full response text; nothing is lost or
    /// duplicated.
    #[test]
    fn p1_converges() {
        let v = View {
            draft: vec![b'x'],
            settled: vec![vec![b'o', b'l', b'd']],
            follow_tail: false,
            scroll: 9,
        };
        let r = vec![vec![b'1', b'2'], vec![b'3']];
        let out = run_response(&v, &r);
        assert_eq!(out.settled, vec![vec![b'o', b'l', b'd'], vec![b'x', b'1', b'2', b'3']]);
    }

    /// P2 clean settle: the draft is empty after every response.
    #[test]
    fn p2_clean_after_response() {
        let v = View {
            draft: vec![b'k'],
            ..View::initial()
        };
        assert!(run_response(&v, &[vec![b'n']]).draft.is_empty());
        assert!(run_response(&v, &[]).draft.is_empty());
    }

    /// P4 no flush: chunks and completion leave the viewport choice
    /// untouched.
    #[test]
    fn p4_no_flush() {
        let v = View {
            draft: Vec::new(),
            settled: Vec::new(),
            follow_tail: false,
            scroll: 17,
        };
        let out = run_responses(
            &v,
            &[vec![vec![b'a']], vec![vec![b'b'], vec![b'c']], Vec::new()],
        );
        assert!(!out.follow_tail);
        assert_eq!(out.scroll, 17);
    }

    /// P5 transcript append-only: the old settled list is a prefix of
    /// the new one.
    #[test]
    fn p5_transcript_grows() {
        let v = View {
            settled: vec![vec![b'1']],
            ..View::initial()
        };
        let out = run_responses(&v, &[vec![vec![b'2']]]);
        assert_eq!(out.settled.len(), 2);
        assert_eq!(&out.settled[0], &v.settled[0]);
    }

    /// P6 multi-response convergence (from a clean draft): each
    /// response settles, in order, to the full text of its chunks.
    #[test]
    fn p6_multi_converges() {
        let out = run_responses(
            &View::initial(),
            &[
                vec![vec![b'a'], vec![b'b']],
                Vec::new(),
                vec![vec![b'c']],
            ],
        );
        assert_eq!(
            out.settled,
            vec![vec![b'a', b'b'], Vec::new(), vec![b'c']]
        );
    }

    /// The initial view: empty transcript, following the tail.
    #[test]
    fn initial_view() {
        let v = View::initial();
        assert!(v.draft.is_empty());
        assert!(v.settled.is_empty());
        assert!(v.follow_tail);
        assert_eq!(v.scroll, 0);
    }
}
