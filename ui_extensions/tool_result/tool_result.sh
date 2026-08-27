#!/usr/bin/env bash
# The reference tool_result renderer (ui-extension-plan stage 2).
#
# The `render` kind owner for `tool_result`. The host forwards every
# tool_result event (live and, at start, the visible transcript). For
# each one the script answers with a `lines` reply:
#   - a header line: an [ext] marker, the tool_call id, and the exit
#     status. Green when ok, red when the result is an error
#   - the result body, one dim line per hard line of the text
#
# Body precedence mirrors the built-in render (docs/tui.md 13.1):
# value.text, then stdout plus stderr, then a string value, then the
# compact JSON of the value. Nothing is truncated: the body is shown
# in full, like the built-in render.
#
# When this script dies the host exhausts the restart budget, drops
# the cached replies, and the built-in render returns
# (ui-extension-plan stage 2 acceptance).

set -u
USE_JQ=0
command -v jq >/dev/null 2>&1 && USE_JQ=1

# The jq filter: read one op line, emit one `lines` reply when the
# event is a tool_result. The reply's `event_id` is the op's `id`
# (the log index the host caches by).
read -r -d '' FILTER <<'JQ' || true
( .event // {} ) as $e
| select( ($e.type // "") == "tool_result" )
| ( $e.id // "?" ) as $tid
| ( $e.value.exit_code // $e.value.exit // null ) as $code
| ( if ($e.is_error // false) then "error" else "ok" end ) as $ok
| ( if $code != null
    then "exit " + ($code | tostring)
         + (if ($e.is_error // false) then " (error)" else "" end)
    else $ok
  end ) as $status
| ( ($e.value // null) as $v
    | if $v == null then ""
      elif ($v | type) == "string" then $v
      else
        ( ($v.text // "") ) as $t
      | ( ($v.stdout // "") ) as $so
      | ( ($v.stderr // "") ) as $se
      | if $t != "" then $t
        elif ($so != "") or ($se != "")
          then $so + (if $se != "" then "\n[stderr]\n" + $se else "" end)
        else $v | tojson
        end
      end
  ) as $body
| {
    v: 1,
    op: "lines",
    event_id: .id,
    lines:
      ( [ [ "[ext] tool:" + $tid + "  " + $status,
            (if ($e.is_error // false)
             then {fg: "red", bold: true}
             else {fg: "green", bold: true} end) ] ]
        + ( $body | split("\n") | map(select(length > 0))
            | map([ ., {fg: "darkgray"} ]) ) )
  }
JQ

while IFS= read -r line; do
  case "$line" in
    *'"op":"event"'*)
      ;;
    *)
      continue
      ;;
  esac
  if [ "$USE_JQ" = 1 ]; then
    # jq emits nothing when the event is not a tool_result.
    printf '%s' "$line" | jq -c "$FILTER" 2>/dev/null
    continue
  fi
  # No jq: emit a minimal header-only reply so the render still
  # degrades to an [ext] marker instead of nothing.
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\),.*/\1/p' | head -n 1)
  [ -n "$id" ] || continue
  printf '{"v":1,"op":"lines","event_id":%s,"lines":[["[ext] tool result",{"fg":"cyan","bold":true}]]}\n' "$id"
done
