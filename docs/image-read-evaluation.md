# Evaluation: `image-read-plan.md` × kernel components

This document evaluates how the image-read plan interacts with every module
of `crates/rushi/` (rushi-common) and every `bin/` kernel member. It records
the concrete interactions, the changes each component needs, and the gaps
the plan does not yet cover. It is a review artifact, not an implementation.

Plan under review: `docs/image-read-plan.md` (with `docs/image-read-pi-study.md`).

---

## 1. Data flow (current state, no images)

The read tool emits one JSON object on stdout:

```
tools/read  →  stdout: {"text": "...", "path": "...", "type": "file", ...}
bin/route   →  tool_result event: value.text (preview), value.details (parsed stdout JSON)
                tool log record: stdout (raw), text (display)
bin/assemble→  request input: function_call_output { output: string }
bin/model   →  Responses: array input  |  Chat Completions: role:"tool" string
```

The image plan keeps this shape and adds `data` (base64) + `mime_type` +
dimensions inside the tool's stdout JSON. The base64 then rides in
`value.details` (event log) and `stdout` (tool log). No top-level event
field is added to the model path. The TUI path adds an optional
`value.images` array (§4.7).

---

## 2. `crates/rushi/` (rushi-common) — module by module

### 2.1 `model_settings.rs`

Plan §4.5 adds `vision: bool` to `ModelSettings`.

- Current `ModelSettings` has no `vision` (or any capability) field.
- `resolve_model_settings` must read `val_bool(mdl, "vision")` with a
  `false` default, per-model then global `[model]` then default.
- `val_bool` already exists and is used by `compact` — no new helper.
- Two more knobs live in `[limits]`, not the model table:
  `image_budget_bytes` (§4.3 step 5) and `block_images` (§4.5 kill
  switch). These are read by `assemble` from the `limits` table. They are
  **not** `ModelSettings` fields.
- Plan §4.10 also needs a per-model **wire-form** flag (Responses
  `input_image` vs Chat `image_url`) and a per-model **bridge** flag
  (synthetic assistant message after tool results, for providers that
  reject a user message directly after tool results). Neither exists
  today. Both should be added to `ModelSettings`.

Interaction verdict: small, mechanical. Add `vision` (and, to fully honor
§4.10, `image_wire` and `needs_bridge`) to `ModelSettings` and read them
in `resolve_model_settings`.

### 2.2 `compact_math.rs`  ← **the main estimator gap**

- `project_event` projects a `tool_result` to
  `Ev::Result { call_id, chars }` where `chars` is **only**
  `value.text` length. It does not look at `value.details` (where the
  image base64 lives).
- `estimate_from_events` and `est_tokens_after*` build on `project_event`.
- The plan §4.3 step 6 adds "4800 chars per image" to **assemble's**
  `estimate_ev_tokens`. It does **not** touch this module.

Consequence: the shared estimator under-counts image tokens. Two of the
kernel's budget decisions use this module, so both are blind to images:

1. `bin/rushi` loop, `step.rs::estimate_context` — the proactive
   threshold check that decides whether to fire a compact before the next
   model call.
2. `bin/compact` — the trigger reading that decides whether compaction
   fires at all.

Why it matters: a session that reads several images inflates the real
context (the provider counts image tokens in `usage.input_tokens`), but
the shared estimate does not add them. The proactive trigger fires late.
It self-corrects eventually, because the next request's measured
`usage.input_tokens` anchor already includes the image tokens — but only
for the request that already ran. Events appended after the last
measurement (the trailing window, often a fresh image read) are
under-counted by one image (~4800 chars ≈ 1200 tokens).

Required change: `project_event` must add a flat per-image estimate when a
`tool_result` carries an image in `value.details` (mirroring the
assemble-side change). This keeps the loop and the compact binary on the
same footing as assemble. Without it the plan's §4.3 step 6 is only half
applied (assemble is right, the loop and compact are left behind).

### 2.3 `event_validation.rs`

- The generic validator (const/enum/required/properties/items) loads
  every `schemas/events/v1/*.json` by glob. No code change is needed for
  image data: `value.details` is an open object, so the base64 passes.
