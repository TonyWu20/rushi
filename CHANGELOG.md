# Changelog

Each binary carries its own version in its `Cargo.toml`. The
version shows up on `--version`. This file records the
versioned change for each kernel binary, newest first.
Internal refactors without user-visible behavior do not get an
entry.

## rushi 0.1.3 (2026-10-14)

The CWD config tier now beats the Nix side-by-side tier
(docs/project-config-precedence.md, revision 2).

- A Nix project CWD with a `config.toml` now resolves that file as
  the top layer. The store config is the base layer under it.
- Before the fix the store tier always won on a Nix install, so the
  project config was dead and the top layer equalled the base layer.
- No-flag launches (TUI, `rushi run`) pick up the project config.
  The `--config` and `$CONFIG` tiers are unchanged.
- Proof: `scripts/config-layer-e2e.sh` scenario 3.

## rushi-common 0.1.7 (2026-10-14)

`resolve_config_path` reorders the ambient tiers: CWD `config.toml`
when present now outranks the Nix side-by-side config.

## rushi 0.1.2 (2026-10-14)

Layered config (docs/project-config-precedence.md). The Nix store
`config.toml` is the base layer. Every config file is the top layer
that deep-merges over it.

- Table keys recurse. A top value wins a conflict. Base keys absent
  from the top survive. A top array or scalar replaces.
- No base file (dev checkout, plain install): behavior unchanged.
- A legacy `[paths]` key in either layer is a load error.
- A project pipeline may reference machine hook defs. The dead-def
  warning is scoped to top-layer defs.
- `rushi config` dumps the top-layer file only. The merged value
  never prints, so store paths stay out of git-tracked files.
- The loop, `assemble`, `model`, `parse`, `compact`, and `user`
  all load through one shared `rushi_common::config_layer`
  function.

## assemble 0.1.2 (2026-10-14)

Adopts the layered config loader. A partial project `config.toml`
now inherits the machine model and tools on a Nix install.

## model 0.1.1 (2026-10-14)

Adopts the layered config loader.

## parse 0.1.1 (2026-10-14)

Adopts the layered config loader.

## compact 0.1.1 (2026-10-14)

Adopts the layered config loader. The loader stays lenient: any
error yields the empty table and the defaults apply.

## user 0.1.1 (2026-10-14)

Adopts the layered config loader for `[loop]` and `[paths]`
resolution.

## assemble 0.1.1 (2026-10-12)

The FT-030 fix. An image tool result never sends its payload as
text, and a compact boundary drops the image part.

- An image tool result projects to a short marker
  (`[image: <path>] <note>`) plus the `input_image` part. The
  full tool-log stdout no longer rides the `input_text` channel
  for image results. The marker replaces the payload in every
  wire form, including the non-vision "Image omitted" form and
  the summary-input compact form.
- The compact drops the image. An image event older than the
  last plain `compaction_summary` projects to the marker only.
  Pixel data does not re-cross a boundary (pi parity). A fresh
  `img` call after the compact still carries its part.
- The tool log keeps the full record. The TUI and the audit
  trail are unchanged.

## assemble 0.1.0

Baseline version of the kernel assemble binary.
