# Bash Tool — Deep Spec

Phase 1: shell command execution for the agent. Closes the filesystem
discovery and command-execution gap exposed in `loop-edit-real-test`
(2026-08-24), where the model spent 98 of 102 tool calls guessing file
paths it could not discover.

P4 checklist: unblocks a current task (the model cannot run commands or
discover files beyond the four file tools). Removes the recurring manual
step of the model guessing paths. Moves the "commands run in the session
cwd" invariant from model discipline to the harness.

Pre-test: a plain `sh -c` wrapper in a script tool.toml cannot provide
per-call timeout with partial output, tail-based output capping,
process-group kill on timeout, or the typed JSON output contract.
The episode is recorded in `sessions/loop-edit-real-test/events.jsonl`.

## 1. Purpose

The `bash` tool lets the model run a shell command in the session working
directory. It covers builds, tests, git, file discovery, and any other
one-shot command that the file tools cannot handle.

`read`, `write`, and `edit` handle file content. `list` handles directory
discovery. `bash` handles everything else.

## 2. Tool manifest

`tools/bash/tool.toml`:

```toml
[tool]
description = "Run a shell command in the session working directory. Returns combined output and exit code."
command = "harness-bash"
args = []
timeout_ms = 310000

[tool.schema]
type = "object"
required = ["command"]

[tool.schema.properties.command]
type = "string"
description = "Shell command to execute via sh -c."

[tool.schema.properties.timeout_secs]
type = "integer"
description = "Kill after this many seconds. Default 60, max 300."
default = 60
```

The tool name is the directory name. The manifest carries no `name` field.
The `command` field names the tool's built binary. It is `harness-bash`
(instead of `bash`) on purpose: the workspace builds that binary into
`target/debug`, and `.envrc` puts `target/debug` on PATH for direnv
shells; a binary named `bash` would shadow the system shell and break
every extension that spawns `bash` (statusline, frame, notify).
`timeout_ms` (310 s) exceeds the tool's own maximum command timeout
(300 s) by a 10 s margin so the harness backstop never fires before the
tool's internal timeout handler completes.

## 3. Model-facing schema

Route and assemble present this schema to the model:

```json
{
  "name": "bash",
  "description": "Run a shell command in the session working directory. Returns combined output and exit code.",
  "parameters": {
    "type": "object",
    "properties": {
      "command": { "type": "string", "description": "Shell command to execute via sh -c." },
      "timeout_secs": { "type": "integer", "description": "Kill after this many seconds. Default 60, max 300." }
    },
    "required": ["command"]
  }
}
```

The `parameters` object matches `[tool.schema]` in the manifest. Route
validates input against this schema before spawning the tool. An input
missing the `command` field is rejected by route before the tool starts.
The rejection produces a `tool_result` with `is_error: true` and text
"Tool arguments failed schema validation: command. Required fields are missing from the call. Resend the call with all required fields filled in."

## 4. Execution

### 4.1 Spawn

- Spawn `sh -c <command>`.
- The tool runs in the working directory the harness sets on the process.
  `step.sh` reads `sessions/<name>/cwd` and passes `--cwd` to `route`.
  `route` sets the child process working directory via `current_dir`.
  The tool inherits this directory. It does not read the cwd file itself.
- If the harness does not set a cwd, the tool runs in the inherited
  working directory of the harness process.
- Close the child's stdin immediately. The JSON payload is consumed before
  spawn. Interactive commands (`vim`, `less`, `ssh`) receive EOF on stdin
  and fail immediately. That is correct behavior.
- Capture stdout and stderr separately so the tool can report them
  distinctly.

### 4.2 Timeout

- Default 60 s. Configurable via CLI flag `--timeout-default` (default 60).
- Hard cap 300 s. Configurable via `--timeout-max` (default 300).
- A `timeout_secs` above the hard cap is an input error. The tool exits
  non-zero with a stderr diagnostic. It never waits past the cap, so
  the route backstop (310 s) cannot fire on a command timeout.
