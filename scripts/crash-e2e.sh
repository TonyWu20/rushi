#!/usr/bin/env bash

# G2a crash-consistency e2e (kill -9 log, route, claim).

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN="$ROOT/target/debug"

cargo build --quiet || { echo "FAIL: cargo build"; exit 1; }

LOG="$BIN/log"
CLAIM="$BIN/claim"
PARSE="$BIN/parse"
ROUTE="$BIN/route"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); }
ko() { FAIL=$((FAIL + 1)); echo "FAIL: $1"; }

# A checks whole-line integrity, plus a simulated torn tail.
cat > "$WORK/config.toml" <<EOF
[paths]
native_tool_paths = [
  "$ROOT/tools/bash",
  "$ROOT/tools/read",
  "$ROOT/tools/write",
  "$ROOT/tools/edit",
]
EOF

check_lines() {
  python3 - "$1/events.jsonl" <<'PY'
import json, os, sys
path = sys.argv[1]
if not os.path.exists(path):
    print("bad=0 last=whole")
    sys.exit(0)
lines = [l for l in open(path).read().split("\n") if l]
bad = 0
for l in lines[:-1]:
    try:
        json.loads(l)
    except Exception:
        bad += 1
last = "whole"
if lines:
    try:
        json.loads(lines[-1])
    except Exception:
        last = "truncated"
print(f"bad={bad} last={last}")
PY
}

python3 - "$WORK/batch.jsonl" 5000 <<'PY'
import sys
out, n = sys.argv[1], int(sys.argv[2])
with open(out, "w") as f:
    for i in range(n):
        f.write('{"v":1,"type":"tool_result","ts":"t","id":"z%d","value":{"text":"%s"},"is_error":false}\n' % (i, "x" * 400))
PY

SA="$WORK/sA"
"$LOG" --session "$SA" < "$WORK/batch.jsonl" &
LPID=$!
sleep 0.1
kill -9 "$LPID" 2>/dev/null
wait "$LPID" 2>/dev/null
OUT_A=$(check_lines "$SA")
case "$OUT_A" in
  bad=0*) ok "A1: kill -9 of log leaves no torn line" ;;
  *) ko "A1: torn lines after the kill ($OUT_A)" ;;
esac

printf '{"v":1,"type":"user_message","ts":"t","content":"after kill"}\n' |
  "$LOG" --session "$SA" && ok "A2: log still accepts appends after the kill" ||
  ko "A2: append after the kill failed"
C_A=$("$CLAIM" --session "$SA" 2>/dev/null)
case "$C_A" in
  *'"state":'*) ok "A3: claim re-derives state after the kill" ;;
  *) ko "A3: claim broken after the kill: $C_A" ;;
esac

SA2="$WORK/sA2"
mkdir -p "$SA2"
printf '{"v":1,"type":"user_message","ts":"t","content":"task"}\n' > "$SA2/events.jsonl"
printf '{"v":1,"type":"tool_result","ts":"t","id":"tc-x","value":{"te' >> "$SA2/events.jsonl"
C_A2=$("$CLAIM" --session "$SA2" 2>/dev/null)
case "$C_A2" in
  *'"state":"awaiting_model"'*) ok "A4: claim skips the truncated tail line" ;;
  *) ko "A4: claim stuck on the truncated line: $C_A2" ;;
esac
printf '{"v":1,"type":"assistant_message","ts":"t","content":"done","tool_calls":[],"stop_reason":"stop"}\n' |
  "$LOG" --session "$SA2" && ok "A5: appends still work over a truncated tail" ||
  ko "A5: append over the truncated tail failed"

# B is the danger zone: the tool result is owed after the kill.
S3="$WORK/sB"
printf '{"v":1,"type":"user_message","ts":"t","content":"run the slow task"}\n' |
  "$LOG" --session "$S3"

cat > "$WORK/model.json" <<'EOF'
{"text":"","tool_calls":[{"id":"tc-slow","name":"bash","arguments":{"command":"sleep 2 && echo done","timeout_secs":60}}],"stop_reason":"tool_calls"}
EOF
"$PARSE" --config "$WORK/config.toml" < "$WORK/model.json" > "$WORK/parsed.jsonl"
grep '"type":"tool_call"' "$WORK/parsed.jsonl" > "$WORK/toolcalls.jsonl"
"$LOG" --session "$S3" < "$WORK/parsed.jsonl"

