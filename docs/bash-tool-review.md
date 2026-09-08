# bash-tool.md — Adversarial Spec Review (Superseded by `bash-tool-review-2.md`)

- Reviewed: `docs/bash-tool.md` (2026-08-26)
- Rubric: `docs/spec-review-criteria.md` (9 criteria)
- Evidence: `docs/architecture.md`, `docs/refinement-policy.md`,
  `docs/SPEC_CONTRACT_TESTS.md`, `config.toml`, `tools/read/tool.toml`,
  `scripts/step.sh`, `scripts/tool-conformance.sh`, `sessions/`
- Role: reviewer only. This file names failing criteria and fixes.
  It does not change the spec. The author owns the edits.

## Verdict

FAIL. The spec is not implementable as written.
It fails criteria 2, 3, 5, 6, 7, and 8.
It partially fails criteria 1, 4, and 9.
Four findings stop the work: the exit-code contradiction, the undefined
cwd channel, the missing Impact and Migration sections, and the inverted
output cap.

## Blocking findings

### B1. The command-not-found test contradicts the exit rules (criteria 2, 3, 6)

- Section 3.4 states that a non-zero command exit is not a tool error.
- Section 5 says "Command runs, non-zero exit" gives tool exit 0 and `is_error` false.
- Section 8 says `bash: command not found` expects non-zero exit and `is_error` true.
- `sh -c "bad_command"` exits 127. That is a non-zero command exit.
- Section 8 contradicts sections 3.4 and 5.
- Fix: pick one rule. If not-found is a normal non-zero exit, the tool exits 0 with `is_error` false and `exit_code` 127.
- If it is a tool error, add that exception to sections 3.4 and 5.
- Make all three sections agree before implementation.

### B2. The cwd channel is undefined and does not match the harness (criteria 2, 3, 6)

- Section 3.1 says the tool reads `sessions/<name>/cwd`.
- The input schema in section 2 has no session name field.
- The tool cannot resolve `<name>` from its own input.
- `scripts/step.sh` already reads `sessions/<name>/cwd` and passes `--cwd` to `route`.
- The tool must take cwd from `route`, not read the file itself.
- Not every session has a cwd file. `sessions/e2e` and `sessions/test1` to `test5` do not.
- The fallback when the cwd file is missing is not stated.
- The `bash: cwd` test cannot run. The harness sends JSON only and no cwd channel.
- Fix: state how cwd reaches the tool. `route` injects it into the input or sets an env var.
- State the fallback when cwd is missing.
- Rewrite the cwd test so it passes cwd explicitly.

### B3. Two sections the rubric lists are missing (criterion 8)

- The rubric lists an Impact section.
- The rubric lists a Migration section.
- `bash-tool.md` has neither.
- Section 9 is Implementation notes. It is not the Impact section.
- Fix: add Impact. Name the binaries, event types, and schemas that change.
- Fix: add Migration. State additive or breaking and old-session replay.

### B4. The output cap is inverted (criterion 5)

- `bash_max_output_bytes` defaults to 32768 bytes.
- The assemble backstop is `tool_result_max_chars = 20000` in `config.toml`.
- For ASCII output, 20000 chars is smaller than 32768 bytes.
- The assemble cap, not the tool cap, is the effective bound.
- Section 3.3 says the tool cap is the primary bound. That is false.
- The two caps also use different units, bytes and chars.
- Fix: set `bash_max_output_bytes` below 20000, or raise the assemble cap.
- Use one unit. State which cap the model actually sees.

## Major findings

### M1. No conformance test for the only `is_error` true case (criterion 6)

- Section 5 lists "Cannot spawn process" as `is_error` true.
- No test in section 8 covers a tool-level failure.
- Fix: add a spawn-failure test that expects non-zero exit and `is_error` true.
- Use a missing or invalid interpreter path to force the failure.

### M2. Section 9 is not a testable contract (criterion 2)

- It names internal mechanisms: threads, a timer thread, tokio, kill_on_drop.
- The rubric keeps mechanism detail out of the testable spec.
- Section 3.2 says "send SIGTERM to the process group". That is not observable.
- Fix: mark section 9 as implementation guidance, not contract.
- Rewrite 3.2 as an observable outcome. No child process survives the timeout.

### M3. The edge cases are incomplete (criterion 2)

