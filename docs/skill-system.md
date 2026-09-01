# Skill system: agent-invoked capabilities

Status: Proposal (2026-09-02). Not built. Do not build until the
human approves.

Seeded from concrete friction this session: the PTY-capture /
TUI-verify procedure was inlined three times before it consolidated
into `scripts/tui-capture.py`. That capability — *the first skill* —
is the concrete case the design must absorb.

## 1. Start from the Unix philosophy

The core principle here is the Unix one: small, single-purpose
parts, composed through plain interfaces. A skill system is not a
plugin runtime, not a capability daemon, and not a protocol server.
It is that principle applied to *procedural capabilities*: named,
discoverable, executable packages the agent invokes the way a user
invokes a command.

Each design choice follows from the principle:

- **Do one thing, do it well.** A skill is one capability with one
  purpose (capture a PTY, verify a border color, compact a log), not
  a kitchen sink.
- **Work through plain interfaces, in series.** A skill reads text
  (arguments, stdin, a named input file) and writes text (stdout, a
  named output file). One skill's output is the next skill's input.
  The pipe already exists: the `bash` tool.
- **Small, well-defined parts, found on the filesystem.** Skills live
  under one well-known root, one directory each, found by scanning
  that root — the `$PATH` / `command -v` pattern. Discovery is cheap
  and flat: read a manifest, not a daemon's catalog.
- **Leverage, via metadata.** Each skill carries a tiny manifest:
  what it is, how to run it. The manifest is read at discovery; the
  deep material (instructions, reference resources) is read only when
  the skill is actually invoked. Progressive disclosure: the one-line
  `usage` is always in memory, the man page is on disk.
- **Prefer the map over the filter, at the host.** The host maps a
  stable interface (the manifest schema) and pushes the variation
  (what the skill does) out to the skill itself. It never
  specializes per skill.
- **No global state.** A skill is short-lived and stateless across
  calls, like a command, not a resident service. It owns no process
  that outlives the call.

## 2. What a skill is, and what it is not

Three existing surfaces must not be confused:

- **Tools** (`tools/`, `schemas/tools/`) — *data operations*
  (read, write, edit, list, bash). Schema-validated, spawned per
  call by `route`. They read and write the world.
- **Extensions** (`ui_extensions/`, `ext-rs/`) — *long-lived UI
  processes* over a JSONL boundary, supervised by the TUI host.
  They render the session; the host owns their lifecycle.
- **Skills** (proposed) — *agent-invoked procedural capabilities*.
  The **agent** chooses to invoke one, the way it chooses to run a
  command. Short-lived, text in / text out, found by scanning a
  directory.

A skill is *how-to knowledge, packaged*: a manifest that names it and
describes it, an entry point that runs it, and optional instructions
and reference resources the agent reads on the way through. It is not
a new transport (it runs as a command), not a daemon (it exits), and
not a schema-validated data op (its contract is prose plus a
command; it *may* expose a JSON schema, but does not have to).

## 3. The first skill: `tui-capture`

The concrete need this started from. To verify a TUI change the
agent must: spawn the TUI under a PTY against a session, feed it a
scenario, let it settle, then assert on the rendered screen text and
the emitted SGR (color) sequences. This session inlined that
procedure three times before it became `scripts/tui-capture.py`.
That file, plus a manifest, *is* the first skill:

```
skills/tui-capture/
  skill.toml     # name, one-line description, entry command
  run.sh         # wraps: python3 <repo>/scripts/tui-capture.py "$@"
  NOTES.md       # how to read the SGR families, what to assert
```

The agent invokes it the way it invokes `bash`: named, discoverable,
documented, short-lived. No re-derivation, no ad-hoc PTY harness.

## 4. Common designs (prior art)

The shape is the one agent harnesses converged on:

- **Agent Skills** (Claude/Anthropic): a folder of `SKILL.md`
  (YAML frontmatter — name, description, optional allowed tools)
  plus bundled scripts and reference files. Progressive disclosure:
  the agent lists names and descriptions, and reads the body only
  when relevant.
- **MCP tools**: a server exposes typed tools, resources, and
  prompts over JSON-RPC; the client discovers and calls them.
- **Agent commands** (Claude Code, Copilot): markdown with
  frontmatter and an argument placeholder; the body is a procedure
  the agent follows, sometimes with executable steps.

What this repo adds, in the Unix direction: a skill is *a command on
a search path with a manifest* — not a protocol server (no JSON-RPC,
no daemon) and not just a prompt to follow. The executable entry
point is first-class; discovery is a directory scan the host does
once.

## 5. Proposed shape

- **Root.** A `skills/` directory at the repo root (configurable,
  like `[ext] dir`). One subdirectory per skill. A directory that
  lacks a manifest is ignored with a one-line note, mirroring the
  extension host's load-order discipline.
- **Manifest** (`skills/<name>/skill.toml`), the minimum the host
  needs:
  ```toml
  [skill]
  name        = "tui-capture"
  description = "Capture the TUI on a PTY for a session; assert screen text and SGR."
  entry       = "run.sh"          # or a Rust binary, or "python3 tui-capture.py"
  # optional
  args        = "SESSION [FLAGS]"  # usage string
  resources   = ["NOTES.md"]      # files the agent may read
  caps        = []                 # network / fs-write, mirroring the ext host
  ```
- **Discovery.** The host (or a small `route` sibling) scans the
  root, reads the manifest, and presents the agent a flat list:
  `name — one-line description`. The agent reads the manifest and
  resources only on the call.
- **Invocation.** The host runs `entry` under the *same supervision
  a tool gets*: a timeout, an output cap, a working directory, and
  the declared `caps`. It is a command, not a session.
- **Composition.** Because I/O is text, skills pipe through `bash`
  with no new mechanism. `skill-a | skill-b` is just a shell
  pipeline of two supervised commands.

## 6. Guardrails (consistent with the tool contract)

- A skill that declares no `caps` gets no network and writes only
  under the repo, exactly as a tool is fenced.
- `entry` resolves relative to the skill directory; no absolute
  paths, no `../` escapes (the same rule the extension host applies
  to its load order).
- A skill that fails supervision (timeout, cap exceeded) is
  reported to the agent as a not-run / failed outcome, not a hang.

## 7. Open (not decided here)

- **Where discovery runs.** A `route` sibling, a host-side lister,
  or a `skills --list` the agent runs. The Unix answer is the last:
  a command whose output the agent can pipe.
- **Argument contract.** Default: prose `args` plus a free-form
  body. A JSON schema is optional, per skill.
- **Versioning.** Default: no `v` field until a skill changes.

## 8. Not doing

- A capability daemon or a JSON-RPC server. A skill is a command.
- A global registry or an install step. Skills are files in the
  tree, found by scan, versioned with the repo.
- Auto-invocation. The agent decides to call a skill; nothing calls
  one on its own.

## 9. Acceptance (for when it is approved)

- `skills/tui-capture` exists with a manifest and runs, replacing
  the ad-hoc PTY harness in the TUI feature checks.
- Discovery lists it by name and one-line description; the agent
  invokes it as a supervised command.
- A capability-less skill is fenced (no network, repo-only writes).
- A skill that times out is a reported failure, not a hang.
