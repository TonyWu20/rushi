//! Signal handling for the loop (docs/phase-2-plan.md section 4.7).
//!
//! The loop installs handlers for `SIGTERM` and `SIGINT` through
//! `libc::signal`. The handler sets an atomic flag (async-signal-safe);
//! the main thread polls the flag at natural points. On a caught
//! signal the in-flight stage child is killed (`LIVE_CHILD` in
//! `stage_runner`), then the process exits with the mapped code.
//!
//! The handlers use the default `SIG_DFL`-reset semantics on `exec`:
//! the stage children regain the normal disposition, so a TUI group
//! kill still reaches them (4.7).

use std::sync::atomic::{AtomicBool, Ordering};

static SIGTERM_FLAG: AtomicBool = AtomicBool::new(false);
static SIGINT_FLAG: AtomicBool = AtomicBool::new(false);

extern "C" fn term_handler(_sig: i32) {
    SIGTERM_FLAG.store(true, Ordering::SeqCst);
}

extern "C" fn int_handler(_sig: i32) {
    SIGINT_FLAG.store(true, Ordering::SeqCst);
}

/// A caught signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    /// `SIGTERM` caught.
    Term,
    /// `SIGINT` caught.
    Int,
}

impl Signal {
    /// The exit code `run` uses (143 on TERM, 130 on INT).
    pub fn run_exit_code(self) -> i32 {
        match self {
            Signal::Term => 143,
            Signal::Int => 130,
        }
    }
}

/// Install the `SIGTERM` and `SIGINT` handlers. Idempotent.
pub fn install() {
    unsafe {
        // `sighandler_t` is a size_t in this libc build; the handler
        // address is passed as its numeric value.
        let term: libc::sighandler_t =
            term_handler as *const () as _;
        let int: libc::sighandler_t = int_handler as *const () as _;
        libc::signal(libc::SIGTERM, term);
        libc::signal(libc::SIGINT, int);
    }
}

/// Poll for a caught signal. Consumes the flag on a hit.
pub fn poll() -> Option<Signal> {
    if SIGTERM_FLAG.swap(false, Ordering::SeqCst) {
        return Some(Signal::Term);
    }
    if SIGINT_FLAG.swap(false, Ordering::SeqCst) {
        return Some(Signal::Int);
    }
    None
}

/// Check for a caught signal. On a hit: cancel the in-flight child and
/// exit with `exit_code` (the caller picks 1 for `step`, 143/130 for `run`).
/// Returns when no signal is pending.
pub fn check_and_exit(exit_code: i32) {
    if poll().is_some() {
        crate::stage_runner::cancel_live_child();
        std::process::exit(exit_code);
    }
}

/// Check for a caught signal and exit with the signal-specific code
/// (143 for SIGTERM, 130 for SIGINT). Used by `harness run`.
/// For `harness step`, use `check_and_exit(1)` instead.
pub fn check_and_exit_for_run() {
    if let Some(sig) = poll() {
        crate::stage_runner::cancel_live_child();
        std::process::exit(sig.run_exit_code());
    }
}
