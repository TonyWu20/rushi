# Monitoring long-running tasks (event-driven reattach)

## When to use

Use this pattern for a background task that runs for a long time. It covers
a build, an external job, or a process that takes minutes to days. Do not
use it for a task that finishes inside the bash tool timeout. Run that task
directly and read its output.

## Why not poll

Polling with `sleep` and a check command spams the context window. A task
that runs for hours adds hundreds of poll turns. The agent cannot block for
hours inside one tool call. This pattern removes the polling. The agent
writes one monitor, launches it detached, and sits idle. The agent wakes
only when the monitor sends a message.

## The pattern

1. Write a small monitor script for the task.
2. Launch it with `nohup` or `setsid nohup`. The bash tool call returns
   immediately.
3. The monitor loops on the task's own conditions.
4. On a state change or finish, the monitor pokes this session with one
   short message. While the loop is alive, use the lock-free
   `rushi run <session> "<msg>" --no-run`. When the loop is dead, use
   `rushi run <session> "<msg>"` to start it.
5. The message reattaches the session and wakes the agent.

The monitor sends a message only on a change, never per poll. It tracks
last-seen state so a relaunch does not re-report. It carries a hard time
cap so it cannot run forever. On error, it folds the stderr tail into the
message.

## Skeleton

```bash
#!/usr/bin/env bash
# monitor: wake session "<session>" when the task state changes.
set -u
cd /path/to/task
SESSION="<session>"
SESSIONS_ROOT="sessions" # match [paths] sessions_root
POLL_SECS=300
MAX_HOURS=24
START_TS=$(date +%s)
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
    # Lock-free append. The running loop drains the message at its
    # next step. `rushi run --no-run` works on plain installs;
    # `user --no-run` is the equivalent dev-install form.
    rushi run "$SESSION" "$msg" --no-run >> "$LOG" 2>&1
  else
    # No live loop: start one. It logs the task and runs it.
    rushi run "$SESSION" "$msg" >> "$LOG" 2>&1
  fi
}

while :; do
  # Check the task. Test each condition you care about.
  if [ -f runs/latest/done ]; then
    notify "task finished. Review the result and the state files."
    break
  fi
  if [ -f runs/latest/err ]; then
    notify "task failed. $(tail -5 runs/latest/err 2>/dev/null)"
    break
  fi
  # Hard time cap: stop this monitor if it has run too long.
  if [ "$(date +%s)" -ge "$((START_TS + MAX_HOURS * 3600))" ]; then
    notify "monitor time cap reached. Check the task state manually."
    break
  fi
  sleep "$POLL_SECS"
done
```

Launch it:

```bash
nohup bash scripts/monitor.sh >/dev/null 2>&1 &
echo $! > logs/monitor.pid
```

The bash call returns immediately. The agent then waits for the message.

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
that case the monitor uses `rushi run --no-run` for live sessions. A
failed poke stays pending for the next cycle.

Liveness checks prefer `loop.meta` (issue #44). The loop writes that
record beside `loop.pid` after the session lock. The record carries
the loop pid and the recorded session identity. The skeleton above
reads the pid from the record and falls back to `loop.pid` for old
loops. The lock stays the authority for liveness.

## Rules

- One message per state change. Never send a message per poll.
- Keep the message short. It reopens the session.
- Record the monitor PID so the agent can check it is alive.
- Cap the monitor. A hard time limit stops it from running forever.
- If a poke fails, keep the message pending and retry it on the next cycle,
  before any new state check.

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
