//! `compact_math` — pure trigger math and cut walk for auto-compaction.
//!
//! This module is I/O-free. It operates on projected event values and
//! token measurements. The harness loop and the `compact` binary both
//! call into it; no persistent state file is kept (Phase 2 removes
//! the Phase 1 `compact.json` sticky state).
//!
//! Trigger model (docs/auto-compact-plan.md, pi `estimateContextTokens`):
//! - The current context size is the last measured `usage.input_tokens`
//!   from the log, plus a chars/4 estimate of trailing messages that
//!   arrived after that usage measurement.
//! - If the total exceeds the trigger level (`budget - reserve`), the
//!   compact fires before the next model call.
//! - The cut walk walks backward from the tail of the kept region,
//!   accumulating chars/4 per-event estimates, until `keep_tokens`
//!   is reached. The cut snaps to a valid boundary (user or assistant
//!   message, never a tool result).

use serde_json::Value;

/// A projected event for token estimation.
#[derive(Clone, Debug, PartialEq)]
pub enum Ev {
    User {
        text: String,
    },
    Assistant {
        text: String,
        calls: Vec<Call>,
        reasoning_chars: u64,
    },
    Result {
        call_id: String,
        chars: u64,
    },
}

/// One tool call inside an assistant message.
#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    pub name: String,
    pub args_str: String,
}

/// Caps for the estimator. `text: None` means no per-event cap.
#[derive(Clone, Copy, Debug)]
pub struct Caps {
    pub text: Option<u64>,
    /// Characters-per-token ratio used by `est_tokens`. Defaults to 4
    /// (rough heuristic for English prose). Code-heavy content often
    /// runs 3–3.5, so a calibratable value lets the user tighten the
    /// estimate. Must be >= 1.
    pub chars_per_token: u64,
}

impl Default for Caps {
    fn default() -> Self {
        Self {
            text: None,
            chars_per_token: 4,
        }
    }
}

/// Estimate the token count for one projected event using the
/// configured chars-per-token ratio.
pub fn est_tokens(ev: &Ev, caps: &Caps) -> u64 {
    let chars: u64 = match ev {
        Ev::User { text } => text.chars().count() as u64,
        Ev::Assistant {
            text,
            calls,
            reasoning_chars,
        } => {
            let text_chars = if let Some(cap) = caps.text {
                std::cmp::min(text.chars().count() as u64, cap)
            } else {
                text.chars().count() as u64
            };
            let call_chars: u64 = calls
                .iter()
                .map(|c| c.args_str.chars().count() as u64)
                .sum();
            let call_cap = if let Some(cap) = caps.text {
                std::cmp::min(call_chars, cap)
            } else {
                call_chars
            };
            text_chars + call_cap + reasoning_chars
        }
        Ev::Result { chars, .. } => *chars,
    };
    chars / caps.chars_per_token.max(1)
}

/// Estimate the context size in tokens for the current context.
///
/// `measured_input` is the last `usage.input_tokens` from the log.
/// `trailing_events` are the events logged after that usage reading
/// (the messages that arrived since the last model call returned).
///
/// This mirrors pi's `estimateContextTokens`: the measured value is
/// authoritative, the trailing estimate fills the gap.
pub fn estimate_context(
    measured_input: u64,
    trailing_events: &[Ev],
    caps: &Caps,
) -> u64 {
    let trailing: u64 = trailing_events.iter().map(|e| est_tokens(e, caps)).sum();
    measured_input + trailing
}

/// The trigger check: does the context size exceed the trigger level?
///
/// `trigger_level` = `context_budget_tokens - compact_reserve_tokens`.
/// `current` is the result of `estimate_context`.
pub fn trigger_fired(current: u64, trigger_level: u64) -> bool {
    current > trigger_level
}

/// The trigger level from a base: `base - reserve`. A zero reserve
/// inverts the trigger; it clamps to one below the base.
pub fn trigger_level_for(base: u64, reserve: u64) -> u64 {
    if reserve == 0 {
        base.saturating_sub(1)
    } else {
        base.saturating_sub(reserve)
    }
}

