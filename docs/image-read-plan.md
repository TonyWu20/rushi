# Image Support for `tools/read`

## 1. Goal

Allow the `read` tool to return image files (PNG, JPEG, GIF, WebP, BMP) so the
model can see screenshots, diagrams, and design mockups. The model must
receive the image as a proper image content part, not as base64 text.

> A source-level study of the pi behavior referenced below is in
> `docs/image-read-pi-study.md`. That study corrects two claims in this
> plan (Responses `output` is array-capable; Chat Completions images ride
> in a user message) and adds the EXIF-orientation, storage, and budget
> considerations.

## 2. How pi Reads Images (Reference)

Source: `src/core/tools/read.ts`, `src/utils/mime.ts`,
`src/utils/image-process.ts`, `src/utils/image-resize-core.ts`,
`src/utils/image-convert.ts` (pi 0.85.1).

### 2.1 Detection

- `detectSupportedImageMimeTypeFromFile` opens the file and reads the first
  4100 bytes.
- Magic-byte sniffing:
  - PNG: `89 50 4E 47 0D 0A 1A 0A` + IHDR chunk. Animated PNGs (acTL chunk)
    are rejected.
  - JPEG: `FF D8 FF` (first 3 bytes).
  - GIF: ASCII "GIF" at offset 0.
  - WebP: "RIFF" at 0, "WEBP" at 8.
  - BMP: "BM" at 0, with DIB header sanity checks.
- Returns a MIME string (`image/png`, `image/jpeg`, etc.) or `null` if the
  file is not a supported image.

### 2.2 Processing Pipeline (`processImage`)

1. **Normalize**: PNG, JPEG, GIF, and WebP pass through unchanged in the
   model path. BMP and any other detected format convert to PNG via
   Photon (Rust WASM). The TUI display path converts GIF/WebP to PNG as
   well, because the Kitty graphics protocol requires PNG.
