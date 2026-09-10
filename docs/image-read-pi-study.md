# How pi Addresses the Gaps Found in `image-read-plan.md`

This document studies how pi 0.85.1 handles each gap identified in the
review of `docs/image-read-plan.md`. All findings are from the pi source
tree at `~/.pi/pkgs/pi-coding-agent-0.85.1`. It is based on the installed
pi 0.85.1 package, not on a local clone.

---

## 1. EXIF Orientation (Gap #2)

**The gap:** The plan mentions EXIF orientation in the pi reference (§2.2)
but omits it from the implementation steps (§4.1). The `image` crate does
not apply EXIF orientation automatically.

**How pi handles it:**

pi has a dedicated module `src/utils/exif-orientation.ts` that:

- Manually parses the EXIF `Orientation` tag (0x0112) from the raw byte
  stream. For JPEG, it walks the SOI marker to find the APP1 (0xE1)
  segment, checks for the `Exif\0\0` header, then reads the TIFF IFD.
  For WebP, it walks RIFF chunks to find the `EXIF` chunk.
- Supports all 8 EXIF orientation values:
  - 1: no-op
  - 2: horizontal flip
  - 3: 180° rotation (flip H + flip V)
  - 4: vertical flip
  - 5: 90° CW + horizontal flip
  - 6: 90° CW
  - 7: 90° CW + vertical flip
  - 8: 90° CCW
- Applies the transform via Photon (`photon.fliph`, `photon.flipv`, and a
  manual pixel-level `rotate90` helper) before resizing.

The orientation is applied in **two** places:
- `convertImageBytesToPng` — used for TUI display (Kitty graphics).
- `resizeImageInProcess` — used for the model path.

**Implication for rushi:** The `image` crate does not expose EXIF
orientation. The plan must add an explicit EXIF-orientation step:
- Parse the EXIF tag from the JPEG/WebP byte stream (or use a small
  crate like `kamadak-exif` or `exif`).
- Apply the rotation/flip to the decoded pixel buffer before resize.
- Alternatively, use a library that does this automatically.

---

## 2. Images in Tool Messages (Gap #1, #4)

**The gap:** The plan says `convert_to_chat_format` "needs one change" to
emit multi-part tool content. The plan also claims the Responses API
`function_call_output.output` is string-only, so it proposes a sibling
`input_image` item.

**How pi handles it — two different strategies per API:**

### Responses API (`openai-responses-shared.js`)

`convertToolResultOutput` shows that `function_call_output.output` CAN be
an **array** of content items, not just a string:

```js
// When model supports images:
output = [
    { type: "input_text", text: "Read image file [image/png]..." },
    { type: "input_image", detail: "auto", image_url: "data:image/png;base64,..." }
];
// When model does NOT support images:
output = "Read image file [image/png]... (see attached image)"
// (a plain string; images are dropped)
```

The plan's claim that "the OpenAI Responses API does not support image
content inside `function_call_output.output`" is **incorrect**. The
`output` field accepts `string | Array<{type, ...}>` where the array
elements include `input_text` and `input_image` items.

### Chat Completions (`openai-completions.js`)

pi does **NOT** put images in the tool message. Instead:

1. The `role: "tool"` message carries only the text portion (or the
   placeholder `"(see attached image)"` if there is no text).
2. Images are extracted and placed in a **separate user message** that
   immediately follows:

   ```js
   {
       role: "user",
       content: [
           { type: "text", text: "Attached image(s) from tool result:" },
           { type: "image_url", image_url: { url: "data:image/png;base64,..." } }
       ]
   }
   ```

This avoids the problem that OpenAI's Chat Completions API does not
support array content in `role: "tool"` messages.

**Implication for rushi:**
- For the Responses API path, embed images directly in
  `function_call_output.output` as an array of content items.
- For the Chat Completions path, extract images from tool results and
  inject them as a follow-up user message, not inside the tool message.
- The plan's proposal to put images in the tool message's content array
  will not work with most Chat Completions providers.

---

## 3. Session / Log Bloat (Gap #3)

**The gap:** Storing multi-MB base64 image data in `tools.jsonl` and
`events.jsonl` will bloat the session files.

**How pi handles it:**

pi stores the full base64 image data in the session JSONL file. The
`ImageContent` type is:

```typescript
interface ImageContent {
    type: "image";
    data: string;      // base64 encoded
    mimeType: string;
}
```

