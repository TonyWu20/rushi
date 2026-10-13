#!/usr/bin/env bash
# The transient model error e2e (docs/failure-tracking.md FT-031).
# An `error` event settles no claim state: a failed model attempt
# stops the loop, the owed work stays in the log, and a new loop (a
# `rushi run` poke or the TUI `Ctrl+R`) retries it. Legacy session
# logs with a bare trailing error resume the same way, with no log
# editing. The endpoint is switched between two stub models:
# STUB_DOWN=1 fails every call with a connection error; otherwise
# the stub answers a clean stop.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN_DIR="$ROOT/target/debug"

cargo build --quiet || { echo "FAIL: cargo build"; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); }
ko() {
  FAIL=$((FAIL + 1))
  echo "FAIL: $1"
}

# ── The stub model ───────────────────────────────────────────────
cat > "$WORK/stub-model" <<'STUB_EOF'
#!/usr/bin/env bash
set -u
if [[ "${1:-}" == "--describe" ]]; then
  echo '{"active":"stub","model_id":"stub-model","reasoning_effort":"none","thinking_level":"off"}'
  exit 0
fi
req=$(cat)
if [[ "${STUB_DOWN:-0}" == "1" ]]; then
  echo "stub: endpoint down" >&2
  echo '{"text":"","tool_calls":[],"reasoning":[],"stop_reason":"error","detail":"connection failed: cannot access the model API endpoint"}'
  exit 0
fi
echo "$req" >> "${STUB_UP_LOG:-/dev/null}"
echo '{"text":"task complete","tool_calls":[],"reasoning":[],"stop_reason":"stop","usage":{"input_tokens":10,"output_tokens":5}}'
exit 0
STUB_EOF
chmod +x "$WORK/stub-model"

# ── The work config ──────────────────────────────────────────────
cat > "$WORK/config.toml" <<CONFIG_EOF
[model]
api = "responses"
max_output_tokens = 4096

[model.stub]
model_id = "stub-model"
base_url = "http://127.0.0.1:1"
api_key_env = "DUMMY"
context_tokens = 8000

[active]
model = "stub"

[paths]
sessions_root = "sessions"

[limits]
context_budget_tokens = 8000
compact_enabled = false

[system_prompt]
text = "test"
CONFIG_EOF

run_loop() {
  # One `rushi run` start: with a task when $2 is non-empty (the
  # spawn form), taskless otherwise (the TUI `Ctrl+R` form).
  local task="${2:-}"
  (
    cd "$WORK"
    export CONFIG="$WORK/config.toml"
    export MODEL_BIN="$WORK/stub-model"
    export STUB_UP_LOG="$WORK/up.log"
    export STUB_DOWN="${STUB_DOWN:-0}"
    if [[ -n "$task" ]]; then
      "$BIN_DIR/rushi" run "$1" "$task"
    else
      "$BIN_DIR/rushi" run "$1"
    fi
  ) 2>/dev/null
}

claim_state() {
  "$BIN_DIR/claim" --session "$WORK/sessions/$1" | jq -r .state
}

error_count() {
  jq -csr 'map(select(.type == "error")) | length' "$WORK/sessions/$1/events.jsonl" 2>/dev/null || echo 0
}

# ── Scenario 1: the endpoint is down ─────────────────────────────
rm -f "$WORK/up.log"
STUB_DOWN=1 run_loop sess "do the task"
[[ -f "$WORK/sessions/sess/events.jsonl" ]] || ko "session log missing after the down run"
[[ "$(claim_state sess)" == "awaiting_model" ]] \
  && ok "the failed attempt keeps the owed model call (claim awaiting_model)" \
  || ko "claim after the failed attempt is $(claim_state sess), want awaiting_model"
[[ "$(error_count sess)" -eq 1 ]] \
  && ok "the stop logged exactly one error event" \
  || ko "error event count is $(error_count sess), want 1"
[[ ! -f "$WORK/up.log" ]] \
  && ok "no call reached the up stub while down" \
  || ko "the up stub saw a request while down"

# ── Scenario 2: resume while still down: bounded, no spin ────────
STUB_DOWN=1 run_loop sess
[[ "$(error_count sess)" -eq 2 ]] \
  && ok "the still-down resume added exactly one more error event" \
  || ko "error count after the still-down resume is $(error_count sess), want 2"
[[ "$(claim_state sess)" == "awaiting_model" ]] \
  && ok "the session is still resumable after the second stop" \
  || ko "claim after the still-down resume is $(claim_state sess)"

# ── Scenario 3: the endpoint recovers, Ctrl+R resumes ────────────
run_loop sess
n_calls=$(wc -l < "$WORK/up.log" 2>/dev/null || echo 0)
[[ "$n_calls" -ge 1 ]] \
  && ok "the resumed loop called the model (${n_calls} request(s))" \
  || ko "the resumed loop made no model call (the bug)"
n_asm=$(jq -csr 'map(select(.type == "assistant_message")) | length' "$WORK/sessions/sess/events.jsonl")
[[ "$n_asm" -ge 1 ]] \
  && ok "the resumed turn logged an assistant_message" \
  || ko "no assistant_message after the resume"
[[ "$(claim_state sess)" == "idle" ]] \
  && ok "the completed turn settled the session to idle" \
  || ko "claim after the completed turn is $(claim_state sess), want idle"

# ── Scenario 4: the pure legacy shape resumes untouched ──────────
mkdir -p "$WORK/sessions/legacy"
cat > "$WORK/sessions/legacy/events.jsonl" <<'LOG_EOF'
{"v":1,"type":"user_message","ts":"2026-10-13T10:00:00Z","content":"do the task"}
{"v":1,"type":"ext_status","ts":"2026-10-13T10:00:01Z","id":"loop_phase","value":"wait"}
{"v":1,"type":"error","ts":"2026-10-13T10:00:02Z","message":"model API call failed after retries: connection refused"}
LOG_EOF
[[ "$(claim_state legacy)" == "awaiting_model" ]] \
  && ok "a legacy bare-error log claims the owed call, no marker needed" \
  || ko "legacy claim is $(claim_state legacy), want awaiting_model"
run_loop legacy
[[ "$(claim_state legacy)" == "idle" ]] \
  && ok "the legacy session resumed through the taskless run and settled idle" \
  || ko "legacy claim after the resume is $(claim_state legacy), want idle"

# ── Scenario 5: the handoff still settles exhausted ──────────────
mkdir -p "$WORK/sessions/handoff"
cat > "$WORK/sessions/handoff/events.jsonl" <<'LOG_EOF'
{"v":1,"type":"user_message","ts":"t1","content":"work"}
{"v":1,"type":"context_exhausted","ts":"t2","message":"out of context","new_session":"handoff"}
LOG_EOF
[[ "$(claim_state handoff)" == "exhausted" ]] \
  && ok "the handoff marker still settles exhausted" \
  || ko "handoff claim is $(claim_state handoff), want exhausted"

echo "PASS=$PASS FAIL=$FAIL"
[[ "$FAIL" -eq 0 ]]
