//! compact: the session-internal auto-compaction binary.
//!
//! It reads the event log, decides whether the in-session trigger fires
//! (docs/auto-compact-plan.md sections 3-5), cuts the old region,
//! calls the LLM once for the summary through the `assemble
//! --summary-input` and `model` binaries, and appends the
//! `compaction_started` / `compaction_summary` / `compaction_failed`
//! markers through `log`. The loop keeps the session: no handoff, no
//! new session directory.
//!
//! The trigger math and the chars/4 estimator are the shared
//! `harness_common::compact_math` module (docs/phase-2-plan.md
//! section 6).

use bon::builder;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::Parser;
use harness_common::compact_math::{self, Caps, Ev};
use serde_json::Value;

/// The one-shot auto-compaction call. It exits 0 with a status JSON on
/// stdout in every state: noop, compacted, failed (exit 1 only when a
/// summary was required and both attempts failed).
#[derive(Parser, Debug)]
#[command(about = "in-session auto-compaction", version)]
struct Args {
    /// The session directory (events.jsonl lives inside).
    session: PathBuf,
    /// The config.toml path.
    #[arg(long, default_value = "config.toml")]
    config: PathBuf,
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
    /// The event schema directory. The default is the sibling of the
    // running binary's repo checkout: the e2e runs the loop from a
    // work directory, where the repo-relative default does not exist.
    #[arg(long)]
    schemas: Option<PathBuf>,
    /// The path of the assemble binary (the sibling by default).
    #[arg(long)]
    assemble: Option<PathBuf>,
    /// The path of the model binary (the sibling by default).
    #[arg(long)]
    model: Option<PathBuf>,
    /// The path of the log binary (the sibling by default).
    #[arg(long)]
    log: Option<PathBuf>,
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
    Fire {
        predicted: u64,
    },
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
        let last = inp
            .trigger_readings
            .last()
            .map(|r| r.1)
            .unwrap_or(0);
        return Decision::Fire {
            predicted: last,
        };
    }
    // Pi-style stateless check: last measured reading vs trigger level.
    match inp.trigger_readings.last() {
        Some((_, tokens)) if *tokens >= inp.trigger_level => Decision::Fire {
            predicted: *tokens,
        },
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
    let raw = match std::fs::read_to_string(path) {
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
fn append_event(
    log_bin: &Path,
    session: &Path,
    schemas: &Path,
    event: &Value,
) -> Result<(), String> {
    let body = serde_json::to_string(event).map_err(|e| e.to_string())?;
    let mut child = Command::new(log_bin)
        .arg("--session")
        .arg(session)
        .arg("--schemas")
        .arg(schemas)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn log: {e}"))?
        ;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(body.as_bytes())
            .map_err(|e| format!("write log stdin: {e}"))?;
    }
    let status = child
        .wait()
        .map_err(|e| format!("wait log: {e}"))?
        ;
    if !status.success() {
        return Err(format!("log exited {status}"));
    }
    Ok(())
}

fn main() {
    let args = Args::parse();
    // The schema directory: the explicit flag, then the repo layout
    // next to the running binary (target/debug/../../schemas), then
    // the repo-relative default for a checkout-local run.
    let schemas_dir: PathBuf = match &args.schemas {
        Some(p) => p.clone(),
        None => std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("..").join("..").join("schemas").join("events").join("v1")))
            .filter(|p| p.exists())
            .unwrap_or_else(|| PathBuf::from("schemas/events/v1")),
    };

    // The cooldown and the trigger need the log. The force and the
    // overflow skip the trigger test but still read the log for the
    // boundary and the cut.
    let events_path = args.session.join("events.jsonl");
    let events = read_events(&events_path);

    // The boundary: the last parseable `compaction_summary`.
    let boundary: Option<Boundary> = events
        .iter()
        .filter_map(parse_boundary)
        .next_back();

    // The cooldown anchor: the `last_user_seq` of the last
    // `compaction_failed` marker.
    let failed_user_seq: Option<usize> = events
        .iter().rfind(|e| e.value.get("type").and_then(|t| t.as_str()) == Some("compaction_failed"))
        .and_then(|e| e.value.get("last_user_seq").and_then(|s| s.as_u64()))
        .map(|s| s as usize);

    // The user messages: the last one anchors the cooldown.
    let last_user_seq: usize = events
        .iter().rfind(|e| e.value.get("type").and_then(|t| t.as_str()) == Some("user_message"))
        .map(|e| e.seq)
        .unwrap_or(0);

    // The kept region: after the boundary, or the whole log.
    let first_kept = boundary.as_ref().map(|b| b.first_kept_seq).unwrap_or(0);
    let kept_events: Vec<LogEvent> = events
        .iter()
        .filter(|e| e.seq >= first_kept)
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
    let cfg = load_config(&args.config);
    let empty = toml::Value::Table(toml::map::Map::new());
    let limits = cfg.get("limits").unwrap_or(&empty);
    let budget = resolve_budget(&cfg);
    let reserve = val_int(limits, "compact_reserve_tokens").unwrap_or(16_384) as u64;
    let enabled = val_bool(limits, "compact_enabled").unwrap_or(true);

    let trigger_level = if reserve == 0 {
        eprintln!(
            "Warning: compact_reserve_tokens is 0: the trigger inverts. Clamping the trigger to one reserve below the budget."
        );
        budget.saturating_sub(1)
    } else {
        budget.saturating_sub(reserve)
    };

    let overflow = matches!(args.reason, Reason::Overflow);
    // The trigger decision. The tokens_before of the Fire branch is
    // the newest measurement of the kept region: the current context
    // size the compact replaces.
    let last_measurement = measurements.last().map(|m| m.1).unwrap_or(0);
    let decision = decide_trigger(&TriggerInput {
        trigger_level,
        trigger_readings: measurements.clone(),
        last_user_seq,
        failed_user_seq,
        cooldown: true,
        enabled,
        force: args.force,
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
        Decision::Fire {
            predicted,
        } => {
            let _ = predicted;
            run_compaction()
                .args(&args)
                .schemas_dir(&schemas_dir)
                .kept_events(&kept_events)
                .boundary(&boundary)
                .tokens_before(last_measurement)
                .overflow(overflow)
                .last_user_seq(last_user_seq)
                .call();
        }
    }
}

