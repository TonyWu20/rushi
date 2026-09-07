# Porting the pi-config extension set into the harness

Status: Investigation (2026-09-10). Maps the eight extensions in
`~/programming/pi-config/flake.nix` onto this harness. It answers one
question per extension: where does it land under the `phase-2-plan.md`
architecture and the `skill-remapped-to-os-apps.md` model, and how much
work does the port take.

This is an investigation, not a build plan. No code lands here.

## 1. The harness has four extension points

The pi extension API is one call, `pi.on(event, handler)`. The harness
splits that one surface into four. Each pi feature maps to one of them.

| Harness point | What it is | Status |
| --- | --- | --- |
| Tool / App | A CLI under `tools/<name>/` with a `tool.toml`. It registers by sitting on the agent-visible PATH. The agent calls it. | Built. `route` dispatches it. |
| Lifecycle hook | A command on a path. It gets JSON on `stdin` and writes one JSON decision to `stdout`. The harness fires it on a named window. | Spec only. `loop-lifecycle-hooks.md`. |
| UI extension | A process over JSONL on stdio. It owns one cap: `render`, `status`, `transform`, `append`, `notify`, or `frame`. | Built. `bin/tui/src/ext.rs`. |
| Loop orchestration | Prompt injection, auto-continue, compaction veto. These sit inside the host loop. | No analog yet. |

The `tool.before` window is the seam for every pre-tool-use check. It is
named in `loop-lifecycle-hooks.md` section 3.5. It is the landing place
for the `no-find-grep` class.

Two gaps existed that several extensions need:

- The lifecycle hook ABI. `bin/harness` and `crates/common` do not
  exist. There is no `[hooks]` in `config.toml`.
- The approval round-trip. The TUI renders a banner. But no code emits
  an `approval_request`. No loop code waits for the answer. No schema
  files exist.

The revised `phase-2-plan.md` now closes both gaps. The hook ABI
lands in stage 2 of the plan. The approval round-trip (section 4.8
of the plan) lands in stage 3. The `run.idle` goal-continuation
window (plan section 4.9) lands in stage 3. The tool sidecar
boundary (plan section 4.10) is a clarification, not a build item.

## 2. Extension-by-extension assessment

### `pi-goal` (narumiruna)

It drives a goal across turns. The user opens a goal. The agent keeps
going until it is done. It ships two tools (`goal_complete`,
`goal_blocked`), a `/goal` command, and about nine lifecycle handlers.

The core logic splits into three parts. Each part lands on a different
harness point.

- The two tools and the pure logic. This is the queue math, the prompts,
  the token accounting, and the persistence. These port as plain tools
  and plain functions. Easy.
- The loop orchestration. This is the hard part. The extension injects
  a goal prompt every turn. It auto-continues a stalled goal. It vetoes
  a host compaction when the budget is low. These need a per-turn prompt
  injection point, an "agent settled" signal, and a compaction veto.
  The harness has none of the three.
- The budget and queue state. This persists on the session log. It fits
  the harness event-log model cleanly.

Your harness already self-hosts this work. It is the loop itself. So the
port is really a re-host of the loop, not a tool.

**Status: Ported (2026-07-04).** The goal tools
(`goal-tools/goal/`, `goal-tools/goal_complete/`,
`goal-tools/goal_blocked/` in the rushi-exts repo), the goal-state crate
(`crates/goal-state/`), the four hook binaries
(`goal-hooks/hook-goal-idle/`, `goal-hooks/hook-goal-compact/`,
`goal-hooks/hook-goal-tools/`, `goal-hooks/hook-goal-arm/` in the
rushi-exts repo), the TUI command-palette
extension (`ui_extensions/goal/`), and the conformance e2e
(`run-idle-continue-e2e.sh`, rushi-exts root) are all built and tested.
See `docs/pi-goal-readiness.md` for the full assessment.

### `rpiv-ask-user-question` (juicesharp)

It adds the `ask_user_question` tool. It shows a structured question in
the TUI. The user picks options. The answer returns to the model.

