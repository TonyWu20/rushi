# Image Read — the `img` Extension Tool (Option B)

Status: Implemented (2026-10-10).

## 1. The Decision

The extension side of `image-read-kiss.md` stayed unbuilt. The
CV-adjustment session (2026-10-10) hit the gap: a bash-only native
tool set, a `vision = true` model, and no image input path. The
agent had to measure the layout through SVG coordinates and pixel
ink profiles instead of seeing the render.

Option B was chosen over the KISS hook design: image reading is a
standalone extension **tool** (`img`), not a `tool.after` hook.

- Smallest surface. One ext repo and one exts-table row. No native
  `read` tool, no hook window wiring.
- Explicit and discoverable. The model calls `img <path>`. The tool
  description in the generated tool list says when to use it.
- No kernel change. The ext tool JSON stdout lands in
  `value.details` (bin/route structured_output). The assemble stage
  carries a `type = "image"` details object into an `input_image`
  part when the model `vision` flag is on (bin/assemble full_items).

Deviation from `image-read-kiss.md`: the image pipeline is an
extension capability in tool form. The KISS hook ABI (`tool.after`
results splice + `hook_io`) stays available for future image hooks.

## 2. The Tool

Repo `rushi-image-read` (sibling of the other ext repos), Mode-B
flake, one package: `packages.<system>.img`.

Arguments (one JSON object on stdin, per the ext tool ABI):

| field      | type   | default | meaning |
|---|---|---|---|
| `path` | string | required | image file; relative paths resolve against the session cwd |
| `max_edge` | int | 2048 | longest-edge cap in pixels (sanity cap 16384) |
| `target_kb` | int | 4096 | target cap in KB for the encoded bytes |

Behavior:

- Formats: png, jpg, gif (first frame), webp, bmp.
- Animated PNG is rejected (image-read-plan.md section 7).
- 10 MB input cap (image-read-kiss.md section 5).
- 3-step compression budget: preferred format at the resized edge,
  then JPEG at half edge, then JPEG at quarter edge. A result still
  over the target is emitted with a note. The flat 4800-char image
  estimate plus compaction is the token-budget safeline.
- Success: one JSON object on stdout
  `{"type":"image","mime_type":"...","data":"<base64>","path":"...","note":"..."}`.
- Failure: a message on stderr and a non-zero exit.

Known v1 limits: no EXIF orientation (screenshots carry none).
GIF and WebP pass the first frame only.

## 3. Verification (2026-10-10)

- Tool level: a 1240x1754 CV screenshot. Default args encode 807 KB
  PNG at step 1. `target_kb = 300` lands at 142 KB JPEG step 2.
  `target_kb = 64` lands at 38 KB JPEG step 3. A text file, an
  11 MB file, and a missing `path` all fail with a stderr message
  and exit 1.
- Kernel e2e (route + assemble, vision on): the request `tools` list
  carries `img`. The `function_call_output` carries an `input_image`
  part `data:image/jpeg;base64,...`. With `vision = false` the
  output stays a string and ends with
  `[Image omitted: model does not support image input.]`.
- Model: a curl to `127.0.0.1:30000/v1/responses` with the img
  payload. The local Qwen model describes the CV page correctly.
  This confirms the earlier curl + base64 vision check.

## 4. Wiring

rushi-config bundles the tool. The input `rushi-image-read` is a
local path pin. Flip it to `github:TonyWu20/rushi-image-read` at
hosting time. The `imgFlake` row lands in the exts table
(`config/exts.nix` extTools). `rushi-full` bundles `tools/img`, and
the generated config carries the img store path in
`extension_tool_paths`.
