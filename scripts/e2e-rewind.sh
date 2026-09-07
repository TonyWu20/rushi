#!/usr/bin/env bash
# The rewind/fork e2e suite (docs/rewind-fork-design.md section 9).
# It drives the real `log`, `claim`, and `assemble` binaries over
# synthetic forked sessions and asserts:
#   A. schema acceptance of the `rewind` marker through `bin/log`,
#      and the append-only discipline (P7: the prefix bytes are
#      untouched by the append)
#   B. the nested-fork mask (P1/P2: branch B is out of the A'
#      context — the single-gap rule's counter-example)
#   C. branch re-entry (P3: B's full active path is rebuilt, A'
#      masked)
#   D. the boundary degrade (P5: a target inside the compacted
#      region degrades; the compacted region stays out)
#   E. the pair-stranding guard (P4: a marker whose context strands
#      a call/result pair is ignored, the branch re-projects linear)
#   F. depth-3 fork composition (P2 beyond depth 2: three nested
#      rewinds compose; every abandoned intermediate span stays out)

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN_DIR="$ROOT/target/debug"

cargo build --quiet || { echo "FAIL: cargo build"; exit 1; }

cd "$ROOT"

LOG_BIN="$BIN_DIR/log"
CLAIM_BIN="$BIN_DIR/claim"
ASSEMBLE_BIN="$BIN_DIR/assemble"
SCHEMAS="$ROOT/schemas/events/v1"
CONFIG="$ROOT/config.toml"

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); }
ko() {
  FAIL=$((FAIL + 1))
  echo "FAIL: $1"
}

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# The model input of one assemble run, as flat text the assertions
# grep. Tag names the $WORK/$tag.out / $tag.err files. A
# context_exhausted form (a misfired budget gate) prints a marker
# the assertions treat as a failure.
assemble_input() {
  "$ASSEMBLE_BIN" --session "$1" --config "$CONFIG" 2>"$WORK/$2.err" >"$WORK/$2.out"
  python3 - "$WORK/$2.out" <<'PY'
import json, sys
req = json.load(open(sys.argv[1]))
if req.get("type") == "context_exhausted":
    print("__context_exhausted__")
    sys.exit(0)
def flat(v):
    if isinstance(v, str):
        return v
    if isinstance(v, dict):
        for k in ("content", "output", "arguments"):
            if k in v:
                return flat(v[k])
    return ""
print(" \n".join(flat(i) for i in req["input"]))
PY
}

# ── A. schema acceptance and the append-only discipline ─────────

mkdir -p "$WORK/sA"
cat > "$WORK/inA.jsonl" <<'EOF'
{"v":1,"type":"user_message","ts":"t","content":"task"}
{"v":1,"type":"rewind","ts":"t","target_seq":1,"mode":"before","reason":"tui_pick"}
EOF
if "$LOG_BIN" --session "$WORK/sA" --schemas "$SCHEMAS" < "$WORK/inA.jsonl"; then
  ok "A: log accepts the valid rewind marker"
else
  ko "A: log rejected the valid rewind marker"
fi
# The marker landed as a line, behind the user message.
if grep -q '"type":"rewind"' "$WORK/sA/events.jsonl"; then
  ok "A: the marker is in the log"
else
  ko "A: the marker is missing from the log"
fi

# The shape guards: a missing required `mode` is rejected.
if printf '%s\n' '{"v":1,"type":"rewind","ts":"t","target_seq":1}' |
  "$LOG_BIN" --session "$WORK/sA" --schemas "$SCHEMAS" 2>/dev/null; then
  ko "A: the mode-less marker must be rejected"
else
  ok "A: the mode-less marker is rejected"
fi
# A non-integer target_seq is rejected.
if printf '%s\n' '{"v":1,"type":"rewind","ts":"t","target_seq":"one","mode":"on"}' |
  "$LOG_BIN" --session "$WORK/sA" --schemas "$SCHEMAS" 2>/dev/null; then
  ko "A: the string target_seq must be rejected"
else
  ok "A: the string target_seq is rejected"
fi

# P7: the append adds exactly one line, the earlier bytes intact.
mkdir -p "$WORK/sP7"
printf '%s\n' '{"v":1,"type":"user_message","ts":"t","content":"task"}' > "$WORK/sP7/events.jsonl"
SIZE_BEFORE=$(wc -c < "$WORK/sP7/events.jsonl")
HASH_BEFORE=$(sha256sum "$WORK/sP7/events.jsonl" | cut -d' ' -f1)
printf '%s\n' '{"v":1,"type":"rewind","ts":"t","target_seq":1,"mode":"before"}' |
  "$LOG_BIN" --session "$WORK/sP7" --schemas "$SCHEMAS" || ko "P7: the append failed"
HASH_PREFIX=$(head -c "$SIZE_BEFORE" "$WORK/sP7/events.jsonl" | sha256sum | cut -d' ' -f1)
if [ "$HASH_PREFIX" = "$HASH_BEFORE" ]; then
  ok "P7: the prefix bytes are untouched by the append"
