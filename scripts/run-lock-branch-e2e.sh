#!/usr/bin/env bash
# The `rushi run` lock-branch e2e (docs/itches.md, 2026-10-06).
#
# Verifies that a plain `rushi run <session> <task>` is the single
# canonical poke of a session, branching on the session lock:
#
#   - it appends the steer user_message against a live loop and exits
#     0 (the live loop drains the message at its next step)
#   - a task-less `rushi run` against a live loop exits 0 without
#     appending anything
#   - `--no-run` is gone: it now fails as an unknown flag
#   - it starts the loop on a dead session (the old full path)
#   - `user --no-run` stays the append-only form
#
# A live loop (stub model, one slow turn) holds the lock during the
# pokes, matching the monitor pattern in docs/reference/monitoring.md.

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

# ── The work config (same shape as steer-inflight-e2e.sh) ──────────

WORK="$ROOT/scratch/e2e-run-lock-branch"
rm -rf "$WORK"
SESSIONS_DIR="$WORK/sessions/session"
SLOG="$SESSIONS_DIR/events.jsonl"
REQLOG="$WORK/reqlog"
mkdir -p "$SESSIONS_DIR"
: > "$SLOG"

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
native_tool_paths = ["$ROOT/tools"]

[limits]
context_budget_tokens = 8000
compact_reserve_tokens = 500
compact_keep_tokens = 2000
compact_enabled = false

[system_prompt]
text = "test"
EOF

# One slow model turn so the loop holds the lock long enough to poke.
cat > "$WORK/stub-model" <<'STUB'
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
STUB
chmod +x "$WORK/stub-model"

export CONFIG="$WORK/config.toml"
export MODEL_BIN="$WORK/stub-model"
export STUB_REQLOG="$REQLOG"
: > "$REQLOG"
cd "$WORK"

# Seed the initial task, then start the live loop in the background.
# This is the dead branch: no lock is held, so the call starts the loop.
echo '{"v":1,"type":"user_message","ts":"t0","seq":1,"content":"first task"}' > "$SLOG"
"$BIN_DIR/rushi" run session >"$WORK/run.log" 2>"$WORK/run.err" &
RUN_PID=$!

# Wait for the first model call to be in flight (lock is held now).
for _ in $(seq 1 100); do
  if rg -q '"model_call_context"' "$SLOG" 2>/dev/null; then
    break
  fi
  sleep 0.1
done
if ! rg -q '"model_call_context"' "$SLOG" 2>/dev/null; then
  echo "FAIL: the live loop never reached a model call"
  kill "$RUN_PID" 2>/dev/null
  exit 1
fi

# ── 1. Plain run with a task against a held lock: append, exit 0 ───
out="$("$BIN_DIR/rushi" run session "poke message" 2>&1)"
rc=$?
if [ "$rc" -eq 0 ] && echo "$out" | rg -q '"content":"poke message"' \
   && echo "$out" | rg -q 'session loop is alive'; then
  ok "plain run against a held lock appends and exits 0 (alive notice)"
else
  ko "plain run against held lock: rc=$rc, output: $out"
fi

if rg -q '"content":"poke message"' "$SLOG"; then
  ok "the poke was appended to the live session log"
else
  ko "the poke is missing from the live session log"
fi

if [ -e "$SESSIONS_DIR/loop.pid" ] && rg -q "^$RUN_PID$" "$SESSIONS_DIR/loop.pid" 2>/dev/null; then
  ok "loop.pid still names the live loop while the pokes ran"
else
  ko "loop.pid missing or changed while the live loop runs"
fi

# ── 2. Task-less run against a held lock: notice, nothing appended ──
USERS_BEFORE=$(rg -c '"type":"user_message"' "$SLOG" || true)
out="$("$BIN_DIR/rushi" run session 2>&1)"
rc=$?
if [ "$rc" -eq 0 ] && echo "$out" | rg -q 'nothing to start'; then
  ok "task-less run against a held lock exits 0 with a notice"
