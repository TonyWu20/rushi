//! Detection of long `sleep` waits and `sleep` poll loops in shell
//! command strings.
//!
//! This is a heuristic, not a shell parser. It tokenizes on
//! whitespace, finds `sleep` command tokens, and reads their numeric
//! arguments. It catches the patterns agents actually write. It does
//! not evaluate arbitrary variables or deeply nested constructs
//! (documented limits, `rushi docs monitoring`).

/// A blocked wait pattern.
#[derive(Debug)]
pub struct Block {
    /// `"long-sleep"` when a literal `sleep` exceeds the cap,
    /// `"poll-loop"` when a loop contains `sleep`.
    pub kind: &'static str,
    /// The offending fragment, e.g. `"300s"` or `"for loop sleeps ~300s"`.
    pub detail: String,
}

/// Inspect a shell command. Returns `Some` when the command blocks
/// the tool call beyond `max_s` seconds, or loops on `sleep`.
pub fn check_command(cmd: &str, max_s: f64) -> Option<Block> {
    let tokens = tokenize(cmd);
    let sleep_idxs: Vec<usize> = (0..tokens.len())
        .filter(|&i| is_sleep_token(&tokens[i]))
        .collect();
    if sleep_idxs.is_empty() {
        return None;
    }

    // Rule 1: a literal sleep longer than the cap. GNU `sleep` sums
    // consecutive values, so sum them the same way.
    for &i in &sleep_idxs {
        if let Some(total) = sleep_wait_secs(&tokens, i) {
            if total > max_s {
                return Some(Block {
                    kind: "long-sleep",
                    detail: format!("{total}s"),
                });
            }
        }
    }

    // Rule 2: a loop that sleeps.
    loop_block(&tokens, &sleep_idxs, max_s)
}

/// A loop containing `sleep` blocks the call for a long or unbounded
/// time. `while`/`until` are unbounded condition polls. `for` is
/// bounded by iteration count, so estimate the total wait and block it
/// when it exceeds the cap or cannot be bounded.
fn loop_block(tokens: &[String], sleep_idxs: &[usize], max_s: f64) -> Option<Block> {
    // while / until: a condition loop with a sleep is an unbounded poll.
    if tokens.iter().any(|t| t == "while" || t == "until") {
        return Some(Block {
            kind: "poll-loop",
            detail: "while/until loop with sleep".into(),
        });
    }

    // for: bounded by iteration count * per-iteration sleep.
    let Some(f) = tokens.iter().position(|t| t == "for") else {
        return None;
    };
    let Some(body) = for_body(tokens, f) else {
        return None;
    };
    // The loop only matters when its body contains a `sleep`.
    if !body.iter().any(|&k| sleep_idxs.contains(&k)) {
        return None;
    }
    // Per-iteration wait: the sum of the literal sleeps in the body.
    let sleeps: Vec<usize> = body
        .iter()
        .copied()
        .filter(|&k| sleep_idxs.contains(&k))
        .collect();
    let per_iter_known = sleeps
        .iter()
        .all(|&k| sleep_wait_secs(tokens, k).is_some());
    let per_iter: f64 = sleeps
        .iter()
        .filter_map(|&k| sleep_wait_secs(tokens, k))
        .sum();

    let Some(count) = for_count(tokens, f) else {
        // The iteration count is uncountable (glob, variable, or a
        // substitution we do not evaluate). The wait is unbounded, so
        // block it.
        return Some(Block {
            kind: "poll-loop",
            detail: "for loop with an uncountable iteration count".into(),
        });
    };

    if !per_iter_known {
        return Some(Block {
            kind: "poll-loop",
            detail: "for loop with a non-literal sleep duration".into(),
        });
    }

    let total = per_iter * count as f64;
    if total > max_s {
        return Some(Block {
            kind: "poll-loop",
            detail: format!("for loop sleeps ~{total}s in total"),
        });
    }

    None
}

