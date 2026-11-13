# Headless missions (agent-orchestrated rushi sessions)

Use this for a task that does not depend on the current session
context: implement, fix, review, or research work. The task text
carries every piece of context the mission needs. The mission
starts from zero context.

Typical shapes: one session per task. One session per issue in a
git worktree. One session per review pass over the same tree.

## Mission shape

A mission script provisions the work, then execs the session. One
mission is one process with one exit code.

```bash
#!/usr/bin/env bash
# mission: provision, exec the session. One mission, one process.
# Flags: --sessions-root pins the store. --cwd pins the workdir.
set -u
cd /path/to/worktree
# Provision: create the worktree, stage files, write the task.
exec rushi run <session> <task> \
  --sessions-root /path/to/project/sessions --cwd /path/to/worktree
```

## Config

The `config.toml` at the worktree root is the top layer for a
launch from that directory. The mission script carries no
`--config`. The machine model, tools, and hooks survive under the
project diff.

## Launch

```bash
# The launch stands alone. It is milliseconds. It does not
# outrun the cap.
nohup ./s.sh </dev/null > log 2>&1 &
echo $!
```

Record the PID that `echo $!` prints. Redirect stdin from
`/dev/null`. The loop never reads a terminal.

## Result on stdout

On a clean idle stop, `rushi run` prints the last assistant
message to stdout. The print lands at the moment the loop stops.
The capture is the Unix form of the mission result.

- Capture it: `result=$(rushi run <s> <task> ...)`
- Or redirect it: `rushi run <s> <task> ... > result`
- Only the idle stop prints. A failed or exhausted stop prints
  nothing on stdout. The logged `error` event is announced on
  stderr at the moment it lands. The session log holds the record.
- Diagnostics stay on stderr, each announced at the moment it is
  recorded: the terminal `error` event, the exhausted stop, the
  failed `run.idle` chain, a failed hook step, a failed compact
  (the binary's own stderr is re-printed), and the approval
  timeout. The log is the record. stderr is the operator's view.
- An empty content prints nothing. That is a tool-call-only final
  turn.
- The live-loop poke prints its notice, not a result. The loop
  has not stopped. The notice and the echoed event line go to
  stderr. The poke's stdout is empty, so a capture of a live poke
  receives no result.

## Ping-back

On mission end, the orchestrator pings the main session. The
main session lives in the project tree. The flags route the
poke there. The poke works whether the main loop is alive or
dead. No liveness check.

```bash
# The poke plus && and || is the event trigger.
# On a clean idle stop, rushi run prints the last assistant
# message to stdout. Capture it and forward it in the poke.
rushi run <s> <task> </dev/null > out \
  && rushi run <main> "done. Mission result: $(cat out)" \
       --sessions-root /path/to/project/sessions --cwd /path/to/project \
  || rushi run <main> "failed (rc=$?). Inspect the mission session dir and the worktree." \
       --sessions-root /path/to/project/sessions --cwd /path/to/project
```

## Fan-out

One bash call starts N missions. Each records its own PID. The
caller blocks on `wait` and reads each exit state:

```bash
# Launch N missions. Record each PID. Wait on each PID.
# On a clean idle stop, the mission prints its last assistant
# message to stdout. Capture it per mission.
for i in 1 2 3; do
  nohup rushi run "$s_$i" "$(cat task_$i)" \
    --sessions-root /path/to/project/sessions --cwd /path/to/project \
    </dev/null > out_$i 2>&1 &
  echo $! > pid_$i
done
for i in 1 2 3; do
  wait "$(cat pid_$i)"
  echo "mission $i rc=$?"
done
```

The `wait` form is for batches that finish inside the tool call
cap. For longer missions, launch and let the ping-back signal.
The agent reads the mission session dir and the worktree when mission `i` ends.
The `out_i` capture holds the mission's last assistant message on a
clean idle stop.

## Difference from monitoring

A mission is the monitor pattern applied to a blocking
`rushi run` call. Its exit is the event. The cap rule does not
carry over: `timeout` never wraps `rushi run`. Watch long
external commands instead: `rushi docs monitoring`.
