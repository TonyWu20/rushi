#!/usr/bin/env bash
# The reference tool_result renderer (ui-extension-plan stage 2).
#
# A `render` kind demo for `tool_result` (ui_extensions-demos, not
# in the default ui_extensions layer since the 2026-09-03 user
# report: the ext reply replaced the built-in box, and the Ctrl+O
# fold key had no effect on the read and edit results). Point
# `[ext] dir` at this layer to opt in. The host forwards every
# tool_result event it sees (live and, at start, the visible
# transcript). For each one the script answers with a `lines`
# reply:
#   - a header line: an [ext] marker, the tool_call id, and the exit
#     status. Green when ok, red when the result is an error
#   - the result body. A body that is a complete JSON document gets
#     JSON syntax highlighting (keys, strings, numbers, literals,
#     punctuation), one multi-span line per hard line. Any other
#     body renders in one muted tone, not a single gray
#     (docs/tui-color-tones.md section 4: stop the gray abuse)
#
# Body precedence mirrors the built-in render (docs/tui.md 13.1):
# value.text, then stdout plus stderr, then a string value, then the
# compact JSON of the value. The body is shown in full: this reply
# protocol has no fold control yet. The built-in render folds long
# bodies to a preview cap (docs/tui-tool-display-port.md); the
# rescoped rule keeps the no-truncation promise on the `content`
# field of user and assistant messages only (docs/
# tui-tool-result-truncation.md section 4).
#
# Colors are catppuccin-macchiato hex values. The host lowers them
# to the terminal capability level at storage time, so the reply
# shows what the TUI actually emits.
#
# When this script dies the host exhausts the restart budget, drops
# the cached replies, and the built-in render returns
# (ui-extension-plan stage 2 acceptance).

set -u
USE_JQ=0
command -v jq >/dev/null 2>&1 && USE_JQ=1
# The JSON tokenizer is awk: available wherever a POSIX shell is.
USE_AWK=0
command -v awk >/dev/null 2>&1 && USE_AWK=1

