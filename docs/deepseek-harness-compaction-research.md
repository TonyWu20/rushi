# How deepseek-harness Compacts While Keeping the Session Log Append-Only

## Core Idea

The harness achieves compaction without ever rewriting or deleting the append-only session log by maintaining a separate **surface projection** layer. The raw log is strictly append-only and immutable. Compaction appends new events (`compaction/start`, `compaction/summary`, a `user/message` with a `replace` surface op, `compaction/end`) that *shadow* older surface nodes without removing them from the log. The model-facing message history is derived from the surface, not the log, so shadowed events disappear from the next LLM request while the log retains full fidelity.

The KV-cache benefit comes from two mechanisms:
1. **Stable request prefix** — the system prompt and tool schemas are logged as `request/header` events and only re-logged when they actually change (`headerEquals` guard). This keeps the longest prefix of every LLM request byte-identical across turns.
2. **Prefix-aligned summarization call** — the summarizer reuses the conversation's own system prompt, tool schemas, and shadowed-region messages as the prompt for the compaction LLM call. Since the shadowed region is the *oldest* part of the surface, the summarization request is a genuine prefix of the most recent full request, so the provider's KV cache is hit for the entire shadowed region. Only the compaction instruction and the summary output are novel tokens.

---

## 1. Append-Only Log (`Session`)

File: `packages/core/session/src/index.ts`

### 1.1 The log is append-only

`Session` is an event-sourced store. Every event is appended in order. Sequence numbers are contiguous from 0. Events are deep-frozen on append (`deepFreeze`), making them immutable. There is no `delete`, `edit`, or `replace` on the log itself.

```ts
// Session.append() — the only write path
append<T extends SessionEventType>(
  type: T, data: SessionEventMap[T], ...opts: ...
): SessionEvent<T>
```

- `seq` is always `this.log.length` — monotonic, contiguous.
- `time` is `Date.now()` — stamped at append time.
- Data is snapshotted via `snapshotJsonValue` and deep-frozen before entering the log.
- A `session/event` observer is notified after commit. Observer failures are contained. They do not roll back the append.

### 1.2 `session.events` is a frozen snapshot

`session.events` returns a frozen copy of the log. It is invalidated (and re-frozen) on every append. No consumer can mutate it.

### 1.3 `session.requestHeader()`

Returns the latest `request/header` snapshot folded from the log. This is the canonical representation of the request envelope (system prompt, tools, provider/model config, adapter defaults). It is used to build the LLM request prefix.

---

## 2. Surface Projection (`SurfaceManager`)

File: `packages/core/session/src/surface.ts`

### 2.1 What the surface is

The surface is an ordered array of event seqs (`nodes: number[]`) representing which log events are model-visible. It is derived from the log via a fold.

- **`append` op**: A new surface-eligible event (`user/message`, `assistant/message`, `tool/result`) is appended to the tail of `nodes`.
- **`replace` op**: A new event carries `surfaceOp: { op: 'replace', start, end }`, which splices `nodes[startIdx..endIdx]` and replaces that range with the new event's seq. The original events remain in the log but are no longer in the surface.

```ts
// surface.ts — applySurfacePlan
if (op === 'append') {
  state.nodes.push(plan.seq)
} else if (op === 'replace') {
  state.nodes.splice(plan.startIdx, plan.endIdx - plan.startIdx + 1, plan.seq)
  state.replaceGeneration += 1
}
```

### 2.2 `SurfaceOp` type

```ts
type SurfaceOp =
  | 'append'
  | { op: 'replace'; start: number; end: number }
```

- `'append'` — normal path for user/assistant/tool messages.
- `{ op: 'replace', start, end }` — replaces surface nodes from `start` (inclusive) through `end` (inclusive) with this node. Both must exist as current surface nodes. The node's `sourceEventSeqs` must include every shadowed surface node.

### 2.3 `replaceGeneration` counter

Increments on every `replace` operation. Used by `deriveMessages()` to detect when the derived-message cache needs rebuilding.

### 2.4 `deriveMessages()`

File: `packages/core/session/src/index.ts`

Projects the surface nodes into LLM messages:

