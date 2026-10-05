# Headless missions (agent-orchestrated rushi sessions)

## What this is

A headless mission is a detached `rushi` session the agent starts from
a step. It runs to completion and pings the main session when it
ends. The scaffold is a plain shell script. No new binary, no
daemon, no owned schema.

This is the async form of subagent mode. The blocking
`spawn_agent` tool in `docs/subagent-design.md` is the in-tool
version of the same pattern (designed, not built). The
headless-mission scaffold is the shell-native one. Any number of
sessions run in parallel. Each is a plain `rushi` process.

## When to use

Use it when the agent orchestrates multiple `rushi` sessions in one
bash tool call. Or when it writes the orchestration to a script and
runs it.

Typical shapes:

- One session per task. Fan out, then wait.
- One session per issue, in a git worktree.
- One session per review pass over the same tree.

For one long external task, use `rushi docs monitoring` instead.

## Mission shape

A mission is a script that provisions the work, then execs the
session:

```bash
#!/usr/bin/env bash
set -u
cd /path/to/work
# Provision: create the worktree, stage files, write the task.
exec rushi run <session> <task>
```

The `exec` replaces the script with the loop. One mission is one
process. It has one exit code.

The worktree-issue pattern is the common provisioning step. One git
worktree per issue, one session per worktree. Provision the
worktree, then run `exec rushi run <session> <task>` inside it.

## Detach and record

The agent's shell is non-interactive. `nohup &` alone is not the
correct detach there. `setsid` starts the mission in a new session.
It survives the tool call's process group ending.

```bash
setsid nohup ./s.sh </dev/null > log 2>&1 & echo $!
```

Record the PID that `echo $!` prints. The agent checks it later.
Redirect stdin from `/dev/null`. The loop never reads a terminal.

## Ping-back contract

On mission end, the orchestrator pings the main session:

```bash
rushi run <s> <task> && rushi run <main> "done" || rushi run <main> "failed: $(tail -n 5 log)"
```

The blocking `rushi run` plus `&&` and `||` is the event trigger.
The ping is the mission's terminal event. No daemon is needed.

The main session is the one the agent is in. It wakes on the ping
and reads the result. That is the same event-driven reattach that
`rushi docs monitoring` teaches. The ping is the event.

## Fan-out

Native parallel orchestration. One bash call starts N missions.
Each mission records its own PID. The caller blocks on `wait`.
Then it checks each exit state:

```bash
for i in 1 2 3; do
  (setsid rushi run "$s_$i" "$(cat task_$i)" > out_$i 2>&1 & echo $! > pid_$i)
done
wait
for p in pid_*; do
  kill -0 "$(cat "$p")" || echo "mission $p ended"
done
```

That is native parallel orchestration with per-mission exit codes.
The agent reads `out_$i` when mission `i` ends.

## Monitor pointer

- Watch one long external task: `rushi docs monitoring`. The
  monitor blocks on a task event and pokes the session.
- Watch N headless missions as a human: `tv rushi-sessions
  [roots…] --watch N`. It is the human-side mirror of the
  ping-back contract.

## The parked tool form

The tool form (an arg-taking tool that compiles this scaffold) is
parked as a P9 itch (`docs/refinement-policy.md` P9, entry in
`docs/itches.md`). It has two concrete triggers:

- Three recorded episodes of an agent mis-composing the scaffold
  despite this doc (the rule of three).
- A non-agent caller (human CLI, cron, CI) that needs a stable,
  machine-checkable arg schema.

If it ships later, it generates from this doc or links to it. It
does not re-encode the scaffold. One source of truth.

## Scope

Implementation stays in this (kernel) repo. The `rushi-queue` repo
links to this doc. It does not duplicate it.
