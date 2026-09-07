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
    Some(RewindRef { seq, target, before })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn r(seq: usize, target: usize, before: bool) -> RewindRef {
        RewindRef { seq, target, before }
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
        let rewinds = vec![
            r(3, 2, false),
            r(6, 4, false),
            r(9, 7, false),
        ];
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
            RewindRef { seq: 5, target: 3, before: false }
        );
        // A target at or after the event's own seq is rejected.
        assert!(parse_rewind_event(&ok, 3).is_none(), "self-target rejected");
        assert!(parse_rewind_event(&ok, 2).is_none(), "future-target rejected");
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
}