```ts
deriveMessages(): Message[] {
  const surface = this.surface
  const nodes = surface.nodes
  const generation = surface.replaceGeneration
  if (generation !== this.derivedGeneration) {
    this.derived = []
    this.derivedNodes = 0
    this.derivedGeneration = generation
  }
  for (const seq of nodes.slice(this.derivedNodes)) {
    const msg = this.deriveEventMessage(this.log[seq])
    if (msg) this.derived.push(msg)
  }
  return [...this.derived]
}
```

- Only surface-eligible event types produce messages: `user/message`, `assistant/message`, `tool/result`.
- All other events (turn/step boundaries, chunks, usage, log-only records like `compaction/*`) produce no message.
- The result is a fresh array of frozen, shared `Message` objects.

**Key point**: After a compaction `replace`, the shadowed events are removed from `surface.nodes`, so `deriveMessages()` skips them. The model never sees them again. But they remain in `session.events` (the raw log) and are recoverable by any consumer that reads the log directly.

---

## 3. Compaction Transaction

Files:
- `packages/compaction/compaction-basic/src/index.ts` (`BasicCompactionEngine`)
- `packages/compaction/compaction-basic/src/region.ts` (`compactSurfaceRegion`)
- `packages/compaction/compaction-basic/src/summarizer.ts` (`summarizeWithLlm`, `COMPACTION_INSTRUCTION`, `frameSummary`)
- `packages/compaction/compaction-basic/src/config.ts` (thresholds, retention)
- `packages/compaction/compaction/src/types.ts` (event types, `CompactionResult`)

### 3.1 Triggers

Two triggers, both handled in `BasicCompactionEngine.compactIfNeeded()`:

| Trigger | When | Policy |
|---|---|---|
| `pressure` | At every `agent/pre-step` when `totalTokens >= thresholdTokens` | Normal threshold + retain-tail policy |
| `context-overflow` | On `agent/request-error` with code `CONTEXT_WINDOW_EXCEEDED` | Bypasses the threshold. It forces one maximal balanced reduction |

Both are registered via `ctx.on('agent/pre-step', ...)` and `ctx.on('agent/request-error', ...)` when `auto: true` (default).

### 3.2 The compaction event sequence

`compactSurfaceRegion` (in `region.ts`) appends exactly four events in order:

```
1. compaction/start    (log-only, acquires lock)
2. compaction/summary  (log-only, records summary + shadowed range + token count)
3. user/message        (surface op: replace [start..end], carries the summary)
4. compaction/end      (log-only, releases lock)
```

Step 3 is the only surface mutation. The `compaction/start` and `compaction/end` are log-only events that carry no `surfaceOp`, so they never appear on the surface. The `compaction/summary` is also log-only.

### 3.3 Lock and crash safety

- `compaction/start` is appended **synchronously before** the async summarization call. This makes it the durable lock.
- If the process crashes between start and end, the log contains a `compaction/start` with no matching `compaction/end` — a detectable orphan that blocks further compaction until the next `session/end-seed` boundary proves it stale.
- A failed compaction (summary error, surface changed) still appends a `compaction/end` with the error, closing the bracket. The shadowed events are not replaced.

### 3.4 Range selection (`selectCompactableRange`)

- Works from the **tail** of the surface: walks backwards accumulating tokens until the `retainTokens` budget is met.
- The compaction range is `[surfaceNodes[0], surfaceNodes[keepFromIdx - 1]]` — i.e., the oldest messages.
- `toolPairingBalancedBefore` ensures the start boundary does not split an assistant tool-call from its result.
- Returns `null` if the entire surface is within the retain budget.

### 3.5 Two-stage optimization

Before summarization, if `ctx.toolResultPruner` is loaded:
1. **Model-free pruning** (`ToolResultPruner.pruneSession`): Trims oversized tool-result text using head/middle/tail character budgets. This is a pure string operation — no LLM call. Each pruned result is replaced via a `surfaceOp: replace` on the `tool/result` event, with a `compaction/prune` shadow-price event for token accounting.
2. **LLM summarization** (only if still over threshold): Calls `summarizeWithLlm` with the shadowed region.

### 3.6 The summary replacement

The summary is framed as:

