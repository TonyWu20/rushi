//! Hook window helpers for the step pipeline: firing observation
//! windows, building the `HARNESS_*` env pairs, logging pipeline
//! results, and the `overflow.resolve` / `exhausted.handle`
//! strategy windows.
//!
//! The pipeline model (docs/loop-lifecycle-hooks.md section 12,
//! issue #38): each window has an ordered step list of named hook
//! defs. Steps run sequentially over the accumulated state; the
//! first abort or fail stops the chain. Markers:
//! `hook.<window>.chain` (one per run: per-step outcomes + stop
//! point), `hook.<window>.error` (per failed step),
//! `hook.<window>.unknown_fields` (inert fields in the final state,
//! once per run, 12.5), `hook.<window>` (the window's resolved
//! outcome).

use std::collections::BTreeSet;

use serde_json::Value;

use rushi_common::hooks::{self, PipelineRun, StepStatus, Window};
use rushi_common::stage::{Claim, SessionDir};

use crate::config::HarnessConfig;
use crate::step::logio::publish_ext_status;

/// The `HARNESS_*` env pairs for a hook invocation. `phase` is the
/// rushi subcommand (`step` or `run`); `window` is the exact window
/// being fired so `HARNESS_WINDOW` always reports the real window
/// (docs/loop-lifecycle-hooks.md section 4.2). `LOG_BIN` points at
/// the `log` binary so a hook may append events to the session log
/// itself (12.5, the `run.idle` follow-up message).
pub fn hook_env(
    cfg: &HarnessConfig,
    session: &SessionDir,
    phase: &'static str,
    window: Window,
) -> Vec<(&'static str, String)> {
    hooks::hook_env(
        &session.path.to_string_lossy(),
        &cfg.sessions_root.to_string_lossy(),
        &cfg.config_path.to_string_lossy(),
        phase,
        window,
        &cfg.log_bin.to_string_lossy(),
    )
}

/// Run one window's pipeline with the config's defs and step list.
pub fn run_window(
    cfg: &HarnessConfig,
    session: &SessionDir,
    window: Window,
    payload: &Value,
    phase: &'static str,
) -> PipelineRun {
    let steps = cfg.pipeline_for(window).to_vec();
    hooks::run_pipeline(
        &cfg.hook_defs,
        &steps,
        window,
        payload,
        &hook_env(cfg, session, phase, window),
        cfg.hooks_timeout_ms,
    )
}

/// The state fields the kernel consumes per window (docs/
/// loop-lifecycle-hooks.md 12.5 effect table). State keys outside
/// this set (and outside the window's base payload) are inert; they
/// are logged once per window firing.
fn effect_fields(window: Window) -> &'static [&'static str] {
    match window {
        Window::ToolBefore => &["blocked_calls", "approval"],
        Window::ToolAfter => &["results", "reason"],
        Window::CompactBefore => &["cancel", "replace"],
        Window::OverflowResolve | Window::ExhaustedHandle => &["stop"],
        // Observation windows have no effect table. `model.before`
        // has an open schema: its state is the request object itself,
        // whose fields are the request's, not kernel state fields.
        _ => &[],
    }
}

/// Fire an observation-only window pipeline and log its results.
///
/// Observation windows have no window effect, so no resolution
/// marker is written: only the chain marker and any error markers.
/// A window with no pipeline entry logs nothing (the byte-identical
/// no-hooks default, P1).
pub fn fire_observation(
    cfg: &HarnessConfig,
    session: &SessionDir,
    window: Window,
    payload: &Value,
) {
    let run = run_window(cfg, session, window, payload, "step");
    log_pipeline(cfg, session, window, &run, None, None);
}

/// Fire the `step.start` observation window (docs/loop-lifecycle-hooks.md
/// section 3.2).
pub fn fire_step_start(cfg: &HarnessConfig, session: &SessionDir, claim: &Claim) {
    let payload = serde_json::json!({
        "window": "step.start",
        "session": session.path.to_string_lossy(),
        "claim_state": claim.state.as_str(),
    });
    fire_observation(cfg, session, Window::StepStart, &payload);
}

