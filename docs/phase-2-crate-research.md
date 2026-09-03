# Phase 2 Crate Research

Status: Research (2026-09-07).

This document maps shell and bash-tool work in the Phase 2 core to Rust
crates. It targets two goals:

- Performance: remove per-call process spawn cost from the hot path.
- Integrity: remove external-binary version drift and shell-quoting risk.

It feeds `docs/phase-2-plan.md`. It adds no new event type and no new config
key. It only changes the implementation underneath. It follows the YAGNI guard
in the plan section 9.

## 1. Scope

The Phase 2 plan moves the loop into a Rust binary named `harness`. It also
builds one shared utility crate named `harness-common`. This document lists the
crates that each part of the shell layer can hand off.

Two layers change:

- `harness-common`: pure logic. No I/O. Must stay wasm-safe for Phase 4.
- `bin/harness`: host process. May use Unix I/O, process spawn, and locks.

The `tools/*` binaries stay unchanged in Phase 2. Native tool candidates live
in a later stage. This document names them so the crate choice is ready.

## 2. Evidence: what the shell does today

This section records the current shell surface. It is the reason the crate
work pays off.

### 2.1 Shell glue in the loop scripts

| Capability | Current form | Location |
|---|---|---|
| Read one TOML key | `awk -F'"' '/^sessions_root=/{...}'` | `scripts/step.sh` line 6 |
| Read `[limits].compact_enabled` | `awk` section scan | `scripts/step.sh` line 99 |
| Read `model.NAME.max_output_tokens` | `awk` section scan | `scripts/step.sh` line 105 |
| Read `model.NAME.context_tokens` | `awk` section scan | `scripts/step.sh` line 113 |
| Read `[limits].context_budget_tokens` | `awk` section scan | `scripts/step.sh` line 122 |
| Compute input budget | `awk BEGIN{...}` | `scripts/step.sh` line 127 |
| Build an event object | `jq -cn --arg ...` | `scripts/step.sh` lines 56, 84 |
| Read claim state | `jq -r .state` | `scripts/step.sh` line 165 |
| Count pending follow-ups | `jq -r '.pending_follow_ups\|length'` | `scripts/step.sh` line 168 |
| Last `model_thinking` in 4096-line tail | `tail -n 4096 \| jq -rs` | `scripts/step.sh` line 81 |
| Rewrite claim tool calls | `jq -c '.pending_tool_calls[]\|...` | `scripts/step.sh` line 192 |
| Print transcript | `jq -s -f transcript.jq` | `scripts/turn.sh` line 32 |
| UTC timestamp | `date -u +%Y-%m-%dT%H:%M:%SZ` | `scripts/step.sh` lines 54, 84, 285 |
| Scratch dir + cleanup | `mktemp -d`, `trap rm -rf` | `scripts/step.sh` lines 18, 20 |

The `awk` scrapes are the highest-integrity risk. Finding R6 in
`docs/phase-2-readiness.md` names the exact failure: one misread model section
shifts the input budget and every compact threshold. A typed TOML read fixes
that class of bug.

### 2.2 Overflow classifier

`scripts/overflow-classify.sh` holds two ERE tables:

- 5 exclusion patterns (`OVERRIDE_OVERFLOW_EXCLUDES`).
- 25 overflow patterns (`OVERRIDE_OVERFLOW_PATTERNS`).

The shell runs `[[ $lower =~ $pat ]]` per pattern. The plan already moves the
ten `--self-test` rows into unit tests. The regex engine should become the
`regex` crate. The plan section 4.4 keeps the tables and the exclusion-first
order.

### 2.3 Bash tool usage from recorded sessions

The `bash` tool is a general shell escape hatch. Recorded session logs show the
dominant command shapes. The counts are a floor, not a ceiling.

| Pattern | Typical command | Why it repeats |
|---|---|---|
| File search | `grep -n "sym" path`, `grep -rn "sym"` | Find a symbol before editing |
| Repo walk | `find . -name "x.rs"` | Locate a file by name |
| Line range | `sed -n '260,320p' file` | Read a region without a pager |
| Build + filter | `cargo test -p tui 2>&1 \| grep -E "..."` | Show only the failing tests |
| Git state | `git status`, `git log --oneline` | Check the tree |
| Ad-hoc script | `python3 - <<'EOF' ... EOF` | One-off data work |
| Env check | `which python3`, `echo $VAR` | Confirm the environment |

