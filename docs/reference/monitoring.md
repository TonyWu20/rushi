# Monitoring long-running tasks (event-driven reattach)

Use this for a task that runs for minutes to days. Do not use it
for a task that finishes inside the bash tool timeout: run that
task directly and read the output.

The monitor blocks on one event and pokes the session with one
short message. It carries no timer. Polling spams the context.
A sleep loop in a monitor is polling too.

## Template: block on task exit

```bash
#!/usr/bin/env bash
# monitor: block on the task exit, poke the session once.
# No polling. No sleep loop. No liveness check.
# Cap: a `timeout` wrap caps the task command. It never wraps
# `rushi run`. Only when the user names a bound. No bound asked,
# no cap.
set -u
cd /path/to/task
SESSION="<session>"
LOG=logs/monitor.log
# The tree that owns the session store. Absolute values: relative
# values resolve against the caller CWD.
SESS_ROOT=/path/to/project/sessions
PROJ=/path/to/project
# Start the task as a child of the monitor. Block on its exit.
bash -c 'the long-running command' > "$LOG" 2>&1 &
TASK_PID=$!
echo "$TASK_PID" > logs/task.pid
wait "$TASK_PID"
RC=$?
if [ "$RC" -eq 0 ]; then
  rushi run "$SESSION" "task finished cleanly. Review the result." \
    --sessions-root "$SESS_ROOT" --cwd "$PROJ"
else
  rushi run "$SESSION" "task failed (exit $RC). $(tail -5 "$LOG")" \
    --sessions-root "$SESS_ROOT" --cwd "$PROJ"
fi
```

## Launch

```bash
# The launch stands alone. It is milliseconds. It does not
# outrun the cap.
nohup bash scripts/monitor.sh >/dev/null 2>&1 &
echo $! > logs/monitor.pid
```

The poke resolves its config from the monitor CWD, or from
`$CONFIG` when set.

## Cap

Ask the user about the cap before writing the monitor. The user
decides. The agent picks no cap. No decision, no cap. The cap
wraps the task command:

```bash
timeout "$((CAP_HOURS * 3600))" bash -c 'the long-running command' \
  > "$LOG" 2>&1 &
```

With a cap, add the 124 branch to the completion check:

```bash
elif [ "$RC" -eq 124 ]; then
  rushi run "$SESSION" "task hit the time cap. Check its state and relaunch the monitor." \
    --sessions-root "$SESS_ROOT" --cwd "$PROJ"
fi
```

## Template: block on a state event

The task has no single clean exit. It appends one line per state
change to an event stream. Block on that stream.

```bash
# Vars: SESSION, SESS_ROOT, PROJ as in the exit template.
# The task appends one line per state change to runs/latest/events.log.
while IFS= read -r line; do
  case "$line" in
    DONE) rushi run "$SESSION" "task finished. Review the result." \
            --sessions-root "$SESS_ROOT" --cwd "$PROJ"; break ;;
    FAIL) rushi run "$SESSION" "task failed. $(tail -5 runs/latest/err)" \
            --sessions-root "$SESS_ROOT" --cwd "$PROJ"; break ;;
  esac
done < <(tail -n0 -F runs/latest/events.log)
```

When the task emits no observable event, add one: a marker file,
a log line, or a process the monitor can wait on.

## Routing

Every `rushi run` above carries `--sessions-root <abs>` and
`--cwd <abs>`. Relative values resolve against the caller CWD,
so the templates use absolute paths.

## Guard

The `no-long-sleep` guard blocks in-call waits that pass the cap
(default 60 s. Set `RUSHI_SLEEP_MAX_S` to change it). It blocks
three shapes: a `sleep` longer than the cap. A `while` or
`until` loop that contains `sleep`. A `for` loop whose total
wait is uncountable or over the cap. A short bounded loop still
runs: `for i in 1 2 3; do sleep 5; done` waits 15 s and passes.