/// The full-form context estimate: the chars/4 sum of the projected
/// events. The measured readings read the (clamped) request. In the
/// trim form that reading is shrunken. The full form is the context
/// the compact actually replaces. Use it as the trigger estimate when
/// the trigger base is the full context budget.
pub fn full_form_estimate(evs: &[Ev], caps: &Caps) -> u64 {
    evs.iter().map(|e| est_tokens(e, caps)).sum()
}

/// Compute a context estimate from raw log events.
///
/// Applies rewind masking so only the active-path events contribute.
/// Finds the last `compaction_summary` boundary, then:
///
/// - Boundary present: full-form estimate of the kept region plus the
///   summary framing cost, with a 25 % safety margin (the margin
///   compensates for chars-per-token underestimation on code-heavy
///   content).
/// - No boundary: the last measured `input_tokens` + `output_tokens`
///   anchor plus the full-form estimate of trailing events. No margin:
///   the measured anchor is the provider's own count for the same
///   request shape.
///
/// Returns 0 when there are no measured-usage events on the active
/// path (nothing to anchor on).
pub fn estimate_from_events(events: &[Value], cpts: u64) -> u64 {
    let cpts = cpts.max(1);
    let caps = Caps { text: None, chars_per_token: cpts };

    let rewinds: Vec<crate::rewind::RewindRef> = events
        .iter()
        .enumerate()
        .filter_map(|(i, v)| crate::rewind::parse_rewind_event(v, i + 1))
        .collect();
    let active = crate::rewind::active_ranges(events.len(), &rewinds);
    // Keep the 1-based positional index alongside each active event so
    // the boundary path can filter by first_kept_seq rather than by
    // log-order position after the marker (the kept region sits *before*
    // the marker in log order but still counts toward the live context).
    let active_events: Vec<(usize, &Value)> = events
        .iter()
        .enumerate()
        .filter(|(i, _)| crate::rewind::seq_in_ranges(i + 1, &active))
        .map(|(i, v)| (i + 1, v))
        .collect();

    let boundary = active_events.iter().rposition(|(_, v)| {
        v.get("type").and_then(|t| t.as_str()) == Some("compaction_summary")
    });

    if let Some(b) = boundary {
        let marker = active_events[b].1;
        let first_kept: u64 = marker
            .get("first_kept_seq")
            .and_then(|v| v.as_u64())
            .unwrap_or(1);
        // Kept region = every active event whose seq is >= first_kept_seq.
        // This covers both the original kept events (before the marker in
        // log order) and any events appended after the marker.
        let kept: Vec<(usize, &Value)> = active_events
            .iter()
            .filter(|(pos, _)| *pos as u64 >= first_kept)
            .cloned()
            .collect();

        // Anchor on the last measured reading in the kept region when
        // available.  The provider's own token count is far more accurate
        // than a chars/cpts estimate, especially for code-heavy content
        // where the real chars-per-token ratio is well under 4
        // (docs/auto-compact-plan.md section 9.1).
        let last_meas_idx = kept.iter().rposition(|(_, v)| {
            v.get("type").and_then(|t| t.as_str()) == Some("assistant_message")
                && v.get("usage")
                    .and_then(|u| u.get("input_tokens"))
                    .and_then(|i| i.as_u64())
                    .is_some()
        });

        match last_meas_idx {
            Some(idx) => {
                let (_, v) = &kept[idx];
                let usage = &v["usage"];
                let measured_input = usage
                    .get("input_tokens")
                    .and_then(|i| i.as_u64())
                    .unwrap_or(0);
                let measured_output = usage
                    .get("output_tokens")
                    .and_then(|o| o.as_u64())
                    .unwrap_or(0);
                let trailing: u64 = kept[idx + 1..]
                    .iter()
                    .map(|(_, v)| est_tokens(&project_event(v), &caps))
                    .sum();
                measured_input + measured_output + trailing
            }
            None => {
                // No measured reading in the kept region: fall back to
                // the chars/cpts full-form estimate with a 25 % safety
                // margin for code-heavy content.
                let evs: Vec<Ev> = kept.iter().map(|(_, v)| project_event(v)).collect();
                let raw = full_form_estimate(&evs, &caps);
                let mut est = raw;
                if let Some(s) = marker.get("summary").and_then(|s| s.as_str()) {
                    est += s.chars().count() as u64 / cpts;
                }
                est * 5 / 4
            }
        }
    } else {
        let last_meas_idx = active_events.iter().rposition(|(_, v)| {
            v.get("type").and_then(|t| t.as_str()) == Some("assistant_message")
                && v.get("usage")
                    .and_then(|u| u.get("input_tokens"))
                    .and_then(|i| i.as_u64())
                    .is_some()
        });
        let Some(idx) = last_meas_idx else {
            return 0;
        };
        let usage = &active_events[idx].1["usage"];
        let measured_input = usage
            .get("input_tokens")
            .and_then(|i| i.as_u64())
            .unwrap_or(0);
        let measured_output = usage
            .get("output_tokens")
            .and_then(|o| o.as_u64())
            .unwrap_or(0);
        let trailing: u64 = active_events[idx + 1..]
            .iter()
            .map(|(_, v)| est_tokens(&project_event(v), &caps))
            .sum();
        measured_input + measured_output + trailing
    }
}