- If §4.7 adds `value.images` (an array of objects), the validator already
  supports `array` + `items` + `object`. No code change. Only the schema
  file changes.
- No interaction risk: adding a new event type needs one schema file and
  zero code here (the module's stated invariant holds for images).

### 2.4 `logline.rs`

- `LogLine` writes one whole line with an exclusive `flock` + one
  `write(2)`. There is no per-line size cap.
- A multi-MB base64 log line is still one atomic commit. The FT-005
  guarantee (line-granular appends) holds for large lines.
- Interaction: none in code. The only effect is **file growth**
  (§4.11), handled by compaction, not by this module.

### 2.5 `stage.rs`

- `ToolResultEvent { id, value, is_error }` carries the full event JSON in
  `value`. The image base64 flows through untouched. No structural change.
- `RequestFile { json }` carries the assembled request. Images are just
  JSON inside it. No change.
- `RouteEnv` / `AssembleOpts` are unaffected.

### 2.6 `hooks.rs`

- Hooks are short-lived commands with one JSON object on stdin and one on
  stdout. They do not inspect image data. They only fire at a window.
- `model.before` receives the assembled **request** JSON (now including
  `input_image` parts) plus `projected_tokens`. Extension hooks that
  *transform* the request must round-trip the image parts or they drop
  them. This is an ABI note for extension authors, not a change to this
  module.
- `tool.before` / `tool.after` see tool call/result events. Image payload
  in `value.details` passes through. No change.
- The hook dispatch code itself needs no change.

### 2.7 `rewind.rs`

- Rewind / active-path masking operates on log sequences, not content.
- Image-carrying `tool_result` events are ordinary log lines: a masked
  branch excludes them from context automatically. This is correct — a
  re-entered branch that re-reads an image re-issues the read.
- No change.

`crates/rushi` summary: `model_settings` grows two-to-three fields.
`compact_math` needs image-aware estimation (the one real functional
gap). The rest are no-ops.

---

## 3. `bin/` kernel members — binary by binary

### 3.1 `tools/read` (the producer)

- New: MIME sniffing, EXIF orientation, decode, resize, format pick,
  base64. New deps: `image` + `base64` (Cargo.toml has neither today).
- Current `main.rs` rejects any binary file (null byte in first 8 KB) and
  only ever emits a text object. The image branch is new.
- The `text` field of the image result is a short note
  (`"Read image file [image/png] (4000x2667 → 2000x1333)"`). That note
  is what every downstream `value.text` shows. The base64 is in `data`.
- `tool.toml` description update is trivial (§4.7 / impl step 7).

### 3.2 `bin/route` — pass-through, as the plan claims

- `structured_output` parses a successful tool's stdout JSON and puts it
  in `value.details`. An image JSON is valid JSON, so it lands intact.
- `process_stdout` extracts the `text` field of a JSON object. For an
  image that is the short note. So `value.text` (and the tool log's
  `text`) = the note, not the base64. Correct.
- Two observations, both benign:
  - `bytes` in the slim event = the display-text length (the short note),
    **not** the base64 size. The index under-reports payload size. Not a
    bug, but misleading.
  - The base64 is stored **twice**: once in the tool log's `stdout`
    (raw) and once in the event log's `value.details`. That is ~2× the
    base64 on disk per image read — more than pi, which stores it once
    in its session JSONL. §4.11 accepts the tool-log copy but does not
    notice the event-log copy. A decision is needed: keep both, or drop
    `details` from the event for image results and let `assemble` read
    the tool log instead.

§4.2 ("no structural change") is correct **for the model path only**.
`value.details` already carries the image to `assemble`, so the model
path needs no route change. But §4.7 adds a `value.images` array to the
event for the TUI, and it is `route` that must populate it ("route
extracts `value.images` from the tool's stdout JSON"). That is a
structural change to the event and to `slim_result_event` / the
`tool_result` schema. So: if §4.7 is in scope, `route` does change
(emit `value.images`). If the TUI channel is descoped, `route` stays
untouched. Reconcile §4.2 / §4.6 / §4.7 accordingly. The storage note
above is a plan gap, not a route bug.

### 3.3 `bin/assemble` — the largest change set

- `Ev::ToolResult` is today `{ id, text }`. Add
  `image: Option<ImageAttachment>` (base64, mime, width, height) per
  §4.3 step 1.
- Source of the image bytes: the tool_result event's `value.details`
  (available in both slim and legacy events). The plan says "read the
  tool log record's details," but the tool log record has no `details`
  field — it has `stdout` (raw) and `text` (display). The clean source
  is the **event**'s `value.details`. `tool_log_texts` (which returns
  `id -> text`) already feeds `Ev::ToolResult.text`. A parallel
  `tool_log_details` (`id -> Value`) or a direct read of
  `event["value"]["details"]` feeds the image. Note this wording fix in
  the plan.
- `full_items`: when `vision == true`, emit the array `output` form
  (`input_text` + `input_image`). When `false`, the plain string + the
  "does not support image input" note.
- `compact_items`: drop the image, substitute the text note
  `[image omitted: read image file <path>]`. The summary-input path
  (`--summary-input`, keep = 0, all-compact) inherits this, so compacted
  regions never re-send images.
- `estimate_ev_tokens`: +4800 chars per image-carrying event (§4.3 step 6).
- Budget + kill switch: `[limits] image_budget_bytes` (default 20 MB)
  and `block_images` (§4.3 step 5, §4.5). When the kept-region image
  total exceeds the budget, drop the **oldest** images first, each
  replaced by the text note.
- The `vision` / `block_images` / `image_budget_bytes` values must be
  threaded into both `full_items` and `compact_items` (today they take
  only `ev`, `caps`, `drop_pairs`, `ptrs`).

Verdict: the plan is right on shape. The two clarifications are
(1) read the image from the event's `value.details`, not the tool log's
"details." (2) The budget/kill-switch plumbing is in `assemble`,
not `model_settings`.

### 3.4 `bin/model` — one required fix

- Responses path: `call_responses_api` sends the request verbatim. The
  array `output` form rides through. No change.
- Chat path: `convert_to_chat_format` reads
  `item.get("output").and_then(|o| o.as_str())`. When `output` is an
  **array** (the image form), `as_str()` is `None` and the output becomes
  `""`. The image is **silently dropped**. This is exactly the silent
  loss §4.4 step 1 warns about.
- Required change: detect an array `output` with `input_image` parts,
  emit the `role:"tool"` message with the text parts joined, then a
  follow-up `role:"user"` message carrying `image_url` parts (drop the
  Responses-only `detail` field here). Also, when the model needs a
  bridge, insert the synthetic assistant message between the tool result
  and the injected user message.

Verdict: §4.4 step 1 is a mandatory fix, not optional. Until it lands,
any image on a Chat-Completions model (both configured models are
Chat-Completions in practice — DeepSeek and local Qwen both fall back
after 404/405) is lost on the wire.

### 3.5 `bin/compact`

- Trigger math uses `compact_math::estimate_from_events` and
  `project_event` — so it inherits the §2.2 under-count. The compact
  trigger for an image-heavy session is late.
- The old-region projection goes through `assemble --summary-input`,
  which (once §3.3 lands) drops images in the compact form. So the
  summary **prompt** is clean. Only the trigger reading is off.
- No code change to the compact binary itself beyond the shared
  `compact_math` fix.

### 3.6 `bin/parse`

- Parses the model's response into `assistant_message` / `tool_call` /
  `error` events. The model does not emit images, so nothing here
  changes. No interaction.

### 3.7 `bin/user`

- Appends `user_message` events. No image involvement. No change.

### 3.8 `bin/claim`

- Derives loop state from event types and sequences. Reads types, not
  `value.details`. Image payload is invisible to it. No change.

### 3.9 `bin/log`

- Validates and appends events. Validation is generic (§2.3). If §4.7
  adds `value.images`, only the schema file changes. The validator
  already handles arrays/objects. No code change.

### 3.10 `bin/hook-compact`

- A hook that shells out to the `compact` binary. No direct image logic.
- It inherits the compact trigger under-count via the shared module
  (§2.2). No change of its own.

### 3.11 `bin/rushi` (loop)

- `step.rs::estimate_context` calls `compact_math::estimate_from_events`,
  so the proactive compact check is blind to images (§2.2). The overflow
  recovery still catches it after a real provider overflow, but the
  proactive path is late.
- The `model.before` payload carries `projected_tokens` (the under-count)
  and the full `request` (with images). Extension hooks that transform
  the request must preserve image parts.
- No loop code change beyond the shared `compact_math` fix and (optionally)
  teaching the loop to honor `block_images` before building the request.

---

## 4. Cross-cutting findings

- **Two estimators, one blind.** `assemble` will count images
  (§4.3 step 6), but `crates/rushi::compact_math` (used by the loop and
  the compact binary) will not, unless §2.2 is fixed. This is the single
  most important plan gap.
- **Chat-Completions drop is silent.** Both configured models are
  Chat-Completions in practice, so §3.4 is on the hot path, not a corner
  case.
- **Double storage.** The base64 lands in both `tools.jsonl` (raw
  `stdout`) and `events.jsonl` (`value.details`). §4.11 accepts the tool
  log copy but not the event copy. Decide which one owns the payload.
- **Schema self-contradiction.** §4.6 says `tool_result.json` needs no
  change. §4.7 adds `value.images` to that same schema. Reconcile: if the
  TUI channel is in scope, the schema does change (add optional
  `images`). If out of scope, §4.7 should be dropped.
- **Provider-specific fields.** `detail` on `input_image` is
  Responses-only, and some providers need the synthetic assistant bridge
  message (§4.10). Both are per-model, so they belong in
  `ModelSettings` (plan §4.10 already says so) and must be plumbed to
  both `assemble` (wire form) and `model` (bridge).

---

## 5. Corrected implementation checklist

The plan's §5 order is sound. Add these rows:

| Step | File(s) | Note |
|------|---------|------|
| 0. Shared estimator | `crates/rushi/src/compact_math.rs` | `project_event` / `estimate_from_events` add +4800/image for `tool_result` carrying an image in `value.details`. Keeps loop + compact + assemble consistent. (NEW — not in plan §5.) |
| 1. Read tool | `tools/read/{src/main.rs,Cargo.toml}` | As in plan. Add `image` + `base64`. |
| 2. Route | `bin/route` | No change (verified). Note the `bytes` under-report and the double storage. |
| 3. Assemble | `bin/assemble` | Read image from event `value.details`; `full_items` array form; `compact_items` drop; `estimate_ev_tokens` +4800; thread `vision`/`block_images`/`image_budget_bytes`. |
| 4. Model | `bin/model` | **Mandatory**: `convert_to_chat_format` array-`output` → user-message `image_url`; add synthetic bridge when flagged. |
| 5. Model settings | `crates/rushi/src/model_settings.rs` | `vision`; plus `image_wire` and `needs_bridge` to fully honor §4.10. |
| 6. Limits | `config.toml` / `config-real.toml` | `[limits] image_budget_bytes`, `block_images`. |
| 6b. Schema (optional) | `schemas/events/v1/tool_result.json` | Only if §4.7 TUI channel is in scope. Reconcile §4.6 vs §4.7. |
| 7. Tool description | `tools/read/tool.toml` | As in plan. |
| 8. Tests | — | As in plan §6, plus: a compact trigger test proving the loop/compact estimator now counts images; a Chat-Completions test proving the array `output` is not dropped. |

---

## 6. Risk ranking

1. **High / must-fix:** `bin/model` Chat-Completions silent drop (§3.4).
   Both configured models use this path.
2. **High / must-fix:** `compact_math` estimator blind to images (§2.2).
   Affects loop proactive compact + compact trigger.
3. **Medium:** Double base64 storage (§4.11 / §3.2). Unbounded log growth
   per image read.
4. **Medium:** `detail` + bridge are provider-specific (§4.10) and need
   per-model plumbing, not a single `vision` flag.
5. **Low:** schema §4.6/§4.7 contradiction. `bytes` under-report.

No component in `crates/rushi/` or `bin/` is structurally blocked by the
plan. The image data flows cleanly through `route`, `log`, `claim`,
`parse`, `user`, `stage`, `rewind`, `hooks`, and `logline` with no
change. The work concentrates in three places: the read tool, `assemble`
(emission + budget), and `model` (Chat-Completions conversion), plus one
shared-estimator fix in `compact_math`.
