# TUI extension design questions

Extension is an important ability for agent harness framework.

In my opinion, there are roughly two categories of extensions for a harness
product: UI extensions, and feature extensions.
In most cases, an extension product is shipped with changes to UI as well as
features.
For closed source product providing limited customization choices, like `Claude
Code`, most plugins are only feature enhancements: skills and hooks based. The
only UI you can change in Claude Code is the statusline, in my impression.
The total opposite on the spectrum, is represented by `Pi` and `deepseek-harness`.
They are open source, lightweight by default, highly extendable and
customizable, although they are written in typescript just like `Claude Code`.
`deepseek-harness` is even wilder: its real core component is a system that can
mount/unmount anything in realtime, the LLM related things are all exchangeable.

Our harness shares the core concepts and designs of `Pi` and `deepseek-harness`
in many ways, but also different.
The major difference is we build on Rust + bash scripts.
The TUI is also written in Rust. For capability extension,
I believe we have the high freedom just like `deepseek-harness`: we can swap
everything, or ad-hoc create/modify anything.
Because we hold the design that the event log is the only truth:
If we want, we can decouple the TUI lifecycle from the agent loop's lifecycle too.
The TUI just serves as a dashboard with interaction ability to the log,
the loop ability is given by the `loop.sh`, `step.sh`, `turn.sh`, etc.
So we are not afraid of stopping the TUI to apply changes from extensions.
The problem is, how to decouple the extension codebase with the TUI codebase, or
fundamentally, is that possible in Rust?