- Missing: an absent or empty `command` field.
- Missing: `timeout_secs` equal to 0.
- Missing: `timeout_secs` below 0.
- Missing: a command that reads stdin and gets immediate EOF.
- Fix: add one row per case. State the tool exit, `is_error`, and text for each.

### M4. No explicit mutation gate (criterion 2)

- The rubric names the mutation gate as the primary Rust gate.
- The spec lists tests but not which test fails when a behavior is removed.
- Fix: for each behavior, name the test that must fail on removal.

### M5. No exact `tool.toml` is shown (criterion 3)

- Section 2 shows a JSON schema, not the TOML manifest.
- The registry reads `tools/*/tool.toml`. See `tools/read/tool.toml`.
- Section 9 says to create the manifest from the schema in section 2.
- That does not match the manifest shape, `[tool]` plus `[tool.schema]`.
- The manifest `timeout_ms` is not matched to the tool's own timeout.
- Fix: show the exact `tool.toml`.
- State how manifest `timeout_ms` and the tool timeout interact.

### M6. The output has no typed schema (criterion 3)

- Section 4 shows a sample and field rules, not a schema.
- The rubric wants every output field typed.
- Fix: add the output JSON Schema with a type per field.

## Minor findings

### m1. The pre-test is not run (criterion 1)

- The spec cites the episode: `loop-edit-real-test`, 98 of 102 calls.
- It does not state the P4 checklist or the pre-test outcome.
- Question: why not a plain script `tool.toml` that wraps `sh -c`?
- The real reason is per-call process-group kill, output cap, stdin close, and the JSON shape.
- State that reason so the pre-test answer is on record.

### m2. Three CLI flags are over-scoped (criteria 4, 9)

- Section 3.2 adds `--timeout-default` and `--timeout-max`.
- Section 3.3 adds `--max-output-bytes`.
- These duplicate the config keys. No third caller needs them.
- The rule of three (refinement P2) does not hold.
- Fix: keep the config keys and drop the flags, or name a third caller.

### m3. The config keys are not tool-prefixed (criterion 9)

- Existing keys use prefixes: `read_*`, `write_*`, `tool_result_max_chars`.
- The new keys `timeout_default` and `timeout_max` are global, not `bash_*`.
- This risks a collision when another tool needs a default timeout.
- Fix: name them `bash_timeout_default` and `bash_timeout_max`.

### m4. "Phase 1.5" is not a roadmap phase (criterion 5)

- `architecture.md` defines phases 1 through 4.
- Fix: mark this a Phase 1 tool. It is subprocess based and uses no shared crate.

### m5. The `exit_code` field name is ambiguous (criterion 3)

- The JSON field `exit_code` holds the command's exit code.
- The tool process also has its own exit code.
- Two different values share one name.
- Fix: rename the field to `command_exit`, or state the difference in section 4.

### m6. The unaffected binaries are not listed (criteria 5, 8)

- The spec names `bash`, `route`, `assemble`, `config.toml`, and `Cargo.toml`.
- It does not name the binaries that do not change.
- Fix: add a line that names `claim`, `model`, `parse`, `log`, and the TUI as unchanged.

### m7. The `route` discovery claim is unverified (criterion 9)

- Section 9 says `route` globs `tools/*/tool.toml`.
- `route` is a compiled binary. The scripts only pass `--tools`.
- Verify the discovery path in the `route` source before building.

## Criteria scorecard

| Criterion | Result | Main gap |
|---|---|---|
| 1 Justification | Partial | pre-test and P4 not stated |
| 2 Testability | Fail | section 9, edge cases, no mutation gate |
| 3 Contract precision | Fail | no tool.toml, no output schema, cwd channel |
| 4 Minimalism | Partial | three CLI flags with no third caller |
| 5 Boundary and phase | Fail | cap inversion, phase 1.5, no impact list |
| 6 Conformance | Fail | not-found contradiction, missing tests |
| 7 Reversibility | Fail | no additive or breaking statement |
| 8 Format | Fail | no Impact, no Migration |
| 9 Consistency | Partial | key naming, unverified route claim |

## Next step

Return the spec to the author. Close the four blocking findings first (B1 to B4).
Then close the major findings (M1 to M6).
Re-run the tester pass on criteria 2, 3, and 6 after the fixes.
The spec must not enter implementation while B1 to B4 are open.
