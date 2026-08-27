# Design Review — UI Extensions (`docs/ui-extension.md`)

Verdict: the capability split is sound, but the protocol cannot express
`notify`, cannot correlate `transform` replies, and misstates `append` trust.

## Top issues

### 1. HIGH — `notify` has no mechanism
- `ui-extension.md:34` lists `notify` as "No protocol".
- The extension is a separate process. Its stdout is the JSONL channel.
- Writing a BEL or OSC to its own stdout corrupts the protocol stream.
- The only path to the host terminal is a message on the channel.
- Fix: add a `notify` op to the ext-to-TUI table. Payload:
  `{"kind":"bell"}` or `{"kind":"osc","code":0,"args":"..."}`.
- The host applies it on the terminal it owns (raw mode, alt screen).

### 2. HIGH — `transform` / `transformed` have no correlation id
- `ui-extension.md:74`: `transform` payload is `{text, width, fence}`.
- `ui-extension.md:82`: `transformed` payload is `{lines}`, no request id.
- Two code blocks in flight, or a resize re-transform, make replies ambiguous.
- The "stale replies are dropped" rule (`ui-extension.md:94`) has no handle.
- Fix: add a `req` id to both messages. The host matches on `req` and
  drops replies whose `req` is no longer current.

### 3. HIGH — a `usage` event duplicates existing log vocabulary
- Usage already lives on `assistant_message`
  (`schemas/events/v1/assistant_message.json:23`:
  `input_tokens`, `output_tokens`, `cached_tokens`).
- `ui-extension.md:98` adds a separate `usage` event with different fields
  (`input`, `cache_read`, `cost`). Two sources of truth for one fact.
- Fix: drop the `usage` event. The statusline sums `assistant_message.usage`.
- Restart semantics: stats survive restart only from the log, and the
  history rule (`ui-extension.md:91`) re-sends only the visible transcript.
- That window is capped: `TRANSCRIPT_EVENT_CAP` 2000 (`render.rs:40`),
  and `read_events` reads the last 50 MB (`docs/tui.md` section 13.1).
- Cumulative stats computed from that window are wrong on a long session.
- Fix: at startup, the host re-sends every event that carries usage, uncapped.

### 4. HIGH — `append` trust is misstated and the blast radius is unbounded
- `ui-extension.md:136`: "An extension with `append` is trusted like a tool."
- The tool contract is one-shot: JSON in, JSON out, never writes the log
  (`docs/tool-interface-registry-idea_from_human.md` section 2.2).
- An extension with `append` is a long-lived log writer.
- It is trusted like the loop, not like a tool.
- Blast radius: a live extension can append a fake `approval` event.
- It pre-answers a pending `approval_request`. The TUI derives pending state
  from the log alone (`app.rs:313`), and the loop polls the same log.
- A fake `tool_result` corrupts the next prompt the model receives.
- The guardrail at `ui-extension.md:135` ("a dead extension degrades a pane")
  covers only dead extensions.
- Fix: state that `append` means loop-level trust.
- Restrict the event types an extension may append, or name this risk.

### 5. MED — two extensions can claim the same kind or `status`
- Two extensions may list `tool_result` in `kinds` (`ui-extension.md:55`).
- Both receive `event`. Both may reply `lines` for one `event_id`.
- The host has no ownership or priority rule.
- Two `status` claimants share one statusline row (`ui-extension.md:109`).
- There is no winner rule.
- Fix: resolve conflicts at manifest scan time. The project directory overrides
  the global directory. Within a directory, first name alphabetically wins.
  Or the host refuses to start and names the conflict.

### 6. MED — lifecycle: no restart policy, no dead/slow display, group details
- "Spawn at TUI start and die at TUI exit" (`ui-extension.md:129`)
  defines no restart policy, backoff, or attempt cap.
- While an extension is dead or slow, the TUI shows nothing. A dead
  statusline row falls back to a hint. A slow one keeps the last row
  forever with no staleness bound. `transform` has a timeout with no
  value set (`ui-extension.md:114`).
- Fix: bounded restart (3 attempts, 1 s / 2 s / 4 s backoff), then mark
  dead, show a hint in the statusline row, and keep the last valid reply.
- "Die at TUI exit" must mirror the `docs/tui.md` section 13.3 pattern:
  dedicated process group, group SIGTERM then SIGKILL, reaper task.
