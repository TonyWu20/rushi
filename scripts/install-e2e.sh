#!/usr/bin/env bash
# Gate: verify the plain install path (P4).
# Builds with cargo, runs install.sh into a scratch PREFIX, then
# confirms the binary is on PATH and responds to --help.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

PREFIX="$(mktemp -d)"
trap 'rm -rf "$PREFIX"' EXIT

echo "=== install-e2e: building and installing into $PREFIX ==="
RUSHI_PROFILE=debug PREFIX="$PREFIX" bash "$ROOT/install.sh"

# P4: the binary is on PATH and runs.
test -x "$PREFIX/bin/rushi" || {
  echo "FAIL: rushi not found at $PREFIX/bin/rushi"
  exit 1
}

# Smoke: --help exits 0.
"$PREFIX/bin/rushi" --help > /dev/null 2>&1 || {
  echo "FAIL: rushi --help exited non-zero"
  exit 1
}

echo "PASS: install-e2e"
