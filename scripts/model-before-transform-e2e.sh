#!/usr/bin/env bash
# The model.before transform e2e (docs/loop-lifecycle-hooks.md 3.3, 4.5).
# Drives `harness step` with a scriptable stub model and a jq-based
# model.before hook. Asserts the transform decision, the `hook_applied`
# marker, the malformed-payload failure path, and the byte-identical
# no-hooks default.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN_DIR="$ROOT/target/debug"

cargo build --quiet || { echo "FAIL: cargo build"; exit 1; }

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); }
ko() {
  FAIL=$((FAIL + 1))
  echo "FAIL: $1"
}

# ── The work config ───────────────────────────────────────────────
# window 8000, output 4096, input budget 3904. A single small turn
# stays far under the trigger level, so no compact fires.

# $1 = a non-empty value adds the [[hooks.on]] registration to the
# config; an empty value leaves the config hook-free.
work_config() {
  cat > "$WORK/config.toml" <<EOF
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
tools_root = "$ROOT/tools"

[limits]
context_budget_tokens = 8000
compact_reserve_tokens = 500
compact_keep_tokens = 2000
compact_enabled = false

[system_prompt]
text = "test"
EOF
  if [[ -n "${1:-}" ]]; then
    cat >>"$WORK/config.toml" <<EOF
[hooks]
timeout_ms = 30000

[[hooks.on]]
window  = "model.before"
command = "$WORK/hook-model-before"
args    = []
EOF
  fi
}

# ── The session builder ───────────────────────────────────────────
# One user message: claim is `awaiting_model`, the step runs one
# model call, then the turn ends idle.
seed_session() {
  cat > "$SLOG" <<EOF
{"v":1,"type":"user_message","ts":"t1","seq":1,"content":"do the task"}
EOF
}

# ── The stub model ─────────────────────────────────────────────────
# Records every normal request on STUB_REQLOG, one JSON line each,
# and answers a final stop.
make_stub() {
  cat > "$WORK/stub-model" <<'EOF'
#!/usr/bin/env bash
set -u
if [[ "${1:-}" == "--describe" ]]; then
  echo '{"active":"stub","model_id":"stub-model","reasoning_effort":"none","thinking_level":"off"}'
  exit 0
fi
req=$(cat)
printf '%s\n' "$req" >>"${STUB_REQLOG:-/dev/null}"
echo '{"text":"done","tool_calls":[],"reasoning":[],"stop_reason":"stop","usage":{"input_tokens":10,"output_tokens":10}}'
EOF
  chmod +x "$WORK/stub-model"
}

# ── The model.before hooks ────────────────────────────────────────
# The transform hook prepends a goal-continuation instruction to the
# request input, as a pi-goal-style continuation prompt would.
make_hook() {
  cat > "$WORK/hook-model-before" <<'EOF'
#!/usr/bin/env bash
set -u
payload=$(cat)
req=$(jq -c '.request // {}' <<<"$payload")
jq -cn --argjson req "$req" '{
  decision: "transform",
  payload: {
    request: ($req | .input = (
      [{"type":"message","role":"user","content":"goal-hook: continue the active goal"}]
      + (.input // [])))
  }
}'
EOF
  chmod +x "$WORK/hook-model-before"
}

# The malformed-transform hook: a transform decision without the
# object `request` field.
make_hook_malformed() {
  cat > "$WORK/hook-model-before" <<'EOF'
#!/usr/bin/env bash
set -u
cat > /dev/null
echo '{"decision":"transform"}'
EOF
  chmod +x "$WORK/hook-model-before"
}

run_step() {
  (
    cd "$WORK"
    export CONFIG="$WORK/config.toml"
    export MODEL_BIN="$WORK/stub-model"
    export STUB_REQLOG="$WORK/reqlog"
    : >"$STUB_REQLOG"
    "$BIN_DIR/harness" step session
  ) >/dev/null 2>&1
  true
}

claim_state() {
  "$BIN_DIR/claim" --session "$SESSIONS_DIR" | jq -r .state
}

last_req() {
  tail -1 "$STUB_REQLOG" 2>/dev/null || true
}

count_markers() {
  local n
  n=$(jq -c "select(.type == \"ext_status\" and .id == \"$1\")" "$SLOG" 2>/dev/null | wc -l)
  echo "$n"
}

assert_eq() {
  local n
  if [ "$1" = "$2" ]; then
    ok
  else
    ko "$3: got [$1], want [$2]"
  fi
}

NEW_WORK() {
  WORK="$ROOT/scratch/e2e-model-before/$1"
  rm -rf "$WORK"
  SESSIONS_DIR="$WORK/sessions/session"
  SLOG="$SESSIONS_DIR/events.jsonl"
  STUB_REQLOG="$WORK/reqlog"
  mkdir -p "$SESSIONS_DIR"
  echo "==== scenario: $1"
}

# ── Scenario 1: the transform decision ────────────────────────────
# The hook transforms the request: the model receives the injected
# instruction, and the log carries the decision and cache-break
# markers.
scenario_transform() {
  NEW_WORK transform
  work_config 1
  seed_session
  make_stub
  make_hook
  run_step
  assert_eq "$(claim_state)" "idle" "the step runs to idle"
  local req
  req="$(last_req)"
  assert_eq "$(jq -r '.input[0].content' <<<"$req")" \
    "goal-hook: continue the active goal" \
    "the model receives the transformed request"
  assert_eq "$(count_markers "hook.model.before")" 1 "one hook.model.before marker"
  assert_eq "$(jq -r "select(.type == \"ext_status\" and .id == \"hook.model.before\") | .value" "$SLOG")" \
    "transform" "the decision marker carries the decision word"
  assert_eq "$(count_markers "hook_applied")" 1 "one hook_applied marker"
  assert_eq "$(jq -r "select(.type == \"ext_status\" and .id == \"hook_applied\") | .value" "$SLOG")" \
    "$WORK/hook-model-before" "the marker names the hook command"
}

# ── Scenario 2: the no-hooks default ──────────────────────────────
# No hook registered: the request goes out untouched, and no hook
# markers appear in the log (the byte-identical default path).
scenario_default() {
  NEW_WORK default
  work_config ""
  seed_session
  make_stub
  run_step
  assert_eq "$(claim_state)" "idle" "the step runs to idle"
  local req
  req="$(last_req)"
  assert_eq "$(jq -r '.input[0].content' <<<"$req")" "do the task" \
    "the model receives the unmodified request"
  assert_eq "$(count_markers "hook.model.before")" 0 "no decision marker"
  assert_eq "$(count_markers "hook_applied")" 0 "no cache-break marker"
}

# ── Scenario 3: the malformed transform payload ───────────────────
# A transform decision without the object `request` field is a
# non-blocking failure: the original request proceeds, and the log
# carries the error marker.
scenario_malformed() {
  NEW_WORK malformed
  work_config 1
  seed_session
  make_stub
  make_hook_malformed
  run_step
  assert_eq "$(claim_state)" "idle" "the step runs to idle"
  local req
  req="$(last_req)"
  assert_eq "$(jq -r '.input[0].content' <<<"$req")" "do the task" \
    "the model receives the unmodified request"
  assert_eq "$(count_markers "hook.model.before.error")" 1 "one error marker"
  assert_eq "$(count_markers "hook_applied")" 0 "no cache-break marker"
}

scenario_transform
scenario_default
scenario_malformed

echo
echo "model-before-transform-e2e: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
