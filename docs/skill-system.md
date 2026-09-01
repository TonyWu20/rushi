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
  deep material (the user manual, reference resources) is read only
  when the skill is actually invoked. Progressive disclosure: the
  one-line description is always in memory, the manual is on disk.
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
describes it, a user manual (prose) the agent reads on demand, and —
optionally — executable entry points. It is not a new transport (an
executable, when present, is a tool `route` runs) and not a daemon
(it exits).

**Capability scope (the axis the design turns on).** Two tiers:

- **Prompt-only skill**: a reusable user manual / prompt, no
  executable. The agent follows the instructions with existing
  tools. No new transport, no `route` involvement.
- **Prompt + executable skill**: a manual *and* one or more
  executables. The executables are **tools run through `route`**
  (the existing supervised path: schema validation, timeout, output
  cap, caps). The manifest names them; the manual tells the agent
  which tool to call with which arguments. The host keeps one
  supervised-execution path; a skill is a naming / packaging layer
  over tools plus prose.

**Trust: if executables are accepted, how do we know one is not
malicious?** We do not try to *detect* malice — static analysis of
arbitrary binaries is undecidable. We instead **bound the blast
radius** and **raise the cost of an attack**:

- **Prefer `route` tools over bundled binaries.** A skill's
  "executable" is a *named existing tool* on the `route` path, not
  a new bundled binary. The host already supervises it; no new
  code is introduced, so the malicious-binary question is moot for
  the common case.
- **Fence bundled code like an extension.** A skill that ships its
  own script / binary gets the same fences: no network unless
  `caps` declares it, fs-write only under the repo, enforced
  timeout, least privilege. What a bad payload can do is bounded to
  those fences.
- **Version control + review + pinning.** A skill's executable is a
  repo file, so it is diff-able, reviewable, and version-pinned. A
  malicious change is a malicious *commit*, caught by the same
  review that catches a malicious commit anywhere in this repo. The
  answer is the trust model of the repo itself: you trust the tree
  and review its diffs.
- **No auto-invocation.** A skill runs only when the agent —
  already trusted to run the `bash` tool — invokes it; never on
  external input.

The first skill (`tui-capture`) is prompt-only and wraps an
*existing* repo script via `bash`, so it ships zero new
executables.

## 3. The first skill: `tui-capture`

The concrete need this started from. To verify a TUI change the
agent must: spawn the TUI under a PTY against a session, feed it a
scenario, let it settle, then assert on the rendered screen text and
the emitted SGR (color) sequences. This session inlined that
procedure three times before it became `scripts/tui-capture.py`.
That file, plus a manifest, *is* the first skill:

```
skills/tui-capture/
  skill.toml     # name, one-line description, tools = [] or ["…"]
  SKILL.md       # the user manual: how to run it, how to read the SGR
  NOTES.md       # reference: SGR families, what to assert
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
point, when present, is a tool on the existing supervised path;
discovery is a directory scan exposed as a command. Where this repo
*diverges* from Agent Skills: the index is not resident in the
prompt — it is an on-demand command result (section 6), so the cached
prefix is untouched by the skill set.

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
  description = "Capture the TUI on a PTY; assert screen text and SGR."
  # prompt-only skill: leave `tools` empty
  # prompt + executable skill: name the tools `route` will run
  tools       = []             # e.g. ["tui-capture-bin"]
  resources   = ["NOTES.md"]   # files the agent may read
  caps        = []             # network / fs-write, mirroring the ext host
  ```
  The user manual is `skills/<name>/SKILL.md`.
- **Discovery = `skills --list`** (the endorsed form). A command the
  agent runs **on demand** (like `bash`): it prints the flat index
  — `name — one-line description`, deterministically ordered. It is
  a *tool result* (transcript tail), not something the prompt
  carries (section 6). The TUI's autocompletion window reads from
  the same command. A sibling `skills show <name>` prints the full
  skill (manifest + manual + resource listing) on demand.
- **Execution.** A skill's executable, when it has one, is a tool
  `route` runs — not a `skills` subcommand. `skills` stays read-only
  (`list` / `show`); execution is supervised by `route`.

## 6. Exposure and cache-friendliness (progressive disclosure)

The constraint: the provider reuses a stable *prefix* of the prompt
via its prefix KV cache; the longer the byte-identical prefix across
consecutive model calls, the more is reused. In this harness the
request prefix is the system prompt, which `assemble` builds from
the frozen call config (`bin/assemble` `main.rs`), and it must stay
byte-stable between turns.

