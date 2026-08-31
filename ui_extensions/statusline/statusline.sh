#!/usr/bin/env bash
# The reference statusline extension (ui-extension-plan stage 2).
#
# One long-lived process. It answers every `tick` op with a status
# row. The row shows:
# - live dir: the config dir, where the TUI and the loop operate
#   ($CONFIG is exported by the host)
# - git branch + dirty mark, TTL-cached at 3 s so a tick never
#   spawns git more than once per 3 s (the design's tick-cost note)
# - model from the tick payload. The session name and the loop state
#   stay in the host's top bar (frame title); the footer does not
#   duplicate them
# - cumulative usage summed over assistant_message.usage events;
#   the numbers shorten to k/M/B, the reference fmtNum rule, with a
#   trailing .0 dropped (5500 -> 5.5k, 5000 -> 5k, 1200000 -> 1.2M)
#   the host re-sends every usage-bearing message at start, so the
#   totals survive a TUI restart from the log alone
# - context fullness, the number to watch for compaction: the last
#   measured request input tokens over the model window, in the
#   starship-statusline style (ctx <pct>% (<tokens>/<window>)). The
#   window is the active model's context_tokens from $CONFIG; the
#   metric is the last usage event's input_tokens, not the
#   cumulative totals. The section hides until both are known. Both
#   numbers shorten with the same k/M/B rule.
# - ext_status values that other extensions published into the log;
#   the row consumes them through the tick payload's `statuses` map
#
# The row is a powerline footer: rounded pill segments with Nerd
# Font glyphs, the starship-statusline reference look. The left hard
# divider U+E0B6 caps each pill; the right hard divider U+E0B4 is
# the arrow between pills (the previous pill's color over the next
# pill's background) and the end cap. The palette is Catppuccin
# Macchiato, the reference's. Each pill is one or more styled spans
# on the wire (docs/ui-extension.md section 4); the host draws the
# spans left to right on one row. Requires a Nerd Font in the
# terminal, like the reference.
#
# Layout: one line on wide terminals, two lines when the terminal is
# narrow (width under 100). The host reserves one terminal row per
# line. Line 1 carries dir, git, and model; line 2 carries the
# stats pill alone. Overflow drops the model pill first, then the
# git pill; the dir and stats pills never drop, so the token
# indicator keeps its space.

set -u
DIR="$(dirname "${CONFIG:-.}")"
USE_JQ=0
command -v jq >/dev/null 2>&1 && USE_JQ=1

in_total=0
out_total=0
cached_total=0
# The last request's measured input tokens. Context fullness rides
# on this value, not on the cumulative totals.
last_in=0

# Context window cache, keyed on the model name. The model name comes
# from the tick payload; the window is the model's context_tokens in
# $CONFIG. A model change re-parses; the same model reuses the value.
ctx_model=""
ctx_window=""

# Git state cache, TTL 3 s. The refresh runs in a background job:
# a tick reply never waits on a slow git (a cold cache can take
# seconds, and the staleness bound is 3 x tick_ms). The job writes
# one "branch dirty" line to a state file; the next tick loads it.
# The row shows the last loaded state until then, git:none at start.
git_branch=""
git_dirty=0
git_ts=$SECONDS
git_job_pid=""
GIT_TTL=3
# The state file lives in a temp file, not the entry dir: the entry
# dir is the user's checkout, and the host chdirs the script there.
GIT_STATE="$(mktemp 2>/dev/null)"
[ -n "$GIT_STATE" ] || GIT_STATE="${EXT_DIR:-.}/.git_state"

git_refresh() {
  local now=$SECONDS
  if (( now - git_ts < GIT_TTL )); then
    return
  fi
  # A running job already owns the refresh; one job at a time.
  [ -n "$git_job_pid" ] && kill -0 "$git_job_pid" 2>/dev/null && return
  git_ts=$now
  # A background job: the tick reply does not wait on git. The job
  # dies with the process group on quit.
  (
    local b d
    b=$(git -C "$DIR" rev-parse --abbrev-ref HEAD 2>/dev/null || true)
    d=$(git -C "$DIR" status --porcelain 2>/dev/null | wc -l | tr -d '[:space:]')
    printf '%s %s\n' "${b:-none}" "${d:-0}" > "$GIT_STATE"
  ) &
  git_job_pid=$!
}

