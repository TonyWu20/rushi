//! `rewind` — the shared active-path computation over the rewind events
//! (docs/rewind-fork-design.md section 3).
//!
//! A `rewind` event at log seq `S` with `target_seq` `T` and `mode`
//! states one fact: the model context at `S` equals the model context
//! at `T_eff`, where `T_eff = T` (mode `on`) or `T - 1` (mode
//! `before`). The events in the open span `(T_eff, S)` — the branch
//! abandoned when the rewind was picked — are masked: they stay in
//! the log (append-only is untouched) but do not enter the context.
//!
//! The mask is not a single gap: a later rewind may target inside an
//! earlier branch, and re-entering a branch rebuilds its full active
//! path. The active path of a prefix is computed recursively through
//! the rewind chain, so every nesting depth masks the right spans.
//! See `active_ranges` for the definition and the counter-example
//! that the single-gap rule gets wrong.

use serde_json::Value;

/// One parsed `rewind` event, at its 1-based log seq.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RewindRef {
    /// The 1-based log seq of the rewind event itself.
    pub seq: usize,
    /// The 1-based log seq of the target event. Always `target < seq`:
    /// a rewind points at an earlier event.
    pub target: usize,
    /// `true` for mode `before`: the target is excluded from the
    /// context (restored to the input box). `false` for mode `on`.
    pub before: bool,
}

impl RewindRef {
    /// The effective context boundary: `target` in `on` mode, `target - 1`
    /// in `before` mode. Zero when `before` mode targets seq 1.
    pub fn eff(&self) -> usize {
        if self.before {
            self.target.saturating_sub(1)
        } else {
            self.target
        }
    }
}

/// Parse one log event into a [`RewindRef`]. The event must be a
/// `rewind` with an integer `target_seq >= 1`, a `mode` of `before`
/// or `on`, and a target strictly earlier than the event itself.
/// A malformed value is rejected (`None`): the caller re-projects
/// as if the event were absent, like a corrupt compaction boundary.
pub fn parse_rewind_event(event: &Value, seq: usize) -> Option<RewindRef> {
    if event.get("type").and_then(|t| t.as_str()) != Some("rewind") {
        return None;
    }
    let target = event
        .get("target_seq")
        .and_then(|v| v.as_u64())
        .filter(|&t| t >= 1)
        .map(|t| t as usize)?;
    if target >= seq {
        // A rewind cannot point at itself or the future.
        return None;
    }
    let before = match event.get("mode").and_then(|m| m.as_str()) {
        Some("before") => true,
        Some("on") => false,
        _ => return None,
    };
    Some(RewindRef {
        seq,
        target,
        before,
    })
}

/// The active path of the log prefix ending at seq `end`, as
/// disjoint ascending inclusive `(lo, hi)` seq ranges.
///
/// Definition: `active(end)` = `[1..end]` when no rewind event sits
/// at or before `end`; otherwise, with `(S, T, m)` the last rewind
/// event at or before `end`,
/// `active(end) = active(T_eff) ∪ {S+1 ..= end}`, where `T_eff` is
/// the effective target. The recursion walks the rewind chain — each
/// step strictly decreases `end` (`T_eff < S ≤ end`), so it
/// terminates — and unions the spans of every branch the log has
/// forked through.
///
/// The single-gap rule ("mask between the picked event and the
/// latest rewind") is the depth-1 case of this recursion. At depth
/// 2 it leaks: after fork A→B, fork B→A', and a rewind inside A',
/// the gap of the last rewind alone leaves branch B unmasked, so B's
/// events would ride the A' context. The recursion masks B because
/// B lies inside the first fork's gap.
pub fn active_ranges(end: usize, rewinds: &[RewindRef]) -> Vec<(usize, usize)> {
    if end == 0 {
        return Vec::new();
    }
    match rewinds.iter().rfind(|r| r.seq <= end) {
        None => vec![(1, end)],
        Some(r) => {
            let mut out = active_ranges(r.eff(), rewinds);
            if end > r.seq {
                out.push((r.seq + 1, end));
            }
            out
        }
    }
}

/// Whether log seq `s` is inside one of the active ranges.
pub fn seq_in_ranges(s: usize, ranges: &[(usize, usize)]) -> bool {
    ranges.iter().any(|&(lo, hi)| s >= lo && s <= hi)
}