```
<preamble>
<compacted-summary>
  <summary content>
</compacted-summary>
```

And appended as a `user/message` with:
```ts
surfaceOp: { op: 'replace', start, end },
sourceEventSeqs: [startEvent.seq, summaryEvent.seq, ...shadowedSeqs]
```

The `sourceEventSeqs` field records the provenance — which log events this checkpoint node replaced. This is how replay can reconstruct the original conversation from the log.

### 3.7 Shrink guarantee

`summarizeCompaction` rejects the summary if the framed checkpoint's estimated token count is >= the shadowed content's token count. The summary must actually reduce the surface token total.

---

## 4. KV Cache / Prefix Stability

This is the core of the "maximize cache prefix length" design.

### 4.1 Stable request header

File: `packages/core/agent-loop/src/agent.ts` — `buildRequest()`

```ts
const header = canonicalHeader({
  config,
  ...preparedCall?.adapterDefaults ? { adapterDefaults: preparedCall.adapterDefaults } : {},
  ...system ? { system } : {},
  ...tools.length > 0 ? { tools } : {},
})
const baseline = this.session.requestHeader()
if (!this.requestHeaderLogged) {
  this.session.append('request/header', { header, reason: 'initial' })
  this.requestHeaderLogged = true
} else if (baseline === undefined || !headerEquals(baseline, header)) {
  this.session.append('request/header', { header, reason: 'change' })
}
```

- `headerEquals` compares config, adapterDefaults, system prompt, and tool schemas field-by-field (tools compared by JSON.stringify order).
- A new `request/header` event is logged **only when the header actually changes**.
- The header is folded via `foldRequestHeader` to get the canonical prefix for the next request.
- This means the system prompt and tool schemas — the largest and most stable part of every request — are sent byte-identically unless they genuinely change.

**KV cache implication**: The provider's prefix cache is keyed on the request prefix. If the system prompt and tools are identical across turns, the provider's KV cache for that prefix is warm, and subsequent requests hit it instead of recomputing attention for those tokens.

### 4.2 Prefix-aligned summarization call

File: `packages/compaction/compaction-basic/src/region.ts` — `buildSummarizationInput()`

```ts
function buildSummarizationInput(session: Session, shadowedSeqs: readonly number[]) {
  const header = session.requestHeader()
  const events = session.events
  const regionMessages = shadowedSeqs
    .map(seq => session.deriveEventMessage(events[seq]))
    .filter((message): message is Message => message !== null)
  return {
    ...header?.system === undefined ? {} : { system: header.system },
    ...header?.tools === undefined ? {} : { tools: header.tools },
    messages: regionMessages,
  }
}
```

The summarizer call (in `summarizer.ts`) builds:

```ts
const messages: Message[] = [
  ...input.messages,   // shadowed region messages (oldest part of surface)
  createUserMessage({ content: COMPACTION_INSTRUCTION, ... }),
]
const options: GenerateOptions = {
  provider, model, messages,
  ...input.system ? { system: input.system } : {},
  ...input.tools ? { tools: input.tools } : {},
  ...
}
```

**Why this is a prefix of the conversation's last request**:

The shadowed region is the **oldest** portion of the surface (selected by `selectCompactableRange`, which compacts from the head). The last routed LLM request before compaction contained:

```
[system, tools, m1, m2, ..., mK, mK+1, ..., mN]
```

The summarization request is:

```
[system, tools, m1, m2, ..., mK, compaction_instruction]
```

Since `m1..mK` are the oldest messages and the summarization instruction is appended after them, the summarization request's prompt is a **prefix** of the last full request (up to the last shadowed message). The provider's KV cache, which cached the full last request, has already computed attention for `[system, tools, m1...mK]`. The summarization call reuses that cache. Only the compaction instruction and the generated summary are novel.

The code comment in `region.ts` states this explicitly:

> "Reconstruct the last routed request's cacheable prefix for the shadowed region: its system prompt and tool schemas, then the region's own derived messages in surface order. The summarizer appends only the compaction instruction after this, so the call is a genuine prefix of the conversation and reuses the provider's KV cache."

### 4.3 Post-compaction request

After compaction, the surface is:

