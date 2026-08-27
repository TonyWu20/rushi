# Root cause: `model returned an empty turn after retries`

Date: 2026-08-27. Session: `sessions/tui-test`.

## Symptom

`sessions/tui-test/events.jsonl` contains two error events:

- 12:09:02 — `model returned an empty turn after retries`
- 12:34:15 — `model returned an empty turn after retries`

Each error follows the last `tool_result` by about 91 seconds. That gap is
exactly three model retries at 30.3 seconds each. The loop stops after the
error (claim reports `idle` on an `error` event).

The active model is a thinking model (Qwen3.8-27B, SGLang backend,
`/v1/responses` streaming, `reasoning effort medium`). Its responses stream
for 30-100 seconds depending on the thinking length.

## Evidence trail

1. **Repro.** Run the `model` binary on the 56k-token context from that
   session. About half the runs return
   `{"text":"","tool_calls":[],"stop_reason":"stop","usage":null}`.
   Successful runs finish in under 30 s. Failed runs stop at 30.3 s.
2. **The model did answer.** SGLang's response store
   (`GET /v1/responses/{id}`) shows the failed requests completed with
   full content: `reasoning` + `message` + `function_call` items and usage.
   The engine finished the work. The client received nothing.
3. **The real error.** Capture `resp.text()`'s `Result` instead of
   `unwrap_or_default()`: every "empty" run fails with
   `reqwest::Error { kind: Decode, source: TimedOut }`.
4. **The 30 s cap.** `reqwest 0.13.4`, `blocking/client.rs`:
   `Timeout::default()` is `Some(30 s)`. `Client::new()` installs it as the
   overall request timeout. `Response::text()` applies it to the whole body
   read (`wait::timeout(self.inner.text(), self.timeout)`). A streaming SSE
   body that runs past 30 s fails the read with a decode timeout.
5. **Silent swallow.** The old code did
   `let body = resp.text().unwrap_or_default();` — the timeout error became
   an empty string, and `parse_sse_response("")` produced a clean empty
   `stop` turn. No error surfaced anywhere.
6. **Misclassification.** `scripts/step.sh` treats a turn with no text and
   no tool calls as a model glitch: retry 3 times, then log
   `model returned an empty turn after retries` and stop. All three
   retries hit the same 30 s cap, so the loop always died with that
   message.

## Why pi does not report this

- pi streams with its own (longer) timeouts, so a 40-100 s thinking
  response completes in pi.
- pi uses the chat completions API for OpenAI-compatible providers, not
  the responses API.
- Even a genuine empty `stop` turn is a normal idle in pi. pi retries
  only retryable API errors (429/5xx/timeout), never an empty model
  answer.

The bug is specific to this harness: a 30 s client-side cap on a stream
that runs for minutes, plus a silent error swallow, plus a loop guard
that turns the result into a fatal "model glitch".

## Fix

| File | Change |
| --- | --- |
| `bin/model/src/main.rs` | Build the client with `.timeout(None)`; bound only connect (30 s) and pool idle (60 s). A failed body read now returns an error instead of an empty string. `parse_sse_response` tracks terminal events: a stream without `response.completed/incomplete/failed` reports `stop_reason "error"` with a `detail`. |
| `bin/parse/src/main.rs` | Pass the model binary's `detail` through into the error event message. |
| `scripts/step.sh` | Transport-level model failures (`stop_reason "error"`) retry up to 2 times with 3 s backoff, then log the detail and stop. A `length`-stopped empty turn (thinking consumed `max_output_tokens`) logs `model output budget exhausted` instead of burning empty-turn retries. Genuine empty `stop` turns keep the existing policy. |

Regression tests in `bin/model/src/main.rs` (`cargo test -p model`):
complete stream, cut stream, empty body, incomplete stream, failed stream.

## Verification

- 43 s thinking turn now completes with content, tool call, and usage
  (previously died at 30.3 s with an empty turn).
- Fake `/v1/responses` server, three modes, driven through the real
  `step.sh`: truncated stream logs the truncation detail; a normal turn
  logs the assistant message; a genuine empty `stop` turn logs
  `model returned an empty turn after retries` (policy preserved).
- `cargo test -p model -p parse`: 5 passed, 0 failed.

## Notes for the operator

- If a real network failure stops the loop, the logged error now says so
  and carries the detail. Retry by sending another user message.
- SGLang keeps generating after the client gives up; the stored response
  at `GET /v1/responses/{id}` is the ground truth for what the model
  produced.
- Run one harness loop at a time per session. Concurrent heavy loops on
  the same backend add KV pressure and slow each stream (the server has
  `max_running_requests = 4`).