/// The backward cut walk. Given the kept events (from the boundary to
/// the end) and the keep token budget, find the 0-based index of the
/// first kept event. The cut snaps to a step group boundary: a user
/// message is its own group; an assistant message starts the group
/// with its tool results. A cut never lands between a call and its
/// result. If the cut would leave a user message as the last event of
/// the old region, that user is pulled into the kept region.
///
/// Returns 0 when the keep window covers the entire kept region.
pub fn find_cut(kept: &[Ev], keep_tokens: u64, caps: &Caps) -> usize {
    let total: u64 = kept.iter().map(|e| est_tokens(e, caps)).sum();
    if total <= keep_tokens {
        return 0;
    }
    let mut acc = 0u64;
    let mut cut = kept.len();
    for ev in kept.iter().rev() {
        acc += est_tokens(ev, caps);
        cut -= 1;
        if acc >= keep_tokens {
            break;
        }
    }
    // Snap the cut to a step group boundary: a user message is its
    // own group; an assistant message starts the group that carries
    // its tool results. The cut never lands between a call and its
    // result (docs/auto-compact-plan.md "Cut point").
    while cut > 0 {
        match &kept[cut] {
            Ev::Assistant { .. } | Ev::User { .. } => break,
            Ev::Result { .. } => {
                cut -= 1;
            }
        }
    }
    // Never orphan a user message: if the cut leaves a user message
    // as the last event of the old region, pull it into the kept
    // region (docs/auto-compact-plan.md "Cut point").
    if cut > 0 && matches!(&kept[cut - 1], Ev::User { .. }) {
        cut -= 1;
    }
    cut
}

/// The estimated tokens after the compact: the handoff document plus
/// the kept events in the full form.
pub fn est_tokens_after(kept: &[Ev], handoff_doc: &str) -> u64 {
    est_tokens_after_with_caps(kept, handoff_doc, &Caps::default())
}

/// Same as `est_tokens_after` but with an explicit `Caps` so callers
/// can override the chars-per-token ratio.
pub fn est_tokens_after_with_caps(kept: &[Ev], handoff_doc: &str, caps: &Caps) -> u64 {
    let full_caps = Caps {
        text: None,
        chars_per_token: caps.chars_per_token,
    };
    let framing: u64 =
        handoff_doc.chars().count() as u64 / full_caps.chars_per_token.max(1) + 64;
    let mut total = framing;
    for ev in kept.iter() {
        total += est_tokens(ev, &full_caps);
    }
    total
}