/// The body token indices of the `for` at `f`: the range between its
/// `do` and the matching `done`. `None` when the body cannot be found.
fn for_body(tokens: &[String], f: usize) -> Option<Vec<usize>> {
    let do_idx = tokens[f + 1..]
        .iter()
        .position(|t| t == "do" || t == "then")?
        + f
        + 1;
    let mut depth = 1;
    let mut j = do_idx + 1;
    while j < tokens.len() {
        match tokens[j].as_str() {
            "do" | "then" => depth += 1,
            "done" => {
                depth -= 1;
                if depth == 0 {
                    return Some((do_idx + 1..j).collect());
                }
            }
            _ => {}
        }
        j += 1;
    }
    None
}

/// The iteration count of the `for` at `f`, or `None` when it cannot
/// be counted. A `$( )` substitution is re-joined before counting.
fn for_count(tokens: &[String], f: usize) -> Option<usize> {
    let mut i = f + 1;
    i += 1; // the loop variable
    if i < tokens.len() && tokens[i] == "in" {
        i += 1;
    }
    let mut items: Vec<String> = Vec::new();
    while i < tokens.len() && !matches!(tokens[i].as_str(), "do" | "then" | ";") {
        let raw = &tokens[i];
        if raw.contains("$(") {
            // Re-join `$( ... )` across whitespace.
            let mut sub = String::new();
            let mut open: i32 = 0;
            loop {
                let cur = tokens[i].clone();
                sub.push_str(&cur);
                let o = cur.matches("$(").count() as i32;
                let c = cur.matches(')').count() as i32;
                open += o - c;
                i += 1;
                if open <= 0 || i >= tokens.len() {
                    break;
                }
                sub.push(' ');
            }
            items.push(sub.trim_end_matches(';').to_string());
            continue;
        }
        items.push(raw.trim_end_matches(';').to_string());
        i += 1;
    }
    count_items(&items)
}

/// The total iteration count across the `in` items. `None` when any
/// item is uncountable.
fn count_items(items: &[String]) -> Option<usize> {
    if items.is_empty() {
        return None;
    }
    let mut total = 0usize;
    for it in items {
        total += count_item(it)?;
    }
    Some(total)
}

/// The iteration count of a single `in` item.
fn count_item(it: &str) -> Option<usize> {
    let t = it.trim_matches(|c: char| {
        c == '(' || c == ')' || c == ';' || c == '&' || c == '|' || c == ','
    });
    // Brace expansion {A..B} or {A..B..S}.
    if t.starts_with('{') && t.ends_with('}') {
        let inner = &t[1..t.len() - 1];
        let parts: Vec<&str> = inner.split("..").collect();
        return match parts.len() {
            2 => brace_count(parts[0], parts[1], None),
            3 => brace_count(parts[0], parts[1], Some(parts[2])),
            _ => None,
        };
    }
    // A `seq` command substitution.
    if t.starts_with("$(seq") || t.starts_with("`seq") {
        let nums = extract_numbers(t);
        return match nums.len() {
            1 => Some(nums[0] as usize), // seq N -> 1..N
            2 => range_count(nums[0], nums[1], None),
            3 => range_count(nums[0], nums[1], Some(nums[2])),
            _ => None,
        };
    }
    // A glob or a variable expands to an unknown count.
    if t.contains('*') || t.contains('?') || t.starts_with('$') {
        return None;
    }
    Some(1)
}

fn brace_count(a: &str, b: &str, step: Option<&str>) -> Option<usize> {
    let (lo, hi) = (a.parse::<f64>().ok()?, b.parse::<f64>().ok()?);
    match step {
        Some(s) => {
            let s = s.parse::<f64>().ok()?;
            if s <= 0.0 {
                return None;
            }
            Some(((hi - lo) / s + 1.0).max(0.0) as usize)
        }
        None => Some(((hi - lo + 1.0).max(0.0)) as usize),
    }
}

fn range_count(lo: f64, hi: f64, step: Option<f64>) -> Option<usize> {
    match step {
        Some(s) if s > 0.0 => Some((((hi - lo) / s) + 1.0).max(0.0) as usize),
        None => Some(((hi - lo + 1.0).max(0.0)) as usize),
        _ => None,
    }
}

/// The numeric runs in a string, e.g. `"$(seq 3 300)" -> [3, 300]`.
fn extract_numbers(s: &str) -> Vec<f64> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let start = i;
        if b[i] == b'-' {
            i += 1;
        }
        if i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
            while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
                i += 1;
            }
            if let Ok(n) = s[start..i].parse::<f64>() {
                out.push(n);
            }
        } else {
            i += 1;
        }
    }
    out
}