- On timeout, the tool kills the command's process group. It sends
  SIGTERM to the group. It sends SIGKILL after 2 s to the processes
  that survive. A process that detaches from the group (for example by
  calling `setsid`) can survive the group kill. That is a known
  limitation. For ordinary commands, nothing the command spawned
  remains after the tool returns.
- A timeout is not a tool error. The command ran. It did not finish.
- The tool exits 0 and sets `"timed_out": true` in the JSON output.
- On timeout, `exit_code` is 143 (128 + SIGTERM, signal 15). The group
  kill sends SIGTERM first.
- The model sees the partial output, the `timed_out` flag, and the 143
  exit code.

### 4.3 Output capping

- Cap the combined stdout and stderr at `bash_max_output_bytes`
  (default 16000, configurable via `--max-output-bytes`).
- The cap is one shared limit for both streams. It is not a per-field
  limit. When the cap is reached, the tool keeps the **tail** of the
  combined output (the last N bytes). Build and test output is most
  relevant at the end.
- The `stdout` field holds the kept tail bytes from stdout. The
  `stderr` field holds the kept tail bytes from stderr. Together they
  hold at most `bash_max_output_bytes` bytes.
- `text` builds from the capped content. Its length stays at or below
  the cap plus fixed overhead (command line, marker, exit code line).
- The default of 16000 bytes is at most 16000 characters. It stays
  below the `route` and `assemble` backstop (`tool_result_max_chars`,
  20000 characters). The tool cap is the effective bound. The backstop
  cannot fire on a well-formed bash result.
- When truncated, `text` carries the marker on the second line, right
  after the `$ <command>` line:

  ```
  $ git status
  [output truncated: showing last 16000 of 148291 bytes]
  <tail of output>
  (exit code: 0)
  ```

- Backstop behavior, stated for completeness. When any tool result
  text exceeds 20000 characters:
  - `route` clips non-JSON tool stdout to the head. It adds no marker.
  - `assemble` clips the `tool_result` text to the head and appends
    `[tool result clipped: <N> -> <M> chars]`.
  A head clip defeats the tail-keeping design. The tool cap must stay
  below the backstop. 16000 stays below 20000.
- The tool counts bytes. `route` and `assemble` count characters. For
  ASCII output the two units are equivalent.

### 4.4 Exit code

- The command exit code is reported in the JSON `exit_code` field.
  This is the exit code of the shell command, not the tool process.
- A non-zero exit code is **not** a tool error. The tool exits 0 and
  reports the command's exit code in the `exit_code` field.
- A command not found (exit 127) is a non-zero command exit. The tool
  exits 0 with `is_error` false and `exit_code` 127.
- The tool exits non-zero only for its own failures: unable to spawn the
  process, invalid input, or an internal error.

## 5. Model-facing output

stdout is one JSON object:

```json
{
  "text": "$ git status\nOn branch main\nChanges to be committed:\n  modified:   src/main.rs\n\n(exit code: 0)",
  "exit_code": 0,
  "stdout": "On branch main\nChanges to be committed:\n  modified:   src/main.rs\n",
  "stderr": "",
  "timed_out": false,
  "truncated": false
}
```

| Field | Type | Description |
|---|---|---|
| `text` | string | Model-facing rendering. Command line, marker (when truncated), capped output, and exit code. Stays at or below the cap plus fixed overhead. |
| `exit_code` | integer | Exit code of the shell command. 0 on success. 143 on timeout. Not the tool process exit code. |
| `stdout` | string | Raw stdout. Kept tail when truncated. Shares the cap with `stderr`. |
| `stderr` | string | Raw stderr. Kept tail when truncated. Shares the cap with `stdout`. |
| `timed_out` | boolean | True if the timeout fired. |
| `truncated` | boolean | True if the output exceeded the byte cap. |

The `text` field format:

```
$ <command>
[output truncated: showing last N of M bytes]
<stdout>
<stderr, prefixed with lines if present>
(exit code: <N>)
```

