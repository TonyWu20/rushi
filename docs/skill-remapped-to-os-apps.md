# OS + Applications: the base distribution and project-specific tools

Status: Approved (2026-09-03). Applied so far: the application
scripts sit on the agent-visible path (`scripts/`), and each of the
four aligned apps self-documents via `--help` (`band_match`,
`timestamp_compare`, `capture-thinking-border`, `verify-reattach`);
no SKILL.md exists. Not built yet: the `tools --list` catalog and
the TUI `/`-window (section 5); the fenced-host failure reporting
audit (section 6).

Seeded from concrete friction this session: the PTY-capture /
TUI-verify procedure was inlined three times before it consolidated
into `scripts/tui-capture.py`. That capability — an *application*,
only needed while developing the TUI — is the concrete case the
design must absorb.

## 1. Start from the OS + Applications hierarchy

The harness is a **distribution** with two layers, the classic Unix
split:

- **The OS (the base distribution):** a small, fixed, well-defined
  set of *core, necessary* tools. The test is one: **without them,
  we cannot (a) complete the agent loop, (b) give the human a basic
  UI, or (c) grow anything we later find necessary.** Anything that
  passes that test is base; it ships for *every* general-purpose
  project.
- **Applications:** everything built *after* the base is
  **project-specific**, tailor-made for one project. `tui-capture`
  is an application: it is only needed while we are developing the
  TUI. It is not in the base; it is installed per-project.

The design work is therefore to (1) name the base precisely, and
(2) keep everything after it as self-documenting, short-lived,
composable commands on a path the agent can see — not a new runtime,
not a capability daemon, not a protocol server.

## 2. The base distribution, in two tiers

**Tier 1 — the kernel (hard-required).** The agent loop and the
growth machinery. Without any of these the distribution stops
working:

- *Agent-loop core:* `assemble` (prompt/request), `model` (call),
  `parse` (actions), `route` (supervise/execute), `user` (human I/O),
  `log` (event/transcript append), `claim`, `compact`.
- *Base agent-facing tools:* `tools/{read, write, edit, list,
  bash}` — the minimal file/ops toolkit any general project needs;
  `bash` is the composability primitive (the pipe).
- *Growth machinery:* the `tools/` directory, `route`'s execution
  path, the extension host (`ui_extensions/`, `ext-rs/`), and the
  **PATH-registration mechanism** itself. Without these we cannot
  add applications, so they are base.