/// A `sleep` command token: a bare `sleep` or a path that ends in
/// `/sleep`. Quoted tokens never match (the quote stays attached).
fn is_sleep_token(tok: &str) -> bool {
    tok.rsplit('/').next().unwrap_or(tok) == "sleep"
}

/// Whitespace tokens, with shell separators trimmed off the edges.
/// A trailing `)` is kept: `$( ... )` re-joining in `for_count`
/// needs it, and `parse_duration` trims its own.
fn tokenize(cmd: &str) -> Vec<String> {
    cmd.split_whitespace()
        .map(|t| {
            t.trim_matches(|c: char| {
                c == ';' || c == '&' || c == '|' || c == '(' || c == ','
            })
            .to_string()
        })
        .filter(|t| !t.is_empty())
        .collect()
}

/// The total seconds the `sleep` at index `i` would wait. Returns
/// `None` when its arguments are not literal numbers (for example a
/// variable), which this checker does not evaluate.
fn sleep_wait_secs(tokens: &[String], i: usize) -> Option<f64> {
    let mut total = 0.0;
    let mut found = false;
    let mut j = i + 1;
    while j < tokens.len() && j - i <= 8 {
        let tok = &tokens[j];
        if let Some(secs) = parse_duration(tok) {
            total += secs;
            found = true;
            j += 1;
            continue;
        }
        if !found && tok.starts_with('-') {
            j += 1; // skip a flag such as `-i`
            continue;
        }
        break;
    }
    found.then_some(total)
}