# The extraction filter: read one op line, emit one compact object
# when the event is a tool_result: the tool_call id, the status
# text, the error flag, and the body in the built-in precedence.
read -r -d '' EXTRACT <<'JQ' || true
( .event // {} ) as $e
| select( ($e.type // "") == "tool_result" )
| ( .id // null ) as $eid
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
| { eid: $eid, tid: $tid, status: $status,
    err: ($e.is_error // false), body: $body }
JQ

# The JSON tokenizer (awk): one hard line of JSON in, one multi-span
# wire line out (an array of [text, style] pairs). A string that a
# colon follows is a key; the other strings are values. Whitespace
# is one plain span. The style objects are the pi catppuccin-
# macchiato `syntax*` token colors (docs/tui-color-pi-alignment.md)
# the host lowers at storage time.
read -r -d '' AWK_TOKENIZER <<'AWK' || true
BEGIN {
  K   = "{\"fg\":\"#cad3f5\"}"
  S   = "{\"fg\":\"#a6da95\"}"
  NUM = "{\"fg\":\"#f5a97f\"}"
  LIT = "{\"fg\":\"#f5a97f\"}"
  NUL = "{\"fg\":\"#f5a97f\"}"
  P   = "{\"fg\":\"#939ab7\"}"
}
{
  line = $0
  if (line == "") next
  n = length(line)
  i = 1
  out = "["
  emitted = 0
  while (i <= n) {
    c = substr(line, i, 1)
    if (c == "\"") {
      j = i + 1
      while (j <= n) {
        d = substr(line, j, 1)
        if (d == "\\") { j += 2; continue }
        if (d == "\"") { j++; break }
        j++
      }
      tok = substr(line, i, j - i)
      k = j
      while (k <= n && substr(line, k, 1) == " ") k++
      st = (substr(line, k, 1) == ":") ? K : S
      out = out sep() span(tok, st)
      emitted++
      i = j
      continue
    }
    if (c == "-" || (c >= "0" && c <= "9")) {
      j = i
      if (substr(line, j, 1) == "-") j++
      while (j <= n && substr(line, j, 1) ~ /[0-9]/) j++
      if (j <= n && substr(line, j, 1) == ".") {
        j++
        while (j <= n && substr(line, j, 1) ~ /[0-9]/) j++
      }
      if (j <= n && (substr(line, j, 1) == "e" || substr(line, j, 1) == "E")) {
        j++
        if (j <= n && (substr(line, j, 1) == "+" || substr(line, j, 1) == "-")) j++
        while (j <= n && substr(line, j, 1) ~ /[0-9]/) j++
      }
      out = out sep() span(substr(line, i, j - i), NUM)
      emitted++
      i = j
      continue
    }
    if (c ~ /[a-zA-Z]/) {
      j = i
      while (j <= n && substr(line, j, 1) ~ /[a-zA-Z]/) j++
      w = substr(line, i, j - i)
      st = (w == "true" || w == "false") ? LIT : NUL
      out = out sep() span(w, st)
      emitted++
      i = j
      continue
    }
    if (c ~ /[{}[\],:]/) {
      out = out sep() span(c, P)
      emitted++
      i++
      continue
    }
    j = i
    while (j <= n && (substr(line, j, 1) == " " || substr(line, j, 1) == "\t")) j++
    out = out sep() span(substr(line, i, j - i), "")
    emitted++
    i = j
  }
  print out "]"
}
function sep(  s) { s = (emitted > 0) ? "," : ""; return s }
function esc(s) {
  gsub(/\\/, "\\\\", s)
  gsub(/"/, "\\\"", s)
  gsub(/\t/, "\\t", s)
  return s
}
function span(t, st) {
  # A wire span is [text, style]; a null style renders plain.
  return "[" "\"" esc(t) "\"," (st == "" ? "null" : st) "]"
}
AWK

# The reply assembly: take the extracted fields and the tokenized
# spans, build the `lines` reply. The JSON path uses the awk spans
# when present; every other path paints each hard line the muted
# tone.
read -r -d '' REPLY <<'JQ' || true
( if $is_json and (($spans | length) > 0)
  then $spans
  else
    ( (.body // "") | split("\n") | map(select(length > 0))
      | map([ ., {"fg": "#cad3f5"} ]) )
  end ) as $body_lines
| {
    v: 1,
    op: "lines",
    event_id: $eid,
    lines:
      ( [ [ "[ext] tool:" + .tid + "  " + .status,
            # The pi macchiato accents (docs/tui-color-pi-
            # alignment.md): the `error` red on a failure, the
            # `success` green otherwise.
            (if .err then {fg: "#ed8796", bold: true}
             else {fg: "#a6da95", bold: true} end) ] ]
        + $body_lines )
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
  if [ "$USE_JQ" = 0 ]; then
    # No jq: emit a minimal header-only reply so the render still
    # degrades to an [ext] marker instead of nothing.
    id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\),.*/\1/p' | head -n 1)
    [ -n "$id" ] || continue
    # The marker in the pi macchiato `toolTitle` accent (docs/
    # tui-color-pi-alignment.md), not a hard-coded cyan.
    printf '{"v":1,"op":"lines","event_id":%s,"lines":[["[ext] tool result",{"fg":"#c6a0f6","bold":true}]]}\n' "$id"
    continue
  fi
  fields=$(printf '%s' "$line" | jq -c "$EXTRACT" 2>/dev/null)
  [ -n "$fields" ] || continue
  # The event id comes out of the extraction object first key (the
  # jq constructor keeps key order), so a pure-shell strip, no
  # second parser spawn per event: `{"eid":342,...}` -> `342`.
  eid=""
  case "$fields" in
    '{"eid":'* )
      rest="${fields:7}"
      eid="${rest%%,*}"
      [ "$eid" = "null" ] && eid=""
      ;;
  esac
  [ -n "$eid" ] || continue
  # JSON detection: a body that trims to a `{` or `[` document and
  # parses is highlighted.
  body=$(printf '%s' "$fields" | jq -r '.body // empty')
  trimmed=${body#"${body%%[![:space:]]*}"}
  is_json=0
  spans_json="[]"
  case "$trimmed" in
    \{*|\[*)
      if [ "$USE_AWK" = 1 ] \
        && jq -en --arg b "$body" '$b | fromjson?' >/dev/null 2>&1; then
        is_json=1
        spans_json=$(printf '%s\n' "$body" | awk "$AWK_TOKENIZER" | jq -cs 'map(select(length > 0))' 2>/dev/null)
        [ -n "$spans_json" ] || spans_json="[]"
      fi
      ;;
  esac
  printf '%s' "$fields" \
    | jq -c --argjson eid "${eid:-0}" \
      --argjson is_json "$is_json" \
      --argjson spans "$spans_json" \
      "$REPLY" 2>/dev/null || continue
done
