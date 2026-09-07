#![deny(clippy::todo, clippy::unimplemented, clippy::unreachable)]

use clap::Parser;
use rushi_common::event_validation;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

/// Derive step state from the session log
#[derive(Parser)]
#[command(
    name = "claim",
    about = "Determine what work is owed from the session log"
)]
struct Args {
    /// Session directory path
    #[arg(long)]
    session: String,

    /// Path to schema directory
    #[arg(long, default_value = "schemas/events/v1")]
    schemas: String,
}

fn main() {
    let args = Args::parse();

    let log_path = PathBuf::from(&args.session).join("events.jsonl");

    if !log_path.exists() {
        // No log means idle
        let session_name = PathBuf::from(&args.session)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        println!(
            "{}",
            serde_json::json!({
                "session": session_name,
                "state": "idle",
                "last_user_message_seq": 0,
                "pending_tool_calls": []
            })
        );
        return;
    }

    let lines = match fs::read_to_string(&log_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: cannot read log: {e}");
            std::process::exit(1);
        }
    };

    // The shared validator keeps the claim in step with the log
    // vocabulary (docs/phase-2-plan.md section 6). When the schemas
    // directory is absent (e.g. temp workdir in e2e tests), skip
    // validation and rely on the structural state machine alone.
    let schemas: Vec<(String, serde_json::Value)> = if std::path::Path::new(&args.schemas).is_dir() {
        event_validation::load_schemas(&args.schemas)
    } else {
        eprintln!("claim: schemas dir not found at {}, skipping validation", args.schemas);
        Vec::new()
    };

    let (state, last_user_message_seq, pending_tool_calls, pending_follow_ups) =
        derive_state(&lines, &schemas);

    let session_name = PathBuf::from(&args.session)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    println!(
        "{}",
        serde_json::json!({
            "session": session_name,
            "state": state,
            "last_user_message_seq": last_user_message_seq,
            "pending_tool_calls": pending_tool_calls,
            "pending_follow_ups": pending_follow_ups
        })
    );
}