- A clean exit stops each extension the way `Action::Quit` stops loops
  (`main.rs:342`). Without a reaper, a live parent leaves zombies.
- `tick` carries no session id (`ui-extension.md:59`). Fix: add `session`
  and model context, so a statusline can recompute stats after a cycle.

### 7. MED — the `(ext, event_id)` reply cache does not fit `transcript_cache`
- `transcript_cache` is one entry keyed by `(events_version, width)`
  (`app.rs:113`, `app.rs:117`).
- `events_version` bumps only on log events.
- An extension reply is not a log event, so it never invalidates the cache.
- A width resize also invalidates pre-wrapped extension lines.
- `lines` carries no width, so the host cannot know when to re-query.
- Fix: keep extension replies in a second cache keyed by `(ext, event_id)`.
  Fold that cache into the transcript cache key. Or force a transcript
  rebuild when a reply lands.
- Keep `lines` width-independent: the extension returns styled segments,
  and the host wraps them. The stale-reply rule needs the `req` id
  from issue 2.

## Minor notes

- `transform` targets code-fence languages only (`ui-extension.md:60`).
  LaTeX `$...$` and `$$...$$` spans live in plain text.
  Cleanest fix: scope-qualified targets, `fence:mermaid` and `inline:latex`.
  The host extracts spans by delimiter, sends each to the extension, and
  replaces the span in place when the reply lands.
- `ui-extension.md:89` cites the G5 rule as "docs/tui.md section 10.4".
  G5 is defined in `docs/refinement-policy.md`. `docs/tui.md` section 10
  item 4 is the log fallback. Cite both.
- "Shows the raw text with a hint" fits the transcript.
  A malformed `status` reply must not dump JSON on the statusline row.
  Define one fallback per op: `lines` falls back to built-in render,
  `status` keeps the last valid row, `transformed` shows the raw block
  (`ui-extension.md:114` already says this), and `append` is rejected
  with a flash.
- `ext_status` is a log event, but `EventKind` (`bin/tui/src/event.rs`)
  has no such type. Until it is added, the transcript renders these events
  as "[unknown event type] raw JSON". High-frequency vim-mode events also
  pile volatile UI state into the log that is the only truth.
  Decide transcript visibility and add schema files. The schema-gap pattern
  is `docs/tui.md` section 13.5.
- A policy-hook extension and the human `y`/`n`/`e` keys can both append an
  `approval` for one id. Define a first-wins rule. The loop and the TUI
  pending-derivation must ignore later duplicates.
- The style model `{fg, bg, bold}` (`ui-extension.md:86`) conflicts with the
  reference statusline, which hard-codes a true-color hex palette and
  Nerd-Font glyphs (`~/programming/pi-config/extensions/starship-statusline.ts`).
  Decide: allow hex values in `style`. Theme mapping becomes optional.
- The tool spec states "no version negotiation, no session state"
  (`docs/tool-interface-registry-idea_from_human.md` section 5).
  The extension protocol adds per-message `v`, long-lived state, and
  bidirectional streaming. State where it intentionally diverges.
- Distribution: "3-8 processes, a few MB each" (`ui-extension.md:128`)
  is sound while idle. A bash statusline with `tick_ms = 1000` spawns
  `git` once per second unless it TTL-caches. The reference uses a 3 s TTL.
  Note the tick cost. The grok-mermaid binary must ship with the examples,
  or "zero new compiled binaries on a user machine" (`ui-extension.md:126`)
  is false.
- There is no `hello` or capability exchange. A host and an extension that
  disagree on ops degrade silently to the G5 fallback.
  Add a spawn-time capability exchange, or a manifest `protocol_v` the host
  checks before spawning.

## Open items the design should settle next

1. Pre-approval blocking semantics (`ui-extension.md:102` flags this) and the
   first-wins rule for duplicate `approval` events.
2. Which event types an `append` extension may write, and the log-poisoning
   risk that names itself.
3. The transform scope grammar: `fence:<lang>` versus `inline:<delims>`.
4. Restart budget, per-op timeout values, and the dead/slow display.
5. Protocol `hello` / capability discovery between TUI builds and extensions.
6. `ext_status`: transcript-visible or suppressed, schema files, log-growth
   bound for high-frequency state.
7. Tick payload additions: session, model, loop running state.
