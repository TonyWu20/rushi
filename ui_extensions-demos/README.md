# Extension layer demos

Optional extension layers, kept out of the default `ui_extensions/`
global layer. Point `[ext] dir` at a layer here to opt in.

| directory | language | kind | notes |
|-----------|----------|------|-------|
| `tool_result/` | bash | `render` | the tool_result render demo (ui-extension-plan stage 2). Not in the default layer since the 2026-09-03 user report: the ext reply replaced the built-in tool box, and the `Ctrl+O` fold key had no effect on the read and edit results. The built-in render (box, fold, expand) owns tool_result in the default UX |
| `cmd_palette/` | bash | `commands` | the command-palette demo (docs/tui-command-palette.md section 10). Registers two commands and one setting via the `commands` cap; the host invokes them through the `invoke` op |