/// The compact run: the cut, the summary call, the marker appends.
#[builder]
fn run_compaction(
    args: &Args,
    schemas_dir: &Path,
    kept_events: &[LogEvent],
    boundary: &Option<Boundary>,
    tokens_before: u64,
    overflow: bool,
    last_user_seq: usize,
) -> ! {
    let cfg = load_config(&args.config);
    let empty = toml::Value::Table(toml::map::Map::new());
    let limits = cfg.get("limits").unwrap_or(&empty);
    let keep_tokens: u64 = val_int(limits, "compact_keep_tokens").unwrap_or(20_000) as u64;
    let clip: u64 = val_int(limits, "tool_result_max_chars").unwrap_or(20_000) as u64;

    // The projected kept events, for the estimator. The marker
    // types project to an empty user: zero tokens, no group effect.
    let projected: Vec<Ev> = kept_events
        .iter()
        .map(|e| compact_math::project_event(&e.value))
        .collect();

    let est_caps: Caps = Caps {
        result: Some(clip),
        text: None,
    };
    let cut = compact_math::find_cut(&projected, keep_tokens, &est_caps);
    if cut == 0 {
        let status = serde_json::json!({
            "status": "noop",
            "reason": args.reason.as_str(),
            "detail": "the old region is empty: the keep window covers the log",
        });
        println!("{}", serde_json::to_string(&status).unwrap());
        std::process::exit(0);
    }

    // The old region: the projected events before the cut.
    let old = &projected[..cut];
    let old_events: Vec<LogEvent> = kept_events[..cut].to_vec();
    let up_to = old_events.last().map(|e| e.seq).unwrap_or(0);

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
    let log_bin = resolve_bin(&args.log, "LOG_BIN", "log");
    if let Err(e) = append_event(&log_bin, &args.session, schemas_dir, &started) {
        eprintln!("compact: append compaction_started failed: {e}");
    }

    // The summary input: assemble prints the request JSON on
    // stdout.
    let assemble_bin = args.assemble.clone().unwrap_or_else(|| sibling("assemble"));
    let up_to_str = up_to.to_string();
    let mut cmd = Command::new(&assemble_bin);
    cmd.arg("--session")
        .arg(&args.session)
        .arg("--config")
        .arg(&args.config)
        .arg("--summary-input")
        .arg("--up-to")
        .arg(&up_to_str);
    if args.strip_last_assistant {
        cmd.arg("--drop-last-assistant");
    }
    let out = cmd.output().unwrap_or_else(|e| {
        fail_and_exit(args, schemas_dir, overflow, last_user_seq, "the assemble call failed", &e.to_string());
    });
    if !out.status.success() {
        fail_and_exit(
            args,
            schemas_dir,
            overflow,
            last_user_seq,
            "the assemble call failed",
            &String::from_utf8_lossy(&out.stderr),
        );
    }
    let request: Value = match serde_json::from_slice(&out.stdout) {
        Ok(v) => v,
        Err(e) => fail_and_exit(
            args,
            schemas_dir,
            overflow,
            last_user_seq,
            "the assemble output is not a request",
            &e.to_string(),
        ),
    };

    // The summary call: two attempts, through the model binary. The
    // request JSON goes to the model's stdin, and the model appends
    // its own assistant_message, tool_result, and stop markers to
    // the session log.
    let model_bin = resolve_bin(&args.model, "MODEL_BIN", "model");
    let request_str = serde_json::to_string(&request).unwrap();
    let mut last_err = String::new();
    let mut summary_usage: Option<Value> = None;
    let summary: Option<String> = (0..2).find_map(|attempt| {
        let mut child = match Command::new(&model_bin)
            .arg("--config")
            .arg(&args.config)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                last_err = format!("attempt {}: spawn model: {e}", attempt + 1);
                return None;
            }
        };
        if let Some(mut stdin) = child.stdin.take() {
            if let Err(e) = stdin.write_all(request_str.as_bytes()) {
                last_err = format!("attempt {}: write model stdin: {e}", attempt + 1);
                return None;
            }
        }
        let out = match child.wait_with_output() {
            Ok(o) => o,
            Err(e) => {
                last_err = format!("attempt {}: wait model: {e}", attempt + 1);
                return None;
            }
        };
        if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr);
                last_err = format!(
                    "attempt {}: the model binary exited {} ({})",
                    attempt + 1,
                    out.status.code().unwrap_or(-1),
                    stderr.trim()
                );
                return None;
            }
            let resp: Value = match serde_json::from_slice(&out.stdout) {
                Ok(v) => v,
                Err(e) => {
                    last_err = format!(
                        "attempt {}: the model output is not JSON: {e}",
                        attempt + 1
                    );
                    return None;
                }
            };
            let stop = resp.get("stop_reason").and_then(|s| s.as_str()).unwrap_or("");
            if stop == "error" {
                let detail = resp
                    .get("detail")
                    .and_then(|d| d.as_str())
                    .unwrap_or("no detail");
                last_err = format!("attempt {}: the model returned an error stop: {detail}", attempt + 1);
                return None;
            }
            let text = resp.get("text").and_then(|t| t.as_str()).unwrap_or("");
            if text.trim().is_empty() {
                last_err = format!("attempt {}: an empty summary (stop reason {stop})", attempt + 1);
                return None;
            }
            // The usage of the winning attempt rides the marker:
            // the statusline sums it into the cumulative totals
            // (docs/auto-compact-plan.md section 4.6).
            let usage = resp.get("usage").cloned();
            summary_usage = usage;
            Some(text.trim().to_string())
        });

    let summary = match summary {
        Some(s) => s,
        None => fail_and_exit(args, schemas_dir, overflow, last_user_seq, "both summary attempts failed", &last_err),
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
    // sequence of the event at the cut.
    let first_kept_seq = kept_events.get(cut).map(|e| e.seq).unwrap_or(1);
    if first_kept_seq < 1 {
        fail_and_exit(
            args,
            schemas_dir,
            overflow,
            last_user_seq,
            "the first_kept_seq is below 1",
            "the producer check rejected the marker",
        );
    }

    // The tokens after: the full-form estimate of the kept region
    // plus the summary framing.
    let kept_projected: Vec<Ev> = projected[cut..].to_vec();
    let tokens_after = compact_math::est_tokens_after(&kept_projected, &summary, clip);

    let mut done = serde_json::json!({
        "v": 1,
        "type": "compaction_summary",
        "ts": ts_now(),
        "summary": summary,
        "first_kept_seq": first_kept_seq,
        "reason": reason_str,
        "tokens_before": tokens_before,
        "tokens_after": tokens_after,
        "read_files": read_files,
        "modified_files": modified_files,
    });
    if let Some(u) = summary_usage {
        done["usage"] = u;
    }
    if let Err(e) = append_event(&log_bin, &args.session, schemas_dir, &done) {
        eprintln!("compact: append compaction_summary failed: {e}");
    }

    let status = serde_json::json!({
        "status": "compacted",
        "reason": reason_str,
        "first_kept_seq": first_kept_seq,
        "tokens_before": tokens_before,
        "tokens_after": tokens_after,
    });
    println!("{}", serde_json::to_string(&status).unwrap());
    std::process::exit(0);
}

