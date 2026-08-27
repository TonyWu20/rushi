#!/usr/bin/env bash
# Broken JSONL on every op. The host drops each bad line and keeps
# its state: the built-in render and the last valid row survive.
# Two flavors of bad: not JSON at all, and JSON with a bad payload
# shape (the per-op G5 fallback, docs/ui-extension.md section 4).
echo 'this line is not json at all'
while IFS= read -r line; do
  case "$line" in
    *'"op":"tick"'*)
      printf '{"v":1,"op":"status","lines":NOT_AN_ARRAY}\n'
      printf '{"v":1,"op":"status","lines":42}\n'
      ;;
    *'"op":"event"'*)
      printf '{"v":1,"op":"lines","event_id":0,"lines":BROKEN}\n'
      printf '{"v":1,"op":"lines","event_id":0,"lines":"nope"}\n'
      ;;
  esac
done
