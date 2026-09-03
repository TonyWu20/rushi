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

/// Caps for the estimator. `None` means no cap.
#[derive(Clone, Copy, Debug, Default)]
pub struct Caps {
    pub result: Option<u64>,
    pub text: Option<u64>,
}

/// The chars/4 estimator for one projected event.
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
        Ev::Result { chars, .. } => {
            if let Some(cap) = caps.result {
                std::cmp::min(*chars, cap)
            } else {
                *chars
            }
        }
    };
    chars / 4
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
pub fn est_tokens_after(kept: &[Ev], handoff_doc: &str, clip: u64) -> u64 {
    let framing: u64 = handoff_doc.chars().count() as u64 / 4 + 64;
    let full_caps = Caps {
        result: Some(clip),
        text: None,
    };
    let mut total = framing;
    for ev in kept.iter() {
        if let Ev::Result { chars, .. } = ev {
            total += std::cmp::min(*chars, clip) / 4;
        } else {
            total += est_tokens(ev, &full_caps);
        }
    }
    total
}

/// The post-compact sanity check: compare the projected post-compact
/// size to the trigger level. If it still meets or exceeds the trigger,
/// the compact is not helping and a warning is returned.
pub fn post_compact_sanity(
    kept: &[Ev],
    handoff_doc: &str,
    clip: u64,
    trigger_level: u64,
) -> bool {
    est_tokens_after(kept, handoff_doc, clip) > trigger_level
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
        "tool_result" => Ev::Result {
            call_id: v.get("id")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string(),
            chars: v.get("value")
                .and_then(|o| o.get("text"))
                .and_then(|s| s.as_str())
                .map(|s| s.chars().count() as u64)
                .unwrap_or(0),
        },
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

    #[test]
    fn est_tokens_mirrors_the_assemble_estimator() {
        let caps = Caps {
            result: Some(500),
            text: Some(100),
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
        assert_eq!(est_tokens(&ev_res("1", &"c".repeat(4000)), &caps), 125);
        let full = Caps {
            result: None,
            text: None,
        };
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
            result: Some(1000),
            text: None,
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
            result: Some(1000),
            text: None,
        };
        let cut = find_cut(&kept, 10, &caps);
        assert_eq!(cut, 0, "the orphan user sits behind the cut");
    }

    #[test]
    fn cut_at_zero_is_the_empty_region() {
        let kept = vec![ev_user("task"), ev_asst("step one")];
        let caps = Caps {
            result: None,
            text: None,
        };
        assert_eq!(find_cut(&kept, 1_000_000, &caps), 0);
    }

    #[test]
    fn trigger_fires_above_level() {
        assert!(trigger_fired(1000, 999));
        assert!(!trigger_fired(999, 1000));
        assert!(!trigger_fired(1000, 1000));
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
        assert!(post_compact_sanity(&kept, &big_handoff, 20000, 100));
    }
}