else
  ko "P7: the append rewrote earlier bytes"
fi

# ── B. the nested-fork mask (P1/P2) ─────────────────────────────

# Log layout (seq = line number): 1..3 branch A, rewind(4,3) forks
# B (5..6), rewind(7,3) forks A' (8..9), rewind(10,9) continues A'
# (11..12).
mkdir -p "$WORK/sB"
cat > "$WORK/sB/events.jsonl" <<'EOF'
{"v":1,"type":"user_message","ts":"t","content":"task"}
{"v":1,"type":"assistant_message","ts":"t","content":"","tool_calls":[{"id":"ca","name":"bash","arguments":{"command":"a"}}],"stop_reason":"tool_calls"}
{"v":1,"type":"tool_result","ts":"t","id":"ca","value":{"text":"A result"},"is_error":false}
{"v":1,"type":"rewind","ts":"t","target_seq":3,"mode":"on"}
{"v":1,"type":"assistant_message","ts":"t","content":"","tool_calls":[{"id":"cb","name":"bash","arguments":{"command":"b"}}],"stop_reason":"tool_calls"}
{"v":1,"type":"tool_result","ts":"t","id":"cb","value":{"text":"B result"},"is_error":false}
{"v":1,"type":"rewind","ts":"t","target_seq":3,"mode":"on"}
{"v":1,"type":"assistant_message","ts":"t","content":"","tool_calls":[{"id":"ca2","name":"bash","arguments":{"command":"a2"}}],"stop_reason":"tool_calls"}
{"v":1,"type":"tool_result","ts":"t","id":"ca2","value":{"text":"A' result"},"is_error":false}
{"v":1,"type":"rewind","ts":"t","target_seq":9,"mode":"on"}
{"v":1,"type":"assistant_message","ts":"t","content":"","tool_calls":[{"id":"ca3","name":"bash","arguments":{"command":"a3"}}],"stop_reason":"tool_calls"}
{"v":1,"type":"tool_result","ts":"t","id":"ca3","value":{"text":"A' done"},"is_error":false}
EOF
CLAIM_B=$("$CLAIM_BIN" --session "$WORK/sB" --schemas "$SCHEMAS")
if grep -q '"state":"awaiting_model"' <<<"$CLAIM_B"; then
  ok "B: claim owes the model call on the finished A' step"
else
  ko "B: claim state is $(grep -o '"state":"[^"]*"' <<<"$CLAIM_B"): $CLAIM_B"
fi
IN_B=$(assemble_input "$WORK/sB" "b")
case "$IN_B" in
  *"A result"*"A' result"*"A' done"*) ok "B: A and A' ride the context" ;;
  *) ko "B: the A/A' events are missing from the input: $IN_B" ;;
esac
if grep -q 'B result' <<<"$IN_B"; then
  ko "B: branch B leaked into the A' context (the single-gap bug)"
else
  ok "B: branch B is masked out of the A' context"
fi

# ── C. branch re-entry (P3) ─────────────────────────────────────

# Re-enter B at its tail: rewind(13,6). The active path is A (1..3)
# plus B (5..6); A' (8..9) and B' (11..12) are masked.
cat >> "$WORK/sB/events.jsonl" <<'EOF'
{"v":1,"type":"rewind","ts":"t","target_seq":6,"mode":"on"}
EOF
CLAIM_C=$("$CLAIM_BIN" --session "$WORK/sB" --schemas "$SCHEMAS")
if grep -q '"state":"awaiting_model"' <<<"$CLAIM_C"; then
  ok "C: claim owes the model call on the finished B step"
else
  ko "C: claim state is wrong after re-entry: $CLAIM_C"
fi
IN_C=$(assemble_input "$WORK/sB" "c")
case "$IN_C" in
  *"A result"*"B result"*) ok "C: B's full active path is rebuilt" ;;
  *) ko "C: B's path is not rebuilt: $IN_C" ;;
esac
if grep -q "A' result" <<<"$IN_C" || grep -q "A' done" <<<"$IN_C"; then
  ko "C: branch A' leaked into the B context"
else
  ok "C: branch A' is masked out of the B context"
fi

# ── F. depth-3 fork composition (P2 beyond depth 2) ─────────────

# Two more markers: 14 re-enters B at its tail (6, a no-op), 15
# forks back to the A' tail (12). The chain 15 -> 12 -> 9 -> 3
# composes three nested rewinds; branch B must stay out of the A'
# context even though it sat in the log between the two A' events.
cat >> "$WORK/sB/events.jsonl" <<'EOF'
{"v":1,"type":"rewind","ts":"t","target_seq":6,"mode":"on"}
{"v":1,"type":"rewind","ts":"t","target_seq":12,"mode":"on"}
EOF
CLAIM_F=$("$CLAIM_BIN" --session "$WORK/sB" --schemas "$SCHEMAS")
if grep -q '"state":"awaiting_model"' <<<"$CLAIM_F"; then
  ok "F: claim owes the model call on the finished A' step"