**So the system prompt carries no skill content.** No index, no
manual, no "which skills are relevant now." Skill detail is never
injected into the prefix, because *any* change to the skill set — a
new skill, a reworded description, a different one being relevant
this turn — would mutate the prefix and invalidate the cache for
that session. Progressive disclosure preserves the prefix exactly
because disclosure is not a prompt change:

- **In the prompt (the stable prefix): at most one pointer line.**
  A single, stable, skill-agnostic instruction — "You have a
  `skills` tool; run `skills --list` to discover procedural
  capabilities, and `skills show <name>` for detail." That line does
  not change when skills are added or removed, so the prefix stays
  byte-stable. The agent learns that skills *can* exist and how to
  find them; it is not handed *which* exist.
- **The index and the manual: on-demand tool results only.**
  `skills --list` and `skills show <name>` are tool calls; their
  output lands in the transcript **tail**, where it is recent
  context. Disclosure is appended at the back, so the stable prefix
  is never mutated. The agent spends tokens only on the skills it
  actually fetches — nothing is up-front.
- **Determinism.** `skills --list` sorts by name (not hash / map
  order), so repeated calls return byte-identical output; the agent
  and the TUI can rely on a stable order.
- **Rule.** We do not inject skill detail into the system prompt,
  and we do nothing that would move the skill set into the prefix.
  The cost is one discovery call when a skill is needed; the gain is
  that the cache prefix is never invalidated by the skill set.

## 7. TUI side (mainstream agent-tool UX)

To match the input-box UX of the mainstream agent tools, `bin/tui`
gains:

- **An autocompletion window widget** in the input area: a popup menu
  listing candidates, filtering by the typed prefix, navigable
  (arrows) and selectable (Enter / Tab). A general widget; the slash
  command is its first consumer.
- **A slash command (`/`)**: typing `/` opens the window listing
  available **skills** (and built-in TUI commands); selecting one
  inserts / triggers it. The data source is the `skills --list`
  command itself — the same on-demand source the agent uses — so
  the TUI and the model share one source of truth, and *neither is
  the prompt* (section 6).
- **Implementation.** A `render.rs` widget plus an `app.rs` /
  `vim_editor.rs` key path, fed by the shared index. It pairs with
  the skill system; it is not a separate transport.

## 8. Guardrails (consistent with the tool contract)

- A skill that declares no `caps` gets no network and writes only
  under the repo, exactly as a tool is fenced.
- A skill's `tools` entries resolve to real, `route`-runnable tools;
  no absolute paths, no `../` escapes (the same rule the extension
  host applies to its load order).
- A skill execution that fails supervision (timeout, cap exceeded) is
  reported to the agent as a not-run / failed outcome, not a hang.
- The system prompt carries no skill content (at most the one
  stable pointer line of section 6); a skill that tries to get its
  index or manual into the prompt prefix is out of scope.

## 9. Open (not decided here)

- Whether a skill may bind **multiple** tools (a small set) or is
  limited to one. Default: allow a list; the manual names which to
  call.
- Where the `skills` command lives: a new `bin/skills`, or a
  subcommand of an existing binary. The Unix answer is a small
  dedicated command on the tool path.
- TUI widget specifics: key bindings, the menu's vertical budget, and
  how `/` interacts with the existing vim modal input.

## 10. Not doing

- A capability daemon or a JSON-RPC server. A skill is a command.
- A global registry or an install step. Skills are files in the
  tree, found by scan, versioned with the repo.
- Auto-invocation. The agent decides to call a skill; nothing calls
  one on its own.
- Any skill content in the system prompt (see section 6); that would
  break the cached prefix.

## 11. Acceptance (for when it is approved)

- `skills/tui-capture` exists with a manifest, a manual, and — if it
  needs one — a `route`-runnable tool; it replaces the ad-hoc PTY
  harness in the TUI feature checks.
- `skills --list` prints the flat index; `skills show <name>` prints
  the full skill. Both feed the TUI window from one source; the
  agent discovers on demand (no prompt index).
- The prompt carries no skill detail (at most the one stable pointer
  line); the index and a skill's manual reach the model only as
  tool results, never as a prompt-prefix mutation.
- A skill with no `caps` is fenced; a timed-out execution is a
  reported failure, not a hang.
- The TUI input box shows the autocompletion window on `/` and lists
  skills.
