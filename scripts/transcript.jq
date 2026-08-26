# Renders the current turn: events from the last user_message to the end.
# Usage: jq -c -s -f transcript.jq events.jsonl

. as $events
| ($events | length) as $n
| [range(0; $n) | select($events[.].type == "user_message")] as $starts
| ($starts | last) as $start
| (if $start == null then $events else $events[$start:] end)
| map(
    if .type == "user_message" then
      "You: " + (.content // "")
    elif .type == "assistant_message" then
      (if ((.content // "") | length) > 0 then "Assistant: " + .content else empty end)
    elif .type == "tool_call" then
      "  -> " + (.name // "") + " " + ((.arguments // {}) | tojson)
    elif .type == "tool_result" then
      ((if .is_error then "  <- [error] " else "  <- " end) +
       (((.value.text // "") | gsub("\n"; " ")) | if (length > 300) then (.[0:300] + " ...") else . end))
    elif .type == "error" then
      "  [error] " + (.message // "")
    else
      empty
    end
  )
| .[]