else
  ko "F: claim state is wrong after the depth-3 fork: $CLAIM_F"
fi
IN_F=$(assemble_input "$WORK/sB" "f")
case "$IN_F" in
  *"A result"*"A' result"*"A' done"*) ok "F: A and A' ride the depth-3 context" ;;
  *) ko "F: the depth-3 context is wrong: $IN_F" ;;
esac
if grep -q 'B result' <<<"$IN_F"; then
  ko "F: branch B leaked into the depth-3 context (the chain broke)"
else
  ok "F: branch B stays masked through the three-level chain"
fi

# ── D. the boundary degrade (P5) ────────────────────────────────

# Events 1..6 are compacted by the boundary at 7 (first_kept_seq
# 8). The rewind at 11 targets seq 3, inside the compacted region:
# it degrades, the compacted region stays out, the framing leads.
mkdir -p "$WORK/sD"
cat > "$WORK/sD/events.jsonl" <<'EOF'
{"v":1,"type":"user_message","ts":"t","content":"task"}
{"v":1,"type":"assistant_message","ts":"t","content":"","tool_calls":[{"id":"c1","name":"bash","arguments":{"command":"x"}}],"stop_reason":"tool_calls"}
{"v":1,"type":"tool_result","ts":"t","id":"c1","value":{"text":"R1 result"},"is_error":false}
{"v":1,"type":"assistant_message","ts":"t","content":"","tool_calls":[{"id":"c2","name":"bash","arguments":{"command":"y"}}],"stop_reason":"tool_calls"}
{"v":1,"type":"tool_result","ts":"t","id":"c2","value":{"text":"R2 result"},"is_error":false}
{"v":1,"type":"assistant_message","ts":"t","content":"step done","tool_calls":[],"stop_reason":"stop"}
{"v":1,"type":"compaction_summary","ts":"t","summary":"the old summary","first_kept_seq":8,"reason":"threshold","tokens_before":0}
{"v":1,"type":"user_message","ts":"t","content":"more"}
{"v":1,"type":"assistant_message","ts":"t","content":"","tool_calls":[{"id":"c3","name":"bash","arguments":{"command":"z"}}],"stop_reason":"tool_calls"}
{"v":1,"type":"tool_result","ts":"t","id":"c3","value":{"text":"R3 result"},"is_error":false}
{"v":1,"type":"rewind","ts":"t","target_seq":3,"mode":"on"}
EOF
IN_D=$(assemble_input "$WORK/sD" "d")
case "$IN_D" in
  *"the old summary"*) ok "D: the framing item still leads" ;;
  *) ko "D: the framing item is missing from the input: $IN_D" ;;
esac
LEAKED=""
for m in "R1 result" "R2 result" "R3 result" "more" "task"; do
  grep -q "$m" <<<"$IN_D" && LEAKED="$LEAKED $m"
done
if [ -z "$LEAKED" ]; then
  ok "D: the compacted and masked regions stay out of the input"
else
  ko "D: masked events leaked into the input:$LEAKED"
fi

# ── E. the pair-stranding guard (P4) ────────────────────────────

# The steer message lands mid-step: between the call and its result.
# A `before`-mode rewind to it masks the result, stranding the call
# in the context: the marker is ignored, the branch re-projects
# linear with the pair intact.
mkdir -p "$WORK/sE"
cat > "$WORK/sE/events.jsonl" <<'EOF'
{"v":1,"type":"user_message","ts":"t","content":"task"}
{"v":1,"type":"assistant_message","ts":"t","content":"","tool_calls":[{"id":"c1","name":"bash","arguments":{"command":"x"}}],"stop_reason":"tool_calls"}
{"v":1,"type":"user_message","ts":"t","content":"steer"}
{"v":1,"type":"tool_result","ts":"t","id":"c1","value":{"text":"R1 result"},"is_error":false}
{"v":1,"type":"rewind","ts":"t","target_seq":3,"mode":"before"}
EOF
CLAIM_E=$("$CLAIM_BIN" --session "$WORK/sE" --schemas "$SCHEMAS")
if grep -q '"state":"idle"' <<<"$CLAIM_E"; then
  ok "E: the before-mode rewind settles the session"
else
  ko "E: claim state after the before-mode rewind: $CLAIM_E"
fi
ERR_E="$WORK/e.err"
IN_E=$(assemble_input "$WORK/sE" "e")
if grep -q 'ignoring rewind at seq 5' "$ERR_E"; then
  ok "E: the stranding marker is ignored with the warning"
else
  ko "E: the warning is missing (stderr: $(cat "$ERR_E" 2>/dev/null))"
fi
case "$IN_E" in
  *"R1 result"*) ok "E: the re-projected pair stays intact" ;;
  *) ko "E: the pair was lost from the input: $IN_E" ;;
esac

echo
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
