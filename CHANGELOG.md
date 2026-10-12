# Changelog

Each binary carries its own version in its `Cargo.toml`. The
version shows up on `--version`. This file records the
versioned change for each kernel binary, newest first.
Internal refactors without user-visible behavior do not get an
entry.

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