else
  ko "task-less run against held lock: rc=$rc, output: $out"
fi
USERS_AFTER=$(rg -c '"type":"user_message"' "$SLOG" || true)
if [ "$USERS_BEFORE" = "$USERS_AFTER" ]; then
  ok "the task-less run appended no message"
else
  ko "task-less run changed the user_message count ($USERS_BEFORE -> $USERS_AFTER)"
fi

# ── 3. --no-run is gone: it fails as an unknown flag ───────────────
out="$("$BIN_DIR/rushi" run session "x" --no-run 2>&1)"
rc=$?
if [ "$rc" -eq 2 ] && echo "$out" | rg -q "unexpected argument '--no-run'"; then
  ok "--no-run fails as an unknown flag (rc=2)"
else
  ko "--no-run: rc=$rc, output: $out"
fi

# ── 4. An empty task fails instead of poking or running ────────────
out="$("$BIN_DIR/rushi" run session "" 2>&1)"
rc=$?
if [ "$rc" -eq 1 ] && echo "$out" | rg -q "task must not be empty"; then
  ok "an empty task fails with the empty-task message"
else
  ko "empty task: rc=$rc, output: $out"
fi

# Wait for the live loop to finish (it drains the poke at step 2).
for _ in $(seq 1 100); do
  if ! kill -0 "$RUN_PID" 2>/dev/null; then
    break
  fi
  sleep 0.1
done
if kill -0 "$RUN_PID" 2>/dev/null; then
  kill "$RUN_PID" 2>/dev/null
  ko "the live loop did not finish in time"
fi
wait "$RUN_PID" 2>/dev/null

# Two model calls: the second one must carry the poked message.
REQS=$(wc -l < "$REQLOG")
SECOND=$(sed -n '2p' "$REQLOG")
if [ "$REQS" -eq 2 ] && echo "$SECOND" | rg -q "poke message"; then
  ok "the live loop drained the poked message (2 model calls)"
else
  ko "drain check: ${REQS} model calls, second: ${SECOND:-none}"
fi

# ── 5. Dead session: a plain run starts the loop ───────────────────
rm -f "$SESSIONS_DIR/loop.pid"
out="$("$BIN_DIR/rushi" run session "seed task" 2>&1)"
rc=$?
if [ "$rc" -eq 0 ] && [ -e "$SESSIONS_DIR/loop.pid" ]; then
  ok "a plain run on a dead session starts the loop (loop.pid written)"
else
  lp_exists=no
  [ -e "$SESSIONS_DIR/loop.pid" ] && lp_exists=yes
  ko "dead session run: rc=$rc, loop.pid exists: $lp_exists, output: $out"
fi

REQS=$(wc -l < "$REQLOG")
THIRD=$(sed -n '3p' "$REQLOG")
if [ "$REQS" -eq 3 ] && echo "$THIRD" | rg -q "seed task"; then
  ok "the started loop ran the seeded task (3 model calls)"
else
  ko "seed check: ${REQS} model calls, third: ${THIRD:-none}"
fi

# ── 6. user --no-run stays the append-only form ────────────────────
REQS_BEFORE=$REQS
out="$("$BIN_DIR/user" --session session --no-run "user poke" 2>&1)"
rc=$?
if [ "$rc" -eq 0 ] && rg -q '"content":"user poke"' "$SLOG"; then
  ok "user --no-run appends to the session log"
else
  ko "user --no-run: rc=$rc, output: $out"
fi
REQS_AFTER=$(wc -l < "$REQLOG")
if [ "$REQS_BEFORE" = "$REQS_AFTER" ]; then
  ok "user --no-run started no loop (model call count unchanged)"
else
  ko "user --no-run changed the model call count ($REQS_BEFORE -> $REQS_AFTER)"
fi

echo
echo "run-lock-branch-e2e: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
