# bash-tool-review.md — Meta-Review (Adversarial)

- Reviewed: `docs/bash-tool-review.md` (2026-08-26)
- Target: `docs/bash-tool.md`
- Evidence checked: `docs/spec-review-criteria.md`, `docs/architecture.md`,
  `docs/refinement-policy.md`, `docs/SPEC_CONTRACT_TESTS.md`, `config.toml`,
  `tools/read/tool.toml`, `tools/{read,write,edit,list}/src/main.rs`,
  `scripts/step.sh`, `scripts/tool-conformance.sh`, `bin/route/src/main.rs`,
  `bin/assemble/src/main.rs`, `sessions/`, shell exit-code check

Role: reviewer of the review. This file judges the review. It does not edit
the spec or the review.

## Verdict

The review is reasonable and largely correct. The FAIL verdict stands.
Criteria 2, 3, 5, 6, 7, and 8 do fail as the review states.
Criteria 1, 4, and 9 are partial as stated.
The four blocking findings (B1 to B4) are all valid and correctly prioritized.
Six findings need correction or downgrade. Two spec defects the review
missed are named below.

## Findings that hold

### B1. Exit-code contradiction — valid

- Section 3.4 says a non-zero command exit is not a tool error.
- Section 5 maps non-zero exit to tool exit 0 and `is_error` false.
- Section 8 expects non-zero exit and `is_error` true for not-found.
- Empirical check: `sh -c "nonexistent_cmd_xyz"` exits 127.
- 127 is a non-zero command exit. The three sections cannot all hold.
- The proposed fix (pick one rule and make all three agree) is correct.

### B2. cwd channel — valid, fix text is incomplete

- Section 3.1 says the tool reads `sessions/<name>/cwd`.
- The section 2 input schema has no session name field.
- `scripts/step.sh` reads `sessions/<name>/cwd` and passes `--cwd` to `route`.
- `bin/route/src/main.rs` applies it with `cmd.current_dir(cwd)`.
- The tool receives cwd as its own process working directory.
- Only `sessions/tool-idea` and `sessions/ux-test` have a `cwd` file.
  The sessions `e2e`, `loop-edit-real-test`, and `test1` to `test5` do not.
- The `bash: cwd` test cannot pass in the harness as written.
- Correction: the review's fix lists "inject into input or env var".
  The actual mechanism is neither. `route` sets the process cwd.
  The fix must name the inherited process cwd.
  It must drop the `sessions/<name>/cwd` read entirely.

### B3. Missing Impact and Migration — valid

- Rubric criterion 8 lists Impact and Migration as required sections.
- The spec has neither. Section 9 is Implementation notes.
- The finding and fix are correct.

### B4. Inverted output cap — valid

- `bash_max_output_bytes` defaults to 32768 bytes (spec section 3.3).
- `config.toml` sets `tool_result_max_chars = 20000`.
- `bin/assemble/src/main.rs` (lines 106 to 110, 225 to 241) caps the
  tool result `text` at 20000 characters.
- For ASCII, 20000 characters is smaller than 32768 bytes.
- The assemble cap, not the tool cap, is the effective bound.
- The review's claim that section 3.3 is false holds.
- The unit mismatch (bytes versus chars) is confirmed.
- Addition the review missed: `assemble` keeps the head of the text
  (`chars.take` from the start). The bash tool keeps the tail.
  Under the inversion, the model sees head-truncated output.
  That defeats the tail-keeping design in section 3.3.
- The proposed fix is sound. Add one line: the model-visible cap must be
  the smaller of the two, stated in one unit.

### M1. No spawn-failure test — valid

- Section 5 lists "Cannot spawn process" as the only `is_error` true row.
- Section 8 has no test for it.
- Nuance: the tool's interpreter is the fixed `sh` binary.
- "Missing or invalid interpreter path" is hard to trigger from input.
- A workable variant: run the tool with a restricted `PATH` so `sh`
  is not found. The harness can do that with the environment.
- The finding holds. The proposed test mechanism needs that refinement.

### M2. Mechanism detail in the spec — valid with a nuance

- Section 9 names threads, a timer thread, tokio, and kill_on_drop.
- Rubric criterion 2 bans implementation words and mechanisms.
- Section 3.2 names SIGTERM and SIGKILL on the process group.
  That is not observable from the input/output boundary.
