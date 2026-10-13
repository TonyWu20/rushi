//! One-step pipeline for `rushi step` (docs/phase-2-plan.md section 4.2).
//!
//! Mirrors `scripts/step.sh`: publish `model_thinking` at entry, run
//! `claim`, then dispatch on the derived state.
//!
//! The pipeline is split across submodules of this module to keep the
//! blast radius of each concern small (decision 2026-09-16, see
//! docs/kernel-complexity-audit.md F8):
//!
//! - [`logio`]   — appending typed events, `ext_status` markers, log scans
//! - [`hook`]    — hook window firing, `HARNESS_*` env pairs, result logging
//! - [`compact`] — compaction with hooks, post-compact sanity, handoff writes
//! - [`model`]   — model description, context estimate, the awaiting-model
//!                 pipeline (assemble, retry loop, overflow/length recovery)
//! - [`tool`]    — tool routing with tool.before/tool.after windows
//! - [`approval`]: approval round-trip and awaiting_approval resume
//!
//! This file keeps the public entry points (`do_step`, `make_runner`,
//! `StepMode`) and the claim-state dispatch.

mod approval;
mod compact;
pub mod hook;
mod logio;
mod model;
mod tool;

use std::path::Path;

use rushi_common::stage::{SessionDir, StageRunner};

use crate::config::HarnessConfig;
use crate::stage_runner::{new_subprocess_runner, SubprocessRunner};
use crate::step::approval::run_awaiting_approval;
use crate::step::tool::run_awaiting_tool_result;

// Re-export the public surface so external consumers keep the same
// paths (`crate::step::append_line`, `crate::step::fire_step_start`,
// …). `pub use` also binds the names locally for this file.
pub use crate::step::hook::fire_step_start;
pub use crate::step::logio::{append_line, line_is_error_event};
pub use crate::step::model::{
    describe_model, publish_model_thinking, run_awaiting_model,
};

/// Create a `SubprocessRunner` from the resolved config.
pub fn make_runner(cfg: &HarnessConfig) -> SubprocessRunner {
    new_subprocess_runner()
        .config_path(cfg.config_path.clone())
        .model_bin(cfg.model_bin.clone())
        .compact_bin(cfg.compact_bin.clone())
        .assemble_bin(cfg.assemble_bin.clone())
        .route_bin(cfg.route_bin.clone())
        .claim_bin(cfg.claim_bin.clone())
        .parse_bin(cfg.parse_bin.clone())
        .call()
}

/// How a caught signal exits the process (docs/phase-2-plan.md 4.7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepMode {
    /// `rushi step`: a caught signal exits with code 1.
    Step,
    /// `rushi run`: a caught signal exits 143 (SIGTERM) or 130 (SIGINT).
    Run,
}

/// The outcome of one step (docs/failure-tracking.md FT-031). An
/// `error` event settles no claim state: the step reports how it
/// ended, and the run loop stops on a failed step. A new loop
/// (a `rushi run` poke or the TUI `Ctrl+R`) retries the owed work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepOutcome {
    /// The step did its owed work (a turn advanced, tools routed,
    /// an approval answered). The run loop continues.
    Completed,
    /// Nothing was owed (idle or exhausted). The run wrapper decides
    /// the stop.
    NoWork,
    /// The step ended with a logged `error` event (a failed model
    /// attempt or a provider-aborted turn). The run loop stops; the
    /// owed work stays in the log for a new loop.
    Failed,
}

/// One step. Exits 0 on any clean stop. Exits 1 on a hard failure.
///
/// Returns the [`StepOutcome`] (FT-031): the run loop stops on a
/// `Failed` step; the `rushi step` one-shot ignores the outcome.
///
/// `mode` picks the signal exit code: `Step` exits 1 on a caught signal,
/// `Run` exits 143 (SIGTERM) or 130 (SIGINT).
pub fn do_step(cfg: &HarnessConfig, session_dir: &Path, mode: StepMode) -> StepOutcome {
    let runner = make_runner(cfg);
    let session = SessionDir {
        path: session_dir.to_path_buf(),
    };

    // Resolve the model description once and reuse it for the
    // `model_thinking` marker and the guard model id
    // (docs/phase-2-plan.md section 4.3 step 2).
    let describe = describe_model(cfg);

    // Step entry: publish `model_thinking` on-change before claim.
    publish_model_thinking(cfg, session_dir, &describe);

    let claim = match runner.claim(&session) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("rushi: claim failed: {e}");
            std::process::exit(1);
        }
    };

    // Fire the `step.start` observation window (docs/loop-lifecycle-hooks.md
    // section 3.2), after claim and before the branch dispatch.
    fire_step_start(cfg, &session, &claim);

    check_signal(mode);

    let outcome = match claim.state.as_str() {
        "idle" => {
            if claim.pending_follow_ups.is_empty() {
                // No follow-up: the `run` wrapper decides whether to
                // keep going via the `run.idle` window.
                StepOutcome::NoWork
            } else {
                // Idle with follow-ups: drain one batch as a new turn.
                run_awaiting_model(cfg, &runner, &session, &claim, true, &describe, mode)
            }
        }
        "exhausted" => {
            // Nothing owed. The handoff or in-session shadow compact
            // already closed this state.
            StepOutcome::NoWork
        }
        "awaiting_tool_result" => {
            run_awaiting_tool_result(cfg, &runner, &session, &claim);
            StepOutcome::Completed
        }
        "awaiting_model" => run_awaiting_model(cfg, &runner, &session, &claim, false, &describe, mode),
        "awaiting_approval" => {
            run_awaiting_approval(cfg, &runner, &session, &claim, mode);
            StepOutcome::Completed
        }
        other => {
            eprintln!("rushi: unknown claim state `{other}`");
            std::process::exit(1);
        }
    };

    check_signal(mode);
    outcome
}

/// Check for a caught signal and exit with the mode-appropriate code.
/// `Step` exits 1; `Run` exits 143 (SIGTERM) or 130 (SIGINT).
fn check_signal(mode: StepMode) {
    match mode {
        StepMode::Step => crate::signals::check_and_exit(1),
        StepMode::Run => crate::signals::check_and_exit_for_run(),
    }
}