git_load() {
  # Load the newest state file without waiting on its writer.
  local st
  st=$(cat "$GIT_STATE" 2>/dev/null) || return 0
  [ -n "$st" ] || return 0
  git_branch=${st%% *}
  git_dirty=${st##* }
  [ "$git_branch" = "none" ] && git_branch=""
  [ "$git_dirty" = "0" ] && git_dirty=0
}

# The active model's context window from $CONFIG. Prints nothing
# when the model or the value is unknown: the ctx section hides.
# The config shape is the harness config: a [model."<name>"] section
# carrying a plain `context_tokens = N` key.
ctx_window_of() {
  # $1 = the model name
  local model=$1
  [ -n "${CONFIG:-}" ] || return 0
  awk -v model="$model" '
    /^\[model\./ {
      s = $0
      sub(/^\[model\./, "", s)
      sub(/\].*$/, "", s)
      gsub(/"/, "", s)
      insec = (s == model)
      next
    }
    /^\[/ { insec = 0 }
    insec && /^[[:space:]]*context_tokens[[:space:]]*=/ {
      v = $0
      sub(/^[^=]*=/, "", v)
      gsub(/[" \t\r]/, "", v)
      print v
      exit
    }
  ' "$CONFIG" 2>/dev/null
}

# JSON-string escape without spawning a process. Covers the values
# this row emits: backslashes, quotes, and control characters.
esc() {
  local s=$1
  s=${s//\\/\\\\}
  s=${s//\"/\\\"}
  s=${s//$'\t'/\\t}
  s=${s//$'\n'/\\n}
  printf '%s' "$s"
}

# One line: the usage totals from a compact "in out cached" triple.
usage_add() {
  # $1 = "in out cached", or empty when the event carries no usage.
  [ -n "$1" ] || return 0
  local in out cached
  # Function-local positionals: the main loop's read keeps its own.
  set -- $1
  in=${1:-0}
  out=${2:-0}
  cached=${3:-0}
  in_total=$(( in_total + in ))
  out_total=$(( out_total + out ))
  cached_total=$(( cached_total + cached ))
  # The last measured request is the context-fullness source.
  last_in=$in
}

# Extract "in out cached" from one op line. jq when available; a sed
# fallback for machines without it. The fallback anchors on the
# flat `"usage":{...}` object, so tool_calls with nested objects
# earlier in the line cannot fragment the match.
usage_pair() {
  local line=$1
  if [ "$USE_JQ" = 1 ]; then
    printf '%s' "$line" | jq -r '
      select((.op // "") == "event")
      | select((.event.type // "") == "assistant_message")
      | .event.usage // empty
      | "\(.input_tokens // 0) \(.output_tokens // 0) \(.cached_tokens // 0)"
    ' 2>/dev/null
    return 0
  fi
  local in out cached
  in=$(printf '%s' "$line" | sed -n \
    's/.*"usage":{[^}]*"input_tokens":\([0-9]*\).*/\1/p')
  out=$(printf '%s' "$line" | sed -n \
    's/.*"usage":{[^}]*"output_tokens":\([0-9]*\).*/\1/p')
  cached=$(printf '%s' "$line" | sed -n \
    's/.*"usage":{[^}]*"cached_tokens":\([0-9]*\).*/\1/p')
  if [ -n "$in" ] && [ -n "$out" ]; then
    printf '%s %s %s\n' "$in" "$out" "${cached:-0}"
  fi
}

# The ext_status values of the tick, compact "k=v" text. The row
# shows at most two; the rest stay in the log. An empty statuses map
# skips the jq spawn.
status_text() {
  # $1 = the tick line
  case "$1" in
    *'"statuses":{}'*) return 0 ;;
  esac
  local out=""
  if [ "$USE_JQ" = 1 ]; then
    out=$(printf '%s' "$1" | jq -r '
      ((.statuses // {}) | to_entries)
      | .[0:2][]
      | "\(.key)=\(.value | if type == "string" then . else tojson end)"
    ' 2>/dev/null)
  fi
  printf '%s' "${out:-}"
}

# The context-fullness section, starship-statusline style:
# `ctx <pct>% (<tokens>/<window>)`. Both numbers need to be known,
# so the section hides until the last usage event and the config
# window are both in hand. The window is the model's context_tokens;
# the tokens are the last measured request input, the same value the
# auto-compact budget watches.
ctx_text() {
  [ -n "$ctx_window" ] || return 0
  [ "$last_in" -gt 0 ] 2>/dev/null || return 0
  [ "$ctx_window" -gt 0 ] 2>/dev/null || return 0
  local pct10 pct tok win
  pct10=$(( last_in * 1000 / ctx_window ))
  pct="$(( pct10 / 10 )).$(( pct10 % 10 ))"
  tok=$(_fmt_num "$last_in")
  win=$(_fmt_num "$ctx_window")
  printf 'ctx:%s%% (%s/%s)' "$pct" "$tok" "$win"
}

# k/M/B abbreviation, the starship-statusline fmtNum rule, with a
# trailing .0 dropped: 5500 -> 5.5k, 5000 -> 5k, 1200000 -> 1.2M,
# 1500000000 -> 1.5B.
_fmt_num() {
  local n=$1 scale=1 unit=""
  if [ "$n" -ge 1000000000 ]; then
    scale=1000000000 unit=B
  elif [ "$n" -ge 1000000 ]; then
    scale=1000000 unit=M
  elif [ "$n" -ge 1000 ]; then
    scale=1000 unit=k
  fi
  [ -n "$unit" ] || { printf '%s' "$n"; return 0; }
  local v
  v=$(awk -v n="$n" -v s="$scale" '
    BEGIN {
      v = n / s
      if (v == int(v)) printf "%d", v
      else printf "%.1f", v
    }')
  printf '%s%s' "$v" "$unit"
}

# The usage totals from one tick: the cumulative line, plus the
# cached total when the session saw any, plus the ctx section when
# both its inputs are known.
usage_text() {
  local t="in:$( _fmt_num "$in_total" ) out:$( _fmt_num "$out_total" ) sum:$( _fmt_num $(( in_total + out_total )) )"
  if [ "$cached_total" -gt 0 ] 2>/dev/null; then
    t="$t R:$( _fmt_num "$cached_total" )"
  fi
  local c
  c="$(ctx_text)"
  [ -n "$c" ] && t="$t $c"
  printf '%s' "$t"
}

# ── Powerline footer ───────────────────────────────────────────
# Nerd Font glyphs: the left hard divider caps a pill; the right
# hard divider joins the pills and closes the line.
SEP_L=$'\uE0B6'
SEP_R=$'\uE0B4'
# ASCII unit separator: packs one segment record "text fg bg bold".
REP=$'\x1f'
# Catppuccin Macchiato, the starship-statusline reference palette.
# The backgrounds match the reference: dark base/surface pills, one
# light mauve pill for the model.
DIR_BG=24273a   # base
GIT_BG=363a4f   # surface0
MODEL_BG=c6a0f6 # mauve
STATS_BG=494d64 # surface1
TXT=cad3f5      # text, on dark backgrounds
TXT_DARK=1e2030 # mantle, on light backgrounds

# One span on the wire: a [text, style] JSON pair. An empty fg or
# bg field leaves the terminal default.
span_json() {
  # $1 text, $2 fg, $3 bg, $4 bold(0|1)
  local o=""
  [ -n "$2" ] && o="\"fg\":\"#$2\""
  [ -n "$3" ] && o="${o:+$o,}\"bg\":\"#$3\""
  [ "$4" = 1 ] && o="${o:+$o,}\"bold\":true"
  printf '[%s,{%s}]' "\"$(esc "$1")\"" "$o"
}

# One segment record: "text fg bg bold", packed with $REP.
seg() {
  printf '%s%s%s%s%s%s%s' "$1" "$REP" "$2" "$REP" "$3" "$REP" "$4"
}

# The span JSON of one pill row from segment records, in display
# order. The row is [cap, body, arrow, cap, body, ..., endcap]; the
# cap and arrow colors follow the reference.
row_json() {
  local out="" s text rest fg bg bold prev_bg=""
  for s in "$@"; do
    text=${s%%"$REP"*}
    rest=${s#*"$REP"}
    fg=${rest%%"$REP"*}
    rest=${rest#*"$REP"}
    bg=${rest%%"$REP"*}
    bold=${rest#*"$REP"}
    [ -n "$prev_bg" ] && out="${out:+$out,}$(span_json "$SEP_R" "$prev_bg" "$bg" 0)"
    out="${out:+$out,}$(span_json "$SEP_L" "$bg" "$bg" 0),$(span_json " $text " "$fg" "$bg" 1)"
    prev_bg=$bg
  done
  out="${out:+$out,}$(span_json "$SEP_R" "$prev_bg" "" 0)"
  printf '[%s]' "$out"
}

# The row's column count: each segment is one cap (1) plus its body
# (text length plus two spaces), plus one arrow per join and the
# end cap (2 per segment).
row_cols() {
  local total=0 s text
  for s in "$@"; do
    text=${s%%"$REP"*}
    total=$(( total + ${#text} + 4 ))
  done
  printf '%s' "$total"
}

# Fit a row to `w` columns: drop the lowest-priority tail segments
# until the row fits. The head segment never drops. The survivors
# land in the global REPLY_SEGS.
row_fit() {
  local w=$1
  shift
  local -a keep=("$@")
  while [ "$(row_cols "${keep[@]}")" -gt "$w" ] && [ "${#keep[@]}" -gt 1 ]; do
    unset "keep[${#keep[@]}-1]"
  done
  REPLY_SEGS=("${keep[@]}")
}

emit_status() {
  # $1 width, $2 model, $3 tick line
  local width=$1 model=$2 tickline=$3
  local stt
  stt=$(status_text "$tickline")

  # The context window is keyed on the model name from the tick.
  # The model rarely changes; the re-parse only fires on a switch.
  if [ "$model" != "$ctx_model" ]; then
    ctx_window="$(ctx_window_of "$model")"
    ctx_model=$model
  fi

  # Truncate the dir to the last 16 chars with a leading ellipsis.
  local dir=$DIR
  if [ ${#dir} -gt 16 ]; then
    dir="...${dir: -16}"
  fi

  # The pill texts. The git pill shows git:none at start; the
  # dirty mark is a trailing *N. The state pill colors with the
  # loop state: green running, blue idle.
  local git_txt="git:${git_branch:-none}"
  [ "$git_dirty" -gt 0 ] 2>/dev/null && git_txt="$git_txt *$git_dirty"
  git_txt="${git_txt:0:24}"
  local model_txt="${model:-no-model}"
  local stats
  stats="$(usage_text)"
  [ -n "$stt" ] && stats="$stats st:${stt}"

  # One record per pill. The head (dir) never drops; the model and
  # git pills drop in that order on overflow; the stats pill keeps
  # its space (the token indicator wins the width fight).
  local dir_seg git_seg model_seg stats_seg
  dir_seg="$(seg "$dir" "$TXT" "$DIR_BG" 1)"
  git_seg="$(seg "$git_txt" "$TXT" "$GIT_BG" 1)"
  model_seg="$(seg "$model_txt" "$TXT_DARK" "$MODEL_BG" 1)"
  stats_seg="$(seg "$stats" "$TXT" "$STATS_BG" 1)"

  if [ "$width" -ge 100 ]; then
    # One line: reserve the stats pill and its join arrow, then fit
    # dir, git, and model into the rest. The join arrow shares the
    # tail cap of the fitted row, so the reservation is the stats
    # width minus one. The tail-drop order is model first, then git.
    local rest_w=$(( width - $(row_cols "$stats_seg") + 1 ))
    row_fit "$rest_w" "$dir_seg" "$git_seg" "$model_seg"
    printf '{"v":1,"op":"status","lines":[%s]}\n' \
      "$(row_json "${REPLY_SEGS[@]}" "$stats_seg")"
  else
    # Two-line layout: line 1 is dir, git, model; line 2 is the
    # stats pill alone. Each row fits the width on its own.
    row_fit "$width" "$dir_seg" "$git_seg" "$model_seg"
    local -a l1=("${REPLY_SEGS[@]}")
    printf '{"v":1,"op":"status","lines":[%s,%s]}\n' \
      "$(row_json "${l1[@]}")" "$(row_json "$stats_seg")"
  fi
}

on_tick() {
  # $1 = the tick line
  git_refresh
  git_load
  # Tick parsing is pure bash: the tick payload is small and the row
  # rebuilds once per second. `width` is the terminal width; terminals
  # under 100 cols get the two-line layout.
  local width=80
  local m_width='"width":'
  case "$1" in *"$m_width"*)
    # Take the value only: cut at the next comma or quote, then
    # strip non-digits. A wider strip grabs the digits of the keys
    # that follow (thinking, seq, model) and fakes a 4-col width.
    local w="${1#*"$m_width"}"
    # The value ends at the next comma: the tick payload is one
    # object, and width is never its last key.
    w="${w%%,*}"
    width="${w//[!0-9]/}"
    [ -n "$width" ] || width=80
    ;;
  esac
  # The tick also carries session and loop_running. The footer does
  # not consume them: the host top bar shows both.
  local model=""
  local m_model='"model":"'
  case "$1" in *"$m_model"*)
    model="${1#*"$m_model"}"
    model="${model%%\"*}"
    ;;
  esac
  emit_status "$width" "$model" "$1"
}

while IFS= read -r line; do
  case "$line" in
    *'"op":"tick"'*)
      on_tick "$line"
      ;;
    *'"op":"event"'*)
      usage_add "$(usage_pair "$line")"
      ;;
  esac
done