There is no reference/pointer mechanism. Each image read appends
potentially several megabytes to the session file. However, pi's
compaction system eventually summarizes away old messages (including
images), so the bloat is bounded by the time between compactions.

pi also has a global `blockImages` setting
(`src/core/settings-manager.ts`) that strips all images from all messages
before sending to the LLM. This is a defense-in-depth kill switch.

**Implication for rushi:** The plan should:
- Consider whether to store the full base64 in the tool log or use a
  file reference. The tool log is the source of truth that `assemble`
  reads; if images are stored as file paths instead of inline base64,
  `assemble` would need to read the file at request-build time.
- Note that pi accepts the bloat and relies on compaction to bound it.
- Consider a `blockImages` equivalent in the rushi config.

---

## 4. Compaction and Images (Gap #5)

**The gap:** The plan does not specify what happens to images when
compaction summarizes old events.

**How pi handles it:**

pi's compaction sends old messages to the LLM for summarization. Images
in the summarized region are included in the prompt to the summarizer but
the summary output is plain text — images are lost after compaction.
The summary text may reference the file path, but the pixel data is gone.

For token estimation, pi uses a flat per-image estimate:
`ESTIMATED_IMAGE_CHARS = 4800` (≈1200 tokens at 4 chars/token). This is
used in `estimateTokens` for the compaction budget calculation.

**Implication for rushi:**
- In `compact_items`, images should be replaced with a short text note
  (e.g., `[image omitted: read image file /path/to/img.png]`).
  The full base64 should not ride into the compacted form.
- In `full_items` (the keep window), images are sent as-is.
- For `estimate_ev_tokens`, use a flat estimate per image (like pi's
  4800 chars) rather than a per-dimension formula. This is simpler and
  more conservative.

---

## 5. Total Image Budget (Gap #6)

**The gap:** No cap on total image data across the context.

**How pi handles it:**

