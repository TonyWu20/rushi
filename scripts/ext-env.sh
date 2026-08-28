#!/usr/bin/env bash
# Build the reference extension binaries and print their PATH dirs,
# colon-joined on one line. Shell-agnostic output: each shell sets
# its own PATH.
#
# The TUI resolves extension commands on PATH (docs/ui-extension.md
# section 6). A missing command refuses the start.
#
# bash / sh / zsh:
#   export PATH="$(bash scripts/ext-env.sh):$PATH"
# fish:
#   set -gx PATH (bash scripts/ext-env.sh) $PATH

set -u
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

pkgs="ui_extensions/mermaid ext-rs/statusline-rs ext-rs/tool_result-rs ext-rs/notify-rs"
out=""
for p in $pkgs; do
  if ! (cd "$root/$p" && cargo build --quiet); then
    echo "ext-env: build failed in $p" >&2
    exit 1
  fi
  [ -n "$out" ] && out="$out:"
  out="$out$root/$p/target/debug"
done
printf '%s\n' "$out"