- Nuance: section 9 is labeled Implementation notes.
  The rubric's required-section list does not include it.
  The stronger half of the finding is section 3.2, which sits in the
  behavior section.
- The fix (mark section 9 as guidance, rewrite 3.2 as observable
  outcomes) is correct.

### M3. Incomplete edge cases — valid

- Rubric criterion 2 lists empty input, missing fields, zero, negative
  values, and timeout.
- The spec tests timeout and max size only.
- Missing: empty or absent `command`, `timeout_secs` of 0, negative
  `timeout_secs`, and a stdin-reading command at immediate EOF.
- Note: an absent `command` is rejected by `route` schema validation
  (see M5 below). The spec must name that harness behavior.
- The fix (one row per case, with tool exit, `is_error`, and `text`)
  matches the rubric style.

### M4. No mutation gate — valid

- Rubric criterion 2 requires a named mutation gate.
- `SPEC_CONTRACT_TESTS.md` calls it the primary gate for Rust.
- Section 8 lists tests without mapping behavior to test.
- Removing a behavior would not be provable from the test list alone.
- The finding and fix are correct.

### M5. No exact tool.toml — valid and sharpenable

- Rubric criterion 3 requires the TOML manifest for tools.
- The spec shows a JSON tool definition, not the manifest.
- `tools/read/tool.toml` shows the shape: `[tool]` plus `[tool.schema]`.
- `route` reads `timeout_ms` from the manifest (default 30000) and kills
  the tool process when it fires.
- The tool's internal timeout (60 s default, 300 s max) can exceed
  `timeout_ms`. Then `route` kills the tool and reports `is_error` true.
  That contradicts sections 3.2 and 5 (timeout: exit 0, `is_error` false).
- Sharpened consequence the review missed: `route` validation reads
  `schema.get("required")` at the manifest top level
  (`bin/route/src/main.rs`, line 370).
  The section 2 JSON wraps `required` under `parameters`.
  Pasted into `[tool.schema]`, the `required: ["command"]` check
  silently stops working.
- Also: architecture section 3.3 says the manifest carries no `name`
  field. The section 2 JSON includes one.
- The finding holds. Add both consequences to it.

### M6. No typed output schema — weak, a minor issue

- Rubric criterion 3 says every output field must be typed.
  It does not demand a formal output JSON Schema.
- Section 4 types each field: `exit_code` is an integer,
  `timed_out` and `truncated` are booleans, and the sample plus field
  rules show `text`, `stdout`, and `stderr` as strings.
- No field is left as "some metadata" or "various fields".
- Downgrade M6 to a minor suggestion. A formal schema is welcome, but the
  rubric letter is met by the field rules.

### m1. Pre-test not recorded — valid

- The spec cites the episode (`loop-edit-real-test`, 98 of 102 calls).
  The session exists at `sessions/loop-edit-real-test`.
- The spec does not state the P4 checklist or the pre-test outcome.
- Rubric criterion 1 requires both.
- The pre-test question is sharp: Phase 1 says tools are arbitrary
  scripts, and a manifest wrapping `sh -c` is a real alternative.
- Recording why the process-group kill, output cap, and JSON shape beat
  that alternative is required.

### m3. Config key naming — valid as a nit, loose rubric mapping

- Existing tool keys are prefixed: `read_*`, `write_*`,
  `tool_result_max_chars`.
- `timeout_default` and `timeout_max` are not prefixed. `bash_max_output_bytes`
  is.
- Criterion 9 forbids conflicts with existing keys. No conflict exists.
  So this is not a criterion 9 failure as the review labels it.
- The naming concern is still worth fixing. Map it to criterion 4 or 9
  as a consistency note, not a violation.

### m4. "Phase 1.5" — valid

- Architecture section 6 defines phases 1 through 4 only.
- Phase 1 is "Bash + separate binaries" with no shared crate.
- A subprocess tool with a TOML manifest is a Phase 1 tool.
- The finding and fix are correct.

### m6. Unaffected binaries not listed — valid

- Rubric criterion 5 requires the spec to name affected and unaffected
  binaries.
