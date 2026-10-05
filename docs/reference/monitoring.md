# Monitoring long-running tasks (event-driven reattach)

## When to use

Use this pattern for a background task that runs for a long time. It
covers a build, an external job, or a process that takes minutes to days.
Do not use it for a task that finishes inside the bash tool timeout. Run
that task directly and read its output.

## Why not poll

Polling with `sleep` and a check command spams the context window. A task
that runs for hours adds hundreds of poll turns. The agent cannot block
for hours inside one tool call.

A `while :; do sleep; done` monitor loop is also polling. It moves the
poll out of the context window, but the monitor still checks on a timer.
Event-driven means the monitor blocks on a real event and wakes only when
that event fires. The event is a task exit or a state the task writes.
The monitor waits for the event. It does not check on a timer.

## The pattern

1. The task emits an event the monitor can block on. The event is a
   process exit. It is a state file the task writes. It is a line the
   task appends to an event stream.
2. Write a small monitor script. It launches the task and blocks on the
   event. It carries no timer.
3. Launch the monitor with `nohup` or `setsid nohup`. The bash tool call
   returns immediately. The monitor blocks in the background.
4. When the event fires, the monitor pokes this session with one short
   message. While the loop is alive, use the lock-free
   `rushi run <session> "<msg>" --no-run`. When the loop is dead, use
   `rushi run <session> "<msg>"` to start it.
5. The message reattaches the session and wakes the agent.

The monitor sends one message per event. It never sends one per timer
tick. On error, it folds the stderr tail into the message.

## Skeleton: block on task exit

The task is a command that finishes. The monitor starts it as a child and
blocks on its exit with `wait`. No timer. A `timeout` wrap is the cap,
not the wait mechanism.

```bash
#!/usr/bin/env bash
# monitor: wake session "<session>" when the task exits.
set -u
cd /path/to/task
SESSION="<session>"
SESSIONS_ROOT="sessions" # match [paths] sessions_root
MAX_HOURS=24
LOG=logs/monitor.log

loop_alive() { # $1 = session. Returns 0 when the loop process is alive.
  local dir="$SESSIONS_ROOT/$1"
  local pid
  # Prefer loop.meta (issue #44): the loop records its own pid and
  # session identity there. Old loops without the record fall back to
  # the bare loop.pid.
  if [ -f "$dir/loop.meta" ]; then
    pid="$(sed -n 's/^pid = //p' "$dir/loop.meta")"
  else
    [ -f "$dir/loop.pid" ] || return 1
    pid="$(cat "$dir/loop.pid")"
  fi
  [ -n "$pid" ] || return 1
  kill -0 "$pid" 2>/dev/null
}

notify() { # $1 = short message. Append to a live loop, or start one.
  local msg="$1"
  if loop_alive "$SESSION"; then
    # Lock-free append. The running loop drains the message at its next
    # step. `user --no-run` is the equivalent dev-install form.
    rushi run "$SESSION" "$msg" --no-run >> "$LOG" 2>&1
  else
    # No live loop: start one. It logs the task and runs it.
    rushi run "$SESSION" "$msg" >> "$LOG" 2>&1
  fi
}

# Start the task as a child of the monitor. The `timeout` wrap is the
# hard cap: a hung task cannot block the monitor forever.
timeout "$((MAX_HOURS * 3600))" bash -c 'the long-running command' \
  > "$LOG" 2>&1 &
TASK_PID=$!
echo "$TASK_PID" > logs/task.pid

# Block on the exit event. No timer, no polling.
wait "$TASK_PID"
RC=$?

# Poke the session on completion.
if [ "$RC" -eq 0 ]; then
  notify "task finished cleanly. Review the result and the state files."
elif [ "$RC" -eq 124 ]; then
  notify "task hit the time cap. Check its state and relaunch the monitor."
else
  notify "task failed (exit $RC). $(tail -5 "$LOG" 2>/dev/null)"
fi
```

Launch it:

```bash
nohup bash scripts/monitor.sh >/dev/null 2>&1 &
echo $! > logs/monitor.pid
```

The bash call returns immediately. The monitor blocks in the background on
the task's exit. The agent then waits for the message.

## Skeleton: block on a state event

The task has no single clean exit. It appends one line per state change to
an event stream. The monitor blocks on that stream and pokes per event.
No timer.

```bash
# The task appends one line per state change to runs/latest/events.log.
# Block on the stream. Event-driven, no timer.
while IFS= read -r line; do
  case "$line" in
    DONE) notify "task finished. Review the result."; break ;;
    FAIL) notify "task failed. $(tail -5 runs/latest/err 2>/dev/null)"; break ;;
  esac
done < <(tail -n0 -F runs/latest/events.log)
```

A blocking `inotifywait -q runs/latest/done` is the same idiom when
`inotifywait` is available. When the task emits no observable event, add
one: a marker file, a log line, or a process the monitor can wait on.
That is what makes the monitor event-driven.

## Poke forms

The monitor pokes the session in two cases:

- The loop is alive. Use the lock-free `rushi run <session> "<msg>"
  --no-run`. It takes only the log-line lock, so it never collides with
  the running loop. The live loop drains the message at its next step.
  `user --session <session> --no-run "<msg>"` is the equivalent
  dev-install form.
- The loop is dead. `rushi run <session> "<msg>"` logs the message and
  starts the loop, which runs it.

Lock behavior, verified in `bin/rushi/src/run_loop.rs`:

- `rushi run` (without `--no-run`) takes the exclusive `.loop.lock`
  before it logs the task.
- `rushi run --no-run` is append-only. It takes only the log-line lock,
  writes the `user_message`, and exits 0. It works against a live loop.
- `user --no-run` has the same lock behavior.

Both forms queue the message as `steer` by default. The live loop drains
it at the next step. Plain installs ship only the `rushi` binary. In
that case the monitor uses `rushi run --no-run` for live sessions.

Liveness checks prefer `loop.meta` (issue #44). The loop writes that
record beside `loop.pid` after the session lock. The record carries
the loop pid and the recorded session identity. The skeleton above
reads the pid from the record and falls back to `loop.pid` for old
loops. The lock stays the authority for liveness.

## Rules

- One message per event. Never send a message per timer tick.
- Keep the message short. It reopens the session.
- Record the monitor PID so the agent can check it is alive.
- Wrap the blocking wait in a hard cap, a `timeout` or a watchdog. The
  cap stops a hung task from blocking the monitor forever. It is a
  safety net, not the wait mechanism.
- The monitor blocks on an event. It does not poll on a timer.

## Enforcement: the no-long-sleep guard

A `tool.before` pipeline that includes
`harness-hook-no-long-sleep` blocks in-call waits that would exceed the
cap (default 60 s, set `RUSHI_SLEEP_MAX_S` to change it). The guard
enforces the rule "do not poll with `sleep`". When it blocks, the tool
call fails and the reason points back to this document.

It blocks:

- A `sleep` longer than the cap. Consecutive values sum and `s`/`m`/`h`/`d`
  suffixes work.
- A `while` or `until` loop that contains `sleep` (an unbounded in-call
  poll).
- A `for` loop whose total wait (iterations times per-iteration `sleep`)
  exceeds the cap.
- A `for` loop whose iteration count or `sleep` duration is not literal
  (a glob, a variable, or an unknown substitution). The guard treats that
  as uncountable and blocks it.

It does not detect a `sleep` inside `$( )` substitution or a variable
duration outside a loop.

A short, bounded loop still runs. `for i in 1 2 3; do sleep 5; done`
waits 15 s and passes. The guard only blocks a total wait that exceeds the
cap or one that cannot be bounded.
