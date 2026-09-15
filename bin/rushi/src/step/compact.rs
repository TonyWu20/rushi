//! Compact with compact.before / compact.after hooks, the
//! post-compact sanity check, and the handoff document writers.

use rushi_common::hooks::{self, Window};
use rushi_common::stage::{
    CompactOutcome, CompactOpts, CompactReason, CompactStatus, SessionDir, StageRunner,
};

use crate::config::HarnessConfig;
use crate::stage_runner::SubprocessRunner;
use crate::step::hook::{hook_env, log_hook_window};
use crate::step::logio::append_event;

/// Compact with compact.before / compact.after hooks.
pub fn try_compact_with_hooks(
    cfg: &HarnessConfig,
    runner: &SubprocessRunner,
    session: &SessionDir,
    reason: CompactReason,
    force: bool,
    strip_last: bool,
) -> CompactStatus {
    let payload = serde_json::json!({
        "window": "compact.before",
        "session": session.path.to_string_lossy(),
        "reason": reason.as_str(),
        "force": force,
    });
    let results = hooks::fire_hooks(
        &cfg.hooks,
        Window::CompactBefore,
        &payload,
        &hook_env(cfg, session, "step", Window::CompactBefore),
        cfg.hooks_timeout_ms,
    );
    let (decision_opt, payload_val) =
        hooks::fold_decision(&results, Window::CompactBefore);
    let decision = decision_opt
        .unwrap_or_else(|| hooks::window_default(Window::CompactBefore).to_string());
    log_hook_window(cfg, session, "compact.before", &decision, &results);

    if decision == "cancel" {
        eprintln!("rushi: compact.before hook cancelled the compaction");
        return CompactStatus::noop();
    }

    if decision == "replace" {
        // The hook supplies its own summary and boundary.
        let summary = payload_val
            .get("summary")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        let first_kept_seq = payload_val
            .get("first_kept_seq")
            .and_then(|v| v.as_u64())
            .unwrap_or(1);
        // Compute version metadata for the DAG link.
        let log_vals: Vec<serde_json::Value> = std::fs::read_to_string(
            session.path.join("events.jsonl"),
        )
        .ok()
        .and_then(|raw| {
            raw.lines()
                .filter(|l| !l.trim().is_empty())
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect()
        })
        .unwrap_or_default();
        let (version, parent_version, diverge_seq) =
            rushi_common::compact_math::handoff_version_meta(&log_vals);
        write_handoff(session, &summary, version);
        // No compact binary ran on this path, so the loop appends the
        // boundary marker itself.
        append_compaction_summary(
            cfg,
            session,
            &summary,
            first_kept_seq,
            &reason,
            version,
            parent_version,
            diverge_seq,
        );
        eprintln!("rushi: compact.before hook replaced the compaction");
        return CompactStatus {
            outcome: CompactOutcome::Compacted,
            first_kept_seq: Some(first_kept_seq),
            tokens_before: None,
            tokens_after: None,
            summary: Some(summary),
            version: Some(version),
            parent_version,
            diverge_seq,
        };
    }

    let opts = CompactOpts {
        reason,
        force,
        strip_last_assistant: strip_last,
    };
    match runner.compact(session, &opts) {
        Ok(status) => {
            // Write the handoff document (docs/handoff-versioning-design.md).
            if status.outcome == CompactOutcome::Compacted {
                if let Some(ref summary_text) = status.summary {
                    let version = status.version.unwrap_or(1);
                    write_handoff(session, summary_text, version);
                }
            }
            let status_str = match status.outcome {
                CompactOutcome::Compacted => "compacted",
                CompactOutcome::Noop => "noop",
                CompactOutcome::Failed => "failed",
            };
            let payload2 = serde_json::json!({
                "window": "compact.after",
                "session": session.path.to_string_lossy(),
                "reason": reason.as_str(),
                "status": status_str,
            });
            let _ = hooks::fire_hooks(
                &cfg.hooks,
                Window::CompactAfter,
                &payload2,
                &hook_env(cfg, session, "step", Window::CompactAfter),
                cfg.hooks_timeout_ms,
            );
            status
        }
        Err(e) => {
            eprintln!("rushi: compact failed: {e}");
            // A failed compact call leaves no compaction event in the log
            // (the binary was killed or hung, and never reached its own
            // failure path). Publish the failure so the session log shows
            // why the context stayed above the trigger.
            let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            let detail = e.to_string();
            let event = serde_json::json!({
                "v": 1,
                "type": "ext_status",
                "ts": ts,
                "id": "compact.failed",
                "value": detail,
                "reason": reason.as_str(),
                "detail": detail,
            });
            append_event(cfg, &session.path, &event);
            CompactStatus::noop()
        }
    }
}

/// Post-compact sanity: if the projected post-compact size still
/// exceeds the trigger level, log a warning marker.
pub fn post_compact_sanity(
    cfg: &HarnessConfig,
    session: &SessionDir,
    status: &CompactStatus,
) {
    if status.outcome != CompactOutcome::Compacted {
        return;
    }
    let Some(tokens_after) = status.tokens_after else {
        return;
    };
    let trigger = cfg.trigger_level();
    if trigger > 0 && tokens_after > trigger {
        let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let event = serde_json::json!({
            "v": 1,
            "type": "ext_status",
            "ts": ts,
            "id": "compact_sanity",
            "value": format!("post-compact estimate {tokens_after} still exceeds trigger {trigger}"),
        });
        append_event(cfg, &session.path, &event);
    }
}

/// Save the handoff document to `sessions/<n>/handoff/v<version>.md`.
/// Also maintains `handoff.md` as a copy of the latest version for
/// backward compatibility (docs/handoff-versioning-design.md).
fn write_handoff(
    session: &SessionDir,
    summary: &str,
    version: u64,
) {
    let vdir = session.path.join("handoff");
    let vpath = vdir.join(format!("v{}.md", version));
    if let Err(e) = std::fs::create_dir_all(&vdir) {
        eprintln!("rushi: cannot create handoff dir: {e}");
    }
    if let Err(e) = std::fs::write(&vpath, summary) {
        eprintln!("rushi: cannot write handoff/v{}.md: {e}", version);
    }
    // Keep handoff.md as the latest version for backward compat.
    if let Err(e) = std::fs::write(session.path.join("handoff.md"), summary) {
        eprintln!("rushi: cannot write handoff.md: {e}");
    }
}

/// Append the `compaction_summary` boundary marker. Used only on the
/// `replace` path, where a hook supplied its own summary and boundary
/// and the `compact` binary did not run (docs/phase-2-plan.md 4.3).
fn append_compaction_summary(
    cfg: &HarnessConfig,
    session: &SessionDir,
    summary: &str,
    first_kept_seq: u64,
    reason: &CompactReason,
    version: u64,
    parent_version: u64,
    diverge_seq: u64,
) {
    let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let reason_str = match reason {
        CompactReason::Threshold => "threshold",
        CompactReason::Overflow | CompactReason::LastResort => "overflow",
    };
    let event = serde_json::json!({
        "v": 1,
        "type": "compaction_summary",
        "ts": ts,
        "summary": summary,
        "first_kept_seq": first_kept_seq,
        "version": version,
        "parent_version": parent_version,
        "diverge_seq": diverge_seq,
        "reason": reason_str,
        "tokens_before": 0,
    });
    append_event(cfg, &session.path, &event);
}