// ======================================================================
// The kernel guard at the write boundary (P8)
// ======================================================================

/// One loud refusal of a proposed rewind at the write boundary
/// (P8): the marker points at no event, or it would land a
/// stranded tool-call pair in the projected context. The write
/// path refuses the append and names the cause; the log stays
/// untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RewindRejection {
    /// The target seq is not a 1-based line of the current log.
    TargetOutOfRange {
        target: u64,
        log_len: usize,
    },
    /// The projected context keeps a tool call whose result is out
    /// of the context.
    StrandedCall {
        call_id: String,
        target: u64,
        before: bool,
    },
    /// The projected context keeps a tool result whose call is out
    /// of the context.
    StrandedResult {
        result_id: String,
        target: u64,
        before: bool,
    },
}

impl std::fmt::Display for RewindRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RewindRejection::TargetOutOfRange { target, log_len } => {
                write!(
                    f,
                    "the rewind target seq {target} is out of range: the log holds {log_len} event line(s)"
                )
            }
            RewindRejection::StrandedCall {
                call_id, target, before,
            } => {
                let b = *before;
                write!(
                    f,
                    "the rewind to seq {target} (mode {}) strands tool call {call_id}: its tool_result would be masked out of the context",
                    mode_word(b)
                )
            }
            RewindRejection::StrandedResult {
                result_id, target, before,
            } => {
                let b = *before;
                write!(
                    f,
                    "the rewind to seq {target} (mode {}) strands tool result {result_id}: its tool_call would be masked out of the context",
                    mode_word(b)
                )
            }
        }
    }
}

fn mode_word(before: bool) -> &'static str {
    if before {
        "before"
    } else {
        "on"
    }
}

/// The kernel guard: whether appending a `rewind` marker with
/// `target` and `before` would strand a tool-call pair in the
/// projected context. `log` is the log without the marker: the
/// marker lands as the next line, so the target must point at an
/// existing line.
///
/// The guard is the write-boundary form of the settled-target rule
/// (section 7) and of the pair-stranding invariant (P4): the
/// projected context of `T_eff`, over the existing rewind chain,
/// must keep every tool call paired with its result, both sides.
/// Malformed earlier markers degrade out, exactly as the
/// projection does.
///
/// A refusal is final and loud: the write path refuses the append
/// and names the stranded pair. The read-side P4 backstop in
/// `bin/assemble` then covers only hand-edited logs.
pub fn guard_rewind(
    log: &[Value],
    target: u64,
    before: bool,
) -> Result<(), RewindRejection> {
    let log_len = log.len();
    let target_seq = target as usize;
    if target == 0 || target_seq > log_len {
        return Err(RewindRejection::TargetOutOfRange {
            target,
            log_len,
        });
    }
    // The existing chain: malformed markers degrade out, as in the
    // projection.
    let markers: Vec<RewindRef> = log
        .iter()
        .enumerate()
        .filter_map(|(i, ev)| parse_rewind_event(ev, i + 1))
        .collect();
    let t_eff = if before { target_seq - 1 } else { target_seq };
    let ranges = active_ranges(t_eff, &markers);
    // The projected context: the kept lines, in seq order.
    let kept: Vec<&Value> = log
        .iter()
        .enumerate()
        .filter(|(i, _)| seq_in_ranges(i + 1, &ranges))
        .map(|(_, ev)| ev)
        .collect();
    let call_ids: std::collections::HashSet<String> =
        kept.iter().flat_map(|ev| call_ids_of(ev)).collect();
    let result_ids: std::collections::HashSet<String> =
        kept.iter().filter_map(|ev| result_id_of(ev)).collect();
    for ev in &kept {
        for id in call_ids_of(ev) {
            if !result_ids.contains(&id) {
                return Err(RewindRejection::StrandedCall {
                    call_id: id,
                    target,
                    before,
                });
            }
        }
    }
    for ev in &kept {
        if let Some(id) = result_id_of(ev) {
            if !call_ids.contains(&id) {
                return Err(RewindRejection::StrandedResult {
                    result_id: id,
                    target,
                    before,
                });
            }
        }
    }
    Ok(())
}