The marker line appears only when truncated. The command line stays
first. The marker is the second line of `text`.

On timeout, append:

```
(timed out after <N>s)
```

Route forwards only the `text` field into the `tool_result` event.
The other fields (`exit_code`, `stdout`, `stderr`, `timed_out`,
`truncated`) are available for direct invocation and conformance tests.
The model sees the `text` field and the `is_error` flag only.

## 6. Failure modes

| Condition | Tool exit | `is_error` | `text` contains |
|---|---|---|---|
| Command runs, exit 0 | 0 | false | output + exit code |
| Command runs, non-zero exit (including 127) | 0 | false | output + exit code |
| Timeout | 0 | false | partial output + timeout note |
| Output exceeds cap | 0 | false | truncated output + marker |
| Cannot spawn process | non-zero | true | stderr diagnostic |
| Invalid input (`timeout_secs` below 1 or above 300) | non-zero | true | stderr diagnostic |

The harness creates a `tool_result` with `is_error: true` when the tool
exits non-zero. The `value.text` field is populated from stderr (capped).

On timeout, the JSON `exit_code` field is 143 and `timed_out` is true.

An input missing the `command` field is rejected by route schema
validation before the tool starts. The `tool_result` has `is_error: true`
and text "Tool arguments failed schema validation: command. Required fields are missing from the call. Resend the call with all required fields filled in."

## 7. Security

Phase 1 security model: **process boundary + cwd only.**

- The command runs as the current user with the current user's permissions.
- The working directory is the session cwd. The command can `cd` elsewhere
  if it wants. This is the model's responsibility.
- No seccomp, no landlock, no network isolation. These are deferred until a
  real security incident or a deployment need triggers them.
- The model is the policy layer. It chooses what commands to run. The harness
  enforces output size, timeout, and process isolation.

Deferred (do not build until triggered):

- Command allowlist / denylist
- OS-level sandboxing (seccomp, landlock, container)
- Approval gates for destructive commands (`rm -rf`, `git push --force`)
- Network access control

## 8. System prompt addition

Append to the existing system prompt in `config.toml`:

```text
Use the bash tool to run shell commands: builds, tests, git, file discovery,
and any task that the read/write/edit tools cannot handle.
The command runs in the session working directory. Use relative paths.
Prefer read/write/edit over bash for file operations.
Use bash for: running tests (cargo test), checking builds (cargo build),
git operations, searching (grep, find), and inspecting the environment.
```

## 9. Conformance tests (extend G4)

Add to `scripts/tool-conformance.sh`:

| Test | Input | Expected |
|---|---|---|
| `bash: echo` | `{"command": "echo hello"}` | exit 0, `text` contains `hello`, `exit_code` 0 |
| `bash: non-zero exit` | `{"command": "exit 42"}` | tool exit 0, `exit_code` 42, `is_error` false |
| `bash: command not found` | `{"command": "nonexistent_cmd_xyz"}` | tool exit 0, `exit_code` 127, `is_error` false |
| `bash: stderr` | `{"command": "echo err >&2"}` | `stderr` contains `err` |
| `bash: combined output` | `{"command": "echo out; echo err >&2"}` | both `stdout` and `stderr` populated |
| `bash: timeout` | `{"command": "sleep 10", "timeout_secs": 2}` | `timed_out` true, `exit_code` 143, tool exit 0, no `sleep` process survives |
| `bash: output cap` | `{"command": "head -c 100000 /dev/zero \| tr '\0' 'a'"}` | `truncated` true, marker is the second line of `text` |
| `bash: cwd` | Run tool with cwd set to a known directory. `{"command": "pwd"}` | `stdout` equals that directory |
| `bash: pipeline` | `{"command": "echo hello \| wc -c"}` | `stdout` contains `6` |
| `bash: spawn failure` | Run with `PATH=/dev/null`. `{"command": "echo hi"}` | non-zero exit, `is_error` true, stderr diagnostic |
| `bash: empty command` | `{"command": ""}` | exit 0, `exit_code` 0, empty output |
| `bash: timeout zero` | `{"command": "echo hi", "timeout_secs": 0}` | non-zero exit, stderr diagnostic |
| `bash: timeout negative` | `{"command": "echo hi", "timeout_secs": -1}` | non-zero exit, stderr diagnostic |
| `bash: timeout too large` | `{"command": "echo hi", "timeout_secs": 500}` | non-zero exit, stderr diagnostic |
| `bash: stdin EOF` | `{"command": "cat"}` | exit 0, `exit_code` 0, empty output |

