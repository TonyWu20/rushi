#!/usr/bin/env bash
# The P7 steer-during-inflight-call e2e
# (docs/tui-pending-user-messages.md P7).
#
# Verifies that a steer message logged while a model call is in flight
# keeps the loop in `awaiting_model` (not `idle`), and that the next
# step delivers it to the model.

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
# Compact disabled; a single small turn stays far under the trigger.

WORK="$ROOT/scratch/e2e-steer-inflight"
rm -rf "$WORK"
SESSIONS_DIR="$WORK/sessions/session"
SLOG="$SESSIONS_DIR/events.jsonl"
STUB_REQLOG="$WORK/reqlog"
mkdir -p "$SESSIONS_DIR"
: >"$SLOG"

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

# ── The slow stub model ───────────────────────────────────────────
# Sleeps 3s before answering, so the test can inject a steer message
# while the call is in flight. Records every request on STUB_REQLOG.

cat > "$WORK/stub-model" <<'EOF'
#!/usr/bin/env bash
set -u
if [[ "${1:-}" == "--describe" ]]; then
  echo '{"active":"stub","model_id":"stub-model","reasoning_effort":"none","thinking_level":"off"}'
  exit 0
fi
req=$(cat)
printf '%s\n' "$req" >>"${STUB_REQLOG:-/dev/null}"
sleep 3
echo '{"text":"done","tool_calls":[],"reasoning":[],"stop_reason":"stop","usage":{"input_tokens":10,"output_tokens":10}}'
EOF
chmod +x "$WORK/stub-model"

# ── The scenario ──────────────────────────────────────────────────
# 1. Seed one user message.
# 2. Run `rushi run` in the background with the slow model.
# 3. Wait for the first model_call_context marker (the model call
#    has started and the delivery boundary is recorded).
# 4. Append a steer user message to the log while the model is in
#    flight.
# 5. Wait for the run to finish.
# 6. Verify: two model calls, the second one carries the steer
#    message, and the final claim is idle.

echo '{"v":1,"type":"user_message","ts":"t1","seq":1,"content":"do the task"}' > "$SLOG"

(
  cd "$WORK"
  export CONFIG="$WORK/config.toml"
  export MODEL_BIN="$WORK/stub-model"
  export STUB_REQLOG="$STUB_REQLOG"
  : >"$STUB_REQLOG"
  "$BIN_DIR/rushi" run session
) &
RUN_PID=$!

# Wait for the first model_call_context marker (model call started).
for _ in $(seq 1 100); do
  if grep -q '"model_call_context"' "$SLOG" 2>/dev/null; then
    break
  fi
  sleep 0.1
done

if ! grep -q '"model_call_context"' "$SLOG" 2>/dev/null; then
  ko "the model_call_context marker never appeared"
fi

# Inject the steer message while the model is in flight.
echo '{"v":1,"type":"user_message","ts":"t2","seq":2,"content":"steer: use the new API"}' >> "$SLOG"

# Wait for the run to finish (up to 30s).
for _ in $(seq 1 300); do
  if ! kill -0 "$RUN_PID" 2>/dev/null; then
    break
  fi
  sleep 0.1
done
if kill -0 "$RUN_PID" 2>/dev/null; then
  kill "$RUN_PID" 2>/dev/null
  wait "$RUN_PID" 2>/dev/null
  ko "the run did not finish in time"
fi
wait "$RUN_PID" 2>/dev/null

# ── Assertions ────────────────────────────────────────────────────

# Two model calls ran.
REQS=$(wc -l < "$STUB_REQLOG" 2>/dev/null || echo 0)
if [ "$REQS" -eq 2 ]; then
  ok "two model calls ran (one per step)"
else
  ko "expected 2 model calls, got $REQS"
fi

# The second request carries the steer message.
if [ "$REQS" -ge 2 ]; then
  SECOND=$(sed -n '2p' "$STUB_REQLOG")
  if echo "$SECOND" | grep -q "steer: use the new API"; then
    ok "the second model call carries the steer message"
  else
    ko "the second model call does not carry the steer message"
  fi
fi

# Two model_call_context markers, the second with a higher user_seq.
M1=$(jq -c 'select(.type=="ext_status" and .id=="model_call_context") | .value.user_seq' "$SLOG" 2>/dev/null | sed -n '1p')
M2=$(jq -c 'select(.type=="ext_status" and .id=="model_call_context") | .value.user_seq' "$SLOG" 2>/dev/null | sed -n '2p')
if [ -n "$M1" ] && [ -n "$M2" ] && [ "$M2" -gt "$M1" ]; then
  ok "the second marker carries a higher user_seq ($M1 → $M2)"
else
  ko "marker user_seq values: first=${M1:-none} second=${M2:-none}"
fi

# The final claim is idle: both steer messages were delivered.
STATE=$("$BIN_DIR/claim" --session "$SESSIONS_DIR" | jq -r .state 2>/dev/null)
if [ "$STATE" = "idle" ]; then
  ok "the final claim is idle"
else
  ko "the final claim is $STATE, want idle"
fi

echo
echo "steer-inflight-e2e: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