- The spec names what changes. It does not name what stays put.
- The four named binaries (`claim`, `model`, `parse`, `log`) all exist
  in the workspace `Cargo.toml` members.
- Small correction: no TUI binary exists in `bin/` yet.
  Phrase it as "no change required" rather than "unchanged".

## Findings that do not hold as written

### m2. "Three CLI flags are over-scoped" — misapplied

- The review says the flags duplicate config keys and break the rule of
  three (refinement P2).
- Check the existing tools. `tools/read/src/main.rs` declares
  `--read-limit`, `--read-max-line-length`, `--read-max-bytes`, and
  `--read-stream-min-size`. `write` and `list` do the same.
- Those flags mirror `config.toml` values. No tool binary reads
  `config.toml`. Only `assemble` reads it. `route` takes
  `--tool-result-max-chars` as a CLI flag with default 20000.
- The bash flags follow the established per-tool pattern.
  The rule of three does not apply to a tool's own CLI the way the
  review states.
- The proposed fix ("keep the config keys and drop the flags") inverts
  the repo convention.
- The real defect to name: the spec adds
  `bash_max_output_bytes`, `timeout_default`, and `timeout_max` to
  `config.toml` but names no reader. Existing tools do not read the
  config. As written, those keys are dead config, or the tool breaks
  the pattern. The spec must pick one mechanism and say it.
- Rewrite m2 around that gap. Drop the rule-of-three argument.

### m5. "exit_code is ambiguous" — mostly addressed

- The field name and the tool process exit code do share a name.
- Section 4 already says "exit_code — integer exit code of the command".
- Section 5 separates "Tool exit" from `exit_code` in its table.
- Section 8 writes "tool exit 0, `exit_code` 42".
- One of the two proposed fixes is already done.
- Downgrade m5 to a naming preference, not a gap.

### m7. "route discovery claim is unverified" — stale

- The review says `route` is a compiled binary and to verify the
  discovery path in its source.
- The source is in this repo at `bin/route/src/main.rs`.
- Lines 54 to 63 read the tools directory and load each `tool.toml`.
- The spec's claim (globs `tools/*/tool.toml`, so adding a directory
  is sufficient) is true.
- The "compiled binary" framing misstates the repo. The source is
  first-class here.
- Drop m7, or mark it verified-true with the file and line cited.

### Scorecard row 9 — overstated

- The real criterion 9 checks pass: no tool-name collision
  (`tools/` holds `edit`, `list`, `read`, `write` and `bash` is new), no
  config-key conflict, the prompt addition is static text, and no
  refinement-policy rule is contradicted by the spec itself.
- The m2 and m3 concerns above do not fail criterion 9 as written.
- Mark criterion 9 as pass with consistency notes, or keep partial only
  if the m2 rewrite lands a real policy conflict.

## What the review missed

1. Schema shape breaks validation. If the section 2 JSON lands in
   `[tool.schema]` verbatim, `route` cannot find `required` at the top
   level. The `required: ["command"]` check stops working silently.
   The spec must show the manifest, with `required` at the schema
   top level and no `name` field.
2. Head versus tail under the cap inversion. `assemble` keeps the first
   20000 characters. The bash tool keeps the last bytes. With both caps
   active, the model sees a head-truncated view of tail-kept output.
   B4 must name this interaction.
3. Config keys with no reader. The spec adds three keys to
   `config.toml`. No tool binary reads that file. The spec must state
   whether the tool reads the config, takes flags only, or takes both.
4. The `bash: cwd` rewrite. The review says "pass cwd explicitly".
   Concretely: the harness launches the tool with a known working
   directory and asserts `pwd` equals it. Name that in the fix.

## Recommendation

Keep the FAIL verdict and B1 to B4 as blocking.
Apply these changes to the review before it goes back to the author:

- Fix the B2 fix text to name the inherited process cwd.
- Rewrite m2 around the missing config reader. Drop the rule-of-three
  argument.
- Downgrade m5 and M6.
- Drop m7, or mark it verified-true with the source lines.
- Sharpen M5 with the silent-validation consequence and the extra
  `name` field.
- Add the four missed items above as new findings.
- Re-score criterion 9.

Then return the spec to the author. Close B1 to B4 first. Re-run the
tester pass on criteria 2, 3, and 6 after the fixes.
