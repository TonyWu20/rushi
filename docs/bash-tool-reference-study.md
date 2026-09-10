# Bash Tool — Reference Study: deepseek-harness (dsh v0.1.5-rc.2) vs pi-0.85.1

Status: Research note (2026-09-11). Companion to `docs/bash-tool.md`
(the deep spec of rushi's own tool) and `docs/loop-lifecycle-hooks.md`.
Every claim in this document was read directly from the two codebases
on 2026-09-11; source paths are listed in section 7.

## 1. Scope

Rushi's `bash` tool spawns `sh -c` (`tools/bash/src/main.rs`). This study
examines how the two major reference harnesses implement the same capability,
along the axes that matter for deterministic control:

- **Shell choice** (which dialect the agent's commands run in)
- **Secrets** (can spawned commands read harness/user credentials)
- **File safety** (can a command write anywhere on the system)
- **Context overflow** (what happens when a command prints 10 MB)
- **Hanging** (can a command escape the timeout and hang the loop)
- **Model-facing contract** (what the tool tells the model to do)

It closes with the design decision for rushi: **adopt pi's philosophy** —
an open, composable core with safety as opt-in layers that ship strong
defaults — and maps every gap to that decision (section 6).

## 2. deepseek-harness (dsh) — closed, enforce-by-default

Source root: `/home/tony/programming/dsh-v0.1.5-rc.2/`. The bash tool is
five cooperating layers:

```
tool-bash (model-facing) → bash-local (executor) → subprocess (spawn/kill service)
                                              ↘ bash-sandbox (confinement, optional mount)
util/timeout (deadline arithmetic)
```

### 2.1 Shell choice

`bash-local/src/index.ts`: `run()` executes
`runArgv(spec, ['bash', '-c', spec.command])`. Always **bash**, never
`sh`. The tool description literally says "Execute a bash command
(`bash -c`)". The sandboxed executor re-wraps the *same* argv through the
confinement runner, so the dialect is identical with and without
sandboxing.

### 2.2 Secrets

`subprocess/subprocess/src/index.ts` defines, at the spawn seam:

- `SENSITIVE_ENV_PATTERN = /KEY|PASSWORD|SECRET|TOKEN/i` — any ambient env
  var whose *name* matches is **not forwarded to children** (the harness's
  own `DEEPSEEK_API_KEY` must not leak into a spawned process implicitly).
- All `DSH_*` harness-identity names are scrubbed too (case-insensitive
  matching, for Windows env semantics).
- Deliberate `env` / `dshEnv` spec layers merge **after** the scrub, so a
  trusted caller can still forward a credential explicitly.
- A proxy overlay is applied so a child Node honors the same routing as
  its parent.

### 2.3 File safety

`bash-sandbox/` mounts a *confining* executor instead of `bash-local`.
It wraps the exact `['bash','-c',command]` argv in a platform runner:
**bubblewrap / Landlock / Seatbelt**.

- Modes: `read-only` (**default** — no writes anywhere; of `/dev` only
  `/dev/null` is writable), `workspace-write` (writes under the policy's
  workspace root plus the platform temp dir), `danger-full-access`
  (never confines).
- **Fail-closed**: if no runner can enforce the confined mode, the call
  fails with `SANDBOX_UNAVAILABLE` — "never a silent unconfined run".
- Denials are *result facts*: the result carries
  `sandbox: { mode, denied, enforcement?, runnerFailed? }` and the model
  sees the marker `[sandbox: file access denied under <mode> mode]`.
- Escalation: the model may retry the *exact same command once* with
  `sandbox_permissions` (the narrowest wider mode) plus a one-sentence
  `justification`; that routes to a **user approval prompt** before
  anything executes. "The executor never negotiates permissions itself —
  the tool layer drives the override."
- Network access and process visibility are explicitly *outside* its
  guarantees.

### 2.4 Output limits

`bash-local` config defaults: `maxOutputBytes = 64KB` per stream
(stdout and stderr each), `maxSpillBytes = 64MB` per stream spill file.
Overflow spills to a temp file; the in-memory reader keeps its tail; the
result carries `truncated` + `spillPath` so the model can retrieve the
full stream later.

### 2.5 Timeout and process containment

`util/timeout/src/index.ts`:

- `clampTimeout(requested, def, max) = min(requested ?? def, max)`.
  Defaults: 120 s default, 600 s hard cap. A requested value must be
  positive and finite; **"zero is not a public disable-timeout
  sentinel"** — the model cannot remove the cap.
- `deadline()` fuses upstream cancellation with the timeout via
  `AbortSignal.any`; the timeout stamps a `TimeoutReason(code, ms)` so a
  timeout is distinguishable from an abort.

`subprocess-local` contains the **whole process range**, not just the
direct child:

- Linux: `linux-scope` — a user systemd scope or private bootstrap
  ("the current user-systemd scope or private bootstrap is unavailable"
  is the documented fallback reason).
- Windows: `windows-job` — a Win32 Job object.
- Fallback: process-group / direct-parent tree, with a logged warning
  that "descendants that escape the process group ... are not guaranteed
  to terminate".
- Kill ladder: SIGTERM → SIGKILL after `graceMs` (default 3 s, "matches
  OpenCode's 3s").
- Live handles are tracked; teardown awaits the whole range so "even a
  surviving descendant cannot outlive the fiber".
- Background: `run_in_background` returns a job id immediately
  (`job_output`, `job_kill`); "no timeout applies" to background jobs.

### 2.6 Model-facing contract

- Every call must carry a `description` (5–10 words, active voice) —
  stated intent, shown in the UI, auditable.
- The tool description teaches the denial protocol verbatim: "a blocked
  file operation is ... a policy denial, not a bug in the command; do
  not retry another way"; "escalate immediately in the same turn — the
  one sanctioned exception to a denial"; "Never escalate speculatively";
  "A rejected escalation is final for that command — stop and explain,
  never work around it".
- System-prompt section: "Check the [exit code: N] marker on every bash
  result; investigate failures before moving on."
- Output hygiene: `NO_COLOR=1, TERM=dumb, PAGER=cat, GIT_PAGER=cat`
  overrides ("the same set Codex hardcodes; Claude Code achieves it via
  TERM=dumb"). Fresh shell per call: "no state (cwd, variables,
  functions) persists between calls — pass `workdir`".

## 3. pi-0.85.1 — open core, opt-in hardening

Source root:
`/nix/store/3s13lfb0l362crm9xif4hyri4gjlpyr4-pi-coding-agent-0.85.1/lib/node_modules/@earendil-works/pi-coding-agent/`.

### 3.1 Tool and executor

`src/core/tools/bash.ts`:

- Schema: `{ command: string, timeout?: number }` — timeout is
  **optional with no default**; hard cap `MAX_TIMEOUT_MS = 2_147_483_647`
  (~24 days, i.e. effectively unbounded unless the operator sets one).
- Description template: "Output is truncated to last 2000 lines or 50KB
  (whichever is hit first). If truncated, full output is saved to a temp
  file."
- `createLocalShellOperations` spawns with `detached: true` (non-Windows)
  and waits via `waitForChildProcess(child)` — "wait for the process to
  terminate without hanging on inherited stdio handles held by detached
  descendants".
- Non-zero exit is a *thrown* error carrying the captured output plus
  "Command exited with code N".

`src/core/tools/truncate.ts`: `DEFAULT_MAX_LINES = 2000`,
`DEFAULT_MAX_BYTES = 50 * 1024` (50KB); tail truncation; the full output
goes to a `pi-bash-<id>.log` temp file; result text appends
`[Showing lines X-Y of N. Full output: <path>]`.

`src/core/bash-executor.ts` runs the same accumulator for remote
execution (`BashOperations` — SSH and friends).

### 3.2 Shell choice

`src/utils/shell.ts`, `getShellConfig()` resolution order:

1. user-specified `shellPath` (from settings, or per-session `bash:
   { shellPath }`),
2. Unix: `/bin/bash` → `bash` on PATH → **fallback `sh -c`**,
3. Windows: Git Bash in known locations → `bash.exe` on PATH (Cygwin,
   MSYS2, WSL) → a helpful install-error otherwise.

Legacy WSL `bash.exe` under `C:\Windows\System32` uses a stdin transport
(`-s` instead of `-c`) — a corner-case quirk handled explicitly.

So pi *prefers* bash and degrades to `sh` only when bash is absent.

### 3.3 Environment

`getShellEnv()` returns `...process.env` with pi's bin dir prepended to
`PATH`. **No credential scrub** in the core. It deletes
`PI_SESSION_ID`, `PI_SESSION_FILE`, `PI_PROVIDER`, `PI_MODEL`,
`PI_REASONING_LEVEL` from the env unless `exposeSessionEnvironment`
(default true). No `ENV`/`BASH_ENV` handling.

### 3.4 File safety (opt-in extension)

`pi-extension-sandbox` ships in pi's node_modules. It *overrides the
built-in bash tool* (the documented mechanism for replacing built-ins)
and wraps commands through `@anthropic-ai/sandbox-runtime`:
**bubblewrap on Linux, sandbox-exec on macOS** (Linux also needs
bubblewrap, socat, ripgrep).

- Config: `~/.pi/agent/extensions/sandbox.json` (global) merged with
  `<cwd>/.pi/sandbox.json` (project takes precedence).
- **Shipped defaults** are strong: `enabled: true`;
  `denyRead: ["~/.ssh", "~/.aws", "~/.gnupg"]`;
  `allowWrite: [".", "/tmp"]`;
  `denyWrite: [".env", ".env.*", "*.pem", "*.key"]`; network-domain
  allowlists for npm/pypi/github.
- Opt-in: `pi -e ./sandbox` enables it, `--no-sandbox` disables it.
  The *core* stays permissive; the safety is a layer the operator loads.

### 3.5 Hangs

`src/utils/child-process.ts`, `waitForChildProcess`: after `exit`, the
waiter holds the pipe readers until they fall **idle** — a 100 ms grace
timer (`EXIT_STDIO_GRACE_MS`) re-armed on every chunk. Rationale: "a
short-lived child can `exit` while a detached descendant keeps its
stdout/stderr pipe open. We must not resolve and destroy the streams on
a fixed deadline measured from `exit`, or output still being written
past that deadline is silently lost (earendil-works/pi#5303)". This is
the most sophisticated anti-hang detail in either codebase: a fixed
post-exit drain deadline either hangs (descendant holds the pipe) or
loses data; an *idle-based* deadline resolves both.

Kill: `killProcessTree(pid)` — `process.kill(-pid, SIGKILL)` on Unix
(whole group), `taskkill /F /T /PID` on Windows. Detached PIDs are
tracked in a Set; `killTrackedDetachedChildren()` runs on parent
SIGHUP/SIGTERM so no descendant outlives the session.

### 3.6 Extensibility

- `commandPrefix` — prepend setup commands to every invocation.
- `BashSpawnHook` — rewrite `{ command, cwd, env }` before spawn.
- `BashOperations` — swap the whole execution backend (remote/SSH).
- Extensions may override built-in tools (the sandbox does exactly that).

### 3.7 Default tool set

`src/core/agent-session.ts`: `defaultActiveToolNames =
this._baseToolsOverride ? ... : ["read", "bash", "edit", "write"]` —
the same four-tool kernel rushi ships. `defaultTools` in settings can
override; `allToolNames` adds `powershell`, `grep`, `find`, `ls`.

## 4. Feature matrix

| Concern | rushi (current) | dsh v0.1.5-rc.2 | pi 0.85.1 |
|---|---|---|---|
| Shell | `sh -c` (name says bash) | `bash -c` always | `/bin/bash` → PATH `bash` → `sh` fallback; `shellPath` override |
| Credential scrub | none (full env inherited) | `KEY\|PASSWORD\|SECRET\|TOKEN` + `DSH_*` scrubbed at spawn seam; explicit layers merge after | none in core |
| `ENV`/`BASH_ENV` auto-source channel | open (full env inherited) | scrubbed (pattern match covers neither, but `DSH_*`/credential names are the target; POSIX rc sourcing of a plain `ENV` is not specifically addressed by either) | open |
| File confinement | none | **in-core option**: bwrap/Landlock/Seatbelt, read-only default, fail-closed, approval-gated escalation | **opt-in extension**: bubblewrap/sandbox-exec, strong secret-path defaults, one-flag enable |
| Output cap | 16 KB combined (stdout+stderr), tail kept, head discarded | 64 KB per stream + 64 MB spill file, `spillPath` in result | 50 KB / 2000 lines per stream + temp-file spill, `fullOutputPath` in result |
| Default timeout | 60 s | 120 s | **none** |
| Max timeout | 300 s | 600 s, un-removable cap (zero ≠ disable) | ~2⁻³¹ ms (~24 d) |
| Kill ladder | SIGTERM → SIGKILL after fixed 2 s | SIGTERM → SIGKILL after 3 s `graceMs` | SIGKILL on group (no TERM grace) |
| Process containment | process group (`setpgid`) | **process range**: Linux systemd-scope/cgroup, Windows Job, tracked live handles | process group (`detached`), tracked PIDs, kill on parent SIGHUP/SIGTERM |
| Post-exit drain | fixed 3 s pipe-drain deadline | managed-range wait (survivors cannot outlive) | **idle-based** pipe drain (100 ms re-armed grace; no hang, no data loss) |
| Background jobs | none | `run_in_background` + `job_output`/`job_kill` | none in tool; abort-controllers for cancellation |
| Model contract | exit code + truncation marker | mandatory `description` arg; denial/escalation protocol prose | plain; `[Showing lines X-Y ...]` spill marker |
| Output hygiene | — | `NO_COLOR`, `TERM=dumb`, `PAGER=cat`, `GIT_PAGER=cat` | ANSI strip + binary-char sanitize in the result text |
| Session context | — | `workdir` per call, no state | `PI_*` session vars (opt-out), `commandPrefix` |

## 5. Gap table for `tools/bash` (before the philosophy decision)

1. **Shell dialect** — switch to bash-first resolution with `sh`
   fallback (pi's pattern).
2. **Environment** — at minimum remove `ENV`/`BASH_ENV` at spawn
   (closes the rc auto-source channel); optionally a dsh-style
   credential-name scrub.
3. ~~**Spill file**~~ — superseded: see 6.2 (tools.jsonl covers
   context; tool-level recoverability is a judgment call).
4. **File confinement** — Landlock read-only/workspace-write as an
   opt-in layer (pi's posture), dsh's mode vocabulary.
5. **Timeouts** — keep 60 s default / 300 s max (better than pi's none;
   dsh proves an un-removable cap is cheap).
6. **Post-exit drain** — replace the fixed 2 s/3 s drain with
   pi's idle-based grace (avoids both the hang and the silent data loss
   that a fixed deadline causes).
7. **Model contract** — optional `description` arg + denial-protocol
   prose when confinement lands.

## 6. Decision: adopt pi's philosophy — open core, opt-in hardening

The user chose pi's posture over dsh's. Concretely:

### 6.1 Core changes (in `tools/bash`, always on)

1. **Shell resolution**: `/bin/bash` → PATH `bash` → `sh -c` fallback
   (pi `getShellConfig`, minus the Windows branches; log which shell
   resolved so a session can see its dialect).
2. **Env hygiene**: `env_remove("ENV")`, `env_remove("BASH_ENV")` at
   spawn. This is the one non-negotiable env change even under an open
   core: it removes a *silent code-execution* channel, not a convention.
3. **Keep bounded timeouts** (60 s default, 300 s max). This is the one
   place we deliberately diverge from pi (which has no default): the
   original goal is *deterministic* hang prevention, and a default costs
   the operator nothing.
4. **Idle-based post-exit drain** (pi `waitForChildProcess` semantics:
   grace re-armed on output, quiet pipes release after a short idle
   window) replacing the fixed 2 s sleep + 3 s drain.
5. **Output spill**: see 6.2.

### 6.2 Opt-in layers (extensions, strong shipped defaults)

- **env-scrub policy**: dsh's `KEY|PASSWORD|SECRET|TOKEN` name scrub as
  an opt-in setting (default: off, or on-for-harness-vars — decide at
  implementation), following dsh's merge order (explicit layers after
  the scrub).
- **file-confinement**: an extension that wraps the command in a
  Landlock policy (Linux) with dsh's mode vocabulary
  (`read-only` default / `workspace-write` / `danger-full-access`),
  fail-closed, with `tool.before`-`approve` escalation mirroring dsh's
  `sandbox_permissions` + `justification`. pi proves the "override the
  built-in tool" mechanism works in this harness family.
- **optional `description` arg** on the bash call (dsh's discipline
  device: stated intent, UI display, audit trail).

### 6.3 The spill-file question (resolved by `tools.jsonl`)

Per `docs/tool-log-design_from_human.md` (correction 58), tool *result
bodies* already live in `sessions/<name>/tools.jsonl`: `events.jsonl`
keeps a slim `tool_result` index (call id, status, byte length,
head/tail preview) and `assemble` reads full bodies from the tool log.
So the **context-overflow role** of pi's temp-file spill is already
covered at the harness level. What remains is only *pre-truncation*
stream recoverability: the bash tool currently drops the head bytes of a
stream that exceeds `bash_max_output_bytes` *before* writing the result,
and `tools.jsonl` faithfully stores the (already truncated) result.
Options:

- (a) nothing — the `[output truncated]` marker plus the agent's own
  bash (`> /tmp/x` re-run) is the Unix-native answer; fits pi's
  lean-tool posture;
- (b) cheap tool-level spill: write the full combined stream to
  `$TMPDIR/rushi-bash-<pid>.log` before truncating, add
  `full_output_path` to the result JSON (~10 lines, mirrors pi's
  `ensureTempFile`).

Recommendation under the chosen philosophy: (a) for now; (b) only if
sessions show the agent repeatedly re-running truncated commands.

### 6.4 Tension and its resolution

The original goal was *deterministic control to prevent accidents*; a
permissive open core is, by itself, less deterministic. Pi's answer —
and the one this decision adopts — is that **openness does not mean
absence of safety: it means safety is a shipped layer with strong
defaults, one flag away** (pi-extension-sandbox ships `enabled: true`,
`denyRead ~/.ssh ~/.aws ~/.gnupg`, `denyWrite .env *.pem *.key`).
What rushi gives up vs dsh: the *inescapable* guarantee ("never a
silent unconfined run"). What it gains: a kernel the operator fully
drives and extends — which fits rushi's existing architecture
(`tool.before`/`approve` hooks, `extra_tools_roots`) far better than
dsh's closed service graph. The parts that stay deterministic regardless
of posture: bounded timeouts, env-channel closure, the post-exit drain
fix, and the `tools.jsonl` context split.

## 7. Native tools vs hooks — synthesis

- **dsh** is the strongest case for *enforcement at the execution
  boundary*: the OS makes the forbidden syscall fail; the model is
  steered by result facts + tool-description protocol. A hook sees
  command *text* and can be outsmarted by obfuscation; a Landlock
  denial cannot be argued around.
- **pi** shows the middle ground: deterministic *input mutation*
  (`spawnHook`, `commandPrefix`) plus OS-level opt-in confinement; its
  permissive core leans on the model plus truncation.
- **Rushi's hooks** (`no-find-grep`) remain the right tool for
  *convention/efficiency steering* — its `reason` payload is a teaching
  channel (verified in-session: a blocked `find` returned a concrete
  fix, and the agent internalized the `fd`/`rg` convention after one
  blocked call).
- Rule of thumb: **hooks steer behavior; enforcement guarantees
  outcomes.** Ship safety as opt-in enforcement layers with strong
  defaults (pi's philosophy); use hooks to keep the agent efficient and
  idiomatic within the envelope those layers set.

## 8. Implementation status (2026-09-12, `tools/bash` — "the pi way")

Implemented in `tools/bash/src/main.rs` (spec: `docs/bash-tool.md`):

- **6.1.1 shell resolution**: `/bin/bash` → `PATH` `bash` → `sh`, with
  `--shell-path` pin override. The resolved path is reported in the
  result JSON as `shell` (dashed on spawn failure) — the session sees
  its dialect.
- **6.1.2 env hygiene**: `ENV` and `BASH_ENV` removed from the spawn
  environment, always on (closes the silent auto-source channel).
- **6.1.3 bounded timeouts kept** (60 s default / 300 s max) — the
  deliberate divergence from pi, per 6.4.
- **6.1.4 idle-based post-exit drain** (100 ms idle window, 3 s hard
  cap, re-armed on output) replaces the fixed 2 s sleep + 3 s drain.
  Fixes the silent data loss a fixed deadline caused (lingering
  writers now have their bytes captured).
- **6.1.5 spill**: option (a) adopted — no tool-level spill file;
  `tools.jsonl` owns the context-overflow role.
- **6.2 env-scrub**: shipped as opt-in `--env-scrub` CLI flag
  (off by default; operator enables via `tool.toml` `args`), dsh
  name-vocab (`KEY|PASSWORD|SECRET|TOKEN`).
- **6.2 file-confinement / `description` arg**: not implemented —
  separate extension work (needs `tool.before`-`approve` round-trip
  design; dsh vocab `read-only`/`workspace-write`/`danger-full-access`).
  Deferred, out of scope for the core changes.

Verification: `scripts/tool-conformance.sh` 43/43 PASS — incl.
`bash: spawn failure` (bad `--shell-path` pin → non-zero exit),
`bash: shell path pin` (`echo pinned` via pinned interpreter),
conditional `bash: spawn failure (empty PATH)` (skipped on hosts with
`/bin/bash`), `bash: timeout` (kill ladder + idle drain: no survivor
hangs the tool), and the `ENV`/`BASH_ENV` auto-source channel tests.

## 9. Sources

dsh (read in full, 2026-09-11):
- `packages/shell/tool-bash/src/index.ts`
- `packages/shell/bash-local/src/index.ts`
- `packages/shell/bash-sandbox/src/index.ts`, `packages/shell/bash-sandbox/README.md`
- `packages/subprocess/subprocess/src/index.ts` (scrub), `src/types.ts`
- `packages/subprocess/subprocess-local/src/index.ts` (containment)
- `packages/util/timeout/src/index.ts`

pi 0.85.1 (`/nix/store/3s13lfb0l362crm9xif4hyri4gjlpyr4-pi-coding-agent-0.85.1/lib/node_modules/`):
- `@earendil-works/pi-coding-agent/src/core/tools/bash.ts`
- `.../src/core/tools/truncate.ts`
- `.../src/core/tools/index.ts`
- `.../src/core/bash-executor.ts`
- `.../src/core/agent-session.ts` (default tool set, line ~2809)
- `.../src/utils/shell.ts`, `.../src/utils/child-process.ts`
- `pi-extension-sandbox/index.ts`

rushi:
- `tools/bash/src/main.rs`, `tools/bash/tool.toml`
- `docs/bash-tool.md`
- `docs/tool-log-design_from_human.md`
- `docs/loop-lifecycle-hooks.md`
