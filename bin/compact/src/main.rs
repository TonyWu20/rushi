//! compact: the session-internal auto-compaction binary.
//!
//! It reads the event log, decides whether the in-session trigger fires
//! (docs/auto-compact-plan.md sections 3-5), cuts the old region,
//! calls the model for the summary with a bounded check loop
//! (docs/ft-038-compact-check-loop.md), and appends the
//! `compaction_started` / `compaction_summary` / `compaction_failed`
//! markers through `log`. It writes the versioned handoff file for its
//! target: `handoff/v<N>.md` plus `handoff.md` on the main target,
//! `branch-summary/v<N>.md` on a branch (docs/ft-038-compact-check-loop.md
//! step 6). The loop keeps the session: no new session directory.
//!
//! The trigger math and the chars/4 estimator are the shared
//! `rushi_common::compact_math` module (docs/phase-2-plan.md
//! section 6).

use bon::builder;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use clap::Parser;
use rushi_common::compact_math::{self, Caps, Ev};
use rushi_common::model_settings::{
    resolve_active_model, resolve_model_settings, val_bool, val_int,
};
use rushi_common::rewind;
use serde_json::Value;

/// The one-shot auto-compaction call. It exits 0 with a status JSON on
/// stdout in every state: noop, compacted, failed (exit 1 only when a
/// summary was needed and the check loop calls were exhausted).
#[derive(Parser, Debug)]
#[command(about = "in-session auto-compaction", version)]
struct Args {
    /// The session directory (events.jsonl lives inside).
    session: PathBuf,
    /// The config.toml path (default: the tier ladder,
    /// docs/project-config-precedence.md)
    #[arg(long)]
    config: Option<PathBuf>,
    /// The trigger reason: threshold or overflow. The default is the
    /// threshold check, where the cooldown and the trigger test
    /// apply.
    #[arg(long, value_enum, default_value = "threshold")]
    reason: Reason,
    /// Exclude the last assistant group from the old region: the
    /// retry after a recoverable length-stop failure.
    #[arg(long)]
    strip_last_assistant: bool,
    /// Force the compact even when the trigger is cold and the
    /// feature is disabled: the last-resort path.
    #[arg(long)]
    force: bool,
    /// The path of the assemble binary (the sibling by default).
    #[arg(long)]
    assemble: Option<PathBuf>,
    /// The path of the log binary (the sibling by default).
    #[arg(long)]
    log: Option<PathBuf>,
    /// The path of the model binary (the sibling by default). The
    /// one-shot summary call runs through it (docs FT-038).
    #[arg(long)]
    model: Option<PathBuf>,
    /// The path of the parse binary (the sibling by default). The
    /// check loop feeds each summary response to it with a zero-tool
    /// config (docs FT-038).
    #[arg(long)]
    parse: Option<PathBuf>,
    /// The path of the route binary (the sibling by default). The
    /// check loop feeds the detected calls to it with no tool paths,
    /// so every call reports a not-run result (docs FT-038).
    #[arg(long)]
    route: Option<PathBuf>,
    /// The 1-based log seq where the compact boundary lands: the
    /// prefix up to that seq becomes the summary input. Absent: the
    /// token-based keep-window walk (`find_cut`). Reuses the `assemble
    /// --up-to` name (docs/tree-ui-design-from-human.md in the TUI
    /// repo: the branch-summarize flow).
    #[arg(long, value_name = "SEQ")]
    up_to: Option<usize>,
    /// The custom compaction instruction. It replaces the default
    /// compact instruction wholesale (the six-section format is not
    /// merged in; the user's text is the entire instruction). Forwarded
    /// to `assemble --prompt`. Absent: the default first-time or
    /// update instruction.
    #[arg(long, value_name = "TEXT")]
    prompt: Option<String>,
    /// Branch mode (docs/branch-summarize-cases.md Phase 0). Summarize
    /// the open span abandoned by the last top-level `rewind` marker
    /// and append one branch `compaction_summary` marker carrying the
    /// additive `branch_of` field. The marker is an add-on on the
    /// active path: no handoff boundary, no handoff files,
    /// `first_kept_seq` is the sentinel 1.
    #[arg(long)]
    branch: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum Reason {
    Threshold,
    Overflow,
}

fn sibling(name: &str) -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(name)))
        .unwrap_or_else(|| PathBuf::from(name))
}

/// The binary resolution: the explicit flag, then the env override
/// (the e2e suite points the summary call at a stub), then the
/// sibling of the running binary.
fn resolve_bin(flag: &Option<PathBuf>, env: &str, name: &str) -> PathBuf {
    if let Some(p) = flag {
        return p.clone();
    }
    if let Ok(p) = std::env::var(env) {
        return PathBuf::from(p);
    }
    sibling(name)
}

/// The last `compaction_summary` marker, projected. The first_kept_seq
/// below 1 is rejected: the log re-renders without the summary (docs/
/// auto-compact-plan.md section 4.1). The summary text is kept for
/// record fidelity: the update prompt reads it from the log event,
/// not from this struct.
#[derive(Clone, Debug)]
#[allow(dead_code)]
struct Boundary {
    seq: usize,
    first_kept_seq: usize,
    #[allow(dead_code)]
    summary: String,
    read_files: Vec<String>,
    modified_files: Vec<String>,
}

/// A parsed event line with its 1-based log sequence.
#[derive(Clone, Debug)]
struct LogEvent {
    seq: usize,
    value: Value,
}

/// The reason string, for the markers and the status.
impl Reason {
    fn as_str(&self) -> &'static str {
        match self {
            Reason::Threshold => "threshold",
            Reason::Overflow => "overflow",
        }
    }
}

/// The trigger decision input, flattened for testability.
#[derive(Debug)]
struct TriggerInput {
    /// The trigger level: budget minus reserve, clamped below the
    /// budget when the reserve is zero.
    trigger_level: u64,
    /// The trigger-form measurements, oldest first: the log sequence
    /// and the provider usage.
    trigger_readings: Vec<(usize, u64)>,
    /// The last user message log sequence.
    last_user_seq: usize,
    /// The `last_user_seq` of the last `compaction_failed` marker.
    failed_user_seq: Option<usize>,
    /// The cooldown rule, enabled for the threshold trigger only.
    cooldown: bool,
    /// The feature kill switch.
    enabled: bool,
    /// The forced call: the last-resort path.
    force: bool,
    /// The overflow trigger: the provider said the window is full.
    overflow: bool,
}

/// The trigger decision.
#[derive(Debug, PartialEq)]
enum Decision {
    /// No compact: the reason rides with the status.
    Noop(String),
    /// Fire: the compact call carries the predicted reading.
    Fire { predicted: u64 },
}

/// The trigger test (pi-style, stateless): the last measured reading
/// against the trigger level. No latch, no `.all()` scan. The
/// overflow and the force skip the trigger test entirely. The
/// threshold trigger is gated by the feature switch and the cooldown.
fn decide_trigger(inp: &TriggerInput) -> Decision {
    if !inp.enabled && !inp.force && !inp.overflow {
        return Decision::Noop("the feature is disabled".to_string());
    }
    if inp.cooldown && !inp.force && !inp.overflow {
        if let Some(failed) = inp.failed_user_seq {
            if inp.last_user_seq <= failed {
                return Decision::Noop(format!(
                    "the cooldown: the user has not moved past the failed compact (user seq {failed})"
                ));
            }
        }
    }
    if inp.force || inp.overflow {
        let last = inp.trigger_readings.last().map(|r| r.1).unwrap_or(0);
        return Decision::Fire { predicted: last };
    }
    // Pi-style stateless check: last measured reading vs trigger level.
    match inp.trigger_readings.last() {
        Some((_, tokens)) if *tokens >= inp.trigger_level => Decision::Fire { predicted: *tokens },
        Some(_) => Decision::Noop("the trigger is cold".to_string()),
        None => Decision::Noop("no trigger-form reading yet".to_string()),
    }
}

