//! Overflow classifier — ported from `scripts/overflow-classify.sh`.
//!
//! The exclusion table is checked first: a detail matching an exclusion
//! is never overflow. The overflow table (25 patterns) matches next.
//! An empty detail is not overflow (the transport path takes it).
//!
//! The ten `--self-test` rows of the script become unit tests below.

use regex::Regex;
use std::sync::OnceLock;

/// The exclusion patterns: rate limits and non-overflow shapes.
const EXCLUDES: &[&str] = &[
    r"^(Throttling error|Service unavailable):",
    r"rate[ _-]?limit",
    "too many requests",
    "429",
    "throttl",
];

/// The overflow patterns (pi table + SGLang + DeepSeek shapes).
const OVERFLOW_PATTERNS: &[&str] = &[
    "prompt is too long",
    "request_too_large",
    "input is too long for requested model",
    "exceeds the context window",
    r"exceeds (the )?(model.s )?maximum context length",
    r"input token count.*exceeds the maximum",
    r"maximum prompt length is [0-9]+",
    "reduce the length of the messages",
    r"maximum context length is [0-9]+ tokens",
    r"exceeds (the )?maximum allowed input length of [0-9,]+ tokens?",
    r"input \([0-9]+ tokens\) is longer than the model.?s context length \([0-9]+ tokens\)",
    r"exceeds the limit of [0-9]+",
    "exceeds the available context size",
    "greater than the context length",
    "context window exceeds limit",
    "exceeded model token limit",
    r"too large for model with [0-9]+ maximum context length",
    r"prompt has [0-9,]+ tokens?, but the configured context size is [0-9,]+ tokens?",
    "model_context_window_exceeded",
    r"prompt too long; exceeded (max )?context length",
    "range of input length should be",
    "context[_ ]length[_ ]exceeded",
    "too many tokens",
    "token limit exceeded",
    r"^4(00|13) (status code)? \(no body\)",
];

fn compiled_excludes() -> &'static [Regex] {
    static RE: OnceLock<Vec<Regex>> = OnceLock::new();
    let v = RE.get_or_init(|| {
        EXCLUDES
            .iter()
            .map(|p| Regex::new(p).expect("compile exclude"))
            .collect()
    });
    v
}

fn compiled_overflows() -> &'static [Regex] {
    static RE: OnceLock<Vec<Regex>> = OnceLock::new();
    let v = RE.get_or_init(|| {
        OVERFLOW_PATTERNS
            .iter()
            .map(|p| Regex::new(p).expect("compile overflow pattern"))
            .collect()
    });
    v
}

/// Return `true` when `detail` is a context-overflow error.
///
/// The check is case-insensitive. Exclusions are checked first: a
/// match there means not-overflow regardless of overflow-pattern hits.
pub fn is_overflow(detail: &str) -> bool {
    if detail.is_empty() {
        return false;
    }
    let lower = detail.to_lowercase();

    for re in compiled_excludes() {
        if re.is_match(&lower) {
            return false;
        }
    }
    for re in compiled_overflows() {
        if re.is_match(&lower) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ten `--self-test` rows from `overflow-classify.sh`.
    #[test]
    fn self_test_rows() {
        let rows: &[(&str, bool)] = &[
            (
                "prompt is too long: 213462 tokens > 200000 maximum",
                true,
            ),
            ("Your input exceeds the context window of this model", true),
            (
                "The input (265330 tokens) is longer than the model's context length (262144 tokens).",
                true,
            ),
            (
                "Input length (265330) exceeds model's maximum context length (262144).",
                true,
            ),
            (
                "Requested token count exceeds the model's maximum context length of 131072 tokens",
                true,
            ),
            (
                r#"413 {"error":{"type":"request_too_large"}}"#,
                true,
            ),
            ("ThrottlingException: Too many tokens, please wait", false),
            ("rate limited: too many requests", false),
            ("500 internal server error", false),
            ("", false),
        ];
        for (detail, expected) in rows {
            let got = is_overflow(detail);
            assert_eq!(
                got, *expected,
                "detail={:?} expected={expected} got={got}",
                detail
            );
        }
    }

    #[test]
    fn exclusion_wins_over_overflow_pattern() {
        // "ThrottlingException: Too many tokens" matches "too many tokens"
        // (overflow) but is excluded by "throttl".
        assert!(!is_overflow("ThrottlingException: Too many tokens, please wait"));
    }

    #[test]
    fn empty_detail_is_not_overflow() {
        assert!(!is_overflow(""));
    }
}