The piece splits in two.

- The data model. One tool, one JSON schema. Questions, options, and
  the answer envelope are plain JSON. This ports to a `tools/` entry
  almost as-is. Easy.
- The dialog. It is a full interactive TUI. It has an option list, a
  multi-select view, a side-by-side markdown preview, and a submit
  picker. That is built on the pi TUI component contract. It has no
  direct harness analog.

For the interface you named, `ui_extensions` is the right home. The
dialog would run as an external process over the JSONL host. It would
send the questionnaire out and read the answer back. That is the
`render` cap, plus a new interactive reply path.

The deeper need is a loop pause. Today the harness loop never waits on
a human. The approval UI has no producer and no consumer. A full
`ask_user_question` port needs the approval round-trip from section 1.

**Verdict: Easy for the tool and the data model. Hard for the dialog
and the loop pause.** The UI-extension host is built. The loop pause is
not. Start with the tool and a simple answer path. Add the full dialog
later.

### `pi-fff` (dmtrKovalenko)

It swaps `find` and `grep` for FFF, a Rust fuzzy-search engine. It
ships three tools: `ffgrep`, `fffind`, and `fff-multi-grep`. It also
adds an `@`-mention autocomplete provider.

The user said this one is not needed yet. I note the fit anyway.

- The engine. FFF is already Rust. The harness is Rust. A Rust harness
  can link the FFF core directly. It skips the napi and JS wrapper.
  This is a plus, not a cost.
- The tools. Each one becomes a CLI under `tools/`. It reads JSON args
  on `stdin` and writes JSON on `stdout`. The frecency and history DBs
  are plain files. Easy.
- The autocomplete. This is a pi TUI provider. It has no Unix-tool
  analog. The harness already has an `@` file picker, see
  `tui-file-picker.md`. That picker can reuse the FFF engine. Drop the
  pi autocomplete provider.

This is the cleanest fit for the OS + App model. A native app on the
PATH. The agent discovers it with `tools --list` once that catalog
lands.

**Verdict: Medium and optional.** The search tools are easy and native.
The autocomplete is pi-only and is already covered by the file picker.

### `agent-simple-english` (jyooi)

It enforces simple-technical-English rules on agent prose. It ships a
pure lint engine. It has two faces.

- A pi extension. It adds a `say` tool and gates the `bash`, `write`,
  and `edit` tools. It also patches the message boundary for strict
  mode. This face is pi-only.
- A Claude Code hook set. This is the face that matters here. It runs
  one CLI on four events: `SessionStart`, `PreToolUse`, `Stop`, and
  `UserPromptSubmit`. The CLI reads a JSON event on `stdin`. It runs
  the lint. It writes a JSON decision on `stdout`.

That CLI shape is exactly the harness hook ABI. One command on a path,
JSON in, JSON decision out. The lint engine is pure. It uses no
binaries and no network. It ports with no change.

The mapping is direct.

- `PreToolUse` maps to the `tool.before` window.
- `SessionStart` maps to the `session.start` window.
- `Stop` maps to the `step.end` or `session.end` window.
- `UserPromptSubmit` maps to the `model.before` window. The hook
  can transform the request to inject lint context before the model
call. No new window is needed for this surface.

The lint rules run on the tool input and the reply. That is a policy
check on the `tool.before` batch and on the final reply.

**Verdict: Easy and it is the best hook template.** It is already in
the harness hook shape. It is the first hook to build once the hook ABI
lands in Phase 2. The pi `say` tool and the strict-mode patch stay
optional.

### `no-find-grep.ts` (local)

It blocks bare `find` and `grep` in a `bash` call. It steers the agent
to `fd` and `rg`. It also flags three `rg` misuses: the `-r` flag, the
`-L` flag, and an escaped pipe.

It is one `tool_call` hook. It scans the command with a few regexes. It
returns a block with a reason. The regex bodies port with no change.

