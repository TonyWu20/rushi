# UI extensions — design

Status: scope agreed. Review round 1 done
(`docs/tui-extension-design-review.md`). Those fixes are folded in below.

Extends `docs/tui.md`. The event log stays the only truth.
UI extension output is a disposable view, like the transcript.

## 1. Goal

Run the class of UI extensions shown by the pi-config inventory
(`~/programming/pi-config/extensions`):

- tool-result rendering
- a powerline statusline
- shared UI state between extensions
- inline mermaid and LaTeX rendering
- host notifications (terminal bell, OSC)

Each extension is an external process.
One JSONL boundary per extension, over stdio.
The TUI hosts. The extension renders, reacts, and appends.
No extension code compiles into the TUI.

## 2. In scope and out of scope

Five capabilities:

| Cap | What it does |
|---|---|
| `render` | Replaces built-in rendering of named event kinds |
| `status` | Owns the statusline row |
| `transform` | Rewrites text at render time (mermaid fences, LaTeX spans) |
| `append` | Writes whitelisted event types to the log |
| `notify` | Terminal effects (bell, OSC), applied by the host |

Out of scope:

- live widget replacement inside the TUI process (pi's `setEditorComponent`)
- hot reload. The update path is stop, edit, restart (section 9)
- direct extension-to-extension IPC. All shared state flows through the log
- audio output

## 3. Manifest

Location: `ui_extensions/<name>/ext.toml`.

```toml
[ext]
command = "bash"
args = ["statusline.sh"]
caps = ["status"]
kinds = ["tool_result"]
tick_ms = 1000
transform = ["fence:mermaid", "inline:latex"]
append_types = []
protocol_v = 1
```

- `caps` — claimed capabilities
- `kinds` — event types the host forwards. Default: all
- `tick_ms` — cadence for `status`
- `transform` — scope-qualified rewrite targets. `fence:mermaid` rewrites
  code blocks of that language. `inline:latex` rewrites `$...$` and
  `$$...$$` spans. The host extracts the span, sends it, replaces it in place
- `append_types` — log event types this extension may append. Default: none
- `protocol_v` — the host checks it before spawn. A mismatch skips the
  extension and flashes
- no ordering field exists in this manifest. Order is host-owned
  (section 6)

The TUI scans the directory at start.
A global directory ships with the harness.
A project directory (`.pi/ui_extensions/`) adds or overrides entries by name.

## 4. Protocol (JSONL over stdio)

TUI to extension:

| op | payload | meaning |
|---|---|---|
| `event` | one full log event | a new event matching `kinds` |
| `tick` | `{seq, width, session, model, loop_running, statuses}` | cadence ping for status extensions |
| `transform` | `{req, text, width, scope}` | rewrite one span |

Extension to TUI:

| op | payload | meaning |
|---|---|---|
| `lines` | `{event_id, lines}` | styled lines replacing the built-in render of one event |
| `status` | `{lines}` | the statusline row |
| `transformed` | `{req, lines}` | result for the matching `req` |
| `append` | `{event}` | append the event via `SessionPort` |
| `notify` | `{kind: "bell"}` or `{kind: "osc", code, args}` | the host applies it on its own terminal |

`lines` is an array of `[text, style]` pairs.
`style` is `{fg, bg, bold}`. Values are theme tokens or hex strings.
The host converts pairs to `Span::styled`.
No raw ANSI crosses the channel.
`lines` are width-independent. The host wraps to the pane width.
Every message carries `"v": 1`.
A `transformed` reply whose `req` is no longer current is dropped.
Malformed replies never crash the TUI. Fallback is per op
(`docs/refinement-policy.md` G5, `docs/tui.md` section 10 item 4):

- `lines`: fall back to the built-in render
- `status`: keep the last valid row
- `transformed`: show the raw block
- `append`: reject with a flash naming the reason

History rule: on start, the host re-sends the events of the visible
transcript for `render`. Replies cache by `(ext, event_id)`.
That cache folds into the transcript cache key.
A new log event or a new reply rebuilds the transcript.
At start, the host also re-sends every `assistant_message` with `usage`,
uncapped, to every `status` extension.
Cumulative stats survive a restart from the log alone.

## 5. Vocabulary

- No new `usage` event. `usage` already lives on
  `assistant_message` (`schemas/events/v1/assistant_message.json`).
  The statusline sums those events
- `ext_status` — `{id, value}`: shared UI state (vim mode, team status).
  An extension publishes, the statusline consumes.
  This is the log-based replacement for pi's `ctx.ui.setStatus`.
  Add it to `EventKind` and to `schemas/events/v1`.
  Suppressed from the transcript by default. The log keeps it.
  Publishers send only on change, not per tick

Policy hooks (the `no-find-grep` class): an extension with
`append_types = ["approval", "ext_status"]` watches `tool_call` and
answers its own `approval_request` with a deny.
First answer wins. The loop and the TUI ignore later duplicates
for the same id.
The TUI stays free of decision logic (`docs/tui.md` section 10.1).
Exact pre-approval blocking semantics is an open item (section 11).

## 6. Layout and load order

Layout:

- the statusline row reserves one line when a `status` extension exists
- without one, the TUI shows its built-in help/status row
- transformed text renders in place in the transcript

Load order is host-owned. This follows the deepseek-harness loader,
where the core decides the sequence and an entry never claims a slot
(`vendor/loader`: the loader remains the sole lifecycle authority).

- layers compose in a fixed host order: built-in renderers, then the
  global directory, then the project directory
- a project entry overrides a global entry by name
- within a layer, alphabetical by name. The rule is host-fixed and
  deterministic. No manifest field changes it
- `kind` ownership: the first extension in the composed sequence that
  lists the kind owns it
- `status` allows one owner across the whole sequence. Two claimants:
  the host refuses to start and names both
- an extension declares what it needs. It never claims when it runs.
  There is no `priority`, `before`, or `after` field, and none will be
  added
- extension-to-extension needs flow through log data (`ext_status`
  publisher and consumer). A consumer that loads late replays the log.
  Load order is presentation, not correctness. The system is
  order-tolerant
- validation fails loud at scan. A malformed manifest or a broken
  command refuses the start and names the file. A wrong entry is a
  misconfiguration, not a silent skip
- the host owns lifecycle: spawn, restart budget, exit. An extension
  never restarts itself

## 7. Lifecycle

- One process per enabled extension, in its own process group
  (`setsid`), like the loop (`docs/tui.md` section 13.3)
- Quit: SIGTERM the group, wait 3 s, SIGKILL the survivors, and
  wait for the deaths, synchronously on the quit path. The pids are
  re-collected after the escalation: a restart that started during
  the wait is a new group, and the kill covers it. No detached
  reaper: a detached thread dies with the process, and a group that
  ignores SIGTERM would orphan
- Restart budget: 3 attempts with 1 s / 2 s / 4 s backoff, then dead.
  The owned row shows a hint
- A slow `transform` times out at 2 s. Fallback is the raw block
- Tick cost note: a bash statusline that spawns `git` on every tick is
  expensive. The reference TTL-caches git calls at 3 s

## 8. Reference extensions, in order

1. `statusline` — bash + `git` + `jq`. Proves the point language-agnostic first
2. `tool_result` — a `render` kind owner
3. `mermaid` — a Rust binary on `grok-mermaid` (Unicode art, like pi)
4. `statusline-rs` — Rust port of item 1

## 9. Distribution and lifecycle policy

- The mechanism ships inside the TUI binary: host, protocol, spawn,
  supervision
- The default examples are scripts (`statusline`, `tool_result`).
  They add zero compiled binaries on a user machine
- The `mermaid` example ships one opt-in Rust binary
- A project carries only what it uses, in its own directory
- No hot reload (decision). Pi accepts `/reload` only when idle.
  deepseek-harness also restarts for some changes, per user reports.
  The update path is stop, edit, restart. Nothing is lost: the log is
  the truth. Until the daemon phase, restarting the TUI stops the loop
  too (`docs/tui.md` section 12). The daemon removes that coupling

## 10. Guardrails

- The TUI holds no extension logic. It hosts and supervises
- A dead extension degrades a pane, not the log
- The `docs/tui.md` section 10 forbidden-string scan stays
- Trust: an extension with `append` is a long-lived log writer.
  It holds loop-level trust, not tool-level trust. The tool contract is
  one-shot and never writes the log. `append_types` bounds the blast
  radius. A fake `approval` or `tool_result` is a log-poisoning path.
  The project owner decides who gets `append_types`
- Intentional divergence from the tool spec: that contract is one-shot
  and stateless (`docs/tool-interface-registry-idea_from_human.md`
  section 5). The extension protocol is bidirectional and stateful.
  It is the UI-side twin, not the tool contract

## 11. Decisions and open items

### Settled in stage 4 (ui-extension-plan)

- **Pre-approval semantics: answer one that does.** An extension with
  `append` may append an `approval` that answers an `approval_request`
  that already exists in the log. The pending state is derived from the
  log in order: an `approval` counts only after the request it answers
  (refinement policy G6). Pre-approval blocking (holding the loop on a
  request that does not exist yet) is ruled out: it would let the
  extension approve tools before the loop asks, crossing the loop
  trust boundary. An extension holds loop-level trust, not tool-level
  trust (section 10)
- **The ext_status growth bound: the log is unbounded, memory is
  capped.** The session log is the audit record: no cap on
  `ext_status` events there. The TUI's in-memory view caps at 128
  distinct ids, oldest-updated out first; the statusline consumes the
  capped map through the tick payload
- **Per-op timeouts beyond transform: a status staleness bound.** A
  status extension that gives no valid reply for three tick intervals
  (3 x tick_ms) drops its row to a stale hint. The bound runs from
  the last valid reply. The first reply of a generation gets a wider
  10 s window: a cold start (a git spawn on a cold cache) is not a
  stuck extension. The reference statuslines defer their first git
  refresh so the first tick reply stays fast. The transform 2 s
  bound is unchanged
- **`order.toml`: declined for now.** The composed order stays
  alphabetical by entry name within a layer, global layer first. An
  explicit host sequence file is the escape valve if alphabetical
  order ever proves insufficient. It stays host-owned, not a
  project-visible knob

### Still open

- A LaTeX renderer binary for `inline:latex` spans. The host
  detection and request path landed with the stage 3 mechanism; until
  a renderer exists, the raw span shows
- Decisions made here, flagged for sign-off: hex colors allowed in
  `style`; `ext_status` suppressed from the transcript

## 12. What we borrow from deepseek-harness

- host-owned load order. The core decides the sequence
  (`packages/boot/app-boot/src/profile.ts` composes layers over an
  empty root. `apps/cli/src/plugin.ts` reconciles the layer stack and
  appends new bundles in dependency order, owned by the host)
- layered composition: base layers, then global, then project
- fail-loud misconfiguration: a listed entry without a valid patch is
  an error, not a silent skip (`profile.ts` `loadProfile`)

What we do not borrow: in-process dynamic mounting (their `node:vm`
sandbox and model-written runtime packages). Our boundary is a
process. Hot reload stays out of scope (section 9).
One consequence worth naming: their ordering is correctness, because
entries need services in sequence. Ours is presentation, because
inter-extension data flows through the log. That makes host-owned
order easier to hold here than there.