/// The state machine over the log lines. Returns the state, the
/// 1-based sequence of the last user message, the unresolved tool
/// calls, and the pending follow-queue message sequences
/// (docs/tui-pending-user-messages.md stage 2).
///
/// Lines that fail schema validation are skipped: an unknown event
/// type or a malformed shape owes no state. A new type joins the
/// vocabulary with one schema file and no code change here
/// (docs/phase-2-plan.md section 6).
///
/// States:
/// - `idle`: nothing owed (no log activity, or a terminal event).
/// - `awaiting_model`: the loop owes a model call.
/// - `awaiting_tool_result`: routed calls still lack results.
/// - `exhausted`: a `context_exhausted` event closed the session
///   through the automatic handoff (correction 57). The TUI offers a
///   one-key resume in the seeded session.
/// - `awaiting_approval`: an unanswered `approval_request` gates the
///   loop. A matching `approval` event answers it. A later terminal
///   event settles the session instead (docs/phase-2-plan.md
///   section 4.8).
///
/// The delivery queues (stage 2): a `user_message` in the `follow`
/// queue wakes no state; it waits for the turn boundary and rides
/// `pending_follow_ups`. A steer message (the missing field) sets
/// `awaiting_model`, as before. A turn boundary (an assistant
/// message with no tool calls, an error, or a closed handoff)
/// consumes the pending follow-ups: they ran as new turns through
/// the boundary.
fn derive_state(
    lines: &str,
    schemas: &[(String, serde_json::Value)],
) -> (String, usize, Vec<serde_json::Value>, Vec<usize>) {
    let mut last_user_message_seq: usize = 0;
    let mut state = "idle".to_string();
    let mut pending_tool_calls: Vec<serde_json::Value> = Vec::new();
    let mut pending_follow_ups: Vec<usize> = Vec::new();
    let mut pending_approval_request: Option<serde_json::Value> = None;
    // The event type of each 1-based log seq: the rewind arm reads
    // its target's type to decide the owed state (docs/rewind-fork-
    // design.md section 5).
    let mut seq_types: HashMap<usize, String> = HashMap::new();
    let mut i = 0;

    for line in lines.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        i += 1;
        let event: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        // The shared validator skips events outside the schema
        // vocabulary (docs/phase-2-plan.md section 6).
        if event_validation::validate_value(&event, schemas).is_err() {
            continue;
        }

        let event_type = event.get("type").and_then(|t| t.as_str()).unwrap_or("");
        seq_types.insert(i, event_type.to_string());

        match event_type {
            "user_message" => {
                last_user_message_seq = i;
                // The follow queue writes the field; a missing
                // field means steer.
                let follow = event
                    .get("queue")
                    .and_then(|q| q.as_str())
                    .unwrap_or("steer")
                    == "follow";
                if follow {
                    pending_follow_ups.push(i);
                } else {
                    state = "awaiting_model".to_string();
                }
            }
            "tool_result" => {
                state = "awaiting_model".to_string();
            }
            "assistant_message" => {
                if let Some(tool_calls) = event.get("tool_calls") {
                    if let Some(tc_arr) = tool_calls.as_array() {
                        if !tc_arr.is_empty() {
                            state = "awaiting_tool_result".to_string();
                            pending_tool_calls.clear();
                            for tc in tc_arr {
                                pending_tool_calls.push(tc.clone());
                            }
                        } else {
                            // The turn boundary: the pending
                            // follow-ups ran as new turns through
                            // it. Consume them.
                            state = "idle".to_string();
                            pending_tool_calls.clear();
                            pending_follow_ups.clear();
                        }
                    }
                } else {
                    state = "idle".to_string();
                    pending_tool_calls.clear();
                    pending_follow_ups.clear();
                }
            }
            "approval_request" => {
                // The `tool.before` hook gated a tool call. The loop
                // now owes the user an answer
                // (docs/phase-2-plan.md section 4.8).
                pending_approval_request = Some(event.clone());
            }
            "approval" => {
                // Only the matching id answers the pending request.
                if let Some(req) = &pending_approval_request {
                    let req_id = req.get("id").and_then(|id| id.as_str());
                    let answer_id = event.get("id").and_then(|id| id.as_str());
                    if req_id == answer_id {
                        pending_approval_request = None;
                    }
                }
            }
            "error" => {
                // The terminal error settles the session. An
                // unanswered request is owed no longer.
                state = "idle".to_string();
                pending_tool_calls.clear();
                pending_follow_ups.clear();
                pending_approval_request = None;
            }
            // The handoff closed the turn. The seeded session holds
            // the task; this one is done.
            "context_exhausted" => {
                state = "exhausted".to_string();
                pending_tool_calls.clear();
                pending_follow_ups.clear();
                pending_approval_request = None;
            }
            "rewind" => {
                // The fork marker settles the session at its target
                // (docs/rewind-fork-design.md section 5): the events
                // it masks owe nothing, so every pending list
                // clears. The owed state is the target's: a
                // `tool_result` target in `on` mode is the finished
                // step, and the model call that continues it is
                // owed; every other target is idle (a `before`-mode
                // user message waits in the input box, unsent).
                pending_tool_calls.clear();
                pending_follow_ups.clear();
                pending_approval_request = None;
                let target = event
                    .get("target_seq")
                    .and_then(|t| t.as_u64())
                    .map(|t| t as usize)
                    .unwrap_or(0);
                let before = event
                    .get("mode")
                    .and_then(|m| m.as_str())
                    .unwrap_or("")
                    == "before";
                let owed = !before
                    && seq_types.get(&target).is_some_and(|t| t == "tool_result");
                state = if owed { "awaiting_model".to_string() } else { "idle".to_string() };
            }
            _ => {}
        }
    }

    // If state is awaiting_tool_result or awaiting_approval, filter
    // out resolved tool calls
    if state == "awaiting_tool_result" || pending_approval_request.is_some() {
        let mut resolved_ids: HashSet<String> = HashSet::new();
        for line in lines.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let event: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if event.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                if let Some(id) = event.get("id").and_then(|id| id.as_str()) {
                    resolved_ids.insert(id.to_string());
                }
            }
        }

        pending_tool_calls.retain(|tc| {
            if let Some(id) = tc.get("id").and_then(|id| id.as_str()) {
                !resolved_ids.contains(id)
            } else {
                false
            }
        });

        if pending_tool_calls.is_empty() {
            state = "idle".to_string();
        }
    }

    // A live request here means no terminal event followed it: the
    // two terminal arms above clear the slot. The wait resumes on
    // the next step (docs/phase-2-plan.md section 4.8).
    if pending_approval_request.is_some() {
        state = "awaiting_approval".to_string();
    }

    (state, last_user_message_seq, pending_tool_calls, pending_follow_ups)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(v: &serde_json::Value) -> String {
        v.to_string()
    }

    /// The shared schema set, loaded the same way the binary loads
    /// it at runtime. The tests run from the crate dir; the schemas
    /// live at the workspace root.
    fn schemas() -> Vec<(String, serde_json::Value)> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../schemas/events/v1");
        event_validation::load_schemas(dir.to_str().expect("schema dir path"))
    }

    #[test]
    fn empty_log_is_idle() {
        let (state, seq, pending, follows) = derive_state("", &schemas());
        assert_eq!(state, "idle");
        assert_eq!(seq, 0);
        assert!(pending.is_empty());
        assert!(follows.is_empty());
    }

    #[test]
    fn user_message_awaits_model() {
        let log = line(&serde_json::json!({"v":1,"type":"user_message","ts":"t","content":"go"}));
        let (state, seq, _, _) = derive_state(&log, &schemas());
        assert_eq!(state, "awaiting_model");
        assert_eq!(seq, 1);
    }

    #[test]
    fn context_exhausted_reports_the_exhausted_state() {
        let log = format!(
            "{}\n{}",
            line(&serde_json::json!({"v":1,"type":"user_message","ts":"t","content":"go"})),
            line(
                &serde_json::json!({"v":1,"type":"context_exhausted","ts":"t","message":"m","new_session":"s1_h1","summary_request":{}})
            )
        );
        let (state, _, pending, _) = derive_state(&log, &schemas());
        assert_eq!(state, "exhausted");
        assert!(pending.is_empty());
    }

    /// A user message after the exhaustion reopens normal work: the
    /// loop runs the new turn, and a later exhaustion re-closes it.
    #[test]
    fn user_message_after_exhaustion_reopens_the_loop() {
        let log = format!(
            "{}\n{}\n{}",
            line(
                &serde_json::json!({"v":1,"type":"context_exhausted","ts":"t","message":"m","new_session":""})
            ),
            line(&serde_json::json!({"v":1,"type":"user_message","ts":"t","content":"continue"})),
            line(
                &serde_json::json!({"v":1,"type":"context_exhausted","ts":"t","message":"m","new_session":"s1_h2"})
            )
        );
        let (state, seq, _, _) = derive_state(&log, &schemas());
        assert_eq!(state, "exhausted");
        assert_eq!(seq, 2);
    }

    /// The three compaction marker types are no-ops for the state
    /// machine (docs/auto-compact-plan.md section 4.1): they leave
    /// the `awaiting_model` and `idle` tails unchanged.
    #[test]
    fn compaction_markers_leave_awaiting_model_unchanged() {
        let markers = [
            r#"{"v":1,"type":"compaction_started","ts":"t","reason":"threshold","tokens_before":212000}"#,
            r#"{"v":1,"type":"compaction_failed","ts":"t","reason":"overflow","detail":"the summary call stopped with error","last_user_seq":3}"#,
            r#"{"v":1,"type":"compaction_summary","ts":"t","summary":"s","first_kept_seq":1,"reason":"threshold","tokens_before":212000,"tokens_after":33000}"#,
        ];
        for m in &markers {
            let log = format!(
                "{}\n{}",
                line(&serde_json::json!({"v":1,"type":"user_message","ts":"t","content":"go"})),
                m
            );
            let (state, seq, pending, _) = derive_state(&log, &schemas());
            assert_eq!(state, "awaiting_model", "{m}");
            assert_eq!(seq, 1, "the marker adds no user message");
            assert!(pending.is_empty());
        }
    }

    /// The markers keep an `idle` tail idle: a finished turn that
    /// compacts in place stays idle, with no pending work.
    #[test]
    fn compaction_markers_leave_idle_unchanged() {
        let done = line(
            &serde_json::json!({
                "v":1,"type":"assistant_message","ts":"t","content":"done","tool_calls":[],"stop_reason":"stop"
            }),
        );
        for m in [
            r#"{"v":1,"type":"compaction_started","ts":"t","reason":"overflow","tokens_before":0}"#,
            r#"{"v":1,"type":"compaction_failed","ts":"t","reason":"threshold","detail":"d","last_user_seq":0}"#,
            r#"{"v":1,"type":"compaction_summary","ts":"t","summary":"s","first_kept_seq":2,"reason":"overflow","tokens_before":212000}"#,
        ] {
            let log = format!("{}\n{}", done, m);
            let (state, _, pending, _) = derive_state(&log, &schemas());
            assert_eq!(state, "idle", "{m}");
            assert!(pending.is_empty());
        }
    }

    #[test]
    fn error_after_exhaustion_stays_exhausted() {
        // The handoff flow logs a failed summary error before the
        // marker event. The marker is the last word on the state.
        let log = format!(
            "{}\n{}",
            line(
                &serde_json::json!({"v":1,"type":"error","ts":"t","message":"summary call failed"})
            ),
            line(
                &serde_json::json!({"v":1,"type":"context_exhausted","ts":"t","message":"m","new_session":""})
            )
        );
        let (state, _, _, _) = derive_state(&log, &schemas());
        assert_eq!(state, "exhausted");
    }

    // ── stage 2: the delivery queues ─────────────────────────
    // docs/tui-pending-user-messages.md section 4.

    /// A follow-queue message wakes no state: the loop stays idle
    /// and the message rides `pending_follow_ups`.
    #[test]
    fn follow_message_keeps_idle_and_counts() {
        let log = line(&serde_json::json!({"v":1,"type":"user_message","ts":"t","content":"later","queue":"follow"}));
        let (state, seq, pending, follows) = derive_state(&log, &schemas());
        assert_eq!(state, "idle", "follow wakes no work");
        assert_eq!(seq, 1);
        assert!(pending.is_empty());
        assert_eq!(follows, vec![1]);
    }

    /// A steer message (the missing queue field) still wakes the
    /// model: the stage-1 behavior holds for old lines.
    #[test]
    fn steer_message_wakes_the_model() {
        let log = line(&serde_json::json!({"v":1,"type":"user_message","ts":"t","content":"now"}));
        let (state, _, pending, follows) = derive_state(&log, &schemas());
        assert_eq!(state, "awaiting_model");
        assert!(pending.is_empty());
        assert!(follows.is_empty());
    }

    /// An explicit steer queue value behaves like the missing field.
    #[test]
    fn explicit_steer_wakes_the_model() {
        let log = line(&serde_json::json!({"v":1,"type":"user_message","ts":"t","content":"now","queue":"steer"}));
        let (state, _, _, follows) = derive_state(&log, &schemas());
        assert_eq!(state, "awaiting_model");
        assert!(follows.is_empty());
    }

    /// The turn boundary consumes the pending follow-ups: an
    /// assistant message with no tool calls runs them as new turns.
    #[test]
    fn turn_boundary_consumes_the_follow_ups() {
        let log = format!(
            "{}\n{}\n{}",
            line(&serde_json::json!({"v":1,"type":"user_message","ts":"t","content":"go"})),
            line(&serde_json::json!({"v":1,"type":"user_message","ts":"t","content":"later","queue":"follow"})),
            line(&serde_json::json!({"v":1,"type":"assistant_message","ts":"t","content":"done","tool_calls":[],"stop_reason":"stop"}))
        );
        let (state, _, _, follows) = derive_state(&log, &schemas());
        assert_eq!(state, "idle");
        assert!(follows.is_empty(), "the boundary consumed the follow-up");
    }

    /// A follow-up pending with a steer wake keeps both: the steer
    /// sets `awaiting_model`, the follow rides the count.
    #[test]
    fn steer_and_follow_coexist() {
        let log = format!(
            "{}\n{}",
            line(&serde_json::json!({"v":1,"type":"user_message","ts":"t","content":"later","queue":"follow"})),
            line(&serde_json::json!({"v":1,"type":"user_message","ts":"t","content":"now"}))
        );
        let (state, _, _, follows) = derive_state(&log, &schemas());
        assert_eq!(state, "awaiting_model");
        assert_eq!(follows, vec![1]);
    }

    // ── awaiting_approval: the approval round-trip ───────────
    // docs/phase-2-plan.md section 4.8.

    fn user_msg() -> String {
        line(&serde_json::json!({"v":1,"type":"user_message","ts":"t","content":"go"}))
    }

    fn assistant_with_call() -> String {
        line(
            &serde_json::json!({
                "v":1,"type":"assistant_message","ts":"t","content":"",
                "tool_calls":[{"id":"tc1","name":"bash","arguments":{"cmd":"x"}}],
                "stop_reason":"tool_calls"
            }),
        )
    }

    fn tool_call() -> String {
        line(&serde_json::json!({"v":1,"type":"tool_call","ts":"t","id":"tc1","name":"bash","arguments":{"cmd":"x"}}))
    }

    fn approval_request(id: &str) -> String {
        line(&serde_json::json!({
            "v":1,"type":"approval_request","ts":"t","id":id,
            "tool_call_id":"tc1","prompt":"run it?"
        }))
    }

    fn approval(id: &str, decision: &str) -> String {
        line(&serde_json::json!({"v":1,"type":"approval","ts":"t","id":id,"decision":decision}))
    }

    fn tool_result() -> String {
        line(&serde_json::json!({"v":1,"type":"tool_result","ts":"t","id":"tc1","value":{},"is_error":false}))
    }

    fn error_event() -> String {
        line(&serde_json::json!({"v":1,"type":"error","ts":"t","message":"boom"}))
    }

    fn context_exhausted() -> String {
        line(&serde_json::json!({"v":1,"type":"context_exhausted","ts":"t","message":"m","new_session":""}))
    }

    /// An unanswered request puts the session in the wait.
    #[test]
    fn unanswered_approval_request_awaits_approval() {
        let log = format!(
            "{}\n{}\n{}\n{}",
            user_msg(),
            assistant_with_call(),
            tool_call(),
            approval_request("a1")
        );
        let (state, seq, pending, _) = derive_state(&log, &schemas());
        assert_eq!(state, "awaiting_approval");
        assert_eq!(seq, 1);
        assert!(
            pending
                .iter()
                .any(|tc| tc.get("id").and_then(|v| v.as_str()) == Some("tc1")),
            "the gated call stays pending"
        );
    }

    /// A matching allow answers the request: the wait clears and the
    /// earlier wake holds.
    #[test]
    fn allow_approval_answers_the_request() {
        let log = format!(
            "{}\n{}\n{}",
            user_msg(),
            approval_request("a1"),
            approval("a1", "allow")
        );
        let (state, _, _, _) = derive_state(&log, &schemas());
        assert_eq!(state, "awaiting_model");
    }

    /// A deny answers the request the same way: no wait remains.
    #[test]
    fn deny_approval_answers_the_request() {
        let log = format!(
            "{}\n{}\n{}",
            user_msg(),
            approval_request("a1"),
            approval("a1", "deny")
        );
        let (state, _, _, _) = derive_state(&log, &schemas());
        assert_eq!(state, "awaiting_model");
    }

    /// An answer for another id leaves this request pending.
    #[test]
    fn foreign_id_approval_leaves_the_request_pending() {
        let log = format!(
            "{}\n{}\n{}",
            user_msg(),
            approval_request("a1"),
            approval("a2", "allow")
        );
        let (state, _, _, _) = derive_state(&log, &schemas());
        assert_eq!(state, "awaiting_approval");
    }

    /// An answered old request then a new one: only the newest
    /// unanswered request sets the wait.
    #[test]
    fn newest_unanswered_request_sets_the_wait() {
        let log = format!(
            "{}\n{}\n{}\n{}",
            user_msg(),
            approval_request("a1"),
            approval("a1", "allow"),
            approval_request("a2")
        );
        let (state, _, _, _) = derive_state(&log, &schemas());
        assert_eq!(state, "awaiting_approval");
    }

    /// A terminal error after the request keeps the terminal state.
    #[test]
    fn error_after_request_keeps_the_idle_state() {
        let log = format!(
            "{}\n{}\n{}",
            user_msg(),
            approval_request("a1"),
            error_event()
        );
        let (state, _, _, _) = derive_state(&log, &schemas());
        assert_eq!(state, "idle");
    }

    /// A handoff after the request keeps the exhausted state.
    #[test]
    fn handoff_after_request_keeps_the_exhausted_state() {
        let log = format!(
            "{}\n{}\n{}",
            user_msg(),
            approval_request("a1"),
            context_exhausted()
        );
        let (state, _, _, _) = derive_state(&log, &schemas());
        assert_eq!(state, "exhausted");
    }

    /// A request after a terminal event re-opens the wait.
    #[test]
    fn request_after_terminal_event_awaits_approval() {
        let log = format!("{}\n{}", error_event(), approval_request("a1"));
        let (state, _, _, _) = derive_state(&log, &schemas());
        assert_eq!(state, "awaiting_approval");
    }

    /// A tool result does not answer the request: the wait holds.
    #[test]
    fn tool_result_does_not_answer_the_request() {
        let log = format!(
            "{}\n{}\n{}\n{}\n{}",
            user_msg(),
            assistant_with_call(),
            tool_call(),
            approval_request("a1"),
            tool_result()
        );
        let (state, _, _, _) = derive_state(&log, &schemas());
        assert_eq!(state, "awaiting_approval");
    }

    /// An event type outside the schema vocabulary owes no state and
    /// breaks no derivation.
    #[test]
    fn unknown_event_type_is_skipped() {
        let log = format!(
            "{}\n{}\n{}",
            user_msg(),
            r#"{"v":1,"type":"future_marker","ts":"t"}"#,
            approval_request("a1")
        );
        let (state, seq, _, _) = derive_state(&log, &schemas());
        assert_eq!(state, "awaiting_approval");
        assert_eq!(seq, 1, "the last user message still counts");
    }

    // ── rewind: the fork marker (docs/rewind-fork-design.md 5) ──

    fn rewind(target: u64, mode: &str) -> String {
        line(&serde_json::json!({
            "v":1,"type":"rewind","ts":"t","target_seq":target,"mode":mode
        }))
    }

    /// A rewind to a user message settles the session: idle, and the
    /// masked events owe nothing (the call in the abandoned branch
    /// must not be re-dispatched).
    #[test]
    fn rewind_to_user_message_settles_the_session() {
        let log = [
            user_msg(),
            assistant_with_call(),
            tool_call(),
            rewind(1, "before"),
        ]
        .join("\n");
        let (state, seq, pending, follows) = derive_state(&log, &schemas());
        assert_eq!(state, "idle");
        assert_eq!(seq, 1, "the last user message still counts");
        assert!(
            pending.is_empty(),
            "the masked call owes nothing: {pending:?}"
        );
        assert!(follows.is_empty());
    }

    /// A rewind to a tool_result target in `on` mode is the finished
    /// step: the model call that continues it is owed.
    #[test]
    fn rewind_to_tool_result_awaits_model() {
        let log = [
            user_msg(),
            assistant_with_call(),
            tool_call(),
            tool_result(),
            rewind(4, "on"),
        ]
        .join("\n");
        let (state, _, pending, _) = derive_state(&log, &schemas());
        assert_eq!(state, "awaiting_model", "the finished step owes its model call");
        assert!(pending.is_empty());
    }

    /// A `before`-mode rewind to a tool_result target is the moment
    /// before the result: the step is un-finished, but claim owes no
    /// re-route. The v1 rule is idle (the producer only picks
    /// settled targets).
    #[test]
    fn rewind_before_mode_never_awaits_model() {
        let log = [
            user_msg(),
            assistant_with_call(),
            tool_call(),
            tool_result(),
            rewind(4, "before"),
        ]
        .join("\n");
        let (state, _, pending, _) = derive_state(&log, &schemas());
        assert_eq!(state, "idle");
        assert!(pending.is_empty());
    }

    /// A rewind clears the pending follow-ups of the masked region:
    /// they waited on a turn boundary that no longer exists in this
    /// branch.
    #[test]
    fn rewind_clears_masked_follow_ups() {
        let follow = line(
            &serde_json::json!({"v":1,"type":"user_message","ts":"t","content":"later","queue":"follow"}),
        );
        let log = format!("{follow}\n{}", rewind(1, "before"));
        let (state, _, _, follows) = derive_state(&log, &schemas());
        assert_eq!(state, "idle");
        assert!(follows.is_empty(), "the masked follow-up is dropped");
    }

    /// A rewind clears an open approval wait of the masked region.
    #[test]
    fn rewind_clears_masked_approval_waits() {
        let log = [
            user_msg(),
            assistant_with_call(),
            tool_call(),
            approval_request("a1"),
            rewind(1, "before"),
        ]
        .join("\n");
        let (state, _, pending, _) = derive_state(&log, &schemas());
        assert_eq!(state, "idle", "the masked wait owes nothing");
        assert!(pending.is_empty());
    }

    /// Events after the rewind ride the new branch: a user message
    /// appended after the marker wakes the model again.
    #[test]
    fn user_message_after_rewind_reopens_the_loop() {
        let log = [
            user_msg(),
            assistant_with_call(),
            tool_call(),
            rewind(1, "before"),
            user_msg(),
        ]
        .join("\n");
        let (state, seq, _, _) = derive_state(&log, &schemas());
        assert_eq!(state, "awaiting_model");
        assert_eq!(seq, 5, "the new-branch message counts");
    }
}