/// The inline tool-call ids of one `assistant_message` line.
fn call_ids_of(ev: &Value) -> Vec<String> {
    match ev.get("type").and_then(|t| t.as_str()) {
        Some("assistant_message") => ev
            .get("tool_calls")
            .and_then(|c| c.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|tc| tc.get("id").and_then(|i| i.as_str()))
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// The call id of one `tool_result` line.
fn result_id_of(ev: &Value) -> Option<String> {
    match ev.get("type").and_then(|t| t.as_str()) {
        Some("tool_result") => ev
            .get("id")
            .and_then(|i| i.as_str())
            .map(String::from),
        _ => None,
    }
}

/// The outcome of a refused or failed marker append.
#[derive(Debug)]
pub enum AppendRejection {
    /// The P8 guard refused the marker. The log stays untouched.
    Guard(RewindRejection),
    /// The line is not a valid `rewind` marker.
    Malformed(String),
    /// The log could not be read or written.
    Io(std::io::Error),
}

impl std::fmt::Display for AppendRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppendRejection::Guard(rej) => write!(f, "{rej}"),
            AppendRejection::Malformed(msg) => {
                write!(f, "the marker was refused: {msg}")
            }
            AppendRejection::Io(e) => write!(f, "the marker append failed: {e}"),
        }
    }
}

