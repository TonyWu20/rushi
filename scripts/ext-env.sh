#!/usr/bin/env bash
# Build the reference extension binaries and print their PATH dirs,
# colon-joined on one line. Shell-agnostic output: each shell sets
# its own PATH.
#
# The global `ui_extensions/` layer resolves the `mermaid` binary by
# a path relative to its own entry (docs/ui-extension.md section 3),
# so it needs no PATH export. This script still builds that binary
# (it must exist for the relative path to resolve). The PATH dirs it
# prints now matter for the opt-in `ext-rs/` Rust ports, whose
# manifests keep bare command names that resolve on PATH
# (docs/ui-extension.md section 6). A missing command still refuses
# the start.
#
# bash / sh / zsh:
#   export PATH="$(bash scripts/ext-env.sh):$PATH"
# fish:
#   set -gx PATH (bash scripts/ext-env.sh) $PATH
#
# EXTS_ROOT env var: build the reference entries under this tree
# instead of the tree this script lives in (docs/tui-ext-repo-split.md
# section 4, item 5: a kernel-side script can build a separate exts
# checkout). Use an absolute path: the printed dirs go on PATH.

set -u
if [ -n "${EXTS_ROOT:-}" ]; then
  root="$(cd "$EXTS_ROOT" && pwd)"
else
  root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fi

pkgs="ui_extensions/mermaid ui_extensions/goal ext-rs/statusline-rs ext-rs/tool_result-rs ext-rs/notify-rs"
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