The top recorded command group is `grep` over the repo. The second group is
`cargo` filtered by `grep`. Both are good candidates for a native tool.

### 2.4 Token counting: the recorded user policy

The user policy: any character-based token estimate is foolish. Count the
tokens. The budget docs record it. Correction 62 removed the char mechanism:
"No char mechanism survives. No chars-per-token rate, no char budget knob,
no char estimate." Correction 57 records the measured failure mode: this
model runs about 8 chars per token, so a `chars / 4` estimate is off by a
factor of 2. The harness believed a wrong budget and compacted late
(FT-010).

The provider counts the tokens for us. Every model call returns
`usage.input_tokens`. `bin/model` already normalizes that field in both API
shapes (the Responses path and the Chat Completions fallback). `bin/assemble`
already decides the budget in token space from that measured value plus the
measured per-event growth. The char estimator is the leftover. `bin/compact` still projects the compact
candidate with `ns / 4` (`est_tokens`, line 299). Its mirror lives in
`bin/assemble`. The user policy states that any character-based token
estimate is foolish — just count the tokens. The plan section 4.3 says
"identical constants" and section 6 ports `compact_math` as the pure trigger
math. Read literally, that carries the char math into the Rust core. This
document reframes it: count tokens from the model server. Do not estimate.
The plan wording needs one sign-off to match this policy.

### 2.4.1 How to count tokens without estimating

The model server already returns `usage.input_tokens` on every response
(`bin/model/src/main.rs` line 614). Two sources cover every case:

- **Measured count.** After any real model call, read `usage.input_tokens`
  from the response JSON. `bin/model` already normalises this field for both
  the Responses API and the Chat-Completions fallback. The value lands in
  `compact.json` as `last_tokens`.
- **Projected count (probe).** For a compact candidate that has not been
  sent yet, the harness assembles the candidate request, sets
  `max_output_tokens = 1`, sends it through `bin/model`, and reads
  `usage.input_tokens` from the one-token response. The cost is one short
  call over a cached prefix — negligible compared to a full generation.

No crate is required for this. The counting happens on the server side.
The `compact_math` module stays pure: it receives a `usize` token count as
an input and performs the cut walk. No tokenizer crate is needed in the
default path. A local tokenizer (`tiktoken-rs` or HF `tokenizers`) remains
the offline fallback for unit tests, log replay, and the Phase 4 wasm
runner where no server is reachable. See § 7.

## 3. Recommendation by capability

Each row names the crate, the version line, and the crate layer it serves.
The "replaces" column names the shell or hand-rolled code it removes.

### 3.1 Headline crates

| Capability | Crate | Version line | Layer | Replaces |
|---|---|---|---|---|
| Line + regex search | `grep` facade, `grep-searcher`, `grep-regex` | `grep` 0.4.x | tool + common | `grep`, `rg`, `grep -n` |
| Directory walk + gitignore | `ignore` | 0.4.x | tool | `find` with ignore rules |
| JSON data tool | `jaq-core`, `jaq-stdlib`, `jaq-json` | `jaq-core` 3.x | tool | `jq`, `jq -rs` |
| JSON Schema validation | `jsonschema` | 0.53.x | common | hand-rolled validator copies |
| Regex table matching | `regex` | 1.x | common | bash `=~` on the classifier tables |
| TOML config read | `toml` | 1.x | common | four `awk` scrapes |
| UTC timestamp | `chrono` | 0.4.x | common | `date -u` |
| In-process diff | `similar` | 2.x | tool | `diff`, `git diff` read-only |
| File hash + encode | `sha2`, `base64`, `hex` | stable | tool | `sha256sum`, `base64` |
| Exact token counts | provider `usage` (measured) + 1-token probe (projected) | n/a (no crate) | common + harness | the char `ns / 4` estimator |

### 3.2 Supporting crates

