//! Hook window helpers for the step pipeline: firing observation
//! windows, building the `HARNESS_*` env pairs, logging window
//! results, and the `overflow.resolve` decision window.

use serde_json::Value;

use rushi_common::hooks::{self, Window};
use rushi_common::stage::{Claim, SessionDir};

use crate::config::HarnessConfig;
use crate::step::logio::append_event;

/// The `HARNESS_*` env pairs for a hook invocation. `phase` is the
/// rushi subcommand (`step` or `run`); `window` is the exact window
/// being fired so `HARNESS_WINDOW` always reports the real window
/// (docs/loop-lifecycle-hooks.md section 4.2).
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
    )
}

/// Fire the `step.start` observation window (docs/loop-lifecycle-hooks.md
/// section 3.2). Observation only, no decision.
pub fn fire_step_start(cfg: &HarnessConfig, session: &SessionDir, claim: &Claim) {
    let payload = serde_json::json!({
        "window": "step.start",
        "session": session.path.to_string_lossy(),
        "claim_state": claim.state.as_str(),
    });
    let results = hooks::fire_hooks(
        &cfg.hooks,
        Window::StepStart,
        &payload,
        &hook_env(cfg, session, "step", Window::StepStart),
        cfg.hooks_timeout_ms,
    );
    log_hook_window(cfg, session, "step.start", "", &results);
}

/// Log hook window results.
pub fn log_hook_window(
    cfg: &HarnessConfig,
    session: &SessionDir,
    window: &str,
    decision: &str,
    results: &[hooks::HookResult],
) {
    for r in results {
        if r.failed {
            let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            let event = serde_json::json!({
                "v": 1,
                "type": "ext_status",
                "ts": ts,
                "id": format!("hook.{window}.error"),
                "value": r.failure_detail.clone(),
            });
            append_event(cfg, &session.path, &event);
        }
    }
    if !decision.is_empty() {
        let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let event = serde_json::json!({
            "v": 1,
            "type": "ext_status",
            "ts": ts,
            "id": format!("hook.{window}"),
            "value": decision,
        });
        append_event(cfg, &session.path, &event);
    }
}

/// Fire the `overflow.resolve` window on overflow classification.
///
/// Carries the fields docs/loop-lifecycle-hooks.md section 3.4 names:
/// `kind`, `stop_reason`, `detail`, `usage`, `input_budget`,
/// `context_tokens`, and `can_recover`. Returns the folded decision:
/// the first explicit decision, then the exit-2 blocking default
/// (`stop`), then the window default (`stay_compact`).
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
    let results = hooks::fire_hooks(
        &cfg.hooks,
        Window::OverflowResolve,
        &payload,
        &hook_env(cfg, session, "step", Window::OverflowResolve),
        cfg.hooks_timeout_ms,
    );
    let (decision, _) = hooks::fold_decision(&results, Window::OverflowResolve);
    let decision = decision
        .unwrap_or_else(|| hooks::window_default(Window::OverflowResolve).to_string());
    log_hook_window(cfg, session, "overflow.resolve", &decision, &results);
    decision
}