The current harness checks the exit code and the stdout and stderr
substrings only. It has no env or cwd control. The bash section extends
`run_test` with three capabilities:

- JSON shape check: on success, parse the tool stdout as one JSON
  object and check the named fields.
- Env control: run the tool under a changed environment
  (`PATH=/dev/null` for the spawn-failure test, which hides `sh`).
- Cwd control: run the tool in a known directory (the cwd test).

With those extensions, each test checks:

- stdout is one JSON object with a `text` field.
- stderr is empty on success.
- exit code is 0 for command-level results.
- exit code is non-zero for tool-level failures.

### Mutation gate

Removing any behavior below must cause at least one conformance test to
fail.

| Behavior | Test that must fail on removal |
|---|---|
| Non-zero exit is not a tool error | `bash: non-zero exit` |
| Command not found is a command-level result | `bash: command not found` |
| Timeout sets `timed_out`, sets `exit_code` 143, and kills the process group | `bash: timeout` |
| Output cap keeps the tail and adds a marker | `bash: output cap` |
| Separate stdout and stderr capture | `bash: stderr`, `bash: combined output` |
| `exit_code` reports the command exit code | `bash: non-zero exit` |
| Spawn failure is a tool error | `bash: spawn failure` |
| Empty command is valid and exits 0 | `bash: empty command` |
| Zero, negative, or above-max `timeout_secs` is rejected | `bash: timeout zero`, `bash: timeout negative`, `bash: timeout too large` |
| Stdin is closed immediately | `bash: stdin EOF` |
| Tool runs in the harness-set cwd | `bash: cwd` |

## 10. Implementation notes

> This section is implementation guidance. It is not part of the testable
> contract. The contract is defined by sections 2 through 9.

- Implement as `tools/bash/src/main.rs`, following the same pattern as
  `tools/read`, `tools/write`, and `tools/list`.
- Use `std::process::Command` with `.stdout(Stdio::piped())` and
  `.stderr(Stdio::piped())`. Read both pipes concurrently.
- Timeout: spawn the command in its own process group. On timeout, send
  SIGTERM to the group. Send SIGKILL after 2 s if the group is still alive.
- Add `tools/bash` to the workspace `Cargo.toml` members list.
- Add `bash_max_output_bytes`, `bash_timeout_default`, and `bash_timeout_max`
  to the `[limits]` section of `config.toml`. The tool reads its limits
  from CLI flags, matching the existing tool pattern (`read`, `write`,
  `edit`, `list`). The config keys document the defaults.
- Add `bash` to the tool registration in `route`. Route discovers tools by
  reading `tools/*/tool.toml` (`bin/route/src/main.rs`, lines 54 to 63).
  Adding the `tools/bash/` directory is sufficient.

## 11. Impact

Affected:

- `tools/bash/` — new tool binary and manifest.
- `config.toml` — three new `[limits]` keys, system prompt text appended.
- `Cargo.toml` — new workspace member.
- `scripts/tool-conformance.sh` — new test section.

Unchanged:

- `claim`, `model`, `parse`, `log`, `assemble`, `route`, `user` binaries.
- All existing event types and schemas.
- All existing tools (`read`, `write`, `edit`, `list`).
- TUI (not yet built. No change required).

