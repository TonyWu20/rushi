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
   message: `rushi run <session> "<msg>"`. The monitor carries no
   liveness check.
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
LOG=logs/monitor.log

notify() { # $1 = short message. Poke the session.
  local msg="$1"
  # The single poke form. It works whether the loop is alive or
  # dead. No liveness check, no timer.
  rushi run "$SESSION" "$msg" >> "$LOG" 2>&1
}

# Start the task as a child of the monitor.
bash -c 'the long-running command' > "$LOG" 2>&1 &
TASK_PID=$!
echo "$TASK_PID" > logs/task.pid

# Block on the exit event. No timer, no polling.
wait "$TASK_PID"
RC=$?

# Poke the session on completion.
if [ "$RC" -eq 0 ]; then
  notify "task finished cleanly. Review the result and the state files."
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
the task's exit. The agent then waits for the message. The poke resolves
its config from the monitor's CWD, or from `$CONFIG` when set.

## Optional: cap the wait

Before writing the monitor, ask the user whether to cap the wait.
Give the decision to the user. A silent default is not transparent.

- No cap: the monitor blocks on the task's exit. The recorded monitor
  PID and the task's own state files are the safety net.
- Cap: the user names the bound. Wrap the launch in `timeout` with it:

```bash
timeout "$((CAP_HOURS * 3600))" bash -c 'the long-running command' \
  > "$LOG" 2>&1 &
```

With a cap, use the completion check with the 124 branch:

```bash
if [ "$RC" -eq 0 ]; then
  notify "task finished cleanly. Review the result and the state files."
elif [ "$RC" -eq 124 ]; then
  notify "task hit the time cap. Check its state and relaunch the monitor."
else
  notify "task failed (exit $RC). $(tail -5 "$LOG" 2>/dev/null)"
fi
```

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

## Rules

- One message per event. Never send a message per timer tick.
- Keep the message short. It reopens the session.
- Record the monitor PID so the agent can check it is alive.
- Ask the user whether to cap the wait. Give the decision to the user.
  A cap stops a hung task from blocking the monitor forever. It is a
  safety net, not the wait mechanism. Without a cap, the recorded
  monitor PID and the task's state files are the net.
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