**Tier 2 — the default, swappable front-end (the WM tier).**
`tui` plus its bundled `ui_extensions` JSON-render architecture (the
`tui` draws and renders per the JSON the extensions emit). It is the
default, more common user entry than `bin/user`; it is rated
**important** (there is a body of TUI work to reach the baseline a
common user expects of an agent tool's TUI) but **swappable**, in
the way a distro's default window manager is: it ships in the base,
you can replace the front-end, and the kernel (loop + tools +
`route`) keeps working. The base *includes* a default front-end; it
does not *mandate* one.

Everything past these two tiers is an application.

## 3. Registration: a tool is on the agent-visible PATH

There is no manifest index baked into config or the prompt. A tool
or extension **registers by being on the path the agent can see** —
the `tools/` directory that `route` and the `bash` tool search.
Drop it there (or add its directory to the search path) and it is
available; that is the whole registration. It is the `$PATH` /
`command -v` pattern, not a registry.

## 4. Self-documentation: `--help`, not a SKILL.md

A tool documents **itself**. Its interface is its **`--help`**; a
man page is an optional human extra. **There is no `SKILL.md`.**
The unit of agent-facing documentation is the command's own help
text, fetched on demand and landing in the transcript tail.

The one thing `SKILL.md` used to carry — a *multi-step procedure* —
has a Unix resolution: **make the procedure a script.** A capability
that is really several steps becomes *one* short-lived,
self-documenting command on the path. Procedure becomes executable,
not a doc file. (`tool.toml` already carries a one-line
`description`; that is the catalog payload, section 5.)

### 4a. Escape hatch: keep a plain-text procedure file

The model above drops `SKILL.md` on purpose. If you want to keep a
free-form text file anyway, here is a cache-safe pattern.

- **Point the agent at the file.** Keep the procedure in a plain
  markdown file on disk. Tell the agent its path in the task. The
  agent reads it with the `read` tool when needed. That read lands
  in the transcript tail, not the prefix. So the cached prefix stays
  byte-identical.
- **Survive compaction with a `compact.before` hook.** Compaction
  may drop the file from the context. If that worries you, register
  a `compact.before` hook that returns `replace`. It supplies its
  own summary with the reminder embedded. That summary shadows the
  old log region, so the reminder persists into the compacted
  context. The hook runs once per compact, not every round. See
  `docs/loop-lifecycle-hooks.md` §3.4. Note that `compact.after`
  is observation-only and cannot inject content.
- **Choose the payload yourself.** Inject a short hint, such as
  "read `docs/foo.md` before continuing," or inject the file's
  full content. The harness does not decide. Pick the amount that
  fits the task.
- **Inject once, not every round.** Do not re-inject the full text
  every round. That brings back the rigidity and token waste of the
  old goal-block design. One injection at compact time is enough.

## 5. Discovery: `ls` gets the floor, a catalog gets the point

- **The floor is `ls`.** The agent already has `bash`; `ls` over the
  tool directory lists **names** with zero new code. For a small set
  of self-explanatory tools, that is enough to discover.
- **`ls` alone misses the point of disclosure.** It gives names
  only and throws away the one-line **`description` that already
  lives in each `tool.toml`** ("Read a file and output its
  contents", "Run a shell command…"). Without it the agent must
  `--help` every tool to learn what it does (cache-safe but N+1),
  and the TUI's window would re-derive the listing on its own.
- **The Unix-native answer to "list what is installed, and what
  each does" is a catalog** — `pkg --list`, `compgen -c`,
  `command -v` — not `ls`. So we add one small **read-only**
  catalog: `tools --list` reads each `tool.toml` and prints
  `name — description`, sorted, with an optional `--json` form for
  the TUI.

**Decision:** `ls` is the floor (works today, no code).
**`tools --list` is the TUI and human catalog**, because
`tool.toml` already carries the `description`. The agent's tool
list lives in the system prompt prefix, generated by `assemble`
from the same manifests (`system-prompt-generation.md` D4).
`tools --list` remains a TUI and human utility, not the model's
discovery path.

## 6. Trust (a tool on the path is not trusted *because* it is on
the path)

- A tool or extension in the tree is trusted the way you trust the
  tree: **you review its diff.** Being on the path makes it
  *runnable*; it is *trusted* only because it is in the reviewed
  tree.
- No auto-invocation. A tool runs only when the agent — already
  trusted to run `bash` — calls it; never on external input.
- The host-only surface is fenced: `route` applies caps, no `../`
  escapes, and a timeout; a failure is **reported** to the agent as
  not-run / failed, never a hang.

## 7. Cache friendliness (generated tool list in the prefix)

Superseded by `system-prompt-generation.md` D4 (2026-09-13).
`assemble` generates the tool list into the prompt prefix from the
discovered manifests. The prefix is byte-stable in steady state.
Adding, removing, or rewording a tool costs one prefix cache rebuild.

- **The tool list is prompt-resident.** `assemble` renders one
  `- name: description` line per tool, in manifest discovery order.
  It is a pure function of the config and the on-disk manifests,
  so it is deterministic per session.
- **`tools --list` remains a TUI and human catalog.** It sorts by
  name, is byte-identical on repeat calls, and feeds the TUI `/`
  window. It is no longer the model's discovery path. The model
  reads its tool list from the system prompt.
- **Fragments join the prefix.** Extension fragments (goal block,
  etc.) are appended after the tool list and cwd line. A fragment
  add or remove costs one prefix rebuild. Steady-state turns are
  byte-identical.

## 8. Prior art, and where we diverge

- **Agent Skills (Claude/Anthropic):** a `SKILL.md` (name +
  description + optional allowed tools) plus bundled scripts, with a
  *progressive-disclosure index resident in the prompt*. We diverge
  on **no `SKILL.md`** (the tool self-documents via `--help`;
  procedure is a script). The tool list is now prompt-resident
  (generated by `assemble`, see `system-prompt-generation.md` D4),
  so the "no prompt index" divergence no longer holds.
- **MCP tools:** a JSON-RPC server exposing typed tools/resources.
  We keep the tool on the existing supervised `route` path; no
  daemon, no protocol.
- **OS + Applications (the model):** kernel + userland +
  applications; tools self-document, are found on a path, and are
  supervised. This is the shape this harness follows.

## 9. Not doing

- No `SKILL.md` (interface is `--help`; procedure is a script).
- No per-tool SKILL.md files. The tool list is prompt-resident,
  generated by `assemble` from manifests (section 7,
  `system-prompt-generation.md` D4).
- No capability daemon or JSON-RPC server. A tool is a command.
- No global registry or install step. Tools are files in the tree,
  found on the path, versioned with the repo.
- No auto-invocation. The agent calls a tool; nothing calls one on
  its own.

## 10. Acceptance (for when it is approved)

- The base is named and matches the "three failures" test: without
  it we cannot complete the loop, give a basic UI, or grow
  anything. Two tiers: the kernel (loop core + base tools + growth
  machinery) and the default, swappable front-end (`tui`).
- A tool registers **by being on the agent-visible path**. It
  self-documents via `--help`. **No `SKILL.md`** exists.
- `tools --list` (catalog) reads the `tool.toml` `description`, is
  sorted and on-demand, and feeds the TUI `/`-window. The agent's
  tool list is in the prompt prefix, generated by `assemble` from
  the same manifests.
- The front-end (`tui` + `ui_extensions`) is default but swappable,
  in the WM sense.
- A fenced tool that fails supervision is a reported failure, not a
  hang. The cached prompt prefix is byte-stable in steady state.
  A tool-set change costs one prefix rebuild.

## 11. Worked example: triaging `scratch/` under the model

`scratch/` is the anti-pattern this model removes: a black hole of
one-off scripts, fixtures, generated output, and external references
with no names, no self-documentation, and no place in the hierarchy.
The model's test — *where does this belong?* — answers for every item,
so the directory itself can be deleted.

The capture pattern that seeded this proposal is the clearest case. It
was inlined three times and coalesced into `scripts/tui-capture.py`.
That generalization collapses the ad-hoc capture scripts into *one*
self-documenting application: a thinking-border check is now
`tui-capture.py --expect "38;5;3"` on the marker session, not a
bespoke script. One application instead of a pile.

The full triage of `scratch/`:

| Item | Role | Where the model puts it |
|---|---|---|
| `capture_tui.py`, `capture-thinking-border.py`, `thinking-cfg.toml` | ad-hoc PTY / border capture | superseded by the `tui-capture` application → delete |
| `band_match.py`, `timestamp_compare.py`, `timestamp_analysis.py` | one-off model-timing analyses | done their job; findings live in `docs/` → delete (or keep one note) |
| `ctest/` | hand-built compact-e2e fixtures | the committed `compact-e2e.sh` **self-generates** its fixtures into `scratch/e2e-compact/`, so `ctest/` was never wired in → stale, delete |
| `replay/`, `replay2/` | generated replay-session data | generated output → delete |
| `color-capture-raw.bin` | raw capture output | an output, not source → delete |
| `verify-reattach.py` | FT-003 reattach feature test | belongs with the reattach feature, not scratch → move |
| `vim_editor_parts_stale/` | literally *stale* | junk → delete |
| `refs/pi-vim` | external reference repo | not committed; **`.gitignore`d** so a `git add -A` cannot sweep it in |

Read through the model: the "capture" entry — the original friction —
stops being a pile and becomes a named application; tool fixtures and
generated data are dropped rather than hoarded; one-off analyses that
have finished are let go; and the one genuinely external reference is
fenced by `.gitignore` instead of committed. The black hole empties;
`scratch/` goes.

_Status as of this writing: `ctest/`, `replay/`, `replay2/` removed;
`refs/pi-vim` gitignored. The redundant capture scripts, the one-off
analyses, and the stale/junk entries above are still pending the
human's call. `ctest/`'s removal is verified safe: `bin/compact` is
now committed, and its `scripts/compact-e2e.sh` self-generates
fixtures into `scratch/e2e-compact/` — no committed test references
`ctest/` or `replay/`, and the `threshold` scenario still passes
(7/7) after the removal._

## Properties

Lean-style invariants for this spec (see `lean-driven-development.md`).

P1. path-registration: given a tool under `tools/` with a `tool.toml`, observe `route` discover it and make it runnable. Being on the path is the whole registration.
P2. self-doc: given each aligned app run with `--help`, observe a self-documenting help text and no `SKILL.md`.
P3. catalog: given `tools --list`, observe a sorted, deterministic listing of `name — description` with an optional `--json` form, shared by the agent and the TUI.
P4. prefix-stability: given an unchanged tool set and fragment set, the prompt prefix stays byte-identical across consecutive turns. A tool-set or fragment change costs one prefix cache rebuild.
P5. fenced-failure: given a fenced tool that fails, observe the host report it as not-run or failed to the agent, never hang.

## Verification

| P# | Property | Proof | Status |
|----|----------|-------|--------|
| P1 | path-registration | `scripts/tool-conformance.sh` runs the `tools/*` binaries through `route` discovery | proven |
| P2 | self-doc | Blocked: no test asserts the `--help` output of the four aligned apps. Unblock with a conformance row that runs each app's `--help` and checks for non-empty help. (The `lean-verify` tool's `--help` is asserted in `scripts/tool-conformance.sh`.) | open (lean-verify covered) |
| P3 | catalog | Blocked: the `tools --list` catalog and the TUI `/`-window are not built. Unblock when the catalog lands with a sorted, byte-identical test. | open |
| P4 | prefix-stability | `scripts/cache-e2e.sh` asserts a byte-identical cached prefix across steady-state turns. A tool-set change triggers one rebuild | proven |
| P5 | fenced-failure | the spawn-failure rows in `scripts/tool-conformance.sh` assert a reported failure, not a hang | proven |

## Gate

Gate: blocked — the `tools --list` catalog and the TUI `/`-window in section 5 are not yet built.

```
cargo build
cargo test
scripts/tool-conformance.sh
scripts/lean-verify-e2e.sh
scripts/cache-e2e.sh
```

The `lean-verify` tool (and its flake-managed toolchains: `devShells.lean`,
`devShells.aeneas`) is registered the way the model says a tool registers:
on the agent-visible path with a `tool.toml` manifest, self-documenting via
`--help`, with no SKILL.md. Its multi-step spec-driven workflow is one
short-lived command with ops (`init`, `build`, `drt`, `translate`), and
every environment dependency is a flake devShell, never an ad-hoc install
(docs/aeneas-rust-to-lean.md). The `--help` output also carries the
spec-driven loop itself — including the "follow the proven spec to
implement the Rust" step that `docs/lean-driven-development.md` §3.3
defines — so the instruction travels with the tool, not in a SKILL.md.
