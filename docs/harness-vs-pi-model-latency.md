# Harness vs pi: model-interaction latency (2026-08-31)

Source data:
- Harness: `sessions/better-ui-colors_h1/events.jsonl` (1441 events at analysis time, loop still live at 15:09Z).
- pi: `~/.pi/agent/sessions/--home-tony-programming-rust-unix-harness--/*.jsonl` (this directory).
- Repro scripts: `scripts/timestamp_compare.py`, `scripts/band_match.py`.
- Both clients hit the same backend: sglang @ `127.0.0.1:30000`, model `Qwen3.8-27B-NVFP4-RTX5090-DSPARK` (262k ctx, fp8 KV, `max_running_requests=4`, fcfs, chunked prefill 2048). Confirmed in `config.toml` and `~/.pi/agent/models.json`.

## Timestamp semantics (verified in source)

- Harness `assistant_message.ts` = response completion. The `model` binary streams SSE to `response.completed`, then `parse` writes the event with `Utc::now()` at 1s resolution. `tool_result.ts` = tool completion.
- pi assistant message `timestamp` = request start (message object creation, ~6-12 ms after the prior event). `toolResult.timestamp` = tool completion, ms precision.
- pi `usage.input` is the UNCACHED delta. Full context = `input + cacheRead + cacheWrite`. Confirmed in pi source: `promptTokens = input + cacheRead + cacheWrite`.
- h1 `usage.input_tokens` is the full prompt (cache included, per `docs/loop-and-edit-implementation.md`).
- Consequence: harness gap `ts(assistant_i) - ts(last tool_result)` is clean model time. pi model time = `ts(last toolResult) - ts(assistant) - tool_exec` (Agent tools: `details.durationMs`; fast tools: 0.3s allowance; bash groups excluded; `ask_user_question` turns excluded).
- h1 `usage.output_tokens` does not match reasoning text length (e.g. out=5474 with 39222 reasoning chars). It likely excludes most reasoning tokens. pi sums `output + reasoning`.

## What h1 shows

- Active window 12:41:08 -> 15:09Z+ (2h28min, still running). 426 assistant turns.
- Context regime: median input 43.5k, p90 53.8k (the 55k `context_budget_tokens` cap).
- Time partition: model time 138-140 min = 96% of active wall time. Tool exec total 0.1 min (every call 0-1s). User think 3.3 min. The loop is model-bound.
- Model round-trip: median 9s, p90 50s, p99 115s, max 242s. Latency floor (out<300 tokens): 3.5-5s.
- Prefix cache hits by hour: 63% (12h) -> 71% (13h) -> 84% (14h) -> 94% (15h).
- Context reset at 14:12:22 (user turn input 26819, down from 55866): auto-compact or handoff rewrote the prefix.

## What pi shows (this directory, same model DSPARK)

- 4851 assistant turns. Correct context = input + cacheRead + cacheWrite.
- Context regime: median 120k, p90 222k, 82.4% of turns at 60k+. pi in this repo runs large-context. The earlier "median 362 input" was the uncached delta, not the context.
- Prefix cache hits: median 99% at 40-60k, 100% overall.
- Model time in h1's operating band (40-60k): 40-50k median 4.6s p90 22.8s; 50-60k median 6.0s p90 18.2s.
- Time-matched sample: pi 01a057dd (ran 12:50-14:45, overlapping h1 on the same GPU) 40-60k turns: median 4.3s (n=4).
- Per-day 40-60k tok/s: 126.8 (08-27), 114.8 (08-28), 101.6 (08-29), 105.7 (08-30), 78.7 (08-31, the day h1 ran).

## Band-matched comparison (40-60k context)

h1 (n=265 turns): median 8.5-18s, p90 50.8-61.8s. pi this dir (n=140): median 4.6-6.0s, p90 18.2-22.8s. h1 is 1.8-3x slower in this band.

Matched on both context and true output length (h1 out + estimated reasoning tokens, pi out + reasoning):

| true out | h1 med gap | pi med gap | h1 tok/s | pi tok/s |
|---|---|---|---|---|
| <300 | 7.0s | 6.9s | 41.1 | 21.1 |
| 300-800 | 10.0s | 5.9s | 43.3 | 77.3 |
| 800-1500 | 8.0s | 9.2s | 128.4 | 123.8 |
| 1500-3000 | 15.0s | 17.3s | 139.7 | 122.2 |
| 3000+ | 41.5s | 22.3s | 193.1 | 196.9 |

Generation speed is equal at long outputs (both ~195 tok/s at 3000+ tokens). h1 loses time at shorter outputs (300-800: 1.7x slower) where prefill dominates.

## Findings

1. The harness client adds no visible per-request overhead. Latency floors match (h1 3.5-5s vs pi 3.4s). Generation speed matches pi at equal context and equal output length (~195 tok/s).
2. The harness is slower per model interaction at equal context (40-60k: median 8.5-18s vs 4.6-6.0s). Even time-matched on the same GPU window, pi 01a057dd reached 4.3s median where h1 took 8-18s. The user's doubt holds at the round-trip level.
3. The mechanism is prefix-cache misses, not slow generation:
   - h1 cache hits: 63-94% by hour. pi this dir: 99-100%.
   - Each h1 miss re-prefills up to 25-37% of a 40-55k prefix (10-20k tokens).
   - Correlating factors: cold start of the session (12h at 63%), two concurrent large-context pi sessions (01a0572a to 14:12, 01a057dd to 14:45; sglang KV pressure with 4 running slots and fp8 KV), and the 14:12 prefix rewrite from auto-compact or handoff.
   - Hits rose to 94% at 15h after the pi sessions ended and h1 held a stable prefix.
4. The initial report draft claimed pi runs at "median 362 input". That was a misread of `usage.input` (uncached delta). The corrected context distribution: pi median 120k. The earlier conclusion "harness is not slower" was computed on that misread banding and is withdrawn.

## Caveats

- h1 timestamps are 1s resolution (truncated); gaps carry +/-1s error.
- pi model time is an estimate: fast-tool exec allowance 0.3s; bash turns excluded; `ask_user_question` turns excluded.
- h1 `usage.output_tokens` likely excludes reasoning tokens (out=5474 vs 39222 reasoning chars in one turn). h1 tok/s figures in the unmatched tables understate h1. The matched table uses estimated true output length.
- pi 01a057dd time-matched sample is n=4 at 40-60k.
- pi data spans 08-27..08-31. The day-matched 08-31 comparison is the fairest: pi 78.7 tok/s vs h1 56.6 tok/s (h1 numerator understated).

## Recommendations

- Track sglang `cache_hit_rate` for the harness loop as a latency SLO signal. Sustained sub-80% hits add real prefill cost.
- Do not run large-context pi sessions and the harness loop at the same time on one GPU, or raise KV capacity.
- Verify the auto-compact path keeps the request prefix byte-stable. A prefix rewrite costs a full re-prefill (the 14:12 reset shows input dropping to 26819).
- Each cache miss at the 55k budget is expensive (10-20k uncached tokens). A tighter budget reduces the per-miss cost.
