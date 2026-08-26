# Tool Interface Spec: A Minimal Standard for Local CLI Tools

> Status: Draft v2, revised 2026-08-25.
>
> v1 proposed a new tool interface protocol and registry. v2 revises the idea:
> a minimal interface for **local CLI tools**, working with MCP, not competing with it.

## 1. Positioning

### 1.1 The gap MCP leaves

MCP is the standard for **remote** tools. Its architecture fits external
services: a JSON-RPC server, an initialize handshake, capability setup,
authentication.

That architecture is wrong for local tools. Consider a coding agent that needs:

- `grep_project` — a shell script the user wrote.
- `run_tests` — a Python script.
- `deploy_staging` — a bash script.
- A one-off tool the agent itself writes mid-session, like `parse_log.py`.

MCP is too heavy for these. It would need:

- An MCP server wrapper for each tool.
- A lifecycle: start, initialize, call, shutdown.
- A JSON-RPC framing layer.
- A config file entry.

The Unix model is simpler: **any executable that takes JSON on stdin and writes
JSON on stdout is a tool.** No wrapper, no lifecycle, no framing.

### 1.2 What this spec does

Define a minimal tool interface for local CLI tools. The contract:

- **Name** and **description** (for the model).
- **Parameters** (JSON Schema).
- **Invocation**: spawn a process, write JSON to stdin, read JSON from stdout,
  check the exit code.

That is the entire interface. No server. No handshake. No protocol version
negotiation.

### 1.3 Relationship to MCP

This spec works with MCP. It does not compete with it.

| Use case | Use |
|---|---|
| Remote services (GitHub, databases, SaaS APIs) | MCP |
| Local CLI tools (scripts, binaries) | This spec |
| Ad-hoc tools written at runtime | This spec |
| A tool that needs both local and remote access | This spec locally, MCP remotely |

An implementation can expose the same tool through both interfaces. The core
contract is the same: name, description, params, invocation. The wire format
differs.

## 2. The interface

### 2.1 Tool manifest

Each tool is described by a manifest file. Format: TOML.

```toml
# tools/fetch_url/tool.toml
[tool]
description = "Fetch a URL and return its text content."
command = "python3"
args = ["main.py"]
timeout_ms = 30000

[tool.schema]
type = "object"
properties = { url = { type = "string" } }
required = ["url"]
```

The tool name is the directory name. The manifest carries no `name` field.

Fields (all under `[tool]`):

- `description` — what the tool does, exposed to the model.
- `command` — the executable to spawn.
- `args` — fixed arguments (optional).
- `timeout_ms` — execution timeout (optional).
- `tool.schema` — JSON Schema for the parameters (optional). If no schema, the tool accepts any JSON object.

### 2.2 Invocation contract

The harness invokes a tool by:

1. Spawning `command` with `args`.
2. Writing one JSON object to stdin (the parameters).
3. Closing stdin.
4. Reading all of stdout.
5. Checking the exit code.

**On success** (exit code 0):

- stdout is one JSON object. The `text` field is the model-facing result.
- If the JSON object has no `text` field, the harness uses the raw stdout as the result.
- If stdout is not valid JSON, the harness uses the raw text as the result.

**On failure** (non-zero exit code):

- stderr is the error message.
- The harness reports the failure to the model with the stderr text.

**Streaming tools** (not yet implemented):

- Planned: JSONL/NDJSON on stdout, one event per line.
- A terminal line `{"type":"result", ...}` marks the end.
- The current harness reads all of stdout as one unit. A streaming tool
  degrades to raw JSONL text as the result.

### 2.3 Command resolution

The harness spawns `command` as-is. The working directory is the tool's
directory, so relative `args` like `"main.py"` resolve correctly.

### 2.4 Why this is enough

The model needs three things from a tool:

1. **What it is** — name and description.
2. **What it takes** — parameter schema.
3. **How to call it** — invocation contract.

This spec provides all three. Nothing more is needed for the model to use the
tool. Everything else (discovery, catalog, MCP bridge) is optional.

## 3. Discovery and catalog

### 3.1 Local discovery

The harness scans a tools directory. Each subdirectory with a `tool.toml` is a
tool. The tool name is the directory name.

```
tools/
  fetch_url/
    tool.toml
    main.py
  grep_project/
    tool.toml
    main.sh
  run_tests/
    tool.toml
    main.py
```

Simple, filesystem-based, no network.

### 3.2 Catalog (optional)

For sharing tools across projects or machines, a catalog can index tool
manifests. The catalog stores:

- The manifest.
- The tool's files (or a reference to them).
- A version.

The catalog is a convenience. It is not part of the core interface. A tool
works without it.

### 3.3 Relationship to ARD and the MCP Registry

This spec can integrate with existing discovery standards:

- **ARD (Agentic Resource Discovery)** — publish an `ai-catalog.json` that
  references tools defined by this spec.
- **MCP Registry** — wrap tools in an MCP server for remote access.

The core interface stays minimal. Integration is a separate concern.

## 4. Implementation plan

This spec aligns with the roadmap in `docs/architecture.md`.

- **Phase 1 (bash + separate binaries)** — tools use this spec from the start.
  The manifest and the invocation contract are the only requirements. No MCP.
  No catalog.
- **Later phases** — an optional MCP bridge and catalog may be added. Neither
  changes the core contract.

## 5. What this is not

- **Not a replacement for MCP.** MCP is the standard for remote tools. This
  spec is for local CLI tools.
- **Not a new protocol.** The interface is a contract, not a protocol. There is
  no handshake, no version negotiation, no session state.
- **Not a framework.** Tools are standalone programs. The harness does not import
  tool code.

## 6. Why this matters

The Unix philosophy is strong: **small tools, simple interfaces, compose via
pipes.** This spec applies that philosophy to agent tools.

It makes it easy to:

- Write a tool as a script.
- Use any language.
- Extend the harness without a plugin system.
- Create ad-hoc tools at runtime.

That is the value. A minimal interface for local CLI tools, working with
MCP, that makes the Unix model the default for agent tooling.