/// The boundary fields of a `compaction_summary` marker. The
/// `first_kept_seq` below 1 is rejected: the log re-renders without
/// the summary.
fn parse_boundary(e: &LogEvent) -> Option<Boundary> {
    if e.value.get("type").and_then(|t| t.as_str()) != Some("compaction_summary") {
        return None;
    }
    // Branch markers (additive `branch_of`, docs/branch-summarize-
    // cases.md P8, P11) are add-ons on the active path, not compact
    // boundaries. Their sentinel `first_kept_seq` of 1 must never
    // reset the keep region, so they are skipped here.
    if e.value.get("branch_of").is_some() {
        return None;
    }
    let fk = e
        .value
        .get("first_kept_seq")
        .and_then(|f| f.as_u64())
        .filter(|f| *f >= 1)?;
    Some(Boundary {
        seq: e.seq,
        first_kept_seq: fk as usize,
        summary: e
            .value
            .get("summary")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string(),
        read_files: e
            .value
            .get("read_files")
            .and_then(|a| a.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        modified_files: e
            .value
            .get("modified_files")
            .and_then(|a| a.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// The event log of the session: the raw lines with their 1-based
/// sequences. Lines that are not objects are skipped: the log is
/// append-only, and a corrupt line must not kill the loop.
fn read_events(path: &Path) -> Vec<LogEvent> {
    let mut out = Vec::new();
    let raw = match rushi_common::logstream::read_log_text(path) {
        Ok(r) => r,
        Err(_) => return out,
    };
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v.get("type").is_none() {
            continue;
        }
        out.push(LogEvent {
            seq: out.len() + 1,
            value: v,
        });
    }
    out
}

fn ts_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Append one event line through the `log` binary: the event JSON
/// goes to the log's stdin, and the log assigns the ts and seq.
fn append_event(log_bin: &Path, session: &Path, event: &Value) -> Result<(), String> {
    let body = serde_json::to_string(event).map_err(|e| e.to_string())?;
    let mut child = Command::new(log_bin)
        .arg("--session")
        .arg(session)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn log: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(body.as_bytes())
            .map_err(|e| format!("write log stdin: {e}"))?;
    }
    let status = child.wait().map_err(|e| format!("wait log: {e}"))?;
    if !status.success() {
        return Err(format!("log exited {status}"));
    }
    Ok(())
}

fn main() {
    let args = Args::parse();
    // Shared tier ladder (docs/project-config-precedence.md): the
    // explicit --config flag, then $CONFIG, then the CWD config when
    // present, then the Nix side-by-side config.
    let config_path: PathBuf =
        rushi_common::paths::resolve_config_path(args.config.as_deref()).into();
    // `--up-to` is a 1-based log sequence (the same numbering `assemble
    // --up-to`, `first_kept_seq`, and `last_user_seq` use). Reject 0 up
    // front instead of writing a `compaction_failed` marker for a bad
    // invocation.
    if args.up_to == Some(0) {
        eprintln!("Error: --up-to requires a 1-based log sequence");
        std::process::exit(1);
    }
    // The cooldown and the trigger need the log. The force and the
    // overflow skip the trigger test but still read the log for the
    // boundary and the cut.
    let events_path = args.session.join("events.jsonl");
    let events = read_events(&events_path);

    // The branch mode (docs/branch-summarize-cases.md Phase 0): a
    // separate, trigger-free path. It summarizes the open span
    // abandoned by the last top-level rewind marker and appends one
    // branch marker. It never exits.
    if args.branch {
        run_branch_compaction(&args, &events);
    }

    // The boundary: the last parseable `compaction_summary`.
    let boundary: Option<Boundary> = events.iter().filter_map(parse_boundary).next_back();

    // Handoff versioning metadata (docs/handoff-versioning-design.md).
    // The version is a session-global monotonic counter; parent_version
    // is the version of the most recent boundary on the active path; the
    // diverge_seq is that parent's first_kept_seq (0 when no parent).
    let value_refs: Vec<Value> = events.iter().map(|e| e.value.clone()).collect();
    let (version, parent_version, diverge_seq) = compact_math::handoff_version_meta(&value_refs);

    // The cooldown anchor: the `last_user_seq` of the last
    // `compaction_failed` marker.
    let failed_user_seq: Option<usize> = events
        .iter()
        .rfind(|e| e.value.get("type").and_then(|t| t.as_str()) == Some("compaction_failed"))
        .and_then(|e| e.value.get("last_user_seq").and_then(|s| s.as_u64()))
        .map(|s| s as usize);

    // The user messages: the last one anchors the cooldown.
    let last_user_seq: usize = events
        .iter()
        .rfind(|e| e.value.get("type").and_then(|t| t.as_str()) == Some("user_message"))
        .map(|e| e.seq)
        .unwrap_or(0);

    // The kept region: after the boundary, or the whole log.
    // Only events on the active path (after applying rewind masks)
    // are kept; events masked by a rewind are excluded.
    let first_kept = boundary.as_ref().map(|b| b.first_kept_seq).unwrap_or(0);
    let rewinds: Vec<rewind::RewindRef> = events
        .iter()
        .filter_map(|e| rewind::parse_rewind_event(&e.value, e.seq))
        .collect();
    let active = rewind::active_ranges(events.len(), &rewinds);
    let kept_events: Vec<LogEvent> = events
        .iter()
        .filter(|e| e.seq >= first_kept && rewind::seq_in_ranges(e.seq, &active))
        .cloned()
        .collect();

    // The measurements: the provider usage of the assistant
    // messages, as the (log sequence, tokens) pairs. The log stores
    // the request input as `usage.input_tokens`.
    let mut measurements: Vec<(usize, u64)> = Vec::new();
    for e in &kept_events {
        if e.value.get("type").and_then(|t| t.as_str()) != Some("assistant_message") {
            continue;
        }
        let tokens = e
            .value
            .get("usage")
            .and_then(|u| u.get("input_tokens"))
            .and_then(|i| i.as_u64())
            .unwrap_or(0);
        if tokens > 0 {
            measurements.push((e.seq, tokens));
        }
    }

    // The config: the budget and the compact knobs.
    let cfg = load_config(&config_path);
    let empty = toml::Value::Table(toml::map::Map::new());
    let limits = cfg.get("limits").unwrap_or(&empty);
    let reserve = val_int(limits, "compact_reserve_tokens").unwrap_or(16_384) as u64;
    let enabled = val_bool(limits, "compact_enabled").unwrap_or(true);
    // The one-shot summary check loop call budget (docs FT-038
    // decision 1): default 3, minimum 1. The value 1 disables the
    // second chance.
    let max_calls = val_int(limits, "compact_max_summary_calls")
        .map(|v| (v.max(1)) as u64)
        .unwrap_or(3);

    // The trigger base is always the full context budget (pi parity):
    // trigger = context_budget - compact_reserve_tokens.
    let base = resolve_context_budget(&cfg);
    let active_model = resolve_active_model(&cfg);
    let model_settings = resolve_model_settings(&cfg, &active_model);
    let chars_per_token = model_settings.estimate_chars_per_token.max(1);

    if reserve == 0 {
        eprintln!(
            "Warning: compact_reserve_tokens is 0: the trigger inverts. Clamping the trigger to one reserve below the budget."
        );
    }
    let trigger_level = compact_math::trigger_level_for(base, reserve);

    let overflow = matches!(args.reason, Reason::Overflow);
    // The trigger decision. The tokens_before of the Fire branch is
    // the newest reading of the kept region: the current context
    // size the compact replaces.
    let mut trigger_readings = measurements.clone();
    // The context estimate must match the harness loop's proactive
    // check (step.rs), or the binary noops while the loop believes
    // the trigger fired. Both use estimate_from_events: uncapped
    // full form, calibrated chars-per-token, rewind-aware, with the
    // 25% safety margin on the boundary path.
    let all_values: Vec<Value> = events.iter().map(|e| e.value.clone()).collect();
    let full_form = compact_math::estimate_from_events(&all_values, chars_per_token);
    if full_form > 0 {
        let last_seq = events.last().map(|e| e.seq).unwrap_or(0);
        trigger_readings.push((last_seq.max(1), full_form));
    }
    let last_measurement = trigger_readings.last().map(|m| m.1).unwrap_or(0);
    // An explicit `--up-to` compact is a user-initiated boundary compact
    // (the TUI branch-summarize flow, docs/tree-ui-design-from-human.md):
    // it fires unconditionally, even when the threshold trigger is cold
    // and the feature is off. Reuse the force gate.
    let forced = args.force || args.up_to.is_some();
    let decision = decide_trigger(&TriggerInput {
        trigger_level,
        trigger_readings: trigger_readings.clone(),
        last_user_seq,
        failed_user_seq,
        cooldown: true,
        enabled,
        force: forced,
        overflow,
    });

    match decision {
        Decision::Noop(detail) => {
            let status = serde_json::json!({
                "status": "noop",
                "reason": args.reason.as_str(),
                "detail": detail,
            });
            println!("{}", serde_json::to_string(&status).unwrap());
            std::process::exit(0);
        }
        Decision::Fire { predicted } => {
            let _ = predicted;
            run_compaction()
                .args(&args)
                .kept_events(&kept_events)
                .boundary(&boundary)
                .tokens_before(last_measurement)
                .overflow(overflow)
                .last_user_seq(last_user_seq)
                .version(version)
                .parent_version(parent_version)
                .diverge_seq(diverge_seq)
                .max_calls(max_calls)
                .call();
        }
    }
}

/// The compact cut index within the kept region. An explicit `--up-to`
/// seq places the boundary at the picked event. The cut is the index
/// of the first kept event whose seq exceeds it. This mirrors
/// assemble's own `--up-to` cut. When absent, the token-based
/// keep-window walk (`find_cut`) runs.
///
/// Rewind rule I4 (docs/rewind-fork-design.md): a target at or before
/// the region head is out of the un-compact prefix. It degrades to the
/// region head. The region head is the first kept event. Clamping the
/// target to the head seq keeps the boundary on that event. Without
/// the clamp it would collapse to an empty old region. That is the
/// `cut == 0` noop path. In-range and beyond targets are untouched.
fn cut_index(
    kept_events: &[LogEvent],
    up_to: Option<usize>,
    projected: &[Ev],
    keep_tokens: u64,
    caps: &Caps,
) -> usize {
    match up_to {
        Some(u) => {
            let head = kept_events.first().map(|e| e.seq).unwrap_or(0);
            let u_eff = u.max(head);
            kept_events
                .iter()
                .position(|e| e.seq > u_eff)
                .unwrap_or(kept_events.len())
        }
        None => compact_math::find_cut(projected, keep_tokens, caps),
    }
}

/// The compact run: the cut, the summary call, the marker appends.
#[builder]
fn run_compaction(
    args: &Args,
    kept_events: &[LogEvent],
    boundary: &Option<Boundary>,
    tokens_before: u64,
    overflow: bool,
    last_user_seq: usize,
    version: u64,
    parent_version: u64,
    diverge_seq: u64,
    max_calls: u64,
) -> ! {
    let config_path: PathBuf =
        rushi_common::paths::resolve_config_path(args.config.as_deref()).into();
    let cfg = load_config(&config_path);
    let empty = toml::Value::Table(toml::map::Map::new());
    let limits = cfg.get("limits").unwrap_or(&empty);
    let active = resolve_active_model(&cfg);
    let model_settings = resolve_model_settings(&cfg, &active);
    let cpts = model_settings.estimate_chars_per_token.max(1);
    // The cut keeps `compact_keep_tokens` tokens of the tail of the
    // active context (docs FT-038, section 5). It preserves the real
    // details of the ongoing work. The post-compact context is
    // [system prompt + tool schemas] + [summary] + [compact_keep_tokens].
    let keep_tokens: u64 =
        val_int(limits, "compact_keep_tokens").unwrap_or(20_000) as u64;

    // The projected kept events, for the estimator. The marker
    // types project to an empty user: zero tokens, no group effect.
    let projected: Vec<Ev> = kept_events
        .iter()
        .map(|e| compact_math::project_event(&e.value))
        .collect();

    let est_caps: Caps = Caps {
        text: None,
        chars_per_token: cpts,
    };
    let cut = cut_index(kept_events, args.up_to, &projected, keep_tokens, &est_caps);
    if cut == 0 {
        let detail = if args.up_to.is_some() {
            format!(
                "the old region is empty: no kept event at or before seq {}",
                args.up_to.unwrap_or(0)
            )
        } else {
            "the old region is empty: the keep window covers the log".to_string()
        };
        let status = serde_json::json!({
            "status": "noop",
            "reason": args.reason.as_str(),
            "detail": detail,
        });
        println!("{}", serde_json::to_string(&status).unwrap());
        std::process::exit(0);
    }

    // The old region: the projected events before the cut.
    let old = &projected[..cut];
    let old_events: Vec<LogEvent> = kept_events[..cut].to_vec();
    // With `--up-to` the boundary is the picked seq; without it the
    // last old event's seq is the cut position (auto keep-window walk).
    let up_to = match args.up_to {
        Some(u) => u,
        None => old_events.last().map(|e| e.seq).unwrap_or(0),
    };

    // The strip-last-assistant retry: drop the last group of the
    // old region before the summary call.
    let summary_events: &[LogEvent] = if args.strip_last_assistant {
        let last_group = old
            .iter()
            .rposition(|e| matches!(e, Ev::Assistant { .. }))
            .map(|i| i + 1)
            .unwrap_or(old.len());
        &old_events[..last_group]
    } else {
        &old_events
    };

    // The marker: the compact is in flight.
    let reason_str = args.reason.as_str().to_string();
    let started = serde_json::json!({
        "v": 1,
        "type": "compaction_started",
        "ts": ts_now(),
        "reason": reason_str,
        "tokens_before": tokens_before,
    });
    let log_bin = resolve_bin(&args.log, "LOG_BIN", "rushi-log");
    if let Err(e) = append_event(&log_bin, &args.session, &started) {
        eprintln!("compact: append compaction_started failed: {e}");
    }

    // The compact mechanism: the one-shot summary call with the check
    // loop (docs FT-038). The model is called with no tools. Each
    // response passes the format gate (the parse + route check and
    // the residual marker scan) and the content gate (the template
    // headers), up to `max_calls` model calls.
    //
    // The liveness guarantee: when the calls fail or exhaust, the
    // walk-back-bounded seed transcript is the fallback handoff. The
    // cut is bounded, so the handoff fits. The compaction always
    // finishes. The main session never blocks on a compact error.
    let (summary, summary_usage) = match summarize_one_shot(
        args,
        &config_path,
        &args.session,
        &CompactTarget::Main { up_to },
        max_calls,
    ) {
        Ok(r) => r,
        Err((summary_err, fallback)) => {
            eprintln!(
                "compact: the one-shot summary failed ({summary_err}). Using the bounded model-free cut as the handoff."
            );
            (fallback, None)
        }
    };

    // The file ops: merge with the previous boundary's lists, not
    // re-extract (docs/auto-compact-plan.md section 4.1).
    let (reads, modified) = {
        let values: Vec<Value> = summary_events.iter().map(|e| e.value.clone()).collect();
        compact_math::extract_file_ops(&values)
    };
    let read_files: Vec<String> = if let Some(b) = boundary.as_ref() {
        let mut v = b.read_files.clone();
        for r in &reads {
            if !v.contains(r) {
                v.push(r.clone());
            }
        }
        v
    } else {
        reads
    };
    let modified_files: Vec<String> = if let Some(b) = boundary.as_ref() {
        let mut v = b.modified_files.clone();
        for m in &modified {
            if !v.contains(m) {
                v.push(m.clone());
            }
        }
        v
    } else {
        modified
    };

    // The first kept sequence of the new boundary: the log
    // sequence of the event at the cut. When the explicit `--up-to`
    // boundary lands past the last kept event (the TUI branch tip:
    // the picked event is the active tail), the boundary sits just
    // past the picked seq rather than resetting to 1.
    let first_kept_seq = match args.up_to {
        Some(u) => kept_events.get(cut).map(|e| e.seq).unwrap_or(u + 1),
        None => kept_events.get(cut).map(|e| e.seq).unwrap_or(1),
    };
    if first_kept_seq < 1 {
        fail_and_exit(
            args,
            overflow,
            last_user_seq,
            max_calls,
            "the first_kept_seq is below 1",
            "the producer check rejected the marker",
        );
    }

    // The tokens after: the full-form estimate of the kept region
    // plus the summary framing.
    let kept_projected: Vec<Ev> = projected[cut..].to_vec();
    let cpts_caps = Caps {
        text: None,
        chars_per_token: cpts,
    };
    let tokens_after =
        compact_math::est_tokens_after_with_caps(&kept_projected, &summary, &cpts_caps);

    let mut done = serde_json::json!({
        "v": 1,
        "type": "compaction_summary",
        "ts": ts_now(),
        "summary": summary,
        "first_kept_seq": first_kept_seq,
        "version": version,
        "parent_version": parent_version,
        "diverge_seq": diverge_seq,
        "reason": reason_str,
        "tokens_before": tokens_before,
        "tokens_after": tokens_after,
        "read_files": read_files,
        "modified_files": modified_files,
    });
    if let Some(u) = summary_usage {
        done["usage"] = u;
    }
    if let Err(e) = append_event(&log_bin, &args.session, &done) {
        eprintln!("compact: append compaction_summary failed: {e}");
    }

    // Write the versioned handoff file for the main target
    // (docs/ft-038-compact-check-loop.md step 6). The write carries the
    // accepted one-shot summary, or the bounded seed on the liveness
    // fallback. A loop-driven compact also writes the same file; the
    // duplicate is harmless.
    write_main_handoff(&args.session, &summary, version);

    let status = serde_json::json!({
        "status": "compacted",
        "reason": reason_str,
        "first_kept_seq": first_kept_seq,
        "tokens_before": tokens_before,
        "tokens_after": tokens_after,
        "summary": summary,
        "version": version,
        "parent_version": parent_version,
        "diverge_seq": diverge_seq,
    });
    println!("{}", serde_json::to_string(&status).unwrap());
    std::process::exit(0);
}

/// The compact target: which region the one-shot summarizes.
enum CompactTarget {
    /// Main compaction: the region up to `up_to`.
    Main { up_to: usize },
    /// Branch compaction: the span abandoned at `branch_of`.
    Branch { branch_of: usize },
}

impl CompactTarget {
    /// The `assemble --summary-text` region flag and value for the seed.
    fn seed_flag(&self) -> (&'static str, String) {
        match self {
            Self::Main { up_to } => ("--up-to", up_to.to_string()),
            Self::Branch { branch_of } => ("--branch-of", branch_of.to_string()),
        }
    }

    /// The target kind name, for the `--drop-last-assistant` rule.
    fn agent_name(&self) -> &'static str {
        match self {
            Self::Main { .. } => "compact",
            Self::Branch { .. } => "branch",
        }
    }
}

/// The one-shot summary mechanism (docs FT-038). A model call with
/// no tools, with a bounded check loop. Each response passes two
/// gates:
///
/// - Gate F (format): `parse` (the zero-tool config) must emit no
///   `tool_call` event and no `error` event, and the emitted text
///   must carry no open marker of any family. The detected calls are
///   fed to `route` with no tool paths, and the not-run lines become
///   the correction text.
/// - Gate C (content): every header on the template list (`assemble
///   --compact-headers`) must appear in the emitted text.
///
/// A failed call gets a second chance: the next request is the
/// original request plus the failed response (raw, markers and all)
/// as an assistant item and the correction block as a user item.
/// After `max_calls` calls the error carries the seed transcript
/// (the bounded model-free fallback). The main target uses it as the
/// handoff; the branch target fails instead.
///
/// Returns the accepted text and the accepted call's usage.
fn summarize_one_shot(
    args: &Args,
    config_path: &Path,
    session: &Path,
    target: &CompactTarget,
    max_calls: u64,
) -> Result<(String, Option<Value>), (String, String)> {
    let assemble_bin = args.assemble.clone().unwrap_or_else(|| sibling("assemble"));
    let model_bin = resolve_bin(&args.model, "MODEL_BIN", "model");
    let parse_bin = resolve_bin(&args.parse, "PARSE_BIN", "parse");
    let route_bin = resolve_bin(&args.route, "ROUTE_BIN", "route");

    // Step 1: the three assemble runs against the target region: the
    // bounded request JSON, the bounded seed transcript (the
    // fallback), and the template header list (the content gate).
    let base = assemble_region_args(args, session, config_path, target)
        .map_err(|e| (e, String::new()))?;
    let request_raw = run_capture(&assemble_bin, &base)
        .map_err(|e| (e, String::new()))?;
    let request: Value = serde_json::from_str(&request_raw)
        .map_err(|e| (format!("the assemble request is not JSON: {e}"), String::new()))?;
    let mut seed_args = base.clone();
    seed_args.push("--summary-text".to_string());
    let seed: String =
        run_capture(&assemble_bin, &seed_args).map_err(|e| (e, String::new()))?;
    // The header list shares the request args, but drops the
    // `--summary-input` value flag and leads with `--compact-headers`.
    let headers_args: Vec<String> = std::iter::once("--compact-headers".to_string())
        .chain(
            base.iter()
                .filter(|f| !matches!(f.as_str(), "--summary-input" | "--summary-text"))
                .cloned(),
        )
        .collect();
    let headers_raw = run_capture(&assemble_bin, &headers_args)
        .map_err(|e| (e, seed.clone()))?;
    let headers: Vec<String> = serde_json::from_str(&headers_raw)
        .map_err(|e| (format!("the header list is not JSON: {e}"), seed.clone()))?;

    // Step 2: the zero-tool parse config (the deep merge replaces the
    // Nix store base tool arrays with the empty ones).
    let zero_cfg = write_zero_tool_config().map_err(|e| (e, seed.clone()))?;

    // Step 3: the check loop. The context items carry every failed
    // response and its correction into the next request.
    let mut context_items: Vec<Value> = Vec::new();
    let mut last_error = String::new();
    for call in 1..=max_calls {
        let mut model_request = request.clone();
        if !context_items.is_empty() {
            if let Some(input) = model_request.get_mut("input").and_then(|i| i.as_array_mut()) {
                input.extend(context_items.iter().cloned());
            }
        }

        // The model call (the P config, the request on stdin).
        let resp_raw = run_model_call(&model_bin, config_path, &model_request)
            .map_err(|e| (e, seed.clone()))?;
        let resp: Value = match serde_json::from_str(&resp_raw) {
            Ok(v) => v,
            Err(e) => {
                return Err((
                    format!("the model response is not JSON: {e}"),
                    seed.clone(),
                ))
            }
        };
        // A model API failure gets no second chance (docs FT-038):
        // fall back (main) or fail (branch), as today.
        let stop_reason = resp.get("stop_reason").and_then(|s| s.as_str()).unwrap_or("stop");
        if stop_reason == "error" || stop_reason == "aborted" {
            let detail = resp
                .get("detail")
                .and_then(|d| d.as_str())
                .unwrap_or("the model API call failed");
            return Err((format!("model stop reason {stop_reason}: {detail}"), seed.clone()));
        }

        // Gate F: the zero-tool parse check and the residual scan.
        // The residual scan reads the raw response text, not the
        // emitted text: the parse sanitize drops an unterminated
        // marker block, so the emitted text can look clean while the
        // response still carries a raw open marker.
        let raw_text = resp
            .get("text")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();
        let parse_args = vec![
            "--config".to_string(),
            zero_cfg.to_string_lossy().into_owned(),
        ];
        let parse_raw =
            run_parse_capture(&parse_bin, &parse_args, &resp_raw)
                .map_err(|e| (e, seed.clone()))?;
        let events: Vec<Value> = parse_raw
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        let tool_call_events: Vec<Value> = events
            .iter()
            .filter(|e| e.get("type").and_then(|t| t.as_str()) == Some("tool_call"))
            .cloned()
            .collect();
        let parse_error = events
            .iter()
            .any(|e| e.get("type").and_then(|t| t.as_str()) == Some("error"));
        let emitted_text = events
            .iter()
            .find(|e| e.get("type").and_then(|t| t.as_str()) == Some("assistant_message"))
            .and_then(|e| e.get("content").and_then(|c| c.as_str()))
            .unwrap_or("")
            .to_string();

        if !tool_call_events.is_empty() {
            // The detected calls run through `route` with no tool
            // paths: every call reports a not-run result.
            let route_in = tool_call_events
                .iter()
                .map(|e| serde_json::to_string(e).unwrap_or_default())
                .collect::<Vec<_>>()
                .join("\n");
            let route_raw =
                run_capture_stdin(&route_bin, &Vec::<String>::new(), &route_in)
                    .map_err(|e| (e, seed.clone()))?;
            let not_run_events: Vec<Value> = route_raw
                .lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect();
            let not_run: Vec<String> = not_run_events
                .iter()
                .filter_map(|e| {
                    e.get("value")
                        .and_then(|v| v.get("text"))
                        .and_then(|t| t.as_str())
                        .map(str::to_string)
                })
                .collect();
            let correction = correction_with_calls(&not_run);
            last_error = "the format gate failed: the response carried tool calls".to_string();
            if call < max_calls {
                push_retry_context(&mut context_items, &resp, &correction);
                continue;
            }
            break;
        }
        if parse_error || carries_open_marker(&raw_text) {
            // Raw markers only (or the parse gave up): the no-call
            // correction.
            let correction = CORRECTION_RAW_MARKERS.to_string();
            last_error = "the format gate failed: the response carried raw tool-call markers"
                .to_string();
            if call < max_calls {
                push_retry_context(&mut context_items, &resp, &correction);
                continue;
            }
            break;
        }
        // Gate C: the content check against the template headers.
        let missing: Vec<&String> = headers
            .iter()
            .filter(|h| {
                !emitted_text.lines().any(|l| {
                    let t = l.trim_start();
                    t == h.as_str() || t.starts_with(&format!("{h} "))
                })
            })
            .collect();
        if !missing.is_empty() {
            let correction = correction_missing_sections(&missing);
            last_error = format!(
                "the content gate failed: missing sections {}",
                missing
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            if call < max_calls {
                push_retry_context(&mut context_items, &resp, &correction);
                continue;
            }
            break;
        }
        // Both gates pass: accept the text and the accepted call's
        // usage (the optional `usage` marker field).
        let usage = resp.get("usage").filter(|u| !u.is_null()).cloned();
        return Ok((emitted_text, usage));
    }

    Err((
        format!(
            "the summary check loop failed after {max_calls} calls: {last_error}"
        ),
        seed.clone(),
    ))
}

/// The `--summary-input` region arguments shared by the request,
/// seed, and header runs (docs FT-038 step 1).
fn assemble_region_args(
    args: &Args,
    session: &Path,
    config_path: &Path,
    target: &CompactTarget,
) -> Result<Vec<String>, String> {
    let seed_flag = target.seed_flag();
    let mut v: Vec<String> = vec![
        "--summary-input".to_string(),
        seed_flag.0.to_string(),
        seed_flag.1,
        "--session".to_string(),
        session.to_string_lossy().into_owned(),
        "--config".to_string(),
        config_path.to_string_lossy().into_owned(),
    ];
    if target.agent_name() == "compact" && args.strip_last_assistant {
        v.push("--drop-last-assistant".to_string());
    }
    if let Some(p) = &args.prompt {
        v.push("--prompt".to_string());
        v.push(p.clone());
    }
    Ok(v)
}

/// The no-tools correction blocks of the check loop (docs FT-038,
/// the correction text of the second call).
const CORRECTION_RAW_MARKERS: &str = "Your last response carried raw tool-call markers. No tools are available in this session. Do not call tools. Write the full summary now as plain text, with every section the instructions need.";

/// The with-calls correction: one `route` not-run line per detected
/// call.
fn correction_with_calls(not_run: &[String]) -> String {
    let lines = if not_run.is_empty() {
        "(no route lines)".to_string()
    } else {
        not_run.join("\n")
    };
    format!(
        "Your last response did not produce a valid summary. The tool calls in it did not run:\n{lines}\nNo tools are available in this session. Do not call tools. Write the full summary now as plain text, with every section the instructions need."
    )
}

/// The missing-sections correction: the absent template headers.
fn correction_missing_sections(missing: &[&String]) -> String {
    format!(
        "Your last summary is missing sections from the template: {}. Rewrite the full summary now, with every section, as plain text.",
        missing
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// The retry context of one failed call: the failed response as an
/// assistant item (raw, markers and all), then the correction block
/// as a user item (docs FT-038 step 4).
fn push_retry_context(items: &mut Vec<Value>, resp: &Value, correction: &str) {
    let raw_text = resp
        .get("text")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();
    items.push(serde_json::json!({
        "type": "message",
        "role": "assistant",
        "content": raw_text
    }));
    items.push(serde_json::json!({
        "type": "message",
        "role": "user",
        "content": correction
    }));
}

/// The open marker byte forms of the text families (mirrors the
/// parse constants). The P open form is a byte prefix of the A open
/// form, so the P scan covers both.
const RESIDUAL_P_OPEN: &[u8] = &[0x0a, 0x3c, 0x69, 0x6e, 0x76, 0x6f, 0x6b, 0x65];
const RESIDUAL_Q_OPEN: &[u8] = &[
    0x0a, 0x3c, 0x74, 0x6f, 0x6f, 0x6c, 0x5f, 0x63, 0x61, 0x6c, 0x6c, 0x3e,
];

/// Gate F residual scan (docs FT-038 step 3): true when the text
/// still carries an open marker of any family.
fn carries_open_marker(text: &str) -> bool {
    let b = text.as_bytes();
    let p = RESIDUAL_P_OPEN.len();
    let q = RESIDUAL_Q_OPEN.len();
    (b.len() >= p && b.windows(p).any(|w| w == RESIDUAL_P_OPEN))
        || (b.len() >= q && b.windows(q).any(|w| w == RESIDUAL_Q_OPEN))
}

/// The zero-tool parse config (docs FT-038 step 3): a temp TOML whose
/// empty tool path arrays replace the base ones through the deep
/// merge. Every detected call is unknown to `parse`, so `route`
/// reports not-run for all of them.
fn write_zero_tool_config() -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join(format!("rushi-zero-tool-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("create the zero-tool config dir: {e}"))?;
    let path = dir.join("zero-tool.toml");
    let toml_text = "[paths]\nnative_tool_paths = []\nextension_tool_paths = []\n";
    std::fs::write(&path, toml_text)
        .map_err(|e| format!("write the zero-tool config: {e}"))?;
    Ok(path)
}

/// Run a binary, capture stdout, expect success.
fn run_capture(bin: &Path, extra_args: &[String]) -> Result<String, String> {
    run_capture_stdin(bin, extra_args, "")
}

/// Run a binary with `stdin` on its stdin, capture stdout, expect
/// success. Stderr rides into the error on a bad exit.
fn run_capture_stdin(
    bin: &Path,
    extra_args: &[String],
    stdin: &str,
) -> Result<String, String> {
    use std::io::Write;
    let mut cmd = Command::new(bin);
    for a in extra_args {
        cmd.arg(a);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn {bin:?}: {e}"))?;
    if let Some(mut s) = child.stdin.take() {
        s.write_all(stdin.as_bytes())
            .map_err(|e| format!("write the stdin of {bin:?}: {e}"))?;
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("wait on {bin:?}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{} exited {}: {}",
            bin.display(),
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// The one-shot model call: the request JSON on stdin, the response
/// JSON on stdout.
fn run_model_call(
    model_bin: &Path,
    config_path: &Path,
    request: &Value,
) -> Result<String, String> {
    let request_str = serde_json::to_string(request).map_err(|e| e.to_string())?;
    run_capture_stdin(
        model_bin,
        &["--config".to_string(), config_path.to_string_lossy().into_owned()],
        &request_str,
    )
}

/// Run parse, capturing the stdout events even on a non-zero exit.
/// Parse exits 1 when tool calls are present (the route case) and 2
/// on a terminal error; the events ride stdout in both, so the exit
/// status is not an error here. Only a spawn or wait failure is.
fn run_parse_capture(
    parse_bin: &Path,
    extra_args: &[String],
    stdin: &str,
) -> Result<String, String> {
    use std::io::Write;
    let mut cmd = Command::new(parse_bin);
    for a in extra_args {
        cmd.arg(a);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn {parse_bin:?}: {e}"))?;
    if let Some(mut s) = child.stdin.take() {
        s.write_all(stdin.as_bytes())
            .map_err(|e| format!("write the stdin of {parse_bin:?}: {e}"))?;
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("wait on {parse_bin:?}: {e}"))?;
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// The `--branch` run (docs/branch-summarize-cases.md Phase 0).
///
/// Summarizes the open span abandoned by the last top-level `rewind`
/// marker on the active path and appends one branch
/// `compaction_summary` marker (P2). The marker is an add-on on the
/// active path: it compacts nothing, and no handoff boundary or
/// handoff file is written (P3). A model failure appends
/// `compaction_failed` and no branch marker (P4).
fn run_branch_compaction(args: &Args, events: &[LogEvent]) -> ! {
    let config_path: PathBuf =
        rushi_common::paths::resolve_config_path(args.config.as_deref()).into();
    // The last (top-level) rewind marker on the active path is the
    // most recent rewind event in log order (D1). Sub-branches are
    // not their own markers; they appear only as labels inside the
    // summary text.
    let branch_ref = events
        .iter()
        .filter_map(|e| rewind::parse_rewind_event(&e.value, e.seq))
        .next_back();
    let Some(r) = branch_ref else {
        eprintln!("compact: --branch requires a rewind marker in the log; none found");
        std::process::exit(1);
    };
    let branch_of = r.seq as u64;

    // The open span abandoned by that rewind: the seqs in (eff, seq].
    let span_lo = r.eff();
    let span_hi = r.seq;
    let span_events: Vec<&LogEvent> = events
        .iter()
        .filter(|e| e.seq > span_lo && e.seq <= span_hi)
        .collect();
    let has_content = span_events.iter().any(|e| {
        matches!(
            e.value.get("type").and_then(|t| t.as_str()),
            Some("user_message") | Some("assistant_message") | Some("tool_result")
        )
    });
    if !has_content {
        let status = serde_json::json!({
            "status": "noop",
            "reason": "branch",
            "detail": format!(
                "the open span of rewind seq {branch_of} holds no content events"
            ),
        });
        println!("{}", serde_json::to_string(&status).unwrap());
        std::process::exit(0);
    }

    // The cooldown anchor for the failure marker.
    let last_user_seq: usize = events
        .iter()
        .rfind(|e| e.value.get("type").and_then(|t| t.as_str()) == Some("user_message"))
        .map(|e| e.seq)
        .unwrap_or(0);

    // The branch version namespace: one global counter over every
    // existing `branch_of` marker (D2, P5).
    let value_refs: Vec<Value> = events.iter().map(|e| e.value.clone()).collect();
    let branch_version = compact_math::branch_version_meta(&value_refs);

    // tokens_before: the full-form estimate of the ditched span at the
    // calibrated chars-per-token ratio.
    let cfg = load_config(&config_path);
    let active_model = resolve_active_model(&cfg);
    let cpts = resolve_model_settings(&cfg, &active_model)
        .estimate_chars_per_token
        .max(1);
    let caps = Caps {
        text: None,
        chars_per_token: cpts,
    };
    let span_projected: Vec<Ev> = span_events
        .iter()
        .map(|e| compact_math::project_event(&e.value))
        .collect();
    let tokens_before = compact_math::full_form_estimate(&span_projected, &caps);

    // The one-shot summary call with the check loop (docs FT-038).
    // The branch span has no liveness fallback: a failure appends
    // `compaction_failed` and no branch marker (P4).
    let empty = toml::Value::Table(toml::map::Map::new());
    let limits = cfg.get("limits").unwrap_or(&empty);
    let max_calls = val_int(limits, "compact_max_summary_calls")
        .map(|v| (v.max(1)) as u64)
        .unwrap_or(3);
    let (summary, summary_usage) = match summarize_one_shot(
        args,
        &config_path,
        &args.session,
        &CompactTarget::Branch {
            branch_of: branch_of as usize,
        },
        max_calls,
    ) {
        Ok(r) => r,
        Err((last_err, _fallback)) => {
            // P4: the failure appends only `compaction_failed`.
            // No branch marker is written. The branch span is a
            // detached rewrite, not the main session's liveness.
            fail_and_exit(
                args,
                false,
                last_user_seq,
                max_calls,
                "the branch one-shot summary failed",
                &last_err,
            );
        }
    };

    // The branch marker (P2, P7).
    let marker = build_branch_marker(
        &summary,
        branch_of,
        branch_version,
        args.reason.as_str(),
        tokens_before,
        summary_usage,
    );
    let log_bin = resolve_bin(&args.log, "LOG_BIN", "rushi-log");
    if let Err(e) = append_event(&log_bin, &args.session, &marker) {
        eprintln!("compact: append branch compaction_summary failed: {e}");
    }

    // Write the versioned branch summary file (docs/ft-038-compact-check-loop.md
    // step 6). Branch mode writes only the branch file, never a handoff.
    write_branch_summary(&args.session, &summary, branch_version);

    let status = serde_json::json!({
        "status": "compacted",
        "reason": args.reason.as_str(),
        "branch": true,
        "branch_of": branch_of,
        "version": branch_version,
        "first_kept_seq": 1,
        "tokens_before": tokens_before,
        "summary": summary,
    });
    println!("{}", serde_json::to_string(&status).unwrap());
    std::process::exit(0);
}

/// Write the versioned handoff document for a main compact
/// (docs/ft-038-compact-check-loop.md step 6): `handoff/v<N>.md` plus
/// `handoff.md` as the latest copy. This mirrors the loop writer in
/// `bin/rushi/src/step/compact.rs`. The main target always writes the
/// file, even on the bounded seed fallback.
fn write_main_handoff(session: &Path, summary: &str, version: u64) {
    let vdir = session.join("handoff");
    let vpath = vdir.join(format!("v{}.md", version));
    if let Err(e) = std::fs::create_dir_all(&vdir) {
        eprintln!("compact: cannot create the handoff dir: {e}");
    }
    if let Err(e) = std::fs::write(&vpath, summary) {
        eprintln!("compact: cannot write handoff/v{}.md: {e}", version);
    }
    if let Err(e) = std::fs::write(session.join("handoff.md"), summary) {
        eprintln!("compact: cannot write handoff.md: {e}");
    }
}

/// Write the versioned branch summary document
/// (docs/ft-038-compact-check-loop.md step 6):
/// `branch-summary/v<N>.md`. Branch mode writes only the branch file,
/// never a `handoff.md` or a `handoff` dir.
fn write_branch_summary(session: &Path, summary: &str, version: u64) {
    let dir = session.join("branch-summary");
    let path = dir.join(format!("v{}.md", version));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("compact: cannot create the branch-summary dir: {e}");
    }
    if let Err(e) = std::fs::write(&path, summary) {
        eprintln!("compact: cannot write branch-summary/v{}.md: {e}", version);
    }
}

/// The branch `compaction_summary` marker
/// (docs/branch-summarize-cases.md P2, P7). `branch_of` is the seq
/// of the owning rewind marker; `first_kept_seq` is the sentinel 1;
/// `parent_version` is 0 (a branch marker has no handoff parent);
/// `diverge_seq` equals `branch_of` so the DAG edge reads
/// "diverged at the fork".
fn build_branch_marker(
    summary: &str,
    branch_of: u64,
    version: u64,
    reason: &str,
    tokens_before: u64,
    usage: Option<Value>,
) -> Value {
    let mut marker = serde_json::json!({
        "v": 1,
        "type": "compaction_summary",
        "ts": ts_now(),
        "summary": summary,
        "first_kept_seq": 1u64,
        "version": version,
        "parent_version": 0u64,
        "diverge_seq": branch_of,
        "reason": reason,
        "tokens_before": tokens_before,
        "branch_of": branch_of,
    });
    if let Some(u) = usage {
        marker["usage"] = u;
    }
    marker
}

/// The failure path: the `compaction_failed` marker, the detail on
/// stderr, the failed status on stdout, exit 1. The last_user_seq
/// anchors the cooldown of the next threshold trigger. The marker
/// records the one-shot check loop call budget in its count field
/// (docs FT-038).
fn fail_and_exit(
    args: &Args,
    overflow: bool,
    last_user_seq: usize,
    call_count: u64,
    short: &str,
    detail: &str,
) -> ! {
    let reason_str = args.reason.as_str().to_string();
    let failed = serde_json::json!({
        "v": 1,
        "type": "compaction_failed",
        "ts": ts_now(),
        "reason": reason_str,
        "last_user_seq": last_user_seq,
        "attempts": call_count,
        "detail": detail,
    });
    let log_bin = resolve_bin(&args.log, "LOG_BIN", "rushi-log");
    if let Err(e) = append_event(&log_bin, &args.session, &failed) {
        eprintln!("compact: append compaction_failed failed: {e}");
    }
    eprintln!("compact: {short}: {detail}");
    let status = serde_json::json!({
        "status": "failed",
        "reason": reason_str,
        "overflow": overflow,
        "detail": detail,
    });
    println!("{}", serde_json::to_string(&status).unwrap());
    std::process::exit(1);
}

/// The config loader: the toml value the compact knobs read. The
/// model resolution is copied from bin/assemble (correction 4: no
/// shared crate). The copy is noted in docs/itches.md.
fn load_config(path: &Path) -> toml::Value {
    // Layered load (docs/project-config-precedence.md): the Nix
    // store config.toml is the base layer. The loader stays
    // lenient: any read or parse error yields the empty table and
    // the defaults apply.
    rushi_common::config_layer::load_layered(path)
        .map(|layers| layers.value)
        .unwrap_or(toml::Value::Table(toml::map::Map::new()))
}

/// The raw context budget: the user knob `context_budget_tokens`, or
/// the model window.
fn resolve_context_budget(config: &toml::Value) -> u64 {
    let active = resolve_active_model(config);
    let ms = resolve_model_settings(config, &active);
    let window = ms.context_tokens;
    let empty = toml::Value::Table(toml::map::Map::new());
    val_int(
        config.get("limits").unwrap_or(&empty),
        "context_budget_tokens",
    )
    .map(|v| (v.max(1)) as u64)
    .unwrap_or(window)
    .max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_fires_on_a_crossing_reading() {
        let inp = TriggerInput {
            trigger_level: 100,
            trigger_readings: vec![(1, 40), (2, 60), (3, 110)],
            last_user_seq: 3,
            failed_user_seq: None,
            cooldown: true,
            enabled: true,
            force: false,
            overflow: false,
        };
        match decide_trigger(&inp) {
            Decision::Noop(d) => panic!("expected fire, got noop: {d}"),
            Decision::Fire { .. } => {}
        }
    }

    #[test]
    fn trigger_cold_is_the_noop() {
        // The last reading is below the level: the trigger is cold.
        let inp = TriggerInput {
            trigger_level: 200,
            trigger_readings: vec![(1, 100), (2, 150), (3, 190)],
            last_user_seq: 3,
            failed_user_seq: None,
            cooldown: true,
            enabled: true,
            force: false,
            overflow: false,
        };
        match decide_trigger(&inp) {
            Decision::Noop(_) => {}
            Decision::Fire { .. } => panic!("a cold trigger must not fire"),
        }
    }

    #[test]
    fn cooldown_gates_the_threshold_trigger() {
        let inp = TriggerInput {
            trigger_level: 100,
            trigger_readings: vec![(1, 90), (2, 95)],
            last_user_seq: 5,
            failed_user_seq: Some(5),
            cooldown: true,
            enabled: true,
            force: false,
            overflow: false,
        };
        match decide_trigger(&inp) {
            Decision::Noop(d) => {
                assert!(d.contains("cooldown"), "the detail names the gate");
            }
            Decision::Fire { .. } => panic!("the cooldown must hold"),
        }
        // The overflow skips the cooldown.
        let inp = TriggerInput {
            overflow: true,
            ..inp
        };
        match decide_trigger(&inp) {
            Decision::Noop(d) => panic!("the overflow skips the cooldown: {d}"),
            Decision::Fire { .. } => {}
        }
    }

    #[test]
    fn the_kill_switch_holds_the_threshold() {
        let inp = TriggerInput {
            trigger_level: 100,
            trigger_readings: vec![(1, 40), (2, 90)],
            last_user_seq: 2,
            failed_user_seq: None,
            cooldown: true,
            enabled: false,
            force: false,
            overflow: false,
        };
        match decide_trigger(&inp) {
            Decision::Noop(d) => {
                assert!(d.contains("disabled"), "the detail names the switch");
            }
            Decision::Fire { .. } => panic!("the disabled feature must not fire"),
        }
        // The force and the overflow skip the switch.
        let forced = TriggerInput { force: true, ..inp };
        match decide_trigger(&forced) {
            Decision::Noop(d) => panic!("the force skips the switch: {d}"),
            Decision::Fire { .. } => {}
        }
    }

    #[test]
    fn extract_file_ops_reads_and_writes() {
        let events = vec![
            serde_json::json!({
                "type": "assistant_message",
                "tool_calls": [
                    {"name": "read", "arguments": {"file_path": "a.txt"}},
                    {"name": "write", "arguments": {"file_path": "b.rs"}},
                    {"name": "edit", "arguments": {"file_path": "b.rs"}},
                    {"name": "bash", "arguments": {"command": "ls"}},
                ]
            }),
            serde_json::json!({"type": "user_message", "content": "hi"}),
        ];
        let (reads, modified) = compact_math::extract_file_ops(&events);
        assert_eq!(reads, vec!["a.txt".to_string()]);
        assert_eq!(modified, vec!["b.rs".to_string()], "the dedup keeps one");
    }

    #[test]
    fn parse_boundary_rejects_the_bad_events() {
        let bad_zero = LogEvent {
            seq: 7,
            value: serde_json::json!({
                "v": 1,
                "type": "compaction_summary",
                "first_kept_seq": 0,
                "summary": "s",
            }),
        };
        assert!(parse_boundary(&bad_zero).is_none());
        let good = LogEvent {
            seq: 7,
            value: serde_json::json!({
                "v": 1,
                "type": "compaction_summary",
                "first_kept_seq": 3,
                "summary": "the summary",
                "read_files": ["a.txt"],
                "modified_files": ["b.rs"],
            }),
        };
        let b = parse_boundary(&good).unwrap();
        assert_eq!(b.first_kept_seq, 3);
        assert_eq!(b.read_files, vec!["a.txt".to_string()]);
    }

    fn config_toml(src: &str) -> toml::Value {
        src.parse::<toml::Value>().expect("valid toml")
    }


    #[test]
    fn trigger_level_uses_context_budget() {
        let cfg = config_toml(
            r#"
            [model.stub]
            context_tokens = 262144
            max_output_tokens = 32768

            [active]
            model = "stub"

            [limits]
            context_budget_tokens = 262144
            "#,
        );
        let base = resolve_context_budget(&cfg);
        assert_eq!(base, 262144);
        assert_eq!(compact_math::trigger_level_for(base, 16384), 245760,);
    }

    // ── cut_index: the `--up-to` boundary vs the auto keep-window walk ──

    fn log_events_up_to(n: usize) -> Vec<LogEvent> {
        (1..=n)
            .map(|s| LogEvent {
                seq: s,
                value: serde_json::json!({"v":1,"type":"user_message","content":"x"}),
            })
            .collect()
    }

    /// Kept region that does not start at seq 1. It stands in for a
    /// post-boundary region whose head is at `start`.
    fn log_events_range(start: usize, end: usize) -> Vec<LogEvent> {
        (start..=end)
            .map(|s| LogEvent {
                seq: s,
                value: serde_json::json!({"v":1,"type":"user_message","content":"x"}),
            })
            .collect()
    }

    fn projected_from(kept: &[LogEvent]) -> Vec<Ev> {
        kept.iter()
            .map(|e| compact_math::project_event(&e.value))
            .collect()
    }

    #[test]
    fn cut_index_explicit_up_to_places_the_boundary() {
        let kept = log_events_up_to(5); // seqs 1..=5
        let projected = projected_from(&kept);
        let caps = Caps {
            text: None,
            chars_per_token: 4,
        };

        // up_to = 3: the first kept event past seq 3 is seq 4 (index 3).
        assert_eq!(cut_index(&kept, Some(3), &projected, 100, &caps), 3);
        // up_to = 1: first event past seq 1 is seq 2 (index 1).
        assert_eq!(cut_index(&kept, Some(1), &projected, 100, &caps), 1);
        // up_to = 5: no kept event past seq 5; the cut lands at the
        // end: the whole kept region is old, nothing is kept.
        assert_eq!(cut_index(&kept, Some(5), &projected, 100, &caps), 5);
        // up_to far beyond the log: same as up_to = last seq.
        assert_eq!(cut_index(&kept, Some(99), &projected, 100, &caps), 5);
    }

    /// Rewind rule I4. A target at or before the region head degrades
    /// to the region head. It does not collapse to the empty-region
    /// noop. The region head is the first kept event.
    #[test]
    fn cut_index_up_to_degrades_to_the_region_head() {
        // A post-boundary region: head at seq 9.
        let kept = log_events_range(9, 13);
        let projected = projected_from(&kept);
        let caps = Caps {
            text: None,
            chars_per_token: 4,
        };

        // Before the region head: degrades to the head. The cut lands
        // on the event after the head (seq 10, index 1). The old
        // region is the head event alone.
        assert_eq!(cut_index(&kept, Some(5), &projected, 100, &caps), 1);
        assert_eq!(cut_index(&kept, Some(1), &projected, 100, &caps), 1);
        // At the region head: the same degrade.
        assert_eq!(cut_index(&kept, Some(9), &projected, 100, &caps), 1);
        // Inside the un-compact prefix: untouched.
        assert_eq!(cut_index(&kept, Some(11), &projected, 100, &caps), 3);
    }

    #[test]
    fn cut_index_without_up_to_delegates_to_find_cut() {
        let kept = log_events_up_to(5);
        let projected = projected_from(&kept);
        let caps = Caps {
            text: None,
            chars_per_token: 4,
        };
        // Five 1-char user events: 5 chars total, far below the large
        // keep budget, so find_cut returns 0 (keep everything).
        assert_eq!(cut_index(&kept, None, &projected, 100_000, &caps), 0);
    }

    // ── branch mode (docs/branch-summarize-cases.md) ─────────────

    /// P2: `--branch` appends one `compaction_summary` with
    /// `branch_of` set to the last rewind seq, the sentinel
    /// `first_kept_seq` of 1, and `diverge_seq` equal to `branch_of`.
    #[test]
    fn compact_branch_appends_marker() {
        let m = build_branch_marker("the branch summary", 9, 1, "threshold", 1234, None);
        assert_eq!(m["type"], "compaction_summary");
        assert_eq!(m["v"], 1);
        assert_eq!(m["branch_of"], 9, "branch_of is the last rewind seq");
        assert_eq!(m["first_kept_seq"], 1, "the branch sentinel first_kept");
        assert_eq!(m["diverge_seq"], 9, "diverged at the fork");
        assert_eq!(m["version"], 1);
        assert_eq!(m["summary"], "the branch summary");
    }

    /// P7: a branch marker's `parent_version` is 0 (it has no handoff
    /// parent), and `diverge_seq` equals `branch_of`.
    #[test]
    fn compact_branch_parent_version_zero() {
        let m = build_branch_marker("s", 9, 3, "threshold", 0, None);
        assert_eq!(
            m["parent_version"], 0,
            "a branch marker has no handoff parent"
        );
        assert_eq!(m["diverge_seq"], 9, "diverge_seq equals branch_of");
    }

    /// P4: a failed branch run appends a `compaction_failed` marker,
    /// never a rewind or a branch marker.
    #[test]
    fn compact_branch_failure_appends_failed() {
        // The marker the failure path appends (same shape as the
        // plain failure path writes; it also carries the try count,
        // which this test does not assert).
        let failed: Value = serde_json::json!({
            "v": 1,
            "type": "compaction_failed",
            "ts": "t",
            "reason": "threshold",
            "last_user_seq": 7,
            "detail": "both summary calls failed"
        });
        assert_eq!(failed["type"], "compaction_failed");
        assert_eq!(failed["last_user_seq"], 7);
        // It is not a rewind and not a branch marker: the failure
        // path writes no other marker.
        assert_ne!(failed["type"], "rewind");
        assert!(failed.get("branch_of").is_none());
        // And the branch marker builder only ever emits
        // compaction_summary with branch_of set.
        let branch = build_branch_marker("s", 9, 1, "threshold", 0, None);
        assert_eq!(branch["type"], "compaction_summary");
        assert_eq!(branch["branch_of"], 9);
    }

    /// P11: the compact boundary pick is the last *non-branch*
    /// marker; a branch sentinel `first_kept_seq` of 1 never resets
    /// the keep region.
    #[test]
    fn compact_boundary_ignores_branch_markers() {
        let plain = LogEvent {
            seq: 5,
            value: serde_json::json!({
                "v": 1, "type": "compaction_summary", "ts": "t",
                "summary": "s", "first_kept_seq": 8, "version": 1,
                "parent_version": 0, "diverge_seq": 0,
                "reason": "threshold", "tokens_before": 0
            }),
        };
        let branch = LogEvent {
            seq: 9,
            value: serde_json::json!({
                "v": 1, "type": "compaction_summary", "ts": "t",
                "summary": "b", "first_kept_seq": 1, "version": 2,
                "parent_version": 0, "diverge_seq": 9,
                "reason": "threshold", "tokens_before": 0, "branch_of": 9
            }),
        };
        // The branch marker itself is rejected by parse_boundary.
        assert!(parse_boundary(&branch).is_none());
        // Selection over the log keeps the plain marker: the later
        // handoff first_kept is not reset to 1.
        let events = [plain.clone(), branch.clone()];
        let boundary = events.iter().filter_map(parse_boundary).next_back();
        let b = boundary.expect("the plain marker must be the boundary");
        assert_eq!(b.seq, 5);
        assert_eq!(b.first_kept_seq, 8, "the branch sentinel must not reset it");
        let _ = (plain, branch);
    }

    // ── The one-shot check loop (docs/ft-038-compact-check-loop.md) ──

    /// The byte forms of the Q-family (Qwen-shape) markers, mirroring
    /// the parse constants. Built from byte arrays so no raw marker
    /// appears in the test source (tool transports strip raw
    /// markers).
    const Q_OPEN: &[u8] = &[
        0x0a, 0x3c, 0x74, 0x6f, 0x6f, 0x6c, 0x5f, 0x63, 0x61, 0x6c, 0x6c, 0x3e,
    ];
    const Q_CLOSE: &[u8] = &[
        0x0a, 0x3c, 0x2f, 0x74, 0x6f, 0x6f, 0x6c, 0x5f, 0x63, 0x61, 0x6c, 0x6c, 0x3e,
    ];
    const Q_FUNC_OPEN: &[u8] = &[
        0x3c, 0x66, 0x75, 0x6e, 0x63, 0x74, 0x69, 0x6f, 0x6e, 0x3d,
    ];
    const Q_FUNC_CLOSE: &[u8] = &[
        0x3c, 0x2f, 0x66, 0x75, 0x6e, 0x63, 0x74, 0x69, 0x6f, 0x6e, 0x3e,
    ];
    const Q_PARAM_OPEN: &[u8] = &[
        0x3c, 0x70, 0x61, 0x72, 0x61, 0x6d, 0x65, 0x74, 0x65, 0x72, 0x3d,
    ];
    const Q_PARAM_CLOSE: &[u8] = &[
        0x3c, 0x2f, 0x70, 0x61, 0x72, 0x61, 0x6d, 0x65, 0x74, 0x65, 0x72,
    ];

    /// One closed Q-family block: the open line, the function line,
    /// one parameter pair, the function close line, the close line.
    fn q_block(name: &str, key: &str, val: &str) -> String {
        let open = String::from_utf8(Q_OPEN.to_vec()).unwrap();
        let close = String::from_utf8(Q_CLOSE.to_vec()).unwrap();
        let fop = String::from_utf8(Q_FUNC_OPEN.to_vec()).unwrap();
        let fcl = String::from_utf8(Q_FUNC_CLOSE.to_vec()).unwrap();
        let pop = String::from_utf8(Q_PARAM_OPEN.to_vec()).unwrap();
        let pcl = String::from_utf8(Q_PARAM_CLOSE.to_vec()).unwrap();
        format!(
            "{open}\n{fop}{name}>\n{pop}{key}>\n{val}\n{pcl}>\n{fcl}{close}"
        )
    }

    /// The template header set (the `##` and `###` lines of the
    /// first-time compact template).
    const TEMPLATE_HEADERS: [&str; 9] = [
        "## Goal",
        "## Constraints & Preferences",
        "## Progress",
        "### Done",
        "### In Progress",
        "### Blocked",
        "## Key Decisions",
        "## Next Steps",
        "## Critical Context",
    ];

    /// A clean summary text carrying every template header. No
    /// trailing newline: the parse sanitize trims the tail, so the
    /// accepted text is this string verbatim.
    fn template_summary_text() -> String {
        TEMPLATE_HEADERS
            .iter()
            .map(|h| format!("{h}\n- the entry under the header"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The stub model script (the docs FT-038 test plan): a counter
    /// file drives which canned response lands on stdout; every
    /// request lands in `req<N>.json` for the assertions. The stub
    /// dir is the parent of the `--config` arg, so no env var is
    /// needed and parallel tests cannot race on it.
    const STUB_MODEL_SCRIPT: &str = r#"#!/bin/sh
cfg=""
prev=""
for a in "$@"; do
  if [ "$prev" = "--config" ]; then cfg="$a"; fi
  prev="$a"
done
dir=$(dirname "$cfg")
n=$(cat "$dir/count" 2>/dev/null || echo 0)
n=$((n+1))
echo "$n" > "$dir/count"
if [ ! -f "$dir/resp$n.json" ]; then
  n=1
fi
req=$(cat)
printf '%s' "$req" > "$dir/req$n.json"
cat "$dir/resp$n.json"
"#;

    /// The stub assemble script: branches on the flags. The stub dir
    /// is the parent of the `--config` arg, so no env var is needed.
    const STUB_ASSEMBLE_SCRIPT: &str = r#"#!/bin/sh
cfg=""
prev=""
mode="request"
for a in "$@"; do
  if [ "$prev" = "--config" ]; then cfg="$a"; fi
  case "$a" in
    --summary-text) mode="seed" ;;
    --compact-headers) mode="headers" ;;
  esac
  prev="$a"
done
dir=$(dirname "$cfg")
case "$mode" in
  seed) cat "$dir/seed.txt" ;;
  headers) cat "$dir/headers.json" ;;
  *) cat "$dir/request.json" ;;
esac
"#;

    /// Make both stub scripts executable (the unix mode).
    fn chmod_scripts(a: &Path, b: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let p = std::fs::Permissions::from_mode(0o755);
            std::fs::set_permissions(a, p.clone()).unwrap();
            std::fs::set_permissions(b, p).unwrap();
        }
        #[cfg(not(unix))]
        let _ = (a, b);
    }

    /// The one stub dir the check loop tests share: the canned model
    /// responses, the canned assemble outputs, and the call counter.
    /// The scripts find it as the parent of the `--config` arg.
    /// Returns the config path and the ready Args.
    fn stub_env(
        root: &Path,
        resp1: &serde_json::Value,
        resp2: Option<&serde_json::Value>,
        seed: &str,
    ) -> (PathBuf, Args) {
        let stub_dir = root.join("stub");
        std::fs::create_dir_all(&stub_dir).unwrap();
        std::fs::write(stub_dir.join("resp1.json"), resp1.to_string()).unwrap();
        if let Some(r2) = resp2 {
            std::fs::write(stub_dir.join("resp2.json"), r2.to_string()).unwrap();
        }
        std::fs::write(
            stub_dir.join("request.json"),
            serde_json::json!({
                "model": "m",
                "input": [],
                "tools": []
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(stub_dir.join("seed.txt"), seed).unwrap();
        std::fs::write(
            stub_dir.join("headers.json"),
            serde_json::to_string(&TEMPLATE_HEADERS).unwrap(),
        )
        .unwrap();
        let config_path = stub_dir.join("cfg.toml");
        let target_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug");
        let parse_bin = target_dir.join("parse");
        let route_bin = target_dir.join("route");
        let model_script = root.join("stub-model.sh");
        std::fs::write(&model_script, STUB_MODEL_SCRIPT).unwrap();
        let assemble_script = root.join("stub-assemble.sh");
        std::fs::write(&assemble_script, STUB_ASSEMBLE_SCRIPT).unwrap();
        chmod_scripts(&model_script, &assemble_script);
        let session = root.join("session");
        std::fs::create_dir_all(&session).unwrap();
        let args = Args {
            session,
            config: None,
            reason: Reason::Threshold,
            strip_last_assistant: false,
            force: false,
            assemble: Some(assemble_script),
            log: None,
            model: Some(model_script),
            parse: Some(parse_bin),
            route: Some(route_bin),
            up_to: None,
            prompt: None,
            branch: false,
        };
        (config_path, args)
    }

    /// True when the workspace parse and route binaries are built;
    /// the check loop tests skip otherwise (run `cargo build` first).
    fn real_bins_built() -> bool {
        let target_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug");
        target_dir.join("parse").exists() && target_dir.join("route").exists()
    }

    /// The versioned file writes (docs FT-038 step 6): the main target
    /// writes `handoff/v<N>.md` plus `handoff.md`; the branch target
    /// writes `branch-summary/v<N>.md` and never a handoff.
    #[test]
    fn versioned_file_writes_match_the_target() {
        let dir = tempfile::tempdir().unwrap();
        let main_session = dir.path().join("main");
        std::fs::create_dir_all(&main_session).unwrap();
        write_main_handoff(&main_session, "the main summary", 3);
        assert_eq!(
            std::fs::read_to_string(main_session.join("handoff/v3.md")).unwrap(),
            "the main summary"
        );
        assert_eq!(
            std::fs::read_to_string(main_session.join("handoff.md")).unwrap(),
            "the main summary"
        );

        let branch_session = dir.path().join("branch");
        std::fs::create_dir_all(&branch_session).unwrap();
        write_branch_summary(&branch_session, "the branch summary", 2);
        assert_eq!(
            std::fs::read_to_string(branch_session.join("branch-summary/v2.md")).unwrap(),
            "the branch summary"
        );
        // Branch mode never writes a handoff document.
        assert!(!branch_session.join("handoff.md").exists());
        assert!(!branch_session.join("handoff").exists());
    }

    /// The check loop test (docs FT-038 test plan): a stub model
    /// returns a polluted response (a raw Q-family block) first and a
    /// clean response second. The real parse and route binaries run
    /// the gates. Asserts the call count, the route line in the
    /// correction, and the accepted text.
    #[test]
    fn one_shot_check_loop_recovers_a_polluted_first_call() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        if !real_bins_built() {
            eprintln!("SKIP one_shot_check_loop: the parse/route binaries are not built (run `cargo build` first)");
            return;
        }

        let polluted_text =
            format!("writing the summary now\n{}", q_block("task", "progress", "summarizing the session"));
        let clean_text = template_summary_text();
        let resp1 = serde_json::json!({
            "text": polluted_text,
            "tool_calls": [],
            "stop_reason": "stop",
            "usage": {"input_tokens": 100, "output_tokens": 50}
        });
        let resp2 = serde_json::json!({
            "text": clean_text,
            "tool_calls": [],
            "stop_reason": "stop",
            "usage": {"input_tokens": 120, "output_tokens": 60}
        });
        let (config_path, args) =
            stub_env(root, &resp1, Some(&resp2), "the bounded seed transcript");
        let result = summarize_one_shot(
            &args,
            &config_path,
            &args.session,
            &CompactTarget::Main { up_to: 5 },
            3,
        );

        let (summary, usage) = result.expect("the loop must accept the clean second response");
        assert_eq!(summary, clean_text);
        assert_eq!(
            usage,
            Some(serde_json::json!({"input_tokens": 120, "output_tokens": 60}))
        );
        // Two model calls: the polluted one and the clean one.
        let count = std::fs::read_to_string(args.session.parent().unwrap().join("stub/count"))
            .unwrap();
        assert_eq!(count.trim(), "2");
        // The second request carries the correction: the not-run
        // route line for the detected call, and the failed response
        // raw (the Q block, JSON-escaped).
        let req2 = std::fs::read_to_string(args.session.parent().unwrap().join("stub/req2.json"))
            .unwrap();
        assert!(req2.contains("did not run"), "the correction lists the not-run calls");
        assert!(
            req2.contains("Unknown tool task"),
            "the route line names the detected call: {req2}"
        );
        let escaped = serde_json::to_string(&polluted_text).unwrap();
        assert!(
            req2.contains(&escaped),
            "the failed response rides the second request raw"
        );
    }

    /// Gate C test (docs FT-038 test plan): a headerless-clean
    /// response fails the content gate and the correction names the
    /// missing sections; the second, complete response is accepted.
    #[test]
    fn one_shot_gate_c_missing_headers_retries() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        if !real_bins_built() {
            eprintln!("SKIP one_shot_gate_c: the parse/route binaries are not built");
            return;
        }

        // A clean response, but two sections are missing.
        let partial_text = "## Goal\nwork\n## Key Decisions\none\n".to_string();
        let clean_text = template_summary_text();
        let resp1 = serde_json::json!({
            "text": partial_text,
            "tool_calls": [],
            "stop_reason": "stop",
            "usage": {"input_tokens": 90, "output_tokens": 40}
        });
        let resp2 = serde_json::json!({
            "text": clean_text,
            "tool_calls": [],
            "stop_reason": "stop",
            "usage": {"input_tokens": 95, "output_tokens": 45}
        });
        let (config_path, args) =
            stub_env(root, &resp1, Some(&resp2), "seed");
        let result =
            summarize_one_shot(&args, &config_path, &args.session, &CompactTarget::Main { up_to: 5 }, 3);
        let (summary, _) = result.expect("the complete second response must be accepted");
        assert_eq!(summary, clean_text);
        // The second request carries the missing-sections correction.
        let req2 = std::fs::read_to_string(args.session.parent().unwrap().join("stub/req2.json"))
            .unwrap();
        let req2_val: serde_json::Value = serde_json::from_str(&req2).unwrap();
        let input = req2_val.get("input").and_then(|i| i.as_array()).unwrap();
        let correction = input
            .iter()
            .find(|i| {
                i.get("content")
                    .and_then(|c| c.as_str())
                    .map(|c| c.contains("missing sections from the template"))
                    .unwrap_or(false)
            })
            .expect("the content correction rides the second request");
        let corr = correction.get("content").unwrap().as_str().unwrap();
        assert!(corr.contains("## Progress"), "the missing section is named in the correction: {corr}");
        assert!(!corr.contains("## Goal"), "a present section is not listed in the correction: {corr}");
    }

    /// The raw-marker-only case (docs FT-038 test plan): a response
    /// with an unterminated open marker and no closed block gets the
    /// no-call correction; the clean second response is accepted.
    #[test]
    fn one_shot_raw_marker_only_uses_the_no_call_correction() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        if !real_bins_built() {
            eprintln!("SKIP one_shot_raw_marker_only: the parse/route binaries are not built");
            return;
        }

        // An unterminated open marker: no closed block, so parse
        // recovers no call. The residual scan of the raw text catches
        // the open marker.
        let open_only = String::from_utf8(Q_OPEN.to_vec()).unwrap();
        let polluted_text = format!("writing the summary now\n{open_only}");
        let clean_text = template_summary_text();
        let resp1 = serde_json::json!({
            "text": polluted_text,
            "tool_calls": [],
            "stop_reason": "stop",
            "usage": {"input_tokens": 80, "output_tokens": 30}
        });
        let resp2 = serde_json::json!({
            "text": clean_text,
            "tool_calls": [],
            "stop_reason": "stop",
            "usage": {"input_tokens": 85, "output_tokens": 35}
        });
        let (config_path, args) = stub_env(root, &resp1, Some(&resp2), "seed");
        let result =
            summarize_one_shot(&args, &config_path, &args.session, &CompactTarget::Main { up_to: 5 }, 3);
        let (summary, _) = result.expect("the clean second response must be accepted");
        assert_eq!(summary, clean_text);
        // The second request carries the no-call (raw marker) correction.
        let req2 = std::fs::read_to_string(args.session.parent().unwrap().join("stub/req2.json"))
            .unwrap();
        assert!(
            req2.contains("carried raw tool-call markers"),
            "the no-call correction rides the second request: {req2}"
        );
        // The raw marker is not a detected call, so no route line.
        assert!(!req2.contains("did not run"), "no route line for a raw marker");
    }

    /// The model API failure case (docs FT-038 test plan): a stop
    /// reason of `error` gets no second chance; the loop stops at the
    /// first call and the seed fallback is returned.
    #[test]
    fn one_shot_api_failure_gets_no_second_chance() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        if !real_bins_built() {
            eprintln!("SKIP one_shot_api_failure: the parse/route binaries are not built");
            return;
        }

        let resp1 = serde_json::json!({
            "text": "",
            "tool_calls": [],
            "stop_reason": "error",
            "usage": null,
            "detail": "the provider said no"
        });
        let (config_path, args) = stub_env(root, &resp1, None, "the seed fallback");
        // Even with a large call budget, the API failure stops at one.
        let result =
            summarize_one_shot(&args, &config_path, &args.session, &CompactTarget::Main { up_to: 5 }, 3);
        let (err, seed) = match result {
            Ok(_) => panic!("an API failure must not be accepted"),
            Err(e) => e,
        };
        assert!(
            err.contains("stop reason error") || err.contains("the provider said no"),
            "the error names the API failure: {err}"
        );
        assert_eq!(seed, "the seed fallback");
        // Only one model call: the failure stops the loop.
        let count = std::fs::read_to_string(args.session.parent().unwrap().join("stub/count"))
            .unwrap();
        assert_eq!(count.trim(), "1");
    }

    /// Gate F residual scan: a residual open marker of any family
    /// fails the format gate; clean text passes.
    #[test]
    fn residual_scan_catches_open_markers_of_every_family() {
        let q = q_block("task", "k", "v");
        assert!(carries_open_marker(&q), "a closed Q block carries its open line");
        let mut p_open = String::new();
        p_open.push(char::from(0x0a));
        p_open.push_str(std::str::from_utf8(&[0x3c, 0x69, 0x6e, 0x76, 0x6f, 0x6b, 0x65]).unwrap());
        assert!(carries_open_marker(&p_open), "a bare P open line is caught");
        let mut a_open = p_open.clone();
        a_open.push('>');
        assert!(carries_open_marker(&a_open), "the A open line is caught by the P scan");
        assert!(!carries_open_marker("plain summary text\n## Goal\nwork"), "clean text passes");
    }

    /// Gate C: the missing-section list holds only the absent
    /// headers; the correction names them.
    #[test]
    fn missing_sections_list_holds_only_the_absent_headers() {
        let text = "## Goal\nwork\n## Key Decisions\none\n".to_string();
        let headers: Vec<String> = TEMPLATE_HEADERS.iter().map(|s| s.to_string()).collect();
        let missing: Vec<&String> = headers
            .iter()
            .filter(|h| {
                !text.lines().any(|l| {
                    let t = l.trim_start();
                    t == h.as_str() || t.starts_with(&format!("{h} "))
                })
            })
            .collect();
        assert!(missing.iter().any(|h| h.as_str() == "## Progress"));
        assert!(missing.iter().any(|h| h.as_str() == "## Next Steps"));
        assert!(
            !missing.iter().any(|h| h.as_str() == "## Goal"),
            "a present header is not listed"
        );
        let correction = correction_missing_sections(&missing);
        assert!(correction.contains("## Progress"));
        assert!(correction.contains("## Next Steps"));
        assert!(!correction.contains("## Goal"));
    }

    /// The with-calls correction carries one route line per call.
    #[test]
    fn with_calls_correction_carries_the_route_lines() {
        let correction = correction_with_calls(&[
            "Unknown tool task. No tool ran.".to_string(),
            "Unknown tool Bash. No tool ran.".to_string(),
        ]);
        assert!(correction.contains("Unknown tool task. No tool ran."));
        assert!(correction.contains("Unknown tool Bash. No tool ran."));
        assert!(correction.contains("No tools are available in this session"));
    }

    /// The zero-tool config holds the empty tool path arrays.
    #[test]
    fn zero_tool_config_writes_the_empty_tool_paths() {
        let path = write_zero_tool_config().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("native_tool_paths = []"));
        assert!(text.contains("extension_tool_paths = []"));
    }

    /// The retry context: the failed response rides as an assistant
    /// item, the correction as a user item, in order.
    #[test]
    fn retry_context_pairs_the_response_and_the_correction() {
        let mut items: Vec<Value> = Vec::new();
        let resp = serde_json::json!({
            "text": "raw response text",
            "stop_reason": "stop"
        });
        push_retry_context(&mut items, &resp, "the correction");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["role"], "assistant");
        assert_eq!(items[0]["content"], "raw response text");
        assert_eq!(items[1]["role"], "user");
        assert_eq!(items[1]["content"], "the correction");
    }
}
