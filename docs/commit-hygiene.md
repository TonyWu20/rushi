# Commit Hygiene

## Policy

This repo has never been developed with Anthropic products.
Trailer lines like `Co-Authored-By: Claude <noreply@anthropic.com>`
that leak in from AI editors are treated as pollution.
We remove them from history and block new ones at commit time.
Plain human co-author trailers are allowed and stay untouched.

## The commit-msg hook

`.githooks/commit-msg` strips `Co-Authored-By:` lines that mention
Claude or Anthropic. It also trims the trailing blank lines that
result. If the message ends up empty, the hook refuses the commit.
It prints a notice on stderr whenever it removes anything.

## The pre-commit hook

`.githooks/pre-commit` verifies that the commit's author and committer
match `git config user.name` / `user.email`. It reads the effective
identity, which is the env-var override if set, otherwise the git
config value. If either field deviates, the hook refuses the commit.

This catches agent sessions that override the identity via environment
variables. Run 3 in the log below was exactly this. It also catches
misconfigured environments with a foreign HOME. It does not catch
`git -c` overrides. Those are rare and intentional.

## Enabling

```
git config core.hooksPath .githooks
```

`.envrc` runs this command on directory entry, so direnv shells
keep it active. On a fresh clone, run the command once.

## 2026-09-15 history rewrite

Run 1 (trailers): three commits carried the anthropic trailers.
A `git filter-branch --msg-filter` pass removed those lines from
the whole main history. The commit trees did not change. Only the
three messages changed.

Run 2 (identity): one commit, the `flake/mkRushi`
`cargoLockContents` fix, had its author and committer truncated to
`t <t@local>`. A `git filter-branch --env-filter` pass restored
`TonyWu20 <tony.w21@gmail.com>` on both fields. Trees, messages,
and timestamps were preserved.

After each rewrite, main was force-pushed with
`git push --force-with-lease origin main`. Each backup branch was
deleted after its push. Stale objects were pruned with
`git reflog expire` plus `git gc --prune=now`.

## Notes

- Three detached `/tmp` worktrees (compact-path alignment,
  token-estimate anchor, TUI head) carried the same trailers on
  their scratch lineages. They were removed on 2026-09-15 with
  the first force-push, and their objects were pruned.
- The docs line in an older commit that cites `Agent Skills
  (Claude/Anthropic)` as an established practice is a genuine
  external reference. It is not a co-author trail and stays.

## 2026-09-16 history rewrite

Run 3 (agent mis-signed commits): two commits on the PR #11
(`issue/10`) lineage had author and committer signed as
`Tony Wu <tony@users.noreply.github.com>`. They were the `mkRushi`
auto-derive commit and the auto-derivation docs commit. A `git
filter-branch --env-filter` pass restored `TonyWu20 <tony.w21@gmail.com>`
on both fields. Only the two commits and the merge that references
them changed SHA. Trees, messages, and timestamps were preserved.

Same follow-up as Run 1/2: force-push main, delete the backup
branch, `git reflog expire` plus `git gc --prune=now`.