In the harness, this is a `tool.before` hook. It reads the pending
`bash` tool call on `stdin`. It runs the same regexes. It returns
`block` with the reason when a bare `find` or `grep` shows up.

There is one plumbing gap. When the hook blocks, the model must read the
reason. That reason has to come back as a `tool_result`. The `route`
binary reports a failed tool as a result today. A blocked tool needs
the same path. The revised plan (section 4.3 step 7) now specifies
this: the loop synthesizes a `tool_result` with `is_error: true` and
the hook's reason for each blocked call. No `route` call is made
for blocked calls.

**Verdict: Easy.** The logic is a small regex set. The work is the
hook ABI and the block-reason reply. Both live in Phase 2.

### `no-bare-python.ts` (local)

It blocks a bare `python` or `python3` call. It allows a call only when
`uv run` is present. It steers the agent to `uv` managed python.

It is the same shape as `no-find-grep`. One `tool_call` hook, one
regex, a block with a reason. It maps to the `tool.before` window in
the same way.

**Verdict: Easy.** Same work as `no-find-grep`. Share one hook binary
that scans the command once and applies both rule sets.

### `pi-lynx` (dabito)

It gives the agent plain-text web access. It ships seven tools. Each
tool shells out to `lynx` and to a search engine. It uses no API keys.

Every tool is a thin wrapper. It builds a URL. It runs `lynx -dump` or
fetches a page. It parses the text. It returns a string.

This is the cleanest 1:1 fit in the set. Each tool becomes a CLI under
`tools/`. It reads JSON args and writes JSON out. `lynx` is just an
external binary, like `fd` or `rg`. Drop the pi `renderCall` and
`renderResult` bits. They are cosmetic.

**Verdict: Easy.** The seven tools are seven small CLIs. The only
dependency is `lynx` on the PATH.

### `pi-terminal-browser` (local, terminal-browser-flake)

It fetches pages through a headless Chromium. It ships three tools:
`tb_fetch`, `tb_browser`, and `tb_shutdown`. It keeps a browser daemon
alive between calls.

The tools are easy. Each one is a thin CLI. It sends a command to a
live terminal-browser daemon and returns text. That fits `tools/`.

The part that does not fit the pure one-shot tool model is the daemon.
It is a long-lived process. It holds a CDP session. It outlives one
tool call. The harness tool contract is one-shot and stateless.

The fix is to treat the daemon as a service, like the model server.
The tools are still one-shot. They just talk to a running daemon over
its socket. The daemon sits outside the harness, beside the model
endpoint. That keeps the tool one-shot.

One note: the user verified it reaches a journal paper behind a
university subscription. That value is real and personal. The port is
worth doing if the model browser use is in scope.

**Verdict: Easy for the tools. Medium for the daemon boundary.** Wrap
the three tools as CLIs. Declare the terminal-browser daemon a
long-lived service, not a tool. That keeps the one-shot contract clean.

### `pi-automode` (czottmann)

It is a permission guardrail in the style of the Claude Code auto mode.
Before each tool call, it runs a chain. It checks glob deny rules. It
runs a shell hard-deny parser. It fast-paths read-only tools. It asks
an LLM classifier when unsure. It blocks the call when the classifier
says no.

The pieces map cleanly.

- The `tool_call` veto. This is the `tool.before` window. It is the
  designed home for policy and approval.
- The deterministic layers. The glob rules, the shell parser, and the
  protected-path list are pure code. They port as-is.
- The LLM classifier. It calls the pi model client. In the harness it
  must call the model endpoint from `config.toml`. That endpoint
  exists. So the classifier can run.
- The `/automode` command and the status line. These are small. They
  map to a tool and an `ext_status` value.

The classifier is the one real piece of work. Everything else is a
port of pure logic onto the `tool.before` window.

**Verdict: Medium.** The shape fits the `tool.before` window. The
deterministic layers port as-is. The classifier needs a model call,
which the harness already has. It is the natural follow-on after the
hook ABI lands.

## 3. What the set says, together

Group the eight by how hard they are to land.