/// Log a decision window's pipeline results (docs/loop-lifecycle-hooks.md
/// 12.7): one `hook.<window>.error` per failed step, one
/// `hook.<window>.unknown_fields` when the final state carries
/// inert fields (12.5, once per run), one `hook.<window>.chain`
/// with the per-step outcomes and the stop point, and — when
/// `resolution` is `Some` — one `hook.<window>` resolution marker.
/// When zero steps ran, nothing is logged (P1).
///
/// `base` is the pipeline's initial payload; unknown-field detection
/// compares the final state against it plus the window's effect
/// table. Pass `None` to skip the check (open-schema windows).
pub fn log_pipeline(
    cfg: &HarnessConfig,
    session: &SessionDir,
    window: Window,
    run: &PipelineRun,
    resolution: Option<&str>,
    base: Option<&Value>,
) {
    if run.steps.is_empty() {
        return;
    }
    let window_name = window.name();
    for s in &run.steps {
        if s.status == StepStatus::Fail {
            publish_ext_status(
                cfg,
                &session.path,
                &format!("hook.{window_name}.error"),
                &Value::String(format!("step '{}': {}", s.def, s.detail)),
            );
        }
    }
    if let (Some(base), Some(state_obj)) = (base, run.state.as_object()) {
        let mut known: BTreeSet<&str> = BTreeSet::new();
        if let Some(base_obj) = base.as_object() {
            for k in base_obj.keys() {
                known.insert(k.as_str());
            }
        }
        for f in effect_fields(window) {
            known.insert(f);
        }
        let unknown: Vec<Value> = state_obj
            .keys()
            .filter(|k| !known.contains(k.as_str()))
            .map(|k| Value::String((*k).clone()))
            .collect();
        if !unknown.is_empty() {
            publish_ext_status(
                cfg,
                &session.path,
                &format!("hook.{window_name}.unknown_fields"),
                &Value::Array(unknown),
            );
        }
    }
    publish_ext_status(
        cfg,
        &session.path,
        &format!("hook.{window_name}.chain"),
        &run.chain_value(),
    );
    if let Some(res) = resolution {
        publish_ext_status(
            cfg,
            &session.path,
            &format!("hook.{window_name}"),
            &Value::String(res.to_string()),
        );
    }
}

/// Strategy-window resolution (12.5 effect table for
/// `overflow.resolve` and `exhausted.handle`): the state field
/// `stop` vetoes the strategy cycle, and an `abort` (exit 2) vetoes
/// the window default (`stay_compact`) — so it also stops.
/// A failed chain falls back to the default (`stay_compact`, P4).
pub fn resolve_strategy_window(run: &PipelineRun) -> &'static str {
    if !run.failed
        && (run.aborted
            || run.state.get("stop").map(|v| !v.is_null()).unwrap_or(false))
    {
        "stop"
    } else {
        "stay_compact"
    }
}

/// Fire the `overflow.resolve` window on overflow classification.
///
/// Carries the fields docs/loop-lifecycle-hooks.md section 3.4
/// names: `kind`, `stop_reason`, `detail`, `usage`, `input_budget`,
/// `context_tokens`, and `can_recover`. Returns the resolved
/// decision: `stop` (veto the strategy cycle) or `stay_compact`
/// (the default: keep the in-session shadow-compact strategy).
pub fn fire_overflow_resolve(
    cfg: &HarnessConfig,
    session: &SessionDir,
    active_model: &str,
    stop_reason: &str,
    detail: &str,
    usage: &Value,
    can_recover: bool,
) -> String {
    let payload = serde_json::json!({
        "window": "overflow.resolve",
        "session": session.path.to_string_lossy(),
        "active_model": active_model,
        "kind": "overflow",
        "stop_reason": stop_reason,
        "detail": detail,
        "usage": usage,
        "input_budget": cfg.input_budget,
        "context_tokens": cfg.context_tokens,
        "can_recover": can_recover,
    });
    let run = run_window(cfg, session, Window::OverflowResolve, &payload, "step");
    let decision = resolve_strategy_window(&run).to_string();
    log_pipeline(
        cfg,
        session,
        Window::OverflowResolve,
        &run,
        Some(&decision),
        Some(&payload),
    );
    decision
}