```
[checkpoint_message (summary), mK+1, mK+2, ..., mN, ...new messages...]
```

The next LLM request is:

```
[system, tools, checkpoint_message, mK+1, ..., mN, new messages...]
```

- The `[system, tools]` prefix is unchanged → cache hit on that prefix.
- The checkpoint message is new (the summary), so everything after it is recomputed.
- But the total request is much shorter than before compaction, so the absolute cost of the "cold" portion is reduced.

### 4.4 The e2e proof

File: `packages/core/agent-loop/tests/request-cache.e2e.ts`

This test runs a real multi-turn conversation against the DeepSeek API and asserts:

```ts
for (const usage of usages.slice(1)) {
  expect(usage.cacheReadTokens ?? 0).toBeGreaterThan(0)
}
```

Every request after the first must report `cacheReadTokens > 0` (the provider's `prompt_cache_hit_tokens` mapped to `cacheReadTokens`). This is the production-level verification that the prefix stability mechanism actually produces KV cache hits.

The test uses a sufficiently long system prompt so the shared prefix spans the provider's cache-block granularity (64 tokens) from the very first request.

### 4.5 Token accounting via shadow-price protocol

File: `packages/llm/token-meter/src/surface-fold.ts`

`foldSurfaceTokens` tracks per-node token prices on the surface. When a `replace` op lands:

```ts
const removed = nodes.slice(startIdx, endIdx + 1)
  .reduce((total, node) => total + node.tokens, 0)
next.splice(startIdx, endIdx - startIdx + 1, { seq: event.seq, tokens })
return { tokens, nodes: next, deltaTokens: tokens - removed }
```

The `compaction/summary` and `compaction/prune` events carry `shadowedTokenCount`, so a pure consumer can subtract the shadowed price without retaining per-node state. This keeps the token meter's O(1) checkpoint state consistent with the surface.

---

## 5. Tool-Result Pruning (Model-Free Compaction)

File: `packages/compaction/compaction-tool-result-pruner/src/index.ts`

`ToolResultPruner.pruneSession()` walks the current surface, finds `tool/result` nodes whose text content exceeds `thresholdChars`, and prunes them:

- Retains `headChars` from the start and `tailChars` from the end of the text.
- Inserts a `PRUNE_MARKER` (e.g., `... [N chars omitted] ...`) in the middle.
- Replaces the original `tool/result` with a new one via `surfaceOp: replace`, preserving the `callId` and all non-content fields.
- Appends a `compaction/prune` shadow-price event before each replacement.

This is a deterministic, model-free optimization that can reduce the surface token count without any LLM call. It runs before summarization in the two-stage pipeline.

---

## 6. Design Invariants and Invariants

| Invariant | How it's enforced |
|---|---|
| Log is append-only | `Session.append` is the only write path. Events are deep-frozen. No delete or edit API exists |
| Surface is a projection of the log | `SurfaceManager` folds events. A `replace` splices the projection, not the log |
| Shadowed events remain in the log | `sourceEventSeqs` on the replacement event records which log events are shadowed |
| Deterministic replay | `foldSurface` replays the log from scratch. It produces the same surface. `deriveEventMessage` is the canonical per-event projection |
| Compaction lock is durable | `compaction/start` is appended before async work. A crash leaves a detectable orphan |
| Summary must shrink | `summarizeCompaction` rejects if framed summary >= shadowed token count |
| Tool-pairing boundaries | `toolPairingBalancedBefore/After` reject ranges that split a tool call from its result |
| Request prefix stability | `headerEquals` guard prevents logging a new `request/header` unless the header changed |
| KV cache reuse for summarization | `buildSummarizationInput` reconstructs the exact prefix of the last routed request |

---

## 7. When Compaction Triggers and How the Count Is Known

### 7.1 The three trigger paths

The engine registers the automatic triggers in `_registerAutomaticCompaction()`.

- **Pre-step pressure.** The `agent/pre-step` hook runs before every model step. It calls `compactIfNeeded(agent, 'pressure', signal)`. This is the normal proactive path.
- **Context-overflow recovery.** The `agent/request-error` hook fires on the `CONTEXT_WINDOW_EXCEEDED` failure code. It calls `compactIfNeeded(agent, 'context-overflow', signal)`. It authorizes a retry only when the surface advanced. The retry cap is `maxOverflowRetries` (default 1). The counter resets when the agent idles or a response succeeds.
- **Manual command.** The `/compact` command calls `ctx.compaction.compactNow()`. It requires an idle agent with no queued work. It forces one useful reduction even below the threshold.

The pressure trigger asks two questions:

- Pressure test: is `totalTokens >= thresholdTokens`?
- Overflow test: did the provider report a context-window overflow?

Pruning may clear the pressure without a summary call.

### 7.2 How the token count is computed

The count is owned by the `TokenMeter` service. The harness injects it as `ctx.tokenMeter`. It is a replay-aware estimator, not a real tokenizer. Two parts combine:

1. **Heuristic per-node pricing.** Each surface message is priced with a fixed density. The density is 4 characters per token. Each block adds 4 tokens of structural overhead. Each message adds 4 tokens of role overhead.
2. **Provider-usage anchor.** An `assistant/message` with provider `usage` sets the anchor. The anchor total is `input + cacheRead + cacheWrite + output` tokens. If it meets the full heuristic estimate, the meter keeps the provider total. Otherwise the meter keeps the heuristic estimate.

On `measure()`:

- `totalTokens` = anchor total plus the growth since that anchor.
- The growth is the heuristic cost of newer surface nodes.
- A cold resume re-folds the log. The total stays exact.

### 7.3 How the threshold is computed

Pressure path:

- The meter resolves the routed model context window.
- `thresholdTokens = floor(contextWindow x thresholdRatio)`. The default ratio is 0.8.
- The retain budget is `retainRatio` (default 0.16) or a fixed `retainTokens`.
- Below the threshold, no compaction runs.

Overflow path:

- The overflow path skips the threshold check.
- It forces one maximal balanced reduction with `retainTokens = 0`.

### 7.4 Why the triggers keep the cache hot

- The header is re-logged only when it changes.
- `headerEquals` compares config, system prompt, and tool schemas.
- So the request prefix stays byte-identical across turns.
- The summarizer reuses that same prefix for its own call.
- The summarizer call is a prefix of the last routed request.
- So the provider reuses the warm cache for the shared part.

---

## 8. Summary: How Append-Only + Compaction + KV Cache Coexist

```
┌─────────────────────────────────────────────────────────────────────┐
│  SESSION LOG (append-only, immutable)                               │
│  [0] user/message "hello"                                           │
│  [1] assistant/message "hi"                                         │
│  [2] tool/call ...                                                  │
│  [3] tool/result ...                                                │
│  [4] user/message "do X"                                            │
│  [5] assistant/message ...                                          │
│  ...                                                                │
│  [N-4] compaction/start                                             │
│  [N-3] compaction/summary (summary text, shadowed range)            │
│  [N-2] user/message <checkpoint> surfaceOp: replace [0..N-5]       │
│  [N-1] compaction/end                                               │
│  [N] user/message "continue"                                        │
└─────────────────────────────────────────────────────────────────────┘
         │
         │  fold
         ▼
┌─────────────────────────────────────────────────────────────────────┐
│  SURFACE (model-visible, ordered projection)                        │
│  [N-2] checkpoint (replaces [0..N-5])                               │
│  [N]   user/message "continue"                                      │
└─────────────────────────────────────────────────────────────────────┘
         │
         │  deriveMessages()
         ▼
┌─────────────────────────────────────────────────────────────────────┐
│  LLM REQUEST                                                       │
│  system: <stable system prompt>          ← prefix cache hit        │
│  tools:  [<stable tool schemas>]         ← prefix cache hit        │
│  messages: [checkpoint, "continue"]      ← short, mostly cold     │
└─────────────────────────────────────────────────────────────────────┘
```

The log grows forever (append-only). The surface shrinks (replace ops shadow old nodes). The request prefix stays stable (system + tools unchanged). The summarization call itself is cache-hot because it reuses the conversation's own prefix. The result: the session log is a complete, durable, replayable record. The model sees a compacted, context-rich view. The provider's KV cache is maximally reused.
