# ui_extensions

The global extension layer for the TUI extension host
(docs/ui-extension.md). Each subdirectory is one extension entry:
an `ext.toml` manifest plus the files its `command` runs. The TUI
loads this directory at start; a project `.pi/ui_extensions/` layer
overrides entries by name.

This repo ships no extension content. Stage 1 of
docs/ui-extension-plan.md builds the host, the discovery, and the
protocol only.