2. **Resize** (auto, on by default):
   - Max dimensions: 2000×2000 px.
   - Max encoded size: 4.5 MB base64 (headroom below Anthropic's 5 MB limit).
   - Strategy: resize to target dimensions, try PNG and JPEG at qualities
     [80, 85, 70, 55, 40], pick the smallest. If still too large, reduce
     dimensions by 25% each iteration down to 1×1.
   - EXIF orientation is applied before resize. pi parses the tag itself
     (`src/utils/exif-orientation.ts`): all 8 values, JPEG APP1 segment
     and WebP `EXIF` chunk, then flip/rotate via Photon.
   - If the image already fits within all limits, it is passed through
     unchanged (no re-encode).
3. **Encode**: The result is base64-encoded.
4. **Output shape**:

   ```json
   {
     "type": "text",  "text": "Read image file [image/png]\n[dimension note]"
   },
   {
     "type": "image", "data": "<base64>", "mimeType": "image/png"
   }
   ```

### 2.3 Model Capability Gate

`getNonVisionImageNote(model)`: if the model's `input` capability array does
not include `"image"`, a note is appended:
`"[Current model does not support images. The image will be omitted from this
request.]"` The image is still read and processed, but the caller (the model
request builder) drops it.

### 2.4 Normalization of Extension-Produced Images

`normalizeToolResultImages` (tool-result-images.ts): after any tool returns an
`ImageContent` block, the data is re-run through `processImage` to guarantee it
meets the size limit. This protects the session history from oversized images
that would cause the provider to reject the entire conversation. When the
re-run fails, pi keeps the original block (it must not delete tool output
because the image backend is unavailable).

### 2.5 Image Delivery to the Provider

pi keeps image blocks in the in-memory message history and converts them
per provider API at request-build time (pi-ai package):

- **Responses API**: `function_call_output.output` accepts a string **or**
  an array of content items. pi builds the array form:

  ```js
  output = [
      { type: "input_text", text: "Read image file [image/png]..." },
      { type: "input_image", detail: "auto", image_url: "data:image/png;base64,..." }
  ];
  ```

  A non-vision model gets the plain string form: images are dropped and
  replaced by the placeholder `"(see attached image)"`.
- **Chat Completions**: the `role: "tool"` message carries text only. pi
  extracts the image blocks and injects a **separate user message**
  immediately after the tool results:

  ```js
  { role: "user", content: [
      { type: "text", text: "Attached image(s) from tool result:" },
      { type: "image_url", image_url: { url: "data:image/png;base64,..." } }
  ] }
  ```
- **Session storage**: the session JSONL stores the full base64
  `ImageContent` blocks. Compaction bounds the growth over time.
- **Token estimation**: a flat `ESTIMATED_IMAGE_CHARS = 4800` per image
  block in the compaction budget (`src/core/compaction/compaction.ts`).
- **Global kill switch**: the `blockImages` setting strips all images from
  all messages before the provider call.

## 3. Rushi Architecture (Current State)

```
tools/read/main.rs   → JSON stdout: { "text": "...", "path": "...", "type": "file" }
bin/route            → tool_result event: value.text (string), value.details (JSON)
bin/assemble         → model request: function_call_output { output: string }
bin/model            → Responses API (input: [ … function_call_output … ])
                       or Chat Completions fallback (messages: [{role:"tool"}])
```

Key constraint: rushi's `bin/assemble` emits `output` as a plain string and
`bin/model` renders `role: "tool"` content as a string. Neither path carries
image content today.

What the provider APIs allow (verified against pi 0.85.1):

- **Responses API**: `function_call_output.output` accepts a string **or an
  array of content items**. pi's request builder emits the array form with
  `input_text` and `input_image` parts. The image data is carried inside
  the tool output; no sibling item is needed.
- **Chat Completions**: `role: "tool"` content is a string. pi does not
  array-ify it. It extracts the images and injects a follow-up
  `role: "user"` message with `image_url` parts.

### 3.1 Model Capability

`crates/rushi/src/model_settings.rs` has no `input` or `vision` field. The
config file (`config-real.toml`) references `deepseek-v4-flash-vision-exp`
in docs but the model table has no capability flag.

## 4. Plan

### 4.1 `tools/read` — Detect and Emit Image Data

**File**: `tools/read/src/main.rs`
**New dependencies**: `image` (for decode/resize), `base64` (for encoding).
Alternative: `image` + `image-png` + `image-jpeg` feature flags to keep the
binary small.

Changes to `main.rs`:

1. **MIME detection** (mirror pi's magic-byte approach, ~60 lines):
   - Read the first 4100 bytes.
   - Sniff PNG / JPEG / GIF / WebP / BMP.
   - Reject animated PNG (acTL chunk) — return an error message suggesting
     the user convert to a static frame.
   - Animated GIF / WebP: accept them (pi does not reject them). The
     decoder takes the first frame; the `text` note says so.
   - If the file is not a recognized image and the binary check (null byte in
     first 8 KB) says it is not text, return the existing error.

2. **Image path** (new branch, before the text path):
   - Read the full file bytes.
   - **Input cap**: if the file exceeds **20 MB**, reject with a clear
     error ("image exceeds 20 MB; resize before reading"). This is the
     memory backstop — a 20 MB source decodes to up to ~80 MB RGBA.
   - Decode with the `image` crate to get width/height.
   - **EXIF orientation** (missing from the first draft): the `image`
     crate does not apply EXIF orientation. Parse the EXIF `Orientation`
     tag (JPEG APP1 segment, WebP `EXIF` chunk) and apply the flip or
     rotation to the pixel buffer before resize. Mirror pi's
     `src/utils/exif-orientation.ts`: all 8 orientation values. Without
     this step, phone photos render rotated.
   - **Resize**: if width or height > 2000, downscale proportionally to fit
     within 2000×2000. Use Lanczos3 filter (the `image` crate's
     `Lanczos3` resampling).
   - **Format pick**: encode the result to PNG and JPEG q80; ship the
     smaller. PNG wins for flat/UI content; JPEG wins for photos.
   - **Size guard**: if the chosen encoding's base64 length > 4.5 MB,
     step JPEG quality 80 → 70 → 55 → 40 and shrink dimensions by 25%
     each pass, down to 1×1. Safety net only (a 2000×2000 JPEG never
     reaches it in practice).
   - **Pass-through**: if the source is already ≤2000×2000 and under the
     base64 budget, send the original bytes untouched (no re-encode).
   - Base64-encode the final bytes.
   - Emit JSON:

     ```json
     {
       "type": "image",
       "mime_type": "image/png",
       "data": "<base64>",
       "width": 2000,
       "height": 1333,
       "original_width": 4000,
       "original_height": 2667,
       "text": "Read image file [image/png] (4000x2667 → 2000x1333)"
     }
     ```

   - The `text` field carries a human/model-readable description. The
     `data` + `mime_type` fields carry the image.

3. **Text path** (unchanged): the existing null-byte check, line
   pagination, and truncation logic remain. A binary file that is not a
   recognized image still produces the existing "not a UTF-8 text file"
   error.

4. **Error paths**:
   - **Corrupt image**: the file passes the magic-byte sniff but fails to
     decode (truncated or corrupt). Return the `text` field with
     `"image could not be decoded; the file may be truncated or corrupt"`
     and omit `data`.
   - **Oversized**: if even a 1×1 re-encode exceeds the byte limit
     (extremely unlikely), return the `text` field with a message and
     omit `data`.

5. **`tool.toml` description update**:

   ```toml
   description = "Read the contents of a file. Supports text files and images (png, jpeg, gif, webp, bmp). Images are resized to 2000x2000 max and sent as attachments. For text files, output is truncated to 2000 lines or 50KB."
   ```

### 4.2 `bin/route` — Pass Through Image Data

**File**: `bin/route/src/main.rs`

The tool's stdout JSON already flows into `value.details` via
`structured_output()`. No structural change is needed if the read tool's
output JSON includes the image fields. However, two adjustments:

1. **`value.text`**: when the tool's stdout JSON has a `text` field, use it
   (existing behavior). The image data is carried in `value.details`.

2. **No change to the event schema**: the `tool_result` event's `value`
   already has `text` and `details`. The image fields live inside
   `details` (the parsed tool stdout JSON). This keeps the schema stable.

   If a future extension needs the image to be a first-class field in the
   event (e.g., for the TUI to render thumbnails), add `value.images` as
   an optional array. Not required for the model path.

### 4.3 `bin/assemble` — Emit Image Content Parts

**File**: `bin/assemble/src/main.rs`

When projecting a `tool_result` event, check the tool log record's
details (the parsed tool stdout JSON) for `type == "image"` with a
non-empty `data` field.

1. **`Ev::ToolResult` struct**: add an optional `image` field:

   ```rust
   Ev::ToolResult {
       id: String,
       text: String,
       image: Option<ImageAttachment>,  // new
   }
   ```

   Where:

   ```rust
   struct ImageAttachment {
       data: String,       // base64
       mime_type: String,  // e.g. "image/png"
       width: u32,
       height: u32,
   }
   ```

   Populated during the event-parse loop by reading the tool log
   record's `details`.

2. **Responses API path**: `function_call_output.output` accepts a
   string **or an array of content items**. Emit the array form when
   the model has the `vision` flag:

   ```json
   {
     "type": "function_call_output",
     "call_id": "call-1",
     "output": [
       { "type": "input_text",  "text": "Read image file [image/png] (4000x2667 → 2000x1333)" },
       { "type": "input_image", "detail": "auto",
         "image_url": "data:image/png;base64,<base64>" }
     ]
   }
   ```

   When `vision` is false, emit the plain string form: the `text`
   description plus the note `[Model does not support image input. Image
   omitted.]`. No sibling `input_image` item is needed — the array form
   carries both. The `detail` field is Responses-specific; omit it on
   any provider that does not document it.

3. **Chat Completions path**: `role: "tool"` content must stay a plain
   string. Emit the tool message with the `text` description only (or
   the placeholder `"(see attached image)"` when there is no text).
   Then emit a follow-up `role: "user"` message carrying the images:

   ```json
   {
     "role": "user",
     "content": [
       { "type": "text", "text": "Attached image(s) from tool result:" },
       { "type": "image_url", "image_url": { "url": "data:image/png;base64,..." } }
     ]
   }
   ```

   This mirrors pi's `openai-completions.js` converter. Multi-part
   arrays in `role: "tool"` messages are not portable across providers.

4. **`full_items` vs `compact_items`**:
   - `full_items` (the keep window): emit the image as described above.
   - `compact_items` (the compacted region): drop the image. Replace it
     with a text note in the `output` string, e.g.
     `"[image omitted: read image file <path>]"`. Old images must not
     enter compacted requests: they would defeat the compaction
     budget. If the model still needs the image, it re-issues the
     read call on the file path.
   - Thread the `vision` flag (or a `&ModelSettings` reference) into
     both builders. Today they receive only `ev`, `caps`, `drop_pairs`,
     and `ptrs`.

5. **Total image budget**: add a `[limits] image_budget_bytes` knob
   (default 20 MB, base64). When the sum of `image.data` lengths in the
   kept events exceeds the budget, `assemble` drops the oldest image
   items (oldest events first), replacing each with the text note from
   step 4.

6. **Token estimation**: `estimate_ev_tokens` adds a flat 4800 chars
   (~1200 tokens at the default ratio of 4 chars/token) per event that
   carries an image. This mirrors pi's `ESTIMATED_IMAGE_CHARS = 4800`
   in `src/core/compaction/compaction.ts`. A per-dimension formula is
   not needed: the provider's `usage` report corrects the estimate on
   the next request.

### 4.4 `bin/model` — Chat Completions Image Handling

The `model` binary passes the assembled Responses request verbatim to
the provider. The Responses path needs no change: the array `output`
form is part of the request body.

The `convert_to_chat_format` function needs two changes:

1. When it reaches a `function_call_output` whose `output` is an array
   containing `input_image` parts, emit a plain-string `role: "tool"`
   message (text parts joined, images extracted) followed by a
   `role: "user"` message with the `image_url` parts, as in §4.3 step 3.
   The current code drops unknown item types in the catch-all arm
   (`_ => {}`). An `input_image`-bearing output must not be silently
   dropped.
2. When the model section has `vision = false`, `assemble` already
   emits the string form. No extra work here.

### 4.5 Model Capability Gate

**File**: `crates/rushi/src/model_settings.rs`

Add a field to `ModelSettings`:

```rust
/// Whether the model accepts image input.
pub vision: bool,
```

Read it from the model TOML section:

```toml
[model.deepseek-vision]
model_id = "deepseek-v4-flash-vision-exp"
vision = true
```

If `vision` is `false` (the default), `assemble` should:
- Still include the `text` description of the image (so the model knows
  the file was read and its dimensions).
- Omit the image content parts.
- Append a note to the text:
  `"[Model does not support image input. Image omitted.]"`

This mirrors pi's `getNonVisionImageNote`.

pi uses three layers; rushi mirrors all three:

1. **Tool-level note** (the read tool text, mirroring
   `getNonVisionImageNote`): the text note above. The image is still
   processed on disk; only the request drops it.
2. **Request-builder drop**: `assemble`/`model` omit the image content
   parts when `vision` is false (§4.3 step 2–3).
3. **Global kill switch**: pi has `blockImages` in settings. Rushi adds
   an equivalent: a `[limits] block_images` boolean (default false).
   When true, `assemble` strips every image from every request and
   appends a `[Images are disabled by config.]` note. This covers the
   case where the user switches to a non-vision model but the session
   history still carries images.

### 4.6 `tool_result` Schema Update

**File**: `schemas/events/v1/tool_result.json`

No change. The image data lives inside `value.details` (the tool's
parsed stdout JSON), which is already an open object. No new top-level
field is needed.

### 4.7 TUI / `bin/rushi` — Interface Now, Rendering Later

The TUI-side thumbnail rendering is an independent track. This phase
creates only the **interface** so the TUI team can render later without a
schema break:

- Add an optional `value.images` array to the `tool_result` event schema
  (`schemas/events/v1/tool_result.json`).
- `route` extracts `value.images` from the tool's stdout JSON when the
  tool emits an image. Each entry: `{ "mime_type": "image/png", "data":
  "<base64>", "width": 2000, "height": 1333 }`.
- The model path (assemble) reads the image from `value.details` (the full
  tool stdout), not from `value.images`. The two are independent:
  `value.images` is the TUI thumbnail channel; `value.details` is the
  model-visibility channel. Both carry the same base64, so a TUI that
  renders the thumbnail does not need to read the tool log.
- Rendering (thumbnail scaling, scroll, click-to-open) is the TUI's
  separate work item; this phase only wires the data through the event.

### 4.8 Practical Size Estimates (Empirical)

Generated with ImageMagick at the two resolutions users actually run.
Each content class was encoded to PNG, JPEG (q85), and WebP (q80). Base64
adds a fixed 33% overhead (4/3).

**1920×1080**

| Content class | PNG | JPEG q85 | WebP q80 |
|---|---|---|---|
| UI screenshot (flat) | 5 KB | 18 KB | 5 KB |
| Smooth gradient | 17 KB | 26 KB | 12 KB |
| Landscape (photo) | 4.83 MB | 0.07 MB | 0.02 MB |
| Random noise (worst) | 5.94 MB | 1.41 MB | 1.34 MB |

**3840×2160**

| Content class | PNG | JPEG q85 | WebP q80 |
|---|---|---|---|
| UI screenshot (flat) | 12 KB | 57 KB | 16 KB |
| Smooth gradient | 59 KB | 75 KB | 38 KB |
| Landscape (photo) | 17.63 MB | 0.22 MB | 0.08 MB |
| Random noise (worst) | 23.75 MB | 5.63 MB | 5.42 MB |

Key observations:

- **PNG is a disaster for photos.** A 4K landscape PNG is 17.63 MB; the
  same image as JPEG q85 is 0.22 MB. Always re-encode photo-like content
  to JPEG or WebP, never ship the original PNG bytes to the model.
- **UI screenshots and diagrams are tiny** in every format, even at 4K
  (12–57 KB). They are the common `read`-an-image case and are cheap.
- **JPEG/WebP dominate for high-entropy content.** Even the random-noise
  worst case is 5.4 MB WebP at 4K, but that is an unrealistic input for a
  coding-agent image read.
- **Base64 is a flat ×1.33.** A 4.5 MB base64 budget is ~3.4 MB binary.

### 4.9 Compression Strategy

The harness **does** compress on its own — it must. A user `read`s a
4K photo (a 17 MB PNG is realistic); the model request cannot carry 17 MB
(base64 would be 23 MB). The pipeline:

1. **Input cap**: reject files over **20 MB** with a clear error. This
   covers 4K photos (typically 5–15 MB) and leaves headroom. Memory note:
   decoding a 20 MB PNG to RGBA is ~20× the byte count in the worst case
   (100 MB source → 400 MB RGBA), so the cap is the memory backstop too.
2. **Resize** to ≤2000×2000 (Lanczos3) when the source exceeds it.
   A 4K photo resized to 2000×2000 and re-encoded as JPEG q80 lands at
   ~200–500 KB — far under any provider limit.
3. **Format pick**: encode the resized image to both PNG and JPEG q80;
   ship the smaller. UI/diagram content → PNG wins; photo content →
   JPEG wins. WebP is optional; JPEG is universally accepted by both
   target providers (see §4.10).
4. **Size guard**: if the chosen encoding still exceeds 4.5 MB base64
   (~3.4 MB binary), step JPEG quality 80 → 70 → 55 → 40 and, if needed,
   shrink dimensions by 25% each pass, down to 1×1. In practice a
   2000×2000 JPEG never needs this, so it is a safety net.
5. **Pass-through**: if the source is already ≤2000×2000 **and** its
   base64 size is under budget, send the original bytes untouched
   (matches pi — no needless re-encode).

### 4.10 Provider Scope: DeepSeek + Local Qwen

The vision path must work against both:

- **DeepSeek** (`deepseek-v4-flash-vision-exp` over `api.deepseek.com`).
  DeepSeek does not serve the OpenAI Responses API; the `model` binary
  falls back to Chat Completions after a 404/405 on `/v1/responses`.
  So the Chat Completions user-message path (§4.3 step 3) is the
  effective wire form. Verify the vision model accepts `image_url`
  parts with a minimal request before wiring the full path.
- **Local Qwen3.8** (`Qwen3.8-27B-NVFP4` over the vLLM/SGLang server at
  `127.0.0.1:30000`). Confirm the serving stack exposes vision input and
  which API shape it expects (Responses `input_image` vs. Chat
  Completions `image_url`).

The Chat Completions `image_url` shape with a `data:`-URL base64
payload is the portable path for both providers. The Responses array-
`output` shape is used when the provider supports it; a per-model
`vision` flag (§4.5) plus, if needed, a per-model `image_input` shape
flag selects the wire form.

Provider compatibility notes (from pi's compat layer):

- Some providers reject a user message directly after tool results;
  pi inserts a synthetic assistant message
  ("I have processed the tool results.") to bridge the gap. Keep this
  bridge in the chat-completions converter when the provider needs it
  (flag it per model in `ModelSettings`).
- The `detail` field on `input_image` is OpenAI-Responses-specific.
  Omit it on DeepSeek and on the local Qwen server.

### 4.11 Storage and Log Growth

The tool log stores the read tool's full stdout, including the base64
image (up to ~6 MB per read). `value.details` in the event log carries
the same payload. This mirrors pi, which stores full base64
`ImageContent` blocks in its session JSONL and bounds growth via
compaction.

- Keep the inline base64 in the tool log (like pi). A file reference
  adds a read-at-request-time dependency and a lifecycle problem
  (when are the files cleaned up?).
- Bound the growth: compaction drops old images (§4.3 step 4), the
  total image budget caps the active context (§4.3 step 5), and
  `block_images` is the kill switch (§4.5).

## 5. Implementation Order

| Step | File(s) | Effort |
|------|---------|--------|
| 1. Read tool: MIME detection + EXIF orientation + resize + base64 output | `tools/read/src/main.rs`, `tools/read/Cargo.toml` | Medium |
| 2. Route: no structural change (verify) | `bin/route/src/main.rs` | Trivial |
| 3. Assemble: `Ev::ToolResult` image field; array `output` (Responses); user-message images (chat); compact drops images; budget + `block_images` | `bin/assemble/src/main.rs` | Medium |
| 4. Model: chat-completions user-message image conversion | `bin/model/src/main.rs` | Small |
| 5. Model settings: `vision` capability flag | `crates/rushi/src/model_settings.rs` | Small |
| 6. Limits: `image_budget_bytes`, `block_images` | `config-real.toml`, limits plumbing | Trivial |
| 7. Tool description update | `tools/read/tool.toml` | Trivial |
| 8. Tests | See §6 | Medium |
| 9. Config: add `vision = true` to the vision model section | `config-real.toml` | Trivial |

## 6. Test Plan

- **Unit (read tool)**:
  - PNG / JPEG / GIF / WebP / BMP files are detected and emitted with
    correct `mime_type`, `data`, `width`, `height`.
  - Animated PNG (acTL) is rejected with a clear error.
  - Animated GIF and WebP are accepted; the first frame is used and the
    text note says so.
  - A JPEG with EXIF orientation 6 (phone portrait photo) is rotated to
    upright before resize (mirror pi's EXIF test vectors).
  - A truncated PNG (passes the magic-byte sniff, fails to decode)
    produces the decode-error text with no `data` field.
  - Non-image binary (ELF, tar) still produces the "not UTF-8" error.
  - A 5000×5000 PNG is resized to 2000×2000.
  - A 10 MB JPEG is re-encoded below 4.5 MB base64.
  - A file over 20 MB is rejected with the size-cap error.
  - A small UI PNG (already under 2000×2000 and under budget) passes
    through without re-encoding (byte-identical data).
  - Text files are unaffected (regression).

- **Integration (assemble)**:
  - A `tool_result` with image details produces a `function_call_output`
    with an **array** `output` (`input_text` + `input_image`) in the
    Responses request when `vision = true`.
  - A non-vision model produces the plain string `output` with the
    "does not support" note and no image parts.
  - The chat-completions conversion emits a text-only `role: "tool"`
    message followed by a `role: "user"` message with `image_url`
    parts.
  - A compacted (old) tool result drops the image and carries the
    `[image omitted: read image file <path>]` note.
  - A context over `image_budget_bytes` drops the oldest image items.
  - `block_images = true` strips every image from the request.
  - `estimate_ev_tokens` adds 4800 chars per image-carrying event.

- **E2E**:
  - Run a session where the model calls `read` on a PNG screenshot.
  - Verify the model receives the image and can describe its contents.
  - Verify the context-budget estimate accounts for the image tokens.


