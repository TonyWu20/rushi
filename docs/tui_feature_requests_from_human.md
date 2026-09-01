# TUI feature requests

- [x] `content` field in the `events.jsonl` must be displayed in full, never
      truncated or folded.
- [x] Use `Ctrl + u/d` for scrolling, like vim.
- [x] Support mouse scrolling: Currently, though not claimed supported, the tui can
      be scrolled by mouse, but it is very laggy and uncontrollable, and will hang
      the process: double `q` cannot exit, no response to other key inputs.
- [x] Syntax highlight for the tool results if possible, markdown syntax highlight
      and proper rendering for `content`.

## New requests (2026-08-29)

- [ ] Truncate `Read` and `Write` tool results. Show `Edit` results as a diff.
      How-to: `pi-tool-display` (github.com/MasuRii/pi-tool-display, v0.5.0,
      pinned rev `91cef758`). It installs via
      `~/programming/pi-config/flake.nix`.
      Behavior: compact read output (preview line count, output modes),
      adaptive edit and write diffs (split or unified layout), syntax
      highlighting inside the diff, width clamping on narrow panes,
      presets opencode / balanced / verbose.
      Today `ui_extensions/tool_result/tool_result.sh` renders the full
      body ("Nothing is truncated"). A `Read` spams the whole screen.
- [ ] Stop the abuse of one gray text color across the UI.
      Show `Read` content with syntax highlighting.
      Today the reference renderer paints every body line `darkgray`
      (`ui_extensions/tool_result/tool_result.sh`). The color map lives in
      `bin/tui/src/ext.rs`. The transcript makes no highlight effort.
      Reference: pi-tool-display highlights syntax inside the diff.
- [x] The statusline extension promised powerline icons.
      The current TUI rendered none.
      `starship-statusline.ts` (pi-config, via flake.nix) draws a rounded
      powerline footer with Nerd Font glyphs `U+E0B4` / `U+E0B6`.
      The harness reference `ui_extensions/statusline/statusline.sh`
      is plain text. It has no glyphs.
      Shipped: the `statusline` extension (bash and the Rust port) now
      emits a powerline footer. Each pill is a rounded segment: the
      left cap is `U+E0B6`, the arrow between pills and the end cap
      are `U+E0B4`, and every span carries its own hex colors
      (Catppuccin Macchiato, the reference palette). The multi-span
      line shape is documented in `docs/ui-extension.md` section 4.
      A row that overflows the terminal drops its lowest-priority
      pills.
- [ ] Display and control the model's thinking (reasoning) block.
      Today `bin/model` sends `reasoning: { effort }` to the API and
      the model can emit thinking. But `bin/parse` keeps only `text`,
      `tool_calls`, `stop_reason`, and `usage`. It drops the thinking
      content. Nothing reaches the log, so the TUI cannot show it.
      Needed: capture thinking from the model response into the log,
      render it as a collapsible dimmed block, and add controls to
      toggle it and set reasoning effort.
- [ ] Fix the transient `[malformed log line]` flash on live loops.
      Root cause: `read_events` (`bin/tui/src/port_file.rs`) reads the
      whole file and splits on newline. A read that races an in-flight
      append sees the last line without its trailing newline. That
      partial segment fails `Event::parse_line` and renders as
      malformed. The persisted line is clean; all lines validate.
      Fix: in `read_events`, when the read data does not end in a
      newline, drop the final segment as in progress. Show it on the
      next read once the append lands.

## New requests (2026-08-31)

- [ ] Show user messages that wait for the busy loop. Render them
      like `pi` renders `steering` and `follow-ups`.
      Today: while the loop is busy, the TUI still accepts input.
      Enter appends a `user_message` to the log and the loop picks
      it up at its next step. The TUI shows nothing about it.
      The user does not know if the input was acknowledged, or
      when the loop will process it.
      Needed: a pending list of unconsumed `user_message` events
      per session. Show the waiting messages, with a count, like
      `pi`'s `steering` (injected at the next step) and
      `follow-ups` (processed after the loop ends).
      Decision: ship in two stages. See "Design decision: pending
      user messages" below.
- [ ] Show thinking content behind a toggle, like `pi`.
      Extends the 2026-08-29 item "Display and control the model's
      thinking (reasoning) block". Beyond capturing thinking into
      the log and rendering it as a collapsible dimmed block, add
      a toggle that shows or hides thinking blocks. The toggle
      follows `pi`'s thinking display.

## New requests (2026-09-01)