No new event types. No new event schemas. The `tool_result` event type
handles the bash tool output with no changes.

## 12. Migration

Additive. No existing event types change. No `v` bump. Old sessions replay
without error because no existing schema or behavior changes. The new tool
is invisible to old sessions. They simply never call it.

Consumers of the new tool:

- `assemble` reads the new manifest for the model tool list. No code change.
- `route` discovers the new tool directory. No code change.
- The system prompt gains a static suffix. The existing prefix stays
  identical for prefix-cache stability.

## 13. What this does not do

- No persistent shell state. Each call is a fresh `sh -c` process.
- No file writing. Use `write` and `edit` for that.
- No file reading. Use `read` for that.
- No directory listing. Use `list` for that.
- No streaming output. The tool waits for the command to finish (or timeout)
  before returning. Streaming is a Phase 2 concern.

## Properties

Lean-style invariants for this spec (see `lean-driven-development.md`).
One property per non-trivial invariant. Each property is observable:
given an input, an output guarantee.

P1. command-execution: given a `command` that runs to completion in the
    session cwd, observe tool exit 0, one JSON object on stdout, and
    `exit_code` equal to the command exit code in `text`, `stdout`, and
    `stderr` fields.
P2. command-failure: given a command that exits non-zero (including 127
    not-found), observe tool exit 0, `is_error` false, and `exit_code`
    equal to the command exit code. A non-zero command exit is a command
    result, not a tool error.
P3. timeout: given a command that exceeds `timeout_secs`, observe tool
    exit 0, `timed_out` true, `exit_code` 143, and no surviving process
    from the command's process group after the tool returns.
P4. output-cap: given combined stdout and stderr exceeding the byte cap,
    observe `truncated` true, the kept bytes are the tail of the combined
    output, `stdout` and `stderr` together hold at most the cap, and the
    marker is the second line of `text`.
P5. input-validation: given `timeout_secs` below 1 or above 300, observe
    tool exit non-zero, a stderr diagnostic, and no command execution.
P6. spawn-failure: given a spawn that cannot start (interpreter missing
    on PATH), observe tool exit non-zero and a stderr diagnostic.
P7. cwd: given the working directory set by the harness, observe the
    command run in that directory and the tool not read the cwd file.
P8. stdin-eof: given the child stdin closed at spawn, observe an
    interactive command (`cat`) receive EOF and exit with empty output.
P9. schema-rejection: given a model tool call missing the `command`
    field, observe the schema validation reject before spawn, a
    `tool_result` with `is_error` true, and the validation message.

## Verification

Each property maps to its proof. `proven` means the cited test or script
exists and passes. `open` names the blocker and what unblocks it.

| P# | Property | Proof | Status |
|----|----------|-------|--------|
| P1 | command-execution | `bash: echo`, `bash: pipeline` in `scripts/tool-conformance.sh` | proven |
| P2 | command-failure | `bash: non-zero exit`, `bash: command not found` in `scripts/tool-conformance.sh` | proven |
| P3 | timeout | `bash: timeout` in `scripts/tool-conformance.sh` | proven |
| P4 | output-cap | `bash: output cap`, `bash: output cap keeps tail` in `scripts/tool-conformance.sh` | proven |
| P5 | input-validation | `bash: timeout zero`, `bash: timeout negative`, `bash: timeout too large` in `scripts/tool-conformance.sh` | proven |
| P6 | spawn-failure | `bash: spawn failure` in `scripts/tool-conformance.sh` | proven |
| P7 | cwd | `bash: cwd` in `scripts/tool-conformance.sh` | proven |
| P8 | stdin-eof | `bash: stdin EOF` in `scripts/tool-conformance.sh` | proven |
| P9 | schema-rejection | `validate_args` tests in `bin/route/src/main.rs` (`mod tests`), the `route` schema-validation path | proven |

## Gate

The acceptance commands. All must exit 0 for this spec to be proven.

```
cargo build
cargo test
scripts/tool-conformance.sh
```
