#!/usr/bin/env bash
# The tool.after transform e2e (docs/image-read-kiss.md section 4.5).
# Drives `rushi step` with a scriptable stub model and a jq-based
# tool.after hook. Asserts the transform decision splices the
# replaced result into the log, that unmentioned results pass
# through, and the byte-identical no-hooks default.

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
window  = "tool.after"
command = "$WORK/hook-tool-after"
args    = []
EOF
  fi
}

# ── The session builder ───────────────────────────────────────────
seed_session() {
  cat > "$SLOG" <<EOF
{"v":1,"type":"user_message","ts":"t1","seq":1,"content":"read the file"}
EOF
}

# ── The stub model ─────────────────────────────────────────────────
# First call: emit a tool_call for `read`. Second call: final answer.
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
n=$(rg -c '"type":"tool_result"' <<<"$req" 2>/dev/null || echo 0)
if [[ "$n" -eq 0 ]]; then
  echo '{"text":"","tool_calls":[{"id":"call-1","name":"read","arguments":"{\"file_path\":\"/nonexistent\"}"}],"reasoning":[],"stop_reason":"tool_calls","usage":{"input_tokens":10,"output_tokens":10}}'
else
  echo '{"text":"done","tool_calls":[],"reasoning":[],"stop_reason":"stop","usage":{"input_tokens":20,"output_tokens":10}}'
fi
EOF
  chmod +x "$WORK/stub-model"
}

# ── The tool.after hook ───────────────────────────────────────────
# Replaces the result for call-1 with an image result. Unmentioned
# calls pass through untouched.
make_hook() {
  cat > "$WORK/hook-tool-after" <<'EOF'
#!/usr/bin/env bash
set -u
payload=$(cat)
jq -cn '{
  decision: "transform",
  payload: {
    results: {
      "call-1": {
        "v": 1,
        "type": "tool_result",
        "id": "call-1",
        "ts": "2026-01-01T00:00:00Z",
        "value": {
          "text": "Read image file [image/png] (4242 bytes)",
          "details": {
            "type": "image",
            "mime_type": "image/png",
            "data": "AAAA",
            "path": "/nonexistent"
          }
        },
        "is_error": false
      }
    },
    reason: "e2e: replaced read result with image"
  }
}'
EOF
  chmod +x "$WORK/hook-tool-after"
}

run_step() {
  (
    cd "$WORK"
    export CONFIG="$WORK/config.toml"
    export MODEL_BIN="$WORK/stub-model"
    export STUB_REQLOG="$WORK/reqlog"
    : >"$STUB_REQLOG"
    # One step: model call, tool routing, then the tool.after hook.
    # The step lands at awaiting_model. The next model call is owed.
    "$BIN_DIR/rushi" step session
  ) >/dev/null 2>&1
  true
}

claim_state() {
  "$BIN_DIR/claim" --session "$SESSIONS_DIR" | jq -r .state
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
  WORK="$ROOT/scratch/e2e-tool-after/$1"
  rm -rf "$WORK"
  SESSIONS_DIR="$WORK/sessions/session"
  SLOG="$SESSIONS_DIR/events.jsonl"
  STUB_REQLOG="$WORK/reqlog"
  mkdir -p "$SESSIONS_DIR"
  echo "==== scenario: $1"
}

# ── Scenario 1: the transform decision ────────────────────────────
# The hook replaces the call-1 result. The log carries the replaced
# result, not the original error.
scenario_transform() {
  NEW_WORK transform
  work_config 1
  seed_session
  make_stub
  make_hook
  run_step
  assert_eq "$(claim_state)" "awaiting_model" "step lands at awaiting_model (tool result logged)"
  # The log should contain the replaced result, not the original error.
  local replaced
  replaced=$(jq -c 'select(.type == "tool_result" and .id == "call-1")' "$SLOG" 2>/dev/null)
  assert_eq "$(jq -r '.value.details.type' <<<"$replaced")" \
    "image" \
    "the tool_result in the log is the replaced image result"
  assert_eq "$(jq -r '.value.details.mime_type' <<<"$replaced")" \
    "image/png" \
    "the replaced result carries the mime_type"
  assert_eq "$(jq -r '.is_error' <<<"$replaced")" \
    "false" \
    "the replaced result is not an error"
  # The decision marker is logged.
  assert_eq "$(count_markers "hook.tool.after")" 1 "one hook.tool.after marker"
  assert_eq "$(jq -r "select(.type == \"ext_status\" and .id == \"hook.tool.after\") | .value" "$SLOG")" \
    "transform" "the decision marker carries the decision word"
}

# ── Scenario 2: the no-hooks default ──────────────────────────────
# No hook registered: the tool result goes through untouched.
scenario_default() {
  NEW_WORK default
  work_config ""
  seed_session
  make_stub
  run_step
  assert_eq "$(claim_state)" "awaiting_model" "step lands at awaiting_model (tool result logged)"
  local orig
  orig=$(jq -c 'select(.type == "tool_result" and .id == "call-1")' "$SLOG" 2>/dev/null)
  assert_eq "$(jq -r '.is_error' <<<"$orig")" \
    "true" \
    "the original error result is in the log"
  assert_eq "$(count_markers "hook.tool.after")" 0 "no decision marker"
}

# ── Scenario 3: the no-op transform ───────────────────────────────
# The hook emits a transform with an empty results map: nothing is
# replaced, the original results stand.
scenario_noop() {
  NEW_WORK noop
  work_config 1
  seed_session
  make_stub
  cat > "$WORK/hook-tool-after" <<'EOF'
#!/usr/bin/env bash
set -u
cat > /dev/null
echo '{"decision":"transform","payload":{"results":{}}}'
EOF
  chmod +x "$WORK/hook-tool-after"
  run_step
  assert_eq "$(claim_state)" "awaiting_model" "step lands at awaiting_model (tool result logged)"
  local orig
  orig=$(jq -c 'select(.type == "tool_result" and .id == "call-1")' "$SLOG" 2>/dev/null)
  assert_eq "$(jq -r '.is_error' <<<"$orig")" \
    "true" \
    "the empty transform leaves the original result"
  assert_eq "$(count_markers "hook.tool.after")" 1 "one hook.tool.after marker"
}

scenario_transform
scenario_default
scenario_noop

echo
echo "tool-after-transform-e2e: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
