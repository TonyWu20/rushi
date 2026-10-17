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

## Ping-back

On mission end, the orchestrator pings the main session. The
main session lives in the project tree. The flags route the
poke there. The poke works whether the main loop is alive or
dead. No liveness check.

```bash
# The poke plus && and || is the event trigger.
rushi run <s> <task> </dev/null > log 2>&1 \
  && rushi run <main> "done" \
       --sessions-root /path/to/project/sessions --cwd /path/to/project \
  || rushi run <main> "failed: $(tail -n 5 log)" \
       --sessions-root /path/to/project/sessions --cwd /path/to/project
```

## Fan-out

One bash call starts N missions. Each records its own PID. The
caller blocks on `wait` and reads each exit state:

```bash
# Launch N missions. Record each PID. Wait on each PID.
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
The agent reads `out_$i` when mission `i` ends.

## Difference from monitoring

A mission is the monitor pattern applied to a blocking
`rushi run` call. Its exit is the event. The cap rule does not
carry over: `timeout` never wraps `rushi run`. Watch long
external commands instead: `rushi docs monitoring`.
