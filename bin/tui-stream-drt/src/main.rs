//! The DRT production executable for `lean/TuiStreamSpec.lean`
//! (docs/tui-streaming-response.md; the Lean model executable is
//! `lean/TuiStreamDrt.lean`).
//!
//! Usage: `tui-stream-drt <scenario line>` — or the same line in the
//! `DRT_INPUT` environment variable when no argument is given (the
//! `lean-verify` op=drt tool passes the input as `$1` and exports the
//! same value as `DRT_INPUT`; the argument wins).
//!
//! The input line is one scenario of the shared protocol (see
//! `tui_stream_drt::line` and `lean/TuiStreamDrt.lean`):
//!
//!     FT SC DRAFT SETTLED RESPONSES
//!
//! The output is the view after the spec's reference renderer
//! (`runResponses`):
//!
//!     follow=<0|1> scroll=<n> draft=<n>:<hex> settled=<k | k,<tok>,...>
//!
//! A malformed line (or a missing input) prints `ERR` and exits 1 —
//! identically to the Lean model executable; that symmetry is what
//! the DRT gate compares.

fn main() {
    let input = std::env::args()
        .nth(1)
        .unwrap_or_else(|| std::env::var("DRT_INPUT").unwrap_or_default());
    match tui_stream_drt::line::run(&input) {
        Some(out) => println!("{out}"),
        None => {
            println!("{}", tui_stream_drt::line::ERR);
            std::process::exit(1);
        }
    }
}