pi has **no total image budget**. It relies on:
- Per-image limit: 4.5 MB base64 (below Anthropic's 5 MB limit).
- Compaction: old images are summarized away, reducing total image
  count in context.
- `blockImages` setting: a global kill switch.

**Implication for rushi:** The plan should add a configurable total
image budget (e.g., `max_total_image_bytes` in the `[limits]` section).
When the total exceeds the budget, the oldest images in the context
should be dropped (replaced with text notes) before sending the request.

---

## 6. Vision Gate Plumbing (Gap #7)

**The gap:** The plan adds `vision: bool` to `ModelSettings` but does
not specify how it reaches `full_items` / `compact_items`.

**How pi handles it:**

pi checks `model.input.includes("image")` in two places:

1. **Read tool** (`read.ts`): `getNonVisionImageNote(model)` returns a
   warning string when the model does not support images. The note is
   appended to the tool output text. The image is still processed and
   returned in the content array.

2. **Provider request builder** (`openai-responses-shared.js`,
   `openai-completions.js`): when building the request, if
   `model.input` does not include `"image"`, image blocks are stripped
   from the content and replaced with a text placeholder
   (`"(see attached image)"`).

3. **`blockImages` setting** (`sdk.ts`): a global setting that strips
   ALL images from ALL messages before sending, regardless of model
   capability. Defense-in-depth.

**Implication for rushi:**
- Thread the `vision` flag (or `ModelSettings` ref) into
  `full_items` / `compact_items`.
- When `vision` is false, omit the `input_image` item and append a note
  to the text: `[Model does not support image input. Image omitted.]`
- Consider a global `blockImages` config option as a kill switch.

---

## 7. Animated GIF / WebP (Gap #9)

**The gap:** The plan rejects animated PNG but does not address animated
GIF or WebP.

**How pi handles it:**

- **Animated PNG**: rejected at detection time (`isAnimatedPng` checks
  for the `acTL` chunk). The file is not treated as an image.
- **Animated GIF / WebP**: NOT rejected. They pass through as their
  native format (GIF → `image/gif`, WebP → `image/webp`). The provider
  receives the full file and handles animation (or takes the first
  frame).
- For TUI display, `convertImageBytesToPng` converts GIF/WebP to PNG
  (first frame only) because the Kitty graphics protocol requires PNG.

**Implication for rushi:** The plan should:
- Reject animated PNG (already planned).
- Pass animated GIF/WebP through natively (the `image` crate decodes
  the first frame; the provider sees a static image).
- Note that the provider sees a static first frame, not the animation.

---

## 8. Corrupt / Undecodable Images (Gap #10)

**The gap:** No error handling for files that pass MIME detection but
fail to decode.

**How pi handles it:**

- `processImage` returns `{ ok: false, message: "..." }` when
  conversion or resize fails (including decode failure).
- The read tool outputs a text note: `"Read image file [mime]\n<message>"`
  instead of the image.
- In `normalizeToolResultImages` (for extension/MCP tools), if
  processing fails, the **original** image block is kept as-is rather
  than dropped. The comment says: "the tool already produced this image
  and the failure may just be an unavailable image backend, so passing
  it through preserves the behavior tools have today."

**Implication for rushi:** The plan should specify:
- Decode failure → return a text error, no image data.
- Conversion failure (e.g., Photon unavailable) → pass through the
  original bytes if within size limits, with a warning note.

---

## 9. GIF / WebP Pass-Through (Correction to Plan §2.2)

**The plan says:** "GIF, WebP, BMP are converted to PNG via Photon."

**Actual pi behavior:**
- PNG, JPEG, GIF, WebP: pass through unchanged in the model path.
- BMP and any other format: converted to PNG via Photon.
- The TUI display path converts GIF/WebP to PNG (Kitty protocol
  requirement), but this does not affect the model request.

The plan's §2.2 is inaccurate. GIF and WebP pass through in the model
path; only BMP (and unknown formats) are converted.

---

## 10. `detail` Field (Gap #11)

**The gap:** The plan uses `detail: "auto"` on `input_image` items.

**How pi handles it:**
- Responses API: `detail: "auto"` is set on every `input_image` item.
- Chat Completions: no `detail` field is used.
- The `detail` field is specific to OpenAI's Responses API. Other
  providers ignore it.

**Implication for rushi:** Include `detail: "auto"` only on the
Responses API path. Omit it on the Chat Completions path. Or make it
conditional on the provider.

---

## 11. Token Estimation for Images (Gap #14)

**The gap:** The plan's formula `(width × height) / 750` clamped to
360–5000 is a rough approximation.

**How pi handles it:**

pi uses a flat constant: `ESTIMATED_IMAGE_CHARS = 4800` for every image
block. This is used in `estimateTokens` for the compaction budget
calculation. It is conservative (overestimates for small images,
underestimates for large ones) but simple and provider-agnostic.

**Implication for rushi:** Use a flat per-image estimate (e.g., 4800
chars ≈ 1200 tokens) in `estimate_ev_tokens`. This is simpler than a
per-dimension formula and avoids the complexity of tracking dimensions
through the estimation pipeline. The actual provider will report the
true token count in the response `usage` field, which the harness
already records for the next request.

---

## 12. Worker-Thread Isolation

pi runs Photon (WASM image processing) in a Node.js worker thread to
avoid blocking the TUI event loop. The rushi harness does not have this
concern: the read tool is a subprocess, so image processing blocks
only that subprocess, not the main loop. No action needed.

---

## Summary of Required Plan Revisions

| # | Gap | Required Plan Change |
|---|-----|---------------------|
| 1 | EXIF orientation | Add an explicit EXIF-orientation step before resize. The `image` crate does not do this. Use `kamadak-exif` or manual parsing. |
| 2 | Chat Completions images | Do NOT embed images in the tool message. Extract them into a follow-up user message with `image_url` parts, matching pi's approach. |
| 3 | Responses API `output` field | `output` can be an array of content items, not just a string. Embed images directly in `function_call_output.output` as `input_text` + `input_image` items. The plan's "sibling `input_image` item" approach is unnecessary. |
| 4 | Log bloat | Decide: inline base64 in tool log (like pi) vs. file reference. Add a `blockImages` kill-switch config. |
| 5 | Compaction | Drop images in `compact_items`, replace with text note. Keep them in `full_items`. |
| 6 | Total image budget | Add a configurable total image budget in `[limits]`. Drop oldest images when exceeded. |
| 7 | Vision plumbing | Thread `vision` flag into `full_items` / `compact_items`. Omit `input_image` when `vision` is false. |
| 8 | Animated GIF/WebP | Pass through natively. Provider sees first frame. |
| 9 | Corrupt images | Decode failure → text error. Conversion failure → pass through original bytes with warning. |
| 10 | GIF/WebP pass-through | Correct §2.2: GIF and WebP pass through in model path; only BMP is converted. |
| 11 | `detail` field | Include only on Responses API path, not Chat Completions. |
| 12 | Token estimation | Use flat 4800-char estimate per image, not a per-dimension formula. |
