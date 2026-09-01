#!/usr/bin/env bash
set -euo pipefail

# Cache e2e test — key-gated
# Runs only when DEEPSEEK_API_KEY is set.
# Verifies that the append-only log and deterministic projection
# produce byte-identical prefixes that hit the DeepSeek provider cache.

if [ -z "${DEEPSEEK_API_KEY:-}" ]; then
  echo "SKIP: DEEPSEEK_API_KEY not set." >&2
  exit 0
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BIN_DIR="$(cd "$SCRIPT_DIR/../target/debug" && pwd)"
TOOL_DIR="$(cd "$SCRIPT_DIR/../tools" && pwd)"
SCHEMA_DIR="$(cd "$SCRIPT_DIR/../schemas/events/v1" && pwd)"
CONFIG="$SCRIPT_DIR/../config.toml"

SESSION="cache-test"
SESSION_DIR="$SCRIPT_DIR/../sessions/$SESSION"

# Cleanup from previous runs
rm -rf "$SESSION_DIR"
mkdir -p "$SESSION_DIR"

echo "=== Turn 1 ==="

# Create initial session with a user message that forces a tool call
TS1=$(date -u +%Y-%m-%dT%H:%M:%SZ)
echo "{\"v\":1,\"type\":\"user_message\",\"ts\":\"$TS1\",\"content\":\"Read config.toml and tell me the model name.\"}" > "$SESSION_DIR/events.jsonl"

# Run turn 1 (will call model API, parse, route, log)
"$SCRIPT_DIR/step.sh" "$SESSION"

# The step publishes the loop phase as an ext_status marker
# (docs/tui-model-wait-indicator.md). The session log must hold at
# least one loop_phase event. This check is the mutation gate:
# removing the emit helper from step.sh fails it.
if ! jq -e 'select(.type == "ext_status" and .id == "loop_phase")' "$SESSION_DIR/events.jsonl" > /dev/null; then
  echo "FAIL: no loop_phase marker in the session log." >&2
  exit 1
fi
PHASES=$(jq -r 'select(.type == "ext_status" and .id == "loop_phase") | .value' "$SESSION_DIR/events.jsonl" | sort -u | tr '\n' ' ')
echo "Turn 1 loop_phase markers: $PHASES"

# Extract cached_tokens from turn 1 assistant_message events
TURN1_CACHED=$(jq -r 'select(.type == "assistant_message") | .usage.cached_tokens // 0' "$SESSION_DIR/events.jsonl" | tail -1)
echo "Turn 1 cached_tokens: $TURN1_CACHED"

echo ""
echo "=== Turn 2 ==="

# Append a new user message (turn 1 prefix stays byte-identical)
TS2=$(date -u +%Y-%m-%dT%H:%M:%SZ)
echo "{\"v\":1,\"type\":\"user_message\",\"ts\":\"$TS2\",\"content\":\"What was the model name you found?\"}" >> "$SESSION_DIR/events.jsonl"

# Run turn 2 — retry up to 5 times while the provider cache constructs
TURN2_CACHED=0
for i in 1 2 3 4 5; do
  echo "Retry $i..."
  "$SCRIPT_DIR/step.sh" "$SESSION"

  # Check cached_tokens in turn 2
  TURN2_CACHED=$(jq -r 'select(.type == "assistant_message") | .usage.cached_tokens // 0' "$SESSION_DIR/events.jsonl" | tail -1)
  echo "Turn 2 cached_tokens: $TURN2_CACHED"

  if [ "$TURN2_CACHED" -gt 0 ] 2>/dev/null; then
    echo ""
    echo "PASS: Cache hit detected (cached_tokens=$TURN2_CACHED)."
    exit 0
  fi

  # Wait for provider cache to construct
  sleep 3
done

echo ""
echo "FAIL: No cache hit detected after 5 retries." >&2
exit 1
