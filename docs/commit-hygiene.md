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

## Enabling

```
git config core.hooksPath .githooks
```

`.envrc` runs this command on directory entry, so direnv shells
keep it active. On a fresh clone, run the command once.

## 2026-09-15 history rewrite

Three commits on main carried the anthropic trailers. A
`git filter-branch --msg-filter` run on 2026-09-15 removed the
trailer lines from the whole main history. The commit trees did
not change. Only the three messages changed.

The backup branch `backup/pre-coauthor-cleanup` still points at
the pre-rewrite state. Delete it once the force-push is
confirmed.

Because the rewrite changed commit hashes, the remote main needs
a force-push:

```
git push --force-with-lease origin main
```

## Notes

- Two detached `/tmp` worktree commits (the Phase 2 harness and
  the Phase 2 plan revision) still carry trailers. They are
  scratch-only and on no branch.
- The docs line in an older commit that cites `Agent Skills
  (Claude/Anthropic)` as an established practice is a genuine
  external reference. It is not a co-author trail and stays.