/// The failure path: the `compaction_failed` marker, the detail on
/// stderr, the failed status on stdout, exit 1. The last_user_seq
/// anchors the cooldown of the next threshold trigger.
fn fail_and_exit(
    args: &Args,
    schemas_dir: &Path,
    overflow: bool,
    last_user_seq: usize,
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
        "attempts": 2,
        "detail": detail,
    });
    let log_bin = resolve_bin(&args.log, "LOG_BIN", "log");
    if let Err(e) = append_event(&log_bin, &args.session, schemas_dir, &failed) {
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
/// shared crate). The copy is noted in notes/itches.md.
fn load_config(path: &Path) -> toml::Value {
    let raw = std::fs::read_to_string(path).unwrap_or_default();
    toml::from_str(&raw).unwrap_or(toml::Value::Table(toml::map::Map::new()))
}

fn val_int(v: &toml::Value, key: &str) -> Option<i64> {
    v.get(key).and_then(|x| x.as_integer())
}

/// The string value. Copied with the int form from bin/assemble so
/// the knob additions stay one edit apart.
#[allow(dead_code)]
fn val_str(v: &toml::Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(|s| s.to_string())
}

fn val_bool(v: &toml::Value, key: &str) -> Option<bool> {
    v.get(key).and_then(|x| x.as_bool())
}

/// The active model name: the MODEL env var or the config.
fn resolve_active_model(config: &toml::Value) -> String {
    std::env::var("MODEL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            config
                .get("active")
                .and_then(|a| a.get("model"))
                .and_then(|m| m.as_str())
                .unwrap_or("deepseek")
                .to_string()
        })
}

/// The context budget: the model window minus the output
/// reservation, clamped by the user knob (the assemble rule).
fn resolve_budget(config: &toml::Value) -> u64 {
    let active = resolve_active_model(config);
    let empty = toml::Value::Table(toml::map::Map::new());
    let model_root = config.get("model").unwrap_or(&empty);
    let mdl = model_root.get(&active).unwrap_or(&empty);
    let max_out = val_int(mdl, "max_output_tokens")
        .or_else(|| val_int(model_root, "max_output_tokens"))
        .unwrap_or(4096) as u64;
    let window = val_int(mdl, "context_tokens").unwrap_or(131072) as u64;
    let window_input = window.saturating_sub(max_out);
    let budget = val_int(config.get("limits").unwrap_or(&empty), "context_budget_tokens")
        .map(|v| (v.max(1)) as u64)
        .unwrap_or(window_input)
        .min(window_input.max(1));
    budget.max(1)
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
        let forced = TriggerInput {
            force: true,
            ..inp
        };
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
}