/// Parse a `sleep` duration argument: a number with an optional
/// `s`/`m`/`h`/`d` suffix. Returns `None` when the token is not a
/// literal duration.
fn parse_duration(tok: &str) -> Option<f64> {
    let t = tok.trim_matches(|c: char| {
        c == ';' || c == '&' || c == '|' || c == ',' || c == ')' || c == '"'
    });
    if t.is_empty() {
        return None;
    }
    let b = t.as_bytes();
    let mut num_end = 0;
    while num_end < b.len() && (b[num_end].is_ascii_digit() || b[num_end] == b'.') {
        num_end += 1;
    }
    if num_end == 0 {
        return None;
    }
    let secs: f64 = t[..num_end].parse().ok()?;
    let mult = match b.get(num_end) {
        None | Some(b's') => 1.0,
        Some(b'm') => 60.0,
        Some(b'h') => 3600.0,
        Some(b'd') => 86400.0,
        _ => return None,
    };
    Some(secs * mult)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAX: f64 = 60.0;

    fn blocked(cmd: &str) -> bool {
        check_command(cmd, MAX).is_some()
    }

    fn kind(cmd: &str) -> Option<String> {
        check_command(cmd, MAX).map(|b| b.kind.to_string())
    }

    // ── Literal long sleep ──

    #[test]
    fn sleep_300_blocked() {
        assert!(blocked("sleep 300"));
        assert_eq!(kind("sleep 300").as_deref(), Some("long-sleep"));
    }

    #[test]
    fn sleep_61_blocked() {
        assert!(blocked("sleep 61"));
    }

    #[test]
    fn sleep_60_allowed() {
        assert!(!blocked("sleep 60"));
    }

    #[test]
    fn short_sleep_allowed() {
        assert!(!blocked("sleep 5"));
        assert!(!blocked("sleep 0.5"));
    }

    #[test]
    fn sleep_after_separator_blocked() {
        assert!(blocked("cargo build && sleep 300"));
        assert!(blocked("check.sh; sleep 300"));
        assert!(blocked("nohup ./run.sh > log 2>&1 & sleep 300"));
    }

    #[test]
    fn sleep_in_path_blocked() {
        assert!(blocked("/usr/bin/sleep 300"));
    }

    #[test]
    fn consecutive_values_sum() {
        // GNU `sleep 30 60` waits 90s total.
        assert!(blocked("sleep 30 60"));
        assert!(!blocked("sleep 30 20"));
    }

    #[test]
    fn duration_suffixes() {
        assert!(blocked("sleep 5m"));
        assert!(!blocked("sleep 1m"));
        assert!(blocked("sleep 1h"));
    }

    #[test]
    fn flags_before_value() {
        assert!(blocked("sleep -i 300"));
    }

    // ── Not caught (documented limits) ──

    #[test]
    fn variable_sleep_not_evaluated() {
        assert!(!blocked("sleep $SECS"));
        assert!(!blocked("S=300; sleep $S"));
    }

    #[test]
    fn quoted_sleep_not_a_command() {
        assert!(!blocked(r#"echo "sleep 300"'"#));
        assert!(!blocked("rg 'sleep 300' file"));
    }

    // ── while / until poll loop ──

    #[test]
    fn while_sleep_loop_blocked() {
        assert!(blocked("while :; do check.sh; sleep 30; done"));
        assert_eq!(
            kind("while :; do sleep 5; done").as_deref(),
            Some("poll-loop")
        );
    }

    #[test]
    fn until_sleep_loop_blocked() {
        assert!(blocked("until job_done; do sleep 30; done"));
    }

    #[test]
    fn loop_keyword_in_quotes_not_blocked() {
        assert!(!blocked(r#"echo 'while sleep'"#));
        assert!(!blocked("rg 'while' sleep.txt"));
    }

    #[test]
    fn long_sleep_wins_over_loop_rule() {
        assert_eq!(
            kind("while :; do sleep 300; done").as_deref(),
            Some("long-sleep")
        );
    }

    // ── for loop: total wait = count * per-iteration sleep ──

    #[test]
    fn for_literal_list_short_allowed() {
        // 3 * 5s = 15s, under the cap.
        assert!(!blocked("for i in 1 2 3; do sleep 5; done"));
    }

    #[test]
    fn for_literal_list_long_blocked() {
        // 10 * 10s = 100s, over the cap.
        assert!(blocked("for i in 1 2 3 4 5 6 7 8 9 10; do sleep 10; done"));
        assert_eq!(kind("for i in 1 2 3 4 5 6 7 8 9 10; do sleep 10; done").as_deref(), Some("poll-loop"));
    }

    #[test]
    fn for_seq_single_blocked() {
        // seq 300 -> 300 * 1s = 300s.
        assert!(blocked("for i in $(seq 300); do sleep 1; done"));
    }

    #[test]
    fn for_seq_range_blocked() {
        // seq 3 60 -> 58 iterations * 2s = 116s.
        assert!(blocked("for i in $(seq 3 60); do sleep 2; done"));
    }

    #[test]
    fn for_seq_short_allowed() {
        // seq 50 -> 50 * 1s = 50s, under the cap.
        assert!(!blocked("for i in $(seq 50); do sleep 1; done"));
    }

    #[test]
    fn for_brace_range_blocked() {
        // {1..300} -> 300 * 1s = 300s.
        assert!(blocked("for i in {1..300}; do sleep 1; done"));
    }

    #[test]
    fn for_brace_short_allowed() {
        // {1..10} -> 10 * 5s = 50s, under the cap.
        assert!(!blocked("for i in {1..10}; do sleep 5; done"));
    }

    #[test]
    fn for_glob_blocked() {
        // An uncountable glob count is treated as unbounded.
        assert!(blocked("for f in *.txt; do sleep 5; done"));
    }

    #[test]
    fn for_variable_blocked() {
        // An uncountable variable count is treated as unbounded.
        assert!(blocked("for i in $items; do sleep 5; done"));
    }

    #[test]
    fn for_nonliteral_sleep_blocked() {
        // A non-literal per-iteration duration is treated as unbounded.
        assert!(blocked("for i in 1 2 3; do sleep $S; done"));
    }

    #[test]
    fn for_without_sleep_in_body_allowed() {
        // The loop body has no sleep; the lone sleep is handled by
        // rule 1 (15s here, so allowed).
        assert!(!blocked("for i in 1 2; do echo hi; done; sleep 5"));
    }

    // ── Threshold respected ──

    #[test]
    fn threshold_is_respected() {
        assert!(check_command("sleep 30", 20.0).is_some());
        assert!(check_command("sleep 30", 30.0).is_none());
        assert!(check_command("sleep 31", 30.0).is_some());
    }

    #[test]
    fn empty_command_allowed() {
        assert!(check_command("", MAX).is_none());
    }
}
