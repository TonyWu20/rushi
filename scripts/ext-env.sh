#!/usr/bin/env bash
# Build the reference extension binaries and print one PATH export.
#
# Usage: eval "$(bash scripts/ext-env.sh)"
#
# The TUI resolves extension commands on PATH (docs/ui-extension.md
# section 6). A missing command refuses the start. This script builds
# the four reference packages and prints the PATH export that puts
# their target/debug dirs first.

set -u
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

pkgs="ui_extensions/mermaid ext-rs/statusline-rs ext-rs/tool_result-rs ext-rs/notify-rs"
prefix=""
for p in $pkgs; do
  if ! (cd "$root/$p" && cargo build --quiet); then
    echo "ext-env: build failed in $p" >&2
    exit 1
  fi
  prefix="$prefix$root/$p/target/debug:"
done

# The caller's PATH is inherited; eval expands the right-hand $PATH.
echo "export PATH=\"$prefix\$PATH\""
