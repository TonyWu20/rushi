#!/usr/bin/env bash
# The overflow classifier (docs/auto-compact-plan.md section 4.4).
# It is sourced by step.sh and by the table tests; it defines one
# function and two tables, and it reads nothing else from the
# caller.
#
# The table is ported from pi's pi-ai utils/overflow.ts
# OVERFLOW_PATTERNS and NON_OVERFLOW_PATTERNS, with the SGLang and
# DeepSeek shapes extended from live probes. The exclusion table is
# checked first: a detail that matches an exclusion is not overflow
# even when it also matches an overflow pattern.

# The exclusion patterns: rate limits and the non-overflow shapes.
# A match here excludes the detail from overflow no matter what.
OVERRIDE_OVERFLOW_EXCLUDES=(
  '^(Throttling error|Service unavailable):'
  'rate[ _-]?limit'
  'too many requests'
  '429'
  'throttl'
)

# The overflow patterns. Each is a case-insensitive ERE over the
# error detail. The SGLang and DeepSeek shapes join the pi table:
# SGLang reports "The input (X tokens) is longer than the model's
# context length (Y tokens)" through its OpenAI-compatible API,
# and DeepSeek reports a context-length error as a 400 with the
# input length in the body.
OVERRIDE_OVERFLOW_PATTERNS=(
  'prompt is too long'
  'request_too_large'
  'input is too long for requested model'
  'exceeds the context window'
  'exceeds (the )?(model.s )?maximum context length'
  'input token count.*exceeds the maximum'
  'maximum prompt length is [0-9]+'
  'reduce the length of the messages'
  'maximum context length is [0-9]+ tokens'
  'exceeds (the )?maximum allowed input length of [0-9,]+ tokens?'
  'input \([0-9]+ tokens\) is longer than the model.?s context length \([0-9]+ tokens\)'
  'exceeds the limit of [0-9]+'
  'exceeds the available context size'
  'greater than the context length'
  'context window exceeds limit'
  'exceeded model token limit'
  'too large for model with [0-9]+ maximum context length'
  'prompt has [0-9,]+ tokens?, but the configured context size is [0-9,]+ tokens?'
  'model_context_window_exceeded'
  'prompt too long; exceeded (max )?context length'
  'range of input length should be'
  'context[_ ]length[_ ]exceeded'
  'too many tokens'
  'token limit exceeded'
  '^4(00|13) (status code)? \(no body\)'
)

# classify_overflow_error DETAIL
# Returns 0 when DETAIL matches the overflow table, 1 otherwise.
# An empty detail returns 1: an error stop with no detail is not
# recoverable (the transport path takes it).
classify_overflow_error() {
  local detail="$1"
  local pat
  [[ -z "$detail" ]] && return 1
  local lower
  lower="${detail,,}"
  for pat in "${OVERRIDE_OVERFLOW_EXCLUDES[@]}"; do
    if [[ "$lower" =~ $pat ]]; then
      return 1
    fi
  done
  for pat in "${OVERRIDE_OVERFLOW_PATTERNS[@]}"; do
    if [[ "$lower" =~ $pat ]]; then
      return 0
    fi
  done
  return 1
}

# The direct-run mode: the table test. `overflow-classify.sh DETAIL`
# prints "overflow" or "not-overflow" and exits 0.
if [[ "${1:-}" == "--self-test" ]]; then
  for detail in \
    "prompt is too long: 213462 tokens > 200000 maximum" \
    "Your input exceeds the context window of this model" \
    "The input (265330 tokens) is longer than the model's context length (262144 tokens)." \
    "Input length (265330) exceeds model's maximum context length (262144)." \
    "Requested token count exceeds the model's maximum context length of 131072 tokens" \
    '413 {"error":{"type":"request_too_large"}}' \
    "ThrottlingException: Too many tokens, please wait" \
    "rate limited: too many requests" \
    "500 internal server error" \
    ""
  do
    if classify_overflow_error "$detail"; then
      printf 'overflow: %s\n' "$detail"
    else
      printf 'not-overflow: %s\n' "$detail"
    fi
  done
fi