/// The post-compact sanity check: compare the projected post-compact
/// size to the trigger level. If it still meets or exceeds the trigger,
/// the compact is not helping and a warning is returned.
pub fn post_compact_sanity(
    kept: &[Ev],
    handoff_doc: &str,
    trigger_level: u64,
) -> bool {
    est_tokens_after(kept, handoff_doc) > trigger_level
}

/// Project a raw event JSON value into the `Ev` estimator shape.
/// Marker types (`compaction_*`, `ext_status`, `context_exhausted`)
/// project to nothing (zero tokens).
pub fn project_event(v: &Value) -> Ev {
    let t = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
    match t {
        "user_message" => Ev::User {
            text: v.get("content")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string(),
        },
        "assistant_message" => {
            let text = v.get("content")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let reasoning_chars: u64 = v
                .get("reasoning")
                .and_then(|r| r.as_array())
                .map(|r| r.iter().map(|b| b.to_string().chars().count() as u64).sum())
                .unwrap_or(0);
            let calls: Vec<Call> = v
                .get("tool_calls")
                .and_then(|c| c.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|c| {
                            Some(Call {
                                name: c.get("name")?.as_str()?.to_string(),
                                args_str: c
                                    .get("arguments")
                                    .map(|a| a.to_string())
                                    .unwrap_or_else(|| "{}".to_string()),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            Ev::Assistant {
                text,
                calls,
                reasoning_chars,
            }
        }
        "tool_result" => {
            let base_chars: u64 = v.get("value")
                .and_then(|o| o.get("text"))
                .and_then(|s| s.as_str())
                .map(|s| s.chars().count() as u64)
                .unwrap_or(0);
            // Flat per-image token estimate (mirrors pi's ESTIMATED_IMAGE_CHARS).
            let image_chars: u64 = v
                .get("value")
                .and_then(|o| o.get("details"))
                .and_then(|d| d.get("type"))
                .and_then(|t| t.as_str())
                .eq(&Some("image")) as u64
                * 4800;
            Ev::Result {
                call_id: v.get("id")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string(),
                chars: base_chars + image_chars,
            }
        }
        _ => Ev::User {
            text: String::new(),
        },
    }
}

/// Extract file operation names from assistant message tool calls.
/// Returns `(read_files, modified_files)` with dedup.
pub fn extract_file_ops(events: &[Value]) -> (Vec<String>, Vec<String>) {
    use std::collections::HashSet;
    let mut reads: Vec<String> = Vec::new();
    let mut modified: Vec<String> = Vec::new();
    for e in events {
        if e.get("type").and_then(|t| t.as_str()) != Some("assistant_message") {
            continue;
        }
        let calls = match e.get("tool_calls").and_then(|c| c.as_array()) {
            Some(c) => c,
            None => continue,
        };
        for call in calls {
            let name = call.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let path = call
                .get("arguments")
                .and_then(|a| a.get("file_path"))
                .and_then(|f| f.as_str())
                .map(str::to_string);
            if name == "read" {
                if let Some(p) = path {
                    reads.push(p);
                }
            } else if name == "write" || name == "edit" {
                if let Some(p) = path {
                    modified.push(p);
                }
            }
        }
    }
    fn dedup(v: &mut Vec<String>) {
        let mut seen = HashSet::new();
        v.retain(|s| seen.insert(s.clone()));
    }
    dedup(&mut reads);
    dedup(&mut modified);
    (reads, modified)
}

/// Handoff versioning metadata (docs/handoff-versioning-design.md).
///
/// Given the raw event log (in log order), derive:
/// - `version`: the 1-based ordinal of the *next* compaction summary
///   (i.e. the count of existing `compaction_summary` events plus one).
/// - `parent_version`: the version of the most recent `compaction_summary`
///   marker in the log (0 when there is none yet).
/// - `diverge_seq`: the `first_kept_seq` of that parent boundary (0 when
///   there is no parent). This is the log seq at which this new handoff
///   diverges from its predecessor — the DAG link that gives each
///   versioned handoff its identity.
pub fn handoff_version_meta(events: &[Value]) -> (u64, u64, u64) {
    let mut count: u64 = 0;
    let mut parent_first_kept: u64 = 0;
    for v in events {
        if v.get("type").and_then(|t| t.as_str()) == Some("compaction_summary") {
            count += 1;
            // Track the most recent boundary's first_kept_seq.
            if let Some(fk) = v
                .get("first_kept_seq")
                .and_then(|f| f.as_u64())
            {
                parent_first_kept = fk;
            }
        }
    }
    let version = count + 1;
    let parent_version = count;
    (version, parent_version, parent_first_kept)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev_user(t: &str) -> Ev {
        Ev::User {
            text: t.to_string(),
        }
    }
    fn ev_asst(text: &str) -> Ev {
        Ev::Assistant {
            text: text.to_string(),
            calls: vec![],
            reasoning_chars: 0,
        }
    }
    fn ev_res(call: &str, body: &str) -> Ev {
        Ev::Result {
            call_id: call.to_string(),
            chars: body.chars().count() as u64,
        }
    }

    /// Build a `serde_json::Value` for a `user_message` event.
    fn jv_user(seq: u64, content: &str) -> Value {
        serde_json::json!({
            "v": 1,
            "type": "user_message",
            "ts": "t",
            "seq": seq,
            "content": content,
        })
    }

    /// Build a `serde_json::Value` for an `assistant_message` event with
    /// measured usage.
    fn jv_assistant(seq: u64, content: &str, input_tokens: u64, output_tokens: u64) -> Value {
        serde_json::json!({
            "v": 1,
            "type": "assistant_message",
            "ts": "t",
            "seq": seq,
            "content": content,
            "reasoning": [],
            "tool_calls": [],
            "usage": { "input_tokens": input_tokens, "output_tokens": output_tokens },
        })
    }

    /// Build a `serde_json::Value` for a `tool_result` event.
    fn jv_result(seq: u64, id: &str, text: &str) -> Value {
        serde_json::json!({
            "v": 1,
            "type": "tool_result",
            "ts": "t",
            "seq": seq,
            "id": id,
            "value": { "text": text },
            "is_error": false,
        })
    }

    /// Build a `serde_json::Value` for a `rewind` event.
    fn jv_rewind(seq: u64, target: u64, mode: &str) -> Value {
        serde_json::json!({
            "v": 1,
            "type": "rewind",
            "ts": "t",
            "seq": seq,
            "target_seq": target,
            "mode": mode,
        })
    }

    /// Build a `serde_json::Value` for a `compaction_summary` event.
    fn jv_compaction(seq: u64, summary: &str, first_kept: u64) -> Value {
        serde_json::json!({
            "v": 1,
            "type": "compaction_summary",
            "ts": "t",
            "seq": seq,
            "summary": summary,
            "first_kept_seq": first_kept,
            "reason": "threshold",
        })
    }

    #[test]
    fn est_tokens_mirrors_the_assemble_estimator() {
        let caps = Caps {
            text: Some(100),
            chars_per_token: 4,
        };
        assert_eq!(est_tokens(&ev_user(&"a".repeat(400)), &caps), 100);
        let big = "b".repeat(400);
        let ev = Ev::Assistant {
            text: big.clone(),
            calls: vec![Call {
                name: "bash".to_string(),
                args_str: big,
            }],
            reasoning_chars: 0,
        };
        assert_eq!(est_tokens(&ev, &caps), 50);
        let ev = Ev::Assistant {
            text: "x".to_string(),
            calls: vec![],
            reasoning_chars: 1000,
        };
        assert_eq!(est_tokens(&ev, &caps), 250);
        // Tool results are no longer capped: full char count is used.
        assert_eq!(est_tokens(&ev_res("1", &"c".repeat(4000)), &caps), 1000);
        let full = Caps::default();
        assert_eq!(est_tokens(&ev_res("1", &"c".repeat(4000)), &full), 1000);
    }

    #[test]
    fn cut_snaps_to_the_group_start() {
        let kept = vec![
            ev_user("task one"),
            ev_asst("step one"),
            ev_res("1", &"x".repeat(4000)),
            ev_user("task two"),
            ev_asst("step two"),
            ev_res("2", &"y".repeat(4000)),
        ];
        let caps = Caps {
            text: None,
            chars_per_token: 4,
        };
        let cut = find_cut(&kept, 150, &caps);
        assert_eq!(cut, 3, "the cut sits at the second user");
    }

    #[test]
    fn cut_pulls_in_the_orphan_user() {
        let kept = vec![
            ev_user("task"),
            ev_asst("step one"),
            ev_res("1", &"x".repeat(4000)),
        ];
        let caps = Caps {
            text: None,
            chars_per_token: 4,
        };
        let cut = find_cut(&kept, 10, &caps);
        assert_eq!(cut, 0, "the orphan user sits behind the cut");
    }

    #[test]
    fn cut_at_zero_is_the_empty_region() {
        let kept = vec![ev_user("task"), ev_asst("step one")];
        let caps = Caps::default();
        assert_eq!(find_cut(&kept, 1_000_000, &caps), 0);
    }

    #[test]
    fn trigger_fires_above_level() {
        assert!(trigger_fired(1000, 999));
        assert!(!trigger_fired(999, 1000));
        assert!(!trigger_fired(1000, 1000));
    }

    #[test]
    fn trigger_level_for_subtracts_the_reserve() {
        assert_eq!(trigger_level_for(229376, 16384), 212992);
        assert_eq!(trigger_level_for(262144, 16384), 245760);
    }

    #[test]
    fn trigger_level_for_a_zero_reserve_clamps() {
        assert_eq!(trigger_level_for(100, 0), 99);
        assert_eq!(trigger_level_for(1, 0), 0);
    }



    #[test]
    fn full_form_estimate_sums_the_projected_events() {
        let caps = Caps::default();
        let evs = vec![
            ev_user(&"a".repeat(400)),
            ev_res("1", &"x".repeat(4000)),
        ];
        assert_eq!(full_form_estimate(&evs, &caps), 100 + 1000);
        assert_eq!(full_form_estimate(&[], &caps), 0);
    }

    #[test]
    fn estimate_context_adds_trailing() {
        let caps = Caps::default();
        let trailing = vec![ev_user(&"a".repeat(800))];
        let est = estimate_context(1000, &trailing, &caps);
        assert_eq!(est, 1000 + 800 / 4);
    }

    #[test]
    fn post_compact_sanity_detects_oversized_handoff() {
        let kept = vec![ev_user(&"a".repeat(400))];
        // Handoff doc that's big enough to push past the trigger.
        let big_handoff = "x".repeat(8000);
        assert!(post_compact_sanity(&kept, &big_handoff, 100));
    }

    // ── Gap 1: rewind-aware estimation ─────────────────────────────

    #[test]
    fn estimate_excludes_masked_branch_events() {
        // Log layout:
        //   seq 1: user "task"
        //   seq 2: assistant with usage (input=2000, output=50)
        //   seq 3: tool_result 40 000 chars (= 10 000 tokens at cpts=4)
        //   seq 4: rewind target=2, mode=on → masks seqs 3 (and 4 itself)
        //   seq 5: user "next"
        let big = "x".repeat(40_000);
        let events: Vec<Value> = vec![
            jv_user(1, "task"),
            jv_assistant(2, "step", 2000, 50),
            jv_result(3, "c1", &big),
            jv_rewind(4, 2, "on"),
            jv_user(5, "next"),
        ];

        // Active path after rewind(4, target=2): [1,2] ∪ [5,5]
        // seq 3 (40 k result) and seq 4 (rewind marker) are masked.
        // Measured anchor: seq 2 → 2000 + 50 = 2050
        // Trailing on active path: seq 5 "next" → 4 chars / 4 = 1
        // Total = 2051
        let est = estimate_from_events(&events, 4);
        assert_eq!(est, 2051, "masked branch events must not contribute");

        // Without rewind masking the 40 000-char result would add 10 000:
        let no_rewind: Vec<Value> = vec![
            jv_user(1, "task"),
            jv_assistant(2, "step", 2000, 50),
            jv_result(3, "c1", &big),
            jv_user(5, "next"),
        ];
        let est_no_rw = estimate_from_events(&no_rewind, 4);
        assert!(est_no_rw > 12000, "without masking the big result inflates the estimate");
        assert!(est < est_no_rw, "rewind-aware estimate must be smaller");
    }

    // ── Gap 2: boundary path applies 25 % margin, no-boundary does not ─

    #[test]
    fn boundary_path_applies_margin_no_boundary_does_not() {
        // No-boundary: measured anchor 1000 + output 100 + trailing 200/4=50
        let events_nb: Vec<Value> = vec![
            jv_user(1, "hi"),
            jv_assistant(2, "ok", 1000, 100),
            jv_user(3, &"a".repeat(800)),
        ];
        let est_nb = estimate_from_events(&events_nb, 4);
        assert_eq!(est_nb, 1000 + 100 + 200, "no margin on measured anchor");

        // Boundary: same trailing region but behind a compaction_summary.
        // full_form of kept: user(800 chars → 200) + result none → 200
        // summary = 400 chars → 100
        // raw = 300, margin = 300*5/4 = 375
        let big_summary = "s".repeat(400);
        let events_b: Vec<Value> = vec![
            jv_user(1, "hi"),
            jv_assistant(2, "ok", 1000, 100),
            jv_compaction(3, &big_summary, 4),
            jv_user(4, &"a".repeat(800)),
        ];
        let est_b = estimate_from_events(&events_b, 4);
        // kept = [user "a"×800] → 200; summary = 100; raw=300; margin → 375
        assert_eq!(est_b, 375, "boundary path applies the 25% safety margin");
    }

    #[test]
    fn boundary_path_anchors_on_measured_reading_in_kept_region() {
        // Regression: a kept region that contains a measured assistant
        // message must anchor on the provider's own token counts, not on
        // a chars/cpts heuristic. This is what keeps the trigger honest
        // for code-heavy sessions where chars/token is well under 4.
        let big_summary = "s".repeat(400);
        let events: Vec<Value> = vec![
            jv_user(1, "hi"),
            jv_assistant(2, "ok", 1000, 100),
            jv_compaction(3, &big_summary, 2),
            jv_assistant(4, "more", 5000, 300),
            jv_user(5, &"a".repeat(800)),
        ];
        // kept (pos >= 2) = [asst(2), compaction(3), asst(4), user(5)]
        // last measured in kept = asst(4): in=5000, out=300
        // trailing after asst(4) = user(5, 800 chars) -> 200
        // estimate = 5000 + 300 + 200 = 5500 (no margin, no summary re-add)
        let est = estimate_from_events(&events, 4);
        assert_eq!(est, 5500, "boundary path must anchor on the measured reading");

        // Same shape, but the kept region holds no measured reading:
        // the estimate must fall back to the chars/cpts + margin path.
        let events_n: Vec<Value> = vec![
            jv_user(1, "hi"),
            jv_compaction(2, &big_summary, 3),
            jv_user(3, &"a".repeat(800)),
        ];
        let est_n = estimate_from_events(&events_n, 4);
        // kept = [user 800 chars -> 200]; summary = 100; raw = 300; margin -> 375
        assert_eq!(est_n, 375, "no measured reading keeps the margin fallback");
    }

    // ── Gap 3: capped vs uncapped estimate divergence ───────────────

    #[test]
    fn find_cut_uses_capped_estimate_vs_full_form_uncapped() {
        // Five events: user, small asst, big result, long asst, big result.
        // The capped and uncapped totals straddle the keep budget, so
        // find_cut produces different cut points under each cap set.
        let evs = vec![
            ev_user("task"),                              // 1 token
            ev_asst("step one"),                          // 2 tokens
            ev_res("1", &"x".repeat(2000)),               // 500 tokens
            Ev::Assistant {
                text: "a".repeat(4000),                   // 1000 uncapped, 50 capped
                calls: vec![],
                reasoning_chars: 0,
            },
            ev_res("2", &"y".repeat(2000)),               // 500 tokens
        ];

        let uncapped_caps = Caps { text: None, chars_per_token: 4 };
        let capped_caps = Caps { text: Some(200), chars_per_token: 4 };

        // Uncapped: 1 + 2 + 500 + 1000 + 500 = 2003
        let full_total = full_form_estimate(&evs, &uncapped_caps);
        assert_eq!(full_total, 2003);

        // Capped: 1 + 2 + 500 + 50 + 500 = 1053
        let capped_total = full_form_estimate(&evs, &capped_caps);
        assert_eq!(capped_total, 1053, "capped total must be much lower");

        // Keep budget between the two: capped fits, uncapped does not.
        let keep = 1500u64;
        let cut_capped = find_cut(&evs, keep, &capped_caps);
        let cut_uncapped = find_cut(&evs, keep, &uncapped_caps);
        // Capped total (1053) ≤ 1500 → keep everything, no cut.
        assert_eq!(cut_capped, 0, "capped estimate fits, so nothing is cut");
        // Uncapped total (2003) > 1500 → walk stops at the long asst
        // (group start), orphan check sees a Result before it, no pull.
        assert_eq!(cut_uncapped, 3, "uncapped estimate exceeds budget, cut lands at the long assistant");
    }

    // ── handoff_version_meta tests ──────────────────────────────

    fn jv_compaction_v2(version: u64, first_kept_seq: u64) -> Value {
        serde_json::json!({
            "v": 1,
            "type": "compaction_summary",
            "ts": "t",
            "summary": "s",
            "first_kept_seq": first_kept_seq,
            "version": version,
            "parent_version": 0,
            "diverge_seq": 0,
            "reason": "threshold",
            "tokens_before": 0
        })
    }

    #[test]
    fn version_meta_no_boundaries() {
        let events: Vec<Value> = vec![
            jv_user(1, "hello"),
            serde_json::json!({"type":"assistant_message","content":"hi"}),
        ];
        let (v, pv, ds) = handoff_version_meta(&events);
        assert_eq!(v, 1, "first compaction is version 1");
        assert_eq!(pv, 0, "no parent for first compaction");
        assert_eq!(ds, 0, "no diverge point for first compaction");
    }

    #[test]
    fn version_meta_one_boundary() {
        let events: Vec<Value> = vec![
            jv_user(1, "hello"),
            jv_compaction_v2(1, 8),
            jv_user(9, "more"),
        ];
        let (v, pv, ds) = handoff_version_meta(&events);
        assert_eq!(v, 2, "second compaction is version 2");
        assert_eq!(pv, 1, "parent is version 1");
        assert_eq!(ds, 8, "diverge point is parent's first_kept_seq");
    }

    #[test]
    fn version_meta_two_boundaries() {
        let events: Vec<Value> = vec![
            jv_user(1, "hello"),
            jv_compaction_v2(1, 8),
            jv_user(9, "more"),
            jv_compaction_v2(2, 15),
        ];
        let (v, pv, ds) = handoff_version_meta(&events);
        assert_eq!(v, 3, "third compaction is version 3");
        assert_eq!(pv, 2, "parent is the most recent boundary");
        assert_eq!(ds, 15, "diverge point is the last boundary's first_kept_seq");
    }

    #[test]
    fn version_meta_legacy_events_without_version() {
        // Simulate old events that lack the version field.
        let events: Vec<Value> = vec![
            serde_json::json!({
                "v": 1, "type": "compaction_summary", "ts": "t",
                "summary": "s", "first_kept_seq": 10,
                "reason": "threshold", "tokens_before": 0
            }),
        ];
        let (v, pv, ds) = handoff_version_meta(&events);
        assert_eq!(v, 2, "version is count+1 regardless of field presence");
        assert_eq!(pv, 1, "parent is the one existing boundary");
        assert_eq!(ds, 10, "diverge point from legacy first_kept_seq");
    }
}
