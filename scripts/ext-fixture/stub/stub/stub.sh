#!/usr/bin/env bash
# A stub status extension: it answers every tick op with a fixed
# status row. The PTY smoke test asserts that row on the screen.
while IFS= read -r line; do
  case "$line" in
    *'"op":"tick"'*)
      printf '{"v":1,"op":"status","lines":[["EXT stub alive",{"fg":"green"}]]}\n'
      ;;
  esac
done