- [x] Show the loop phase while the loop runs. The TUI must state
      that the loop waits for the model response. Today nothing
      shows the phase. The model call is silent until the response
      lands. The wait runs long.
  - Data: the 2026-08-31 DSPARK analysis
      (notes/harness-vs-pi-model-latency.md) puts the model
      round-trip at a median of 9 s, a p90 of 50 s, and a max of
      242 s.
  - Spec: docs/tui-model-wait-indicator.md. The loop publishes
      the phase as an `ext_status` event (id `loop_phase`). The
      TUI renders the last value gated on the loop-running bit.
  - Shipped: `scripts/step.sh` publishes the `loop_phase` marker
    (`wait` before the model call, `tools` before the routing)
    as an `ext_status` event through `bin/log` with schema
    validation. `bin/tui` derives four display states (idle,
    running, wait, tools) from the last marker value and the
    loop-running bit. The session title shows the phase bit.
    A working row above the input box shows the wait
    since the marker (`waiting for model · Ns`,
    `tools running · Ns`, `Working...`). The row is its own
    layout cell and shows under any statusline. A statusline
    extension picks the marker up through the existing `statuses`
    map as a `loop_phase=wait` pill. `scripts/cache-e2e.sh`
    checks the marker in the session log (the mutation gate).

## Design decision: pending user messages (2026-08-31)

Decision: ship the pending-message work in two stages.
Stage 1 is a TUI-only indicator. Stage 2 adds the loop-side
`steering` / `follow-up` split.

Reason: the indicator states the delivery time to the user.
Today, every `user_message` injects at the next step. That
is `steering`, drain mode `all`.
A "follow-up: processed after the loop ends" hint is false
until the loop supports the split.
A wrong hint is worse than no hint.

How `pi` splits the two queues (reference, `pi-agent-core/src/agent.ts`):

- `steer()`: queue a message to be injected after the current
      assistant turn finishes. The loop polls the queue at the
      next step inside the running execution.
- `followUp()`: queue a message to run only after the agent
      would otherwise stop. The run delivers it as a new prompt.
- Both queues use two drain modes: `all` and `one-at-a-time`.
- The UI reads `pendingMessageCount`, `steeringMode`,
  `followUpMode`.

Stage 1 (TUI only, lands first):

- [x] Show the pending list of unconsumed `user_message` events
      per session, with a count.
- [x] Label it `steering — injected at the next step`.
      The label states today's real loop behavior.
- No loop changes. No schema changes.
- Shipped: the steering block renders between the transcript and
      the input box. Header row with the count and the delivery
      hint; up to three preview rows; a `+N more` row for the
      rest. The running label is `steering, injected at the next
      step`; the stopped label points at `Ctrl+R`.
      `App::pending_user_messages` (bin/tui/src/app.rs) and
      `pending_steering_lines` (bin/tui/src/render.rs).

Stage 2 (loop-side split, lands second):

- [ ] Schema: add `queue: "steer" | "follow"` to `user_message`.
      A missing field means `steer`. Old logs stay valid.
- [ ] `bin/claim`: pending `steer` gives `awaiting_model`.
      Only pending `follow` gives `idle` plus a
      `pending_follow_ups` count.
- [ ] `scripts/turn.sh`: do not break on `idle` when follow-ups
      are pending. Continue the loop. They run as a new turn.
- [ ] `bin/assemble`: inject `steer` messages into the in-flight
      step. Inject `follow` messages only on a turn restart.
- [ ] `bin/user` and the TUI input: pick the queue while the
      loop is busy.
- [ ] TUI: render both pending lists, each with a count.
      Replace the stage-1 label.

Notes:

- Stage 2 ships drain mode `all` first.
      The `one-at-a-time` mode is a later refinement.
- Stage 1 ships before stage 2. Stage 2 replaces the stage-1
      label when it lands.

## Pending corrections (2026-08-29)

The "never truncate" rule in item 1 applies to the `content` field
of `user_message` and `assistant_message` events. Tool result
bodies may be truncated. The code encodes the wider rule. These
fixes are recorded here and left for later:

- [ ] `bin/tui/src/render.rs` module header: "Text content (user/
      assistant messages, tool output) ... never truncated or
      folded". Rescope to user and assistant messages.
- [ ] `bin/tui/src/render.rs` `TOOL_CALL_BODY_LINES` comment: "The
      `content` field and tool result text have no cap". Rescope.
- [ ] `bin/tui/src/render.rs` `result_text` doc comment: "Nothing is
      hidden ... (item 1: no truncation)". Rescope.
- [ ] `ui_extensions/tool_result/tool_result.sh` header: "Nothing is
      truncated: the body is shown in full". Rescope.
- [ ] `ext-rs/tool_result-rs/src/main.rs` header: same text.
      Rescope.
- [ ] `ui_extensions/README.md` tool_result row: "styled header
      plus the full body". Annotate the truncation request above.