/// The one kernel entry point for appending a `rewind` marker
/// (P8). The P8 guard runs against the current log. Only a valid
/// marker reaches the log. The commit takes the FT-005 `LogLine`
/// path. A `Guard` refusal leaves the log untouched and names the
/// cause. Every marker writer goes through this call: `rushi-log`
/// in this repo, the Tier-2 TUI append path, and any later stage.
pub fn append_rewind(
    line: &str,
    log_path: &std::path::Path,
) -> Result<(), AppendRejection> {
    // The typed vocabulary is the shape validator. The marker must
    // parse into the `rewind` event with its required fields.
    let marker = match crate::event::parse_event(line) {
        Ok(crate::event::Event::Rewind(m)) => m,
        Ok(_) => {
            return Err(AppendRejection::Malformed(
                "the append path takes only `rewind` markers".into(),
            ))
        }
        Err(e) => {
            return Err(AppendRejection::Malformed(format!(
                "the line is not a valid event: {e}"
            )))
        }
    };
    // The existing log: a missing log is an empty log.
    let text = match crate::logstream::read_log_text(log_path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(AppendRejection::Io(e)),
    };
    let existing: Vec<Value> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let before = marker.mode == crate::event::RewindMode::Before;
    if let Err(rej) = guard_rewind(&existing, marker.target_seq, before) {
        return Err(AppendRejection::Guard(rej));
    }
    crate::logline::LogLine::from_json(line)
        .commit(log_path)
        .map_err(AppendRejection::Io)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(seq: usize, target: usize, before: bool) -> RewindRef {
        RewindRef {
            seq,
            target,
            before,
        }
    }

    #[test]
    fn no_rewinds_is_the_full_prefix() {
        assert_eq!(active_ranges(5, &[]), vec![(1, 5)]);
        assert_eq!(active_ranges(0, &[]), Vec::<(usize, usize)>::new());
    }

    #[test]
    fn on_mode_includes_the_target() {
        // Log 1..3, rewind at 4 to 3 (on): the span (3,4) is empty,
        // the context is 1..3 plus everything after the rewind.
        let rewinds = vec![r(4, 3, false)];
        assert_eq!(active_ranges(6, &rewinds), vec![(1, 3), (5, 6)]);
    }

    #[test]
    fn before_mode_excludes_the_target() {
        // Log 1..5, rewind at 6 to the user message 4 (before): the
        // context ends at 3; the span 4..5 is masked.
        let rewinds = vec![r(6, 4, true)];
        assert_eq!(active_ranges(7, &rewinds), vec![(1, 3), (7, 7)]);
    }

    #[test]
    fn before_mode_at_seq_one_covers_nothing() {
        let rewinds = vec![r(3, 1, true)];
        assert_eq!(active_ranges(4, &rewinds), vec![(4, 4)]);
    }

    /// The counter-example of the depth-2 fork (docs/rewind-fork-
    /// design.md P2): branch B is masked when the log forks A->B,
    /// then B->A', then rewinds inside A'. The single-gap rule keeps
    /// B; the recursion masks it.
    #[test]
    fn nested_forks_mask_the_abandoned_branch() {
        // 1..3 = A, rewind(S=4,T=3) forks B (5..6),
        // rewind(S=7,T=3) forks A' (8..9), rewind(S=10,T=9)
        // continues A'.
        let rewinds = vec![r(4, 3, false), r(7, 3, false), r(10, 9, false)];
        let ranges = active_ranges(10, &rewinds);
        // A (1..3) and A' (8..9) are active; B (5..6) is masked.
        assert_eq!(ranges, vec![(1, 3), (8, 9)]);
        assert!(!seq_in_ranges(5, &ranges), "branch B must be masked");
        assert!(!seq_in_ranges(6, &ranges), "branch B must be masked");
        assert!(seq_in_ranges(3, &ranges));
        assert!(seq_in_ranges(8, &ranges));
        assert!(seq_in_ranges(9, &ranges));
    }

    /// Re-entering a forked branch (docs/rewind-fork-design.md P3):
    /// a rewind to the tail of B rebuilds B's full active path, with
    /// A' out of the context.
    #[test]
    fn reentering_a_branch_rebuilds_its_path() {
        // 1..3 = A, rewind(S=4,T=3) forks B (5..6),
        // rewind(S=7,T=3) forks A' (8..9), rewind(S=10,T=6)
        // re-enters B at its tail.
        let rewinds = vec![r(4, 3, false), r(7, 3, false), r(10, 6, false)];
        let ranges = active_ranges(12, &rewinds);
        // A (1..3) and B (5..6) are active; A' (8..9) is masked.
        assert_eq!(ranges, vec![(1, 3), (5, 6), (11, 12)]);
        assert!(!seq_in_ranges(8, &ranges), "branch A' must be masked");
        assert!(!seq_in_ranges(9, &ranges), "branch A' must be masked");
        assert!(seq_in_ranges(5, &ranges));
        assert!(seq_in_ranges(6, &ranges));
    }

    /// A rewind that lands exactly on its event's own prefix: the
    /// continuation range is empty, the context is the target path.
    #[test]
    fn rewind_at_log_end_has_no_continuation() {
        let rewinds = vec![r(5, 2, false)];
        assert_eq!(active_ranges(5, &rewinds), vec![(1, 2)]);
    }

    /// Rewind events later than the prefix end are not in play: the
    /// prefix context is frozen at the point it was reached.
    #[test]
    fn rewinds_after_the_prefix_do_not_apply() {
        let rewinds = vec![r(4, 1, false), r(9, 3, false)];
        // Prefix 5: only the first rewind has happened; the one at
        // seq 9 is not in play.
        assert_eq!(active_ranges(5, &rewinds), vec![(1, 1), (5, 5)]);
        // Prefix 9: the chain is 9 -> 3, and no rewind precedes 3,
        // so the base case [1..3] stands. The rewind at 4 sits in
        // the span (3, 9) that the fork at 9 abandoned; it is not
        // in the chain, and the log ends exactly at the marker, so
        // there is no continuation range.
        assert_eq!(active_ranges(9, &rewinds), vec![(1, 3)]);
    }

    /// A three-rewind chain: each fork nests inside the previous
    /// branch, so the active path follows the targets 10 -> 7 -> 4.
    /// The recursion composes all three masks; a single-gap rule
    /// (at any depth) masks only the outermost span.
    #[test]
    fn deep_chains_follow_the_nested_targets() {
        // Log layout (seq = line number):
        //   1,2 = A; rewind(3,2) forks B at 4;
        //   rewind(6,4) forks C at 7; rewind(9,7) forks D at 10.
        let rewinds = vec![r(3, 2, false), r(6, 4, false), r(9, 7, false)];
        // Chain at 10: 10 -> 7 (rewind 9) -> 4 (rewind 6) -> 2
        // (rewind 3) -> base [1..2].
        let ranges = active_ranges(10, &rewinds);
        assert_eq!(ranges, vec![(1, 2), (4, 4), (7, 7), (10, 10)]);
        // Each abandoned branch tail stays masked: seq 5 (B) and
        // seq 8 (C) are out, the live tail of every branch is in.
        assert!(!seq_in_ranges(5, &ranges));
        assert!(!seq_in_ranges(8, &ranges));
        assert!(seq_in_ranges(4, &ranges));
        assert!(seq_in_ranges(7, &ranges));
        assert!(seq_in_ranges(10, &ranges));
    }

    #[test]
    fn parse_requires_an_earlier_target() {
        let ok: Value = serde_json::json!({
            "v": 1, "type": "rewind", "ts": "t", "target_seq": 3, "mode": "on"
        });
        assert_eq!(
            parse_rewind_event(&ok, 5).unwrap(),
            RewindRef {
                seq: 5,
                target: 3,
                before: false
            }
        );
        // A target at or after the event's own seq is rejected.
        assert!(parse_rewind_event(&ok, 3).is_none(), "self-target rejected");
        assert!(
            parse_rewind_event(&ok, 2).is_none(),
            "future-target rejected"
        );
        // A missing mode is rejected.
        let no_mode: Value = serde_json::json!({
            "v": 1, "type": "rewind", "ts": "t", "target_seq": 3
        });
        assert!(parse_rewind_event(&no_mode, 5).is_none());
        // A zero target is below the 1-based log sequence.
        let zero: Value = serde_json::json!({
            "v": 1, "type": "rewind", "ts": "t", "target_seq": 0, "mode": "on"
        });
        assert!(parse_rewind_event(&zero, 5).is_none());
        // A non-rewind event is not a rewind.
        let other: Value = serde_json::json!({
            "v": 1, "type": "error", "ts": "t", "message": "x"
        });
        assert!(parse_rewind_event(&other, 5).is_none());
    }

    /// Three nested rewinds compose: the chain 13 -> 8 -> 5 -> 3
    /// carries the base prefix forward, every abandoned
    /// intermediate span stays out, and a prefix that lands inside
    /// the chain sees its own branch.
    #[test]
    fn depth_three_forks_compose_through_the_chain() {
        // 1..3 base, rewind(4,3) forks B at 5, rewind(8,5) forks
        // B' (9..10), rewind(11,8) forks C (12..13). The marker
        // seqs (4, 8, 11) project to nothing: the continuation of a
        // fork starts at the marker's successor.
        let rewinds = vec![r(4, 3, false), r(8, 5, false), r(11, 8, false)];
        // Chain: 13 -> 8 (marker 11) -> 5 (marker 8) -> 3 (marker
        // 4) -> base [1..3]. B' (9..10) is masked by marker 11's
        // span (8, 11); B (5) rides as the continuation of marker
        // 4's fork.
        let ranges = active_ranges(13, &rewinds);
        assert_eq!(ranges, vec![(1, 3), (5, 5), (12, 13)]);
        assert!(seq_in_ranges(5, &ranges), "B (5) is on the chain");
        assert!(!seq_in_ranges(9, &ranges), "B' (9) must be masked");
        assert!(!seq_in_ranges(10, &ranges), "B' (10) must be masked");
        // A prefix inside the chain: 10 ends in the B' branch. The
        // chain is 10 -> 5 (marker 8) -> 3 (marker 4): B and B'
        // ride, nothing past 10 exists yet.
        assert_eq!(active_ranges(10, &rewinds), vec![(1, 3), (5, 5), (9, 10)]);
    }

    // ------------------------------------------------------------------
    // P8: the kernel guard at the write boundary
    // ------------------------------------------------------------------

    fn g_user(content: &str) -> Value {
        serde_json::json!({"v":1,"type":"user_message","ts":"t","content":content})
    }

    fn g_assistant(calls: &[&str]) -> Value {
        let tool_calls: Vec<Value> = calls
            .iter()
            .map(|id| {
                serde_json::json!({"id":id,"name":"bash","arguments":{"command":"c"}})
            })
            .collect();
        serde_json::json!({
            "v":1,"type":"assistant_message","ts":"t","content":"",
            "tool_calls":tool_calls,"stop_reason":"tool_calls"
        })
    }

    fn g_result(id: &str) -> Value {
        serde_json::json!({"v":1,"type":"tool_result","ts":"t","id":id,"value":{"text":"ok"},"is_error":false})
    }

    fn g_rewind(target: u64, mode: &str) -> Value {
        serde_json::json!({"v":1,"type":"rewind","ts":"t","target_seq":target,"mode":mode})
    }

    #[test]
    fn guard_rejects_a_stranded_call() {
        // user(1), asst+c1(2), res c1(3): a rewind to seq 2 (on)
        // keeps c1 without its result.
        let log = vec![g_user("task"), g_assistant(&["c1"]), g_result("c1")];
        let err = guard_rewind(&log, 2, false).unwrap_err();
        assert_eq!(
            err,
            RewindRejection::StrandedCall {
                call_id: "c1".into(),
                target: 2,
                before: false
            }
        );
    }

    #[test]
    fn guard_rejects_the_steer_mid_step_target() {
        // user(1), asst+c1(2), steer(3), res c1(4): the before-
        // mode pick of the steer masks the result and strands the
        // call (the e2e scenario E shape, refused at the write
        // boundary).
        let log = vec![g_user("task"), g_assistant(&["c1"]), g_user("steer"), g_result("c1")];
        let err = guard_rewind(&log, 3, true).unwrap_err();
        assert_eq!(
            err,
            RewindRejection::StrandedCall {
                call_id: "c1".into(),
                target: 3,
                before: true
            }
        );
    }

    #[test]
    fn guard_rejects_a_stranded_result() {
        // The orphan: the fork at 3 masks the old branch's call,
        // and the result at 4 rides the new branch alone. A
        // rewind to 4 (on) keeps a result without its call.
        let log = vec![
            g_user("task"),        // 1
            g_assistant(&["c1"]),  // 2
            g_rewind(1, "before"), // 3
            g_result("c1"),        // 4
        ];
        let err = guard_rewind(&log, 4, false).unwrap_err();
        assert_eq!(
            err,
            RewindRejection::StrandedResult {
                result_id: "c1".into(),
                target: 4,
                before: false
            }
        );
    }

    #[test]
    fn guard_accepts_the_settled_points() {
        let log = vec![
            g_user("task"),         // 1
            g_assistant(&["c1"]),   // 2
            g_result("c1"),        // 3
            g_user("more"),         // 4
        ];
        // A user message in before mode, a tool_result in on mode.
        assert_eq!(guard_rewind(&log, 1, true), Ok(()));
        assert_eq!(guard_rewind(&log, 3, false), Ok(()));
        assert_eq!(guard_rewind(&log, 4, true), Ok(()));
        // A finished assistant message (no calls) in on mode.
        let settled: Value = serde_json::json!({
            "v":1,"type":"assistant_message","ts":"t","content":"done",
            "tool_calls":[],"stop_reason":"stop"
        });
        let log2 = vec![g_user("task"), settled];
        assert_eq!(guard_rewind(&log2, 2, false), Ok(()));
    }

    #[test]
    fn guard_walks_the_fork_chain() {
        // Three nested forks (the e2e F layout): the new branch
        // keeps its own pair, so a rewind to its tail settles.
        let log = vec![
            g_user("task"),        // 1
            g_assistant(&["c1"]),  // 2
            g_result("c1"),       // 3
            g_rewind(3, "on"),     // 4  fork B
            g_assistant(&["c2"]),  // 5
            g_result("c2"),       // 6
            g_rewind(3, "on"),     // 7  fork A'
            g_assistant(&["c3"]),  // 8
            g_result("c3"),       // 9
        ];
        assert_eq!(guard_rewind(&log, 9, false), Ok(()));
    }

    #[test]
    fn guard_degrades_malformed_markers_like_the_projection() {
        // A malformed marker (target 0) degrades out of the chain,
        // so the guard must agree with the projection.
        let bad: Value =
            serde_json::json!({"v":1,"type":"rewind","ts":"t","target_seq":0,"mode":"on"});
        let log = vec![g_user("task"), bad, g_assistant(&["c1"]), g_result("c1")];
        assert_eq!(guard_rewind(&log, 4, false), Ok(()));
    }

    #[test]
    fn guard_rejects_an_out_of_range_target() {
        let log = vec![g_user("task")];
        assert_eq!(
            guard_rewind(&log, 0, false),
            Err(RewindRejection::TargetOutOfRange {
                target: 0,
                log_len: 1
            })
        );
        assert_eq!(
            guard_rewind(&log, 2, false),
            Err(RewindRejection::TargetOutOfRange {
                target: 2,
                log_len: 1
            })
        );
        let empty: Vec<Value> = Vec::new();
        assert_eq!(
            guard_rewind(&empty, 1, false),
            Err(RewindRejection::TargetOutOfRange {
                target: 1,
                log_len: 0
            })
        );
    }

    #[test]
    fn guard_rejection_messages_name_the_stranded_pair() {
        let e = RewindRejection::StrandedCall {
            call_id: "c1".into(),
            target: 2,
            before: false,
        };
        let m = e.to_string();
        assert!(m.contains("c1"), "the message names the pair: {m}");
        assert!(m.contains("strands"), "the message names the cause: {m}");
        let o = RewindRejection::TargetOutOfRange {
            target: 5,
            log_len: 3,
        };
        assert!(o.to_string().contains("out of range"));
    }

    // ------------------------------------------------------------------
    // P8: the kernel append entry point
    // ------------------------------------------------------------------

    fn g_rewind_line(target: u64, mode: &str) -> String {
        format!(
            r#"{{"v":1,"type":"rewind","ts":"t","target_seq":{target},"mode":"{mode}"}}"#
        )
    }

    fn write_log(dir: &std::path::Path, lines: &[&str]) -> std::path::PathBuf {
        let log = dir.join("events.jsonl");
        let mut text = lines.join("\n");
        if !lines.is_empty() {
            text.push('\n');
        }
        std::fs::write(&log, text).unwrap();
        log
    }

    #[test]
    fn append_rewind_appends_a_settled_marker() {
        let dir = tempfile::tempdir().unwrap();
        let log = write_log(
            dir.path(),
            &[
                r#"{"v":1,"type":"user_message","ts":"t","content":"task"}"#,
                r#"{"v":1,"type":"assistant_message","ts":"t","content":"","tool_calls":[{"id":"c1","name":"bash","arguments":{}}],"stop_reason":"tool_calls"}"#,
                r#"{"v":1,"type":"tool_result","ts":"t","id":"c1","value":{"text":"ok"},"is_error":false}"#,
            ],
        );
        append_rewind(&g_rewind_line(3, "on"), &log).unwrap();
        let text = crate::logstream::read_log_text(&log).unwrap();
        assert_eq!(text.lines().count(), 4, "the marker is the fourth line");
    }

    #[test]
    fn append_rewind_refuses_a_stranded_marker_and_keeps_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let log = write_log(
            dir.path(),
            &[
                r#"{"v":1,"type":"user_message","ts":"t","content":"task"}"#,
                r#"{"v":1,"type":"assistant_message","ts":"t","content":"","tool_calls":[{"id":"c1","name":"bash","arguments":{}}],"stop_reason":"tool_calls"}"#,
            ],
        );
        let before = std::fs::read(&log).unwrap();
        let err = append_rewind(&g_rewind_line(2, "on"), &log).unwrap_err();
        match err {
            AppendRejection::Guard(RewindRejection::StrandedCall { call_id, .. }) => {
                assert_eq!(call_id, "c1");
            }
            other => panic!("expected the guard refusal, got {other:?}"),
        }
        let after = std::fs::read(&log).unwrap();
        assert_eq!(before, after, "the log stays untouched");
    }

    #[test]
    fn append_rewind_refuses_a_non_rewind_line() {
        let dir = tempfile::tempdir().unwrap();
        let log = write_log(dir.path(), &[]);
        let err = append_rewind(
            r#"{"v":1,"type":"user_message","ts":"t","content":"x"}"#,
            &log,
        )
        .unwrap_err();
        assert!(matches!(err, AppendRejection::Malformed(_)));
        assert!(!log.exists() || std::fs::read(&log).unwrap().is_empty());
    }

    #[test]
    fn append_rewind_refuses_a_target_past_the_log_end() {
        let dir = tempfile::tempdir().unwrap();
        let log = write_log(
            dir.path(),
            &[r#"{"v":1,"type":"user_message","ts":"t","content":"task"}"#],
        );
        let err = append_rewind(&g_rewind_line(2, "on"), &log).unwrap_err();
        match err {
            AppendRejection::Guard(RewindRejection::TargetOutOfRange { target, .. }) => {
                assert_eq!(target, 2);
            }
            other => panic!("expected the range refusal, got {other:?}"),
        }
    }
}