"$ROUTE" --native-tool-path "$ROOT/tools/bash" --cwd "$WORK" < "$WORK/toolcalls.jsonl" > "$WORK/first_route.out" &
RPID=$!
sleep 0.5
kill -9 "$RPID"
wait "$RPID" 2>/dev/null
sleep 3

grep -q '"type":"tool_call"' "$S3/events.jsonl" || ko "B1: the tool_call is missing from the log"
if grep -q '"type":"tool_result"' "$S3/events.jsonl"; then
  ko "B2: a tool_result survived the kill"
else
  ok "B2: no tool_result after the kill"
fi
C_B=$("$CLAIM" --session "$S3")
if grep -q '"state":"awaiting_tool_result"' <<<"$C_B" && grep -q '"tc-slow"' <<<"$C_B"; then
  ok "B3: claim still owes the result after the kill"
else
  ko "B3: claim lost the owed call: $C_B"
fi

"$ROUTE" --native-tool-path "$ROOT/tools/bash" --cwd "$WORK" < "$WORK/toolcalls.jsonl" > "$WORK/second_route.out"
grep '"type":"tool_result"' "$WORK/second_route.out" | "$LOG" --session "$S3"
N_RES=$(grep '"type":"tool_result"' "$S3/events.jsonl" | grep -c '"id":"tc-slow"' || true)
if [ "$N_RES" = "1" ]; then
  ok "B4: exactly one tool_result after recovery"
else
  ko "B4: $N_RES tool_result lines for tc-slow"
fi
C_B2=$("$CLAIM" --session "$S3")
case "$C_B2" in
  *'"state":"awaiting_model"'*) ok "B5: the recovered step owes the next model call" ;;
  *) ko "B5: state after recovery: $C_B2" ;;
esac

# C and D are read-only checks on the log bytes and state.
S4="$WORK/sC"
mkdir -p "$S4"
python3 - "$S4" <<'PY'
import os, sys
d = sys.argv[1]
with open(os.path.join(d, "events.jsonl"), "w") as f:
    for i in range(2000):
        f.write('{"v":1,"type":"ext_status","ts":"t","id":"tick","value":{"n":%d}}\n' % i)
    f.write('{"v":1,"type":"user_message","ts":"t","content":"task"}\n')
PY
H1=$(sha256sum "$S4/events.jsonl" | cut -d' ' -f1)
"$CLAIM" --session "$S4" > /dev/null &
CPID=$!
sleep 0.01
kill -9 "$CPID" 2>/dev/null
wait "$CPID" 2>/dev/null
H2=$(sha256sum "$S4/events.jsonl" | cut -d' ' -f1)
if [ "$H1" = "$H2" ]; then
  ok "C1: a claim crash changes no log bytes"
else
  ko "C1: the log bytes changed during the claim crash"
fi
C_C=$("$CLAIM" --session "$S4" 2>/dev/null)
case "$C_C" in
  *'"state":"awaiting_model"'*) ok "C2: claim re-derives the owed state" ;;
  *) ko "C2: claim state after the kill: $C_C" ;;
esac

python3 - "$S3/events.jsonl" <<'PY'
import json, sys
calls, results, terminal = set(), set(), False
for line in open(sys.argv[1]):
    try:
        ev = json.loads(line)
    except Exception:
        continue
    t = ev.get("type")
    if t == "tool_call":
        calls.add(ev.get("id"))
    elif t == "tool_result":
        results.add(ev.get("id"))
    elif t in ("error", "context_exhausted"):
        terminal = True
unresolved = calls - results
if unresolved and not terminal:
    print("UNRESOLVED:", sorted(unresolved))
    sys.exit(1)
print("OK")
PY
if [ $? -eq 0 ]; then
  ok "D1: every tool_call has one tool_result or a terminal error"
else
  ko "D1: dangling tool calls remain"
fi

echo
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