| Capability | Crate | Layer | Note |
|---|---|---|---|
| Advisory file lock | `libc::flock` (already used) | harness | No new crate. Reuse the pattern from `bin/user/src/logline.rs`. |
| Process group + signal | `libc::setsid`, `libc::kill` | harness | Already used in `bin/tui/src/port_file.rs`. |
| Async runner (optional) | `tokio` | harness | Only if the `StageRunner` goes async. The plan keeps it sync. |
| Scratch dir | `tempfile` | harness | Replaces `mktemp -d` plus `trap rm -rf`. |
| Fast repo walk | `walkdir` or `jwalk` | tool | `ignore` covers the gitignore walk already. |
| Binary-safe text | `bstr` | tool | Search bytes before decoding UTF-8. |
| Typed config | `serde` + `serde_json` | common | Both are already workspace deps. |

### 3.3 Why the headline choices

`grep` and `grep-searcher` are the ripgrep engine as a library. They give
byte-oriented search, binary detection, and a parallel walker from `ignore`.
They match the top recorded bash-tool command. This is the performance win.

`jaq-core` is a jq clone as a library. The project is small and memory-safe.
It is audited by two penetration tests. It removes the `jq` process and the
system-package dependency. The `jaq-stdlib` crate carries the standard filters.
The `jaq-json` crate carries the value type. The `jaq-all` crate wraps all
three but changes fast. I recommend building on the three core crates so the
API surface is stable.

`jsonschema` removes the third validator copy. The plan section 6 says the
shared validator is a superset of the TUI copy. A spec-grade engine closes that
gap. It also keeps the "glob the schema dir" rule in the plan. New event
schemas still need zero code changes.

`regex` ports the overflow tables directly. The 30 patterns are plain EREs.
None use backreferences. The `regex` crate compiles each pattern once and
checks exclusion first. The unit tests pin the table.

`toml` replaces the four `awk` scrapes with one typed read. The read is
deterministic. The input budget math then runs in Rust with the same numbers.
The provider prefix cache still sees byte-identical requests.

### Token counting (replaces the char estimator)

The model server counts the tokens. Two sources feed the budget math. The
measured count is the `usage.input_tokens` of the last real call. `bin/model`
already emits that field in both API shapes. `compact.json` already stores it
as `last_tokens`. The projected count is a one-token probe. The harness
assembles the projected candidate, sets the output budget to 1 token, and
calls `model` through the existing stdin request path. It reads
`usage.input_tokens` from the output JSON. The probe is one call over a
cached prefix with a single output token. It is cheap. The `compact_math`
module stays pure. It consumes counts as inputs and owns no estimator. The
measured count and the probe are the only token sources. No crate is needed
for the default path. An in-process tokenizer is the offline fallback only,
see section 7.

## 4. Performance case

The loop calls `jq` and `tail` once per step for the thinking gate. That gate
runs every step, including idle ones. Each call forks and starts a process.
A `serde_json` read of the last 4096 lines runs in-process. It removes the
fork from the hot path.

The `grep` tool path forks a `grep` process per call. The `grep` crate runs
the search in the tool process. It uses the ripgrep search core. It also walks
the tree with `ignore` in parallel. On a repo of this size the in-process path
is faster than the shell path and does not spawn a child.

`jaq` startup cost is the known jq weakness. The jaq project notes a long jq
startup. Embedding `jaq-core` removes that cost. The tool compiles the filter
once and runs it in-process.

The `toml` read is a single parse. The `awk` path reads the file line by line
and tracks the section by hand. The typed parse is faster and safer. It also
fails fast on a bad key.

## 5. Integrity case

- One misread section no longer shifts the budget. A typed `toml` read
  returns a clear error on a missing key.
- The overflow table runs through one regex engine. The shell `=~` and the
  Rust engine agree on these plain EREs. The unit tests lock that in.
- The validator is spec-grade. It matches the schema draft, not a hand-built
  subset. The three copies collapse into one shared module.
- No external binary version drift. The tool uses the crate version in
  `Cargo.lock`. It does not depend on the user shell `jq` or `grep`.
- No shell-quoting surface. The tool builds a `Regex` or a `jaq` program
  from a string argument. It does not pass that string to `sh -c`.
- The trigger timing is exact. The char estimate runs off by up to 2x on
  the recorded model. The compact trigger now fires on the counted tokens,
  not on a rate.
- The byte output stays stable. The in-process math and the JSON build use
  the same code that the stage binaries use. The prefix cache stays warm.

