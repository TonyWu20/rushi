# Tool Entry Resolution — Shared in `rushi-common`

**Decision:** Tool entry resolution and tool dir enumeration live in
`rushi-common` `paths` (`resolve_tool_entry`, `tool_dirs_in`,
`crates/rushi/src/paths.rs`, shipped 0.1.5). The kernel `assemble` and
every front-end call these functions. No consumer keeps a private
copy.

**Rationale:** The kernel `assemble` and the TUI each carried their own
copy of the resolution. The copies drifted. The drift shipped a TUI
package whose `[paths]` entries all dangled, so the model request
carried zero tool schemas and tool calls leaked into message content.
One shared scan cannot drift.

Rejected: re-bundling `tools/` into the TUI package, and a TUI-local
mirror of the scan.

The flake writes absolute store paths into the TUI config, so
resolution is independent of the working directory.

**Verification:** Unit tests `resolve_tool_entry_keeps_absolute_and_joins_relative`
and `tool_dirs_in_direct_manifest_and_root_layouts` pin the shared
semantics. `assemble` replay: zero tools on the old dangling config,
nine on the new one. The TUI warns at startup when an entry dangles.
