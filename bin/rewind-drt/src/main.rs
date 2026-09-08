//! `rewind-drt` — the DRT production executable for the
//! `rushi_common::rewind::active_ranges` mirror
//! (`lean/RewindDrt.lean` is the Lean model executable; the two must
//! agree on every input, per docs/rewind-fork-design.md "Verification").
//!
//! Usage: `rewind-drt <scenario line>` — or the same line in the
//! `DRT_INPUT` environment variable when no argument is given (the
//! `lean-verify` op=drt tool passes the input as `$1` and exports the
//! same value as `DRT_INPUT`; the argument wins).
//!
//! The input line is one scenario of the shared protocol (mirrored
//! verbatim from `lean/RewindDrt.lean`):
//!
//!     END <pos> [<seq>:<target>:<mode> ...]
//!
//! `pos` is the decimal prefix end; each triple is one rewind event —
//! `seq` and `target` are decimal Nats, `mode` a single bit
//! (`0` = on, `1` = before). The output is the active range list after
//! `rushi_common::rewind::active_ranges`:
//!
//!     EMPTY                       when there are no active ranges
//!     <lo>-<hi> <lo>-<hi> ...    otherwise, ranges in ascending order
//!
//! A malformed line (or a missing input) prints `ERR` and exits 1 —
//! identically to the Lean model executable; that symmetry is what the
//! DRT gate compares (after stripping trailing whitespace).

use rushi_common::rewind::{active_ranges, RewindRef};

/// Decimal string to a `usize`; `None` when empty or not all digits.
/// (Mirrors the Lean model's `natOfDec`.)
fn nat_of_dec(s: &str) -> Option<usize> {
    if s.is_empty() {
        return None;
    }
    s.parse::<usize>().ok()
}

/// A single-bit mode field: `0` is on mode, `1` is before mode.
/// (Mirrors the Lean model's `boolOfBit`.)
fn bool_of_bit(s: &str) -> Option<bool> {
    match s {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

/// One rewind triple `<seq>:<target>:<mode>`; `None` on a malformed
/// triple. (Mirrors the Lean model's `parseRewind`.)
fn parse_rewind(tok: &str) -> Option<RewindRef> {
    let parts: Vec<&str> = tok.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let seq = nat_of_dec(parts[0])?;
    let target = nat_of_dec(parts[1])?;
    let before = bool_of_bit(parts[2])?;
    Some(RewindRef {
        seq,
        target,
        before,
    })
}

/// One DRT input line to a scenario; `None` on a malformed line.
/// (Mirrors the Lean model's `parseLine`: `END <pos> [triple ...]`,
/// zero or more triples, every triple must parse.)
fn parse_line(line: &str) -> Option<(usize, Vec<RewindRef>)> {
    let tokens: Vec<&str> = line.split(' ').collect();
    // `"END" :: posTok :: tripleToks`: at least two tokens, first "END".
    if tokens.len() < 2 || tokens[0] != "END" {
        return None;
    }
    let pos = nat_of_dec(tokens[1])?;
    let mut rewinds = Vec::new();
    for triple in &tokens[2..] {
        let r = parse_rewind(triple)?;
        rewinds.push(r);
    }
    Some((pos, rewinds))
}

/// The canonical range-list rendering: the DRT output line (no
/// newline). `EMPTY` for the empty list, otherwise the ascending
/// `<lo>-<hi>` tokens single-space joined. (Mirrors the Lean model's
/// `renderRanges`.)
fn render_ranges(rs: &[(usize, usize)]) -> String {
    if rs.is_empty() {
        "EMPTY".to_string()
    } else {
        rs.iter()
            .map(|&(lo, hi)| format!("{lo}-{hi}"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn main() {
    let input = std::env::args()
        .nth(1)
        .unwrap_or_else(|| std::env::var("DRT_INPUT").unwrap_or_default());
    match parse_line(&input) {
        Some((pos, rewinds)) => {
            let ranges = active_ranges(pos, &rewinds);
            println!("{}", render_ranges(&ranges));
        }
        None => {
            println!("ERR");
            std::process::exit(1);
        }
    }
}