## 6. Placement map

This map shows which crate lands where. It keeps the guardrail that
`harness-common` is pure and wasm-safe.

| Crate | `harness-common` | `bin/harness` | `tools/*` (later) |
|---|---|---|---|
| `toml` | yes | yes | no |
| `regex` | yes | yes | yes (search tool) |
| `serde_json` | yes | yes | yes |
| `chrono` | yes | yes | yes |
| `jsonschema` | yes | yes | no |
| `grep`, `grep-searcher`, `grep-regex` | no | no | yes (search tool) |
| `ignore` | no | no | yes (search tool) |
| `jaq-core`, `jaq-stdlib`, `jaq-json` | no | no | yes (json tool) |
| `similar` | no | no | yes (diff read) |
| `sha2`, `base64`, `hex` | no | no | yes |
| `libc::flock` | no | yes | no |
| `libc::setsid`, `libc::kill` | no | yes | no |
| `tempfile` | no | yes | no |
| Token counts | yes (the math consumes counts) | yes (the probe call and the measured count) | no |

The rule: pure logic goes to `harness-common`. It must stay wasm-safe. Unix
I/O, process spawn, and locks stay in `bin/harness`. Search and query crates
stay in the later tool layer. They are not Phase 2 core.

## 7. Deferred items

These are out of scope for Phase 2. I list them so the guardrail is clear.

- Git: keep the `git` CLI. The `git2` and `gix` crates add weight. The bash
  tool still covers git.
- Compression: `flate2`, `zip`, `tar`. Add only when a recorded session needs
  it.
- Offline token counting (fallback only): `tiktoken-rs` or the HF
  `tokenizers` crate. The default path counts tokens via the model server
  (measured `usage.input_tokens` or a 1-token probe). An in-process
  tokenizer is only needed when no server is reachable: unit tests, log
  replay, and the Phase 4 wasm runner. Defer until one of those requires a
  count without a server. `tokenizers` reads the model's Hugging Face
  `tokenizer.json`; `tiktoken-rs` covers the OpenAI-family BPE tables.
- `gix` for fast `git log`. Not needed until the git tool grows.

## 8. Risks

| Risk | Note | Mitigation |
|---|---|---|
| `jaq-all` churn | It is 0.1.x and moves fast. | Build on `jaq-core`, `jaq-stdlib`, `jaq-json` directly. |
| `jsonschema` build time | It is a large crate. The build is around a minute. | Accept it. It replaces three validator copies. |
| `toml` version split | The repo mixes 0.7 and 1.x. | Pick one line. Put it in `[workspace.dependencies]`. |
| wasm Phase 4 | `libc` and `flock` are Unix-only. | Keep them in `bin/harness`. Keep `harness-common` pure. |
| Pattern mismatch | A regex that is valid ERE may differ from bash `=~`. | Run the ten self-test rows as unit tests before merge. |
| Probe cost | One extra model call per step the trigger runs. | Gate the probe behind the measured last-count screen. It sends one output token over a cached prefix. |
| Byte-stable migration gate | Swapping `ns/4` for measured/probed counts changes the compact candidate and breaks the Stage 0 byte-identity check on `assemble` output. | Land the byte-identical move first (Stage 0). Land the counting swap as a separate, gated step after Stage 0, with its own e2e gate on trigger timing. |

## 9. Sources

- ripgrep engine: [docs.rs grep 0.4.1](https://docs.rs/crate/grep/latest),
  [BurntSushi/ripgrep](https://github.com/BurntSushi/ripgrep).
- jaq: [docs.rs jaq-core 3.1.1](https://docs.rs/crate/jaq-core/latest),
  [01mf02/jaq](https://github.com/01mf02/jaq),
  [jaq-core docs](https://docs.rs/jaq-core/3.1.1/jaq_core/).
- jsonschema: [docs.rs jsonschema 0.53.0](https://docs.rs/crate/jsonschema/latest),
  [Stranger6667/jsonschema](https://github.com/Stranger6667/jsonschema).
- repo evidence: `scripts/step.sh`, `scripts/turn.sh`,
  `scripts/overflow-classify.sh`, `docs/phase-2-plan.md`,
  `docs/phase-2-readiness.md`, `docs/bash-tool.md`.
