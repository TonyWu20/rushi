//! `harness-hook-handoff` — reserved seam for a future handoff strategy.
//!
//! This hook is registered on `exhausted.handle` as a reserved seam.
//! Phase 2 ships the `stay_compact` default; this binary exists so a
//! future handoff strategy can be registered without touching the loop.
//!
//! It currently returns `{}` (no decision) so the loop applies the
//! window default (`stay_compact`).

use std::io::{self, Read};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return;
    }

    // Read stdin (the window JSON) but ignore it for now.
    let mut buf = String::new();
    let _ = io::stdin().read_to_string(&mut buf);

    // No decision: the loop applies the window default (stay_compact).
    println!("{{}}");
}

fn print_help() {
    println!("harness-hook-handoff — reserved handoff-strategy hook");
    println!();
    println!("Window: exhausted.handle");
    println!("Input (stdin): window JSON object");
    println!("Output (stdout): {{}} (no decision, default applies)");
    println!("Exit codes: 0 = ok");
}
