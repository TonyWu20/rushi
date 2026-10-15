#!/usr/bin/env bash
# Layered config e2e (docs/project-config-precedence.md,
# consequence table: P-L5 / P-L6 / P-L7 / P-L11).
# A fake Nix layout proves:
#   P-L6    a partial top config inherits the store base
#   P-L7    the store config alone behaves as today
#   P-L11   no-flag launch from a project CWD resolves the CWD config
#           as the top layer (it beats the store side-by-side config)
#   P-L7nf  no flag, no CWD config on a Nix install: the side-by-side
#           tier resolves the store config alone (today's behavior)
#   P-L5c   CWD is the store dir itself: top equals base, the
#           same-file guard parses alone
set -euo pipefail
cd "$(dirname "$0")/.."

BIN=${BIN:-target/release/assemble}
[ -x "$BIN" ] || { echo "build first: cargo build --release (BIN=$BIN)" >&2; exit 1; }

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# ── fake Nix store layout: bin/assemble + ../config.toml ──
mkdir -p "$WORK/store/bin" "$WORK/store/tools/dummy"
cp "$BIN" "$WORK/store/bin/assemble"
cat > "$WORK/store/config.toml" <<STORECFG
[active]
model = "basemodel"

[model.basemodel]
model_id = "base-model-id"
base_url = "http://127.0.0.1:9999"

[paths]
native_tool_paths = ["$WORK/store/tools/dummy"]
STORECFG
cat > "$WORK/store/tools/dummy/tool.toml" <<TOOLCFG
[tool]
command = "true"
TOOLCFG

# ── scratch session ──
mkdir -p "$WORK/session"
printf '%s\n' '{"content":"hi","id":"00000000-0000-0000-0000-000000000001","ts":"2026-01-01T00:00:00Z","type":"user_message","v":1}' \
  > "$WORK/session/events.jsonl"
echo "$WORK" > "$WORK/session/cwd"

# ── scenario 1 (P-L6): the partial project config is the top layer ──
mkdir -p "$WORK/project"
cat > "$WORK/project/config.toml" <<PROJCFG
[system_prompt]
text = "top-layer prompt"
PROJCFG

$WORK/store/bin/assemble --session "$WORK/session" --config "$WORK/project/config.toml" \
  > "$WORK/req1.json"
python3 - "$WORK/req1.json" <<'PYCHECK'
import json, sys
req = json.load(open(sys.argv[1]))
assert req["model"] == "base-model-id", "P-L6 base model missing: %r" % req.get("model")
assert "top-layer prompt" in req["instructions"], "P-L6 top prompt missing"
names = [t["name"] for t in req.get("tools", [])]
assert "dummy" in names, "P-L6 base tool missing: %r" % names
print("P-L6: partial top inherits base model, tools, and carries the top prompt")
PYCHECK

# ── scenario 2 (P-L7): the store config alone behaves as today ──
$WORK/store/bin/assemble --session "$WORK/session" --config "$WORK/store/config.toml" \
  > "$WORK/req2.json"
python3 - "$WORK/req2.json" <<'PYCHECK'
import json, sys
req = json.load(open(sys.argv[1]))
assert req["model"] == "base-model-id", "P-L7 model missing: %r" % req.get("model")
assert "top-layer prompt" not in req["instructions"], "P-L7 foreign prompt leaked"
print("P-L7: store config alone is unchanged (no top layer, no self-merge)")
PYCHECK

# ── scenario 3 (P-L11): no flag, CWD config beats the store config ──
RUSHI_BIN=${RUSHI_BIN:-target/release/rushi}
[ -x "$RUSHI_BIN" ] || { echo "P-L11 skipped: build the rushi loop binary" >&2; exit 1; }
cp "$RUSHI_BIN" "$WORK/store/bin/rushi"
# An ambient $CONFIG (issue #36) sits above the CWD tier and would mask
# it, so the no-flag run clears the variable.
( cd "$WORK/project" && env -u CONFIG "$WORK/store/bin/rushi" config ) > "$WORK/dump.txt"
python3 - "$WORK/dump.txt" <<'PYCHECK'
import sys
text = open(sys.argv[1]).read()
assert "top-layer prompt" in text, "P-L11: CWD config did not win the resolution"
assert "basemodel" not in text, "P-L11: store config leaked into the dump"
print("P-L11: no-flag launch from a project CWD resolves the CWD config as top layer")
PYCHECK

# ── scenario 4 (P-L7 no-flag): no flag, no CWD config, Nix layout ──
# The loop's resolver walks past the CWD tier (no config.toml in CWD)
# to the side-by-side tier. Top equals base: the store config parses
# alone, today's behavior. The stage binaries resolve no tiers: their
# --config flag keeps the CWD-relative default.
mkdir -p "$WORK/empty"
( cd "$WORK/empty" && env -u CONFIG "$WORK/store/bin/rushi" config ) > "$WORK/dump4.txt"
python3 - "$WORK/dump4.txt" <<'PYCHECK'
import sys
text = open(sys.argv[1]).read()
assert "basemodel" in text, "P-L7nf store config not resolved alone"
assert "top-layer prompt" not in text, "P-L7nf foreign prompt leaked"
print("P-L7 no-flag: no CWD config on a Nix install resolves the store config alone")
PYCHECK

# ── scenario 5 (P-L5 CWD-is-store): CWD holds the store config ──
# The CWD tier returns the CWD config.toml, which is the side-by-side
# base itself (relative vs absolute, the canonicalize branch of the
# same-file guard). It parses alone: never a self-merge.
( cd "$WORK/store" && env -u CONFIG "$WORK/store/bin/rushi" config ) > "$WORK/dump5.txt"
python3 - "$WORK/dump5.txt" <<'PYCHECK'
import sys
text = open(sys.argv[1]).read()
assert "basemodel" in text, "P-L5c store config not resolved alone"
assert "top-layer prompt" not in text, "P-L5c foreign prompt leaked"
print("P-L5: CWD is the store dir, top equals base, the config parses alone")
PYCHECK

echo "config-layer e2e: all scenarios pass"