| Effort | Extensions | What they need |
| --- | --- | --- |
| Easy, no new infra | `pi-lynx`, `pi-terminal-browser` | A `tools/` entry each. Both are tool CLIs. `tb` needs a service note for its daemon. |
| Easy, needs the hook ABI | `no-find-grep`, `no-bare-python`, `agent-simple-english` | The `tool.before` window and a block-reason reply. `simple-english` adds one prompt-submit window. |
| Medium, needs the hook ABI | `pi-automode`, `pi-fff` | The `tool.before` window. `automode` adds a classifier model call. `fff` is optional. |
| Hard, needs loop seams | `pi-goal`, `rpiv-ask-user-question` | New loop seams. `goal` needs prompt injection, idle-continue, and a compaction veto. `ask-user` needs the approval round-trip. |

The easy rows wait on nothing. The tools can land today as `tools/`
entries. The hook rows wait on one thing: the Phase 2 hook ABI.

The two hard rows wait on two deeper seams. They are not tool ports.
They are loop features.

## 4. The two blockers, stated plainly

Two infra pieces decide the whole set.

1. **The lifecycle hook ABI.** This is the `tool.before` window and
   its friends. It is named in `loop-lifecycle-hooks.md`. It is built:
   `crates/common/src/hooks.rs` holds the dispatcher and
   `bin/harness` fires all 13 windows.
2. **The approval round-trip.** The `approval_request` and `approval`
   schema files exist in `schemas/events/v1/`, `bin/claim` derives the
   `awaiting_approval` state, and `bin/harness` waits for the answer.
   The agent can pause for a human answer.

Both were on the Phase 2 path. As of 2026-09-13 both are built:

- The hook ABI is in `crates/common/src/hooks.rs` and fires from the
  `harness` loop. All 13 windows (including `tool.before`,
  `compact.before`, `model.before`, and `run.idle`) are wired and
  gated by `scripts/model-before-transform-e2e.sh` and the compact
  e2e suite.
- The approval round-trip is in `bin/claim` (`awaiting_approval`
  state), `bin/harness` (`run_awaiting_approval`), the two schema
  files in `schemas/events/v1/`, and the `[limits].approval_timeout_s`
  config key.

The `run.idle` window (plan section 4.9) and the `model.before`
transform path address the `pi-goal` idle-continue and per-turn
prompt-injection needs. Both are built. `pi-goal` is now
application-level porting work; see `docs/pi-goal-readiness.md`.

## 5. Suggested order

Work the set from easy to hard. As of 2026-09-13 the Phase 2 loop is
built: the hook ABI, the approval round-trip, the `run.idle` window,
and the `model.before` transform path all ship in `harness`.

1. Land the easy tool ports. `pi-lynx` and `pi-terminal-browser`
   as `tools/` CLIs. No new infra needed.
2. Port `no-find-grep` and `no-bare-python` as one `tool.before`
   hook. The block-reason reply is already in `route`.
3. Port `agent-simple-english` as a `tool.before` and `session.start`
   hook.
4. Port `pi-automode` as a `tool.before` hook with a classifier call.
5. Port `rpiv-ask-user-question` on the approval round-trip.
6. Port `pi-goal` as application-level tools plus hooks. See
   `docs/pi-goal-readiness.md`.
7. Keep `pi-fff` in reserve. Its engine is native and its picker is
   covered. Port it only when the need returns.

## 6. Sources

- `~/programming/pi-config/flake.nix` — the extension set.
- `docs/phase-2-plan.md` — the `harness` and `harness-common` spec.
- `docs/skill-remapped-to-os-apps.md` — the OS + App model.
- `docs/loop-lifecycle-hooks.md` — the hook ABI and the windows.
- `docs/ui-extension.md` — the UI extension host and caps.
- `bin/tui/src/ext.rs` — the built UI extension host.
- `bin/route/src/main.rs` — the tool dispatch.
- `~/programming/terminal-browser-flake/pi-terminal-browser/` — the
  `tb` extension source.
