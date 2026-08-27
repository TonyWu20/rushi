# bash-tool.md — Second Review Pass (Meta-Meta-Review)

- Reviewed: `docs/bash-tool.md`, `docs/bash-tool-review.md`,
  `docs/bash-tool-review-meta-review.md`.
- Evidence: all source files, `config.toml`, scripts, sessions, git history.
- Role: reviewer of the spec, the review, and the meta-review.
- Date: 2026-08-26.

## Summary of method

I verified every factual claim against the current repo.
I read the route and assemble sources.
I read the tool manifests and the conformance harness.
I checked the sessions and the git history.
The git history shows the key fact.

Commit `227a1f6` rewrote `docs/bash-tool.md` after both reviews.
The commit message names B1 to B4 and most other findings.
The current spec resolves them.
The review and the meta-review describe an older draft.
Their section numbers do not match the current document.

## Part A. Problems the two reviews missed

### A1. The truncation marker position contradicts the text format

The spec gives three rules for truncation.
Section 4.3 says the tool prepends a marker to the output.
Section 5 says the text field begins with the marker.
Section 9 says the test checks that text starts with the marker.
Section 5 also defines the text format as command, then output, then exit code.
The three rules cannot all hold.

The text begins with the `$ <command>` line, not the marker.
The conformance test cannot pass under the stated format.
The author resolved B1 to B4 but did not touch this conflict.
Neither review noticed it.

### A2. The output cap semantics stay under-specified

Section 4.3 caps the combined stdout and stderr.
Section 5 caps stdout, stderr, and text at the same limit.
The spec never states whether the cap is shared or per-field.
Separate caps let the text reach two times the limit.
The text also adds the command echo and the exit code line.
The "tool cap is the primary bound" claim can then fail.

Route also caps the extracted text at 20000 characters.
Route keeps the head and adds no marker.
Assemble does the same with a marker.
The model then sees head-truncated output.
That defeats the tail-keeping design in section 4.3.
The spec must state one rule for all three fields.

I verified this in the route source.
`process_stdout` clips `text.chars().take(max_chars)`.
It keeps the head with no marker.
The meta-review caught assemble's head-keep.
It missed route's identical behavior.

### A3. exit_code on timeout stays undefined

On timeout, the tool kills the command.
The spec never states the exit_code value for that case.
The conformance test checks only the timed_out flag.
A tester cannot write the full timeout test without clarification.

### A4. timeout_secs above 300 stays unhandled

The schema says max 300 seconds.
The failure table rejects only values below 1.
The spec never states the behavior for values above 300.
A value of 500 makes the tool wait past the route backstop.
Route then reports a tool error.
That contradicts the "timeout is not a tool error" rule.

### A5. The process-group claim is too strong

Section 4.2 says no spawned process survives.
A process that calls setsid leaves the group.
A daemon can survive the group kill.
The claim is too strong.
State the limitation instead.

### A6. The conformance harness lacks the required controls

The harness has no env or cwd control.
The spawn-failure test needs PATH=/dev/null.
The cwd test needs a known directory.
The harness checks exit code and substring only.
The spec claims each test checks JSON shape and empty stderr.
The two descriptions do not match.

I verified the empirical facts.
`sh -c "nonexistent_cmd_xyz"` exits 127.
`sh -c ""` exits 0.
`echo hello | wc -c` prints 6.
PATH=/dev/null hides sh.
The spec test expectations are sound.

## Part B. Incorrect judgments in the review

### B1. m2 misapplies the rule of three

The review said the three CLI flags break the rule of three.
Every existing tool declares flags that mirror config values.
Route declares --tool-result-max-chars.
The rule does not apply to a tool's own CLI.
The meta-review corrected this correctly.
The review's m2 was an incorrect judgment.

### B2. m5, m6, and m7 overstate their gaps

The review called exit_code ambiguous.
Section 4 already states the difference.
The review demanded a formal output schema.
The field table already types every field.
The review doubted the route discovery claim, but the source in this repo confirms it.
These three findings overstated small gaps.

### B3. Scorecard row 9 is wrong

The review marked criterion 9 as partial.
No tool-name or config-key conflict exists.
The prompt addition stays static.
Criterion 9 passes.
The meta-review reached the same conclusion.

### B4. The FAIL verdict is now stale

The review concluded FAIL with four blocking findings.
The meta-review said to keep that verdict.
Commit `227a1f6` then rewrote the spec.
It resolved all blocking findings and most majors.
The current spec passes every criterion except the cap issues above.
The review and meta-review stay out of sync with the fixed spec, so a reader cannot map their section numbers to the current document.

### B5. The meta-review states a false fact

The meta-review said only assemble reads config.toml.
Model, parse, and user also read it.
I verified this in the source.
The substance of the point stays correct.
The claim itself is false.

## Part C. Ratings

### bash-tool.md — 8 of 10

The spec is complete and evidence-backed.
It resolves every finding from the review cycle.
It still has the cap issues A1, A2, and A3.
Close those before a tester writes the conformance tests.

### bash-tool-review.md — 6 of 10

The review was strong at the time.
Its repo facts were accurate.
Its blocking findings matched the old draft.
Its m2, m5, m6, and m7 judgments were wrong or overstated.
Its row 9 score was wrong.
Its section citations no longer match the current document, so the review misleads a current reader.

### bash-tool-review-meta-review.md — 7 of 10

The meta-review is the sharpest document.
It corrected the review's errors correctly.
It added verified consequences from the sources.
It states one false fact about config readers.
Its B4 fix stays incomplete.
It repeats stale spec citations, but it stays the most useful of the three for technical accuracy.

## Final word

Close A1 and A2 before implementation.
Update the review and the meta-review to match the fixed spec.
Then the cycle is consistent.
