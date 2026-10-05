# 0016 — Merged-Worktree Removal: Ancestry Is Not Enough

- Status: Accepted
- Date: 2026-10-04

## Context

A monorepo used through disposable worktrees accumulates them: on the machine measured for ADR
0014, 152 registered worktrees, 102 under one directory, each a full checkout and each owning a
Bazel output base. Ancestry alone looks like the obvious test — if a worktree's `HEAD` is in
`origin/master`, its work is merged. On that machine 24 worktrees passed it. Checking their
working trees showed **20 of the 24 held real uncommitted work**: twelve had around 73,000
changed paths including about 2,800 modified files, one had 21,000 staged modifications. Only
four were clean apart from two deleted `dist/*.svg` files. "Merged" describes the commits, and
the commits are the part that is already safe.

ADR 0011 keeps git housekeeping to steps git considers safe and idempotent. Removing a checkout
is not one of them.

## Decision

`--remove-merged-worktrees`, on a sweep and on `voom git-prune`, removes a linked worktree with
`git worktree remove` only when **all** hold:

1. its `HEAD` is an ancestor of the default branch — the local `refs/remotes/origin/HEAD`,
   else `origin/main`, `origin/master`, `main`, `master`. voom never fetches, so a stale ref
   makes fewer worktrees look merged, the safe direction;
2. `git status --porcelain` reports nothing except unstaged deletions of tracked files under a
   build-output directory (`dist`, `build`, `target`, `out`, `node_modules`, `__pycache__`).
   Those are regenerable by definition and the only change removal discards; `--force` is passed
   to git only in that case. Any modified, staged, added, renamed or untracked path keeps the
   worktree, reported as merged-with-local-changes with a count;
3. it is not the main worktree, not locked, not missing (`git worktree prune` owns that), and
   voom was not started inside it.

Branches are never deleted. The flag is off by default, has no `voom.toml` key, and is not
implied by `--clear-caches`: every other voom default can be wrong only in disk or rebuild
time, and this one can be wrong in lost work. `-n` runs every check and removes nothing.

Repositories come from the walk the sweep already does (or `git-prune`'s discovery), are
deduplicated by git's common directory, and each is examined from its main checkout, so a
worktree stored outside the swept tree is still found through its repository.

Order matters in a sweep: worktrees are removed before the Bazel stage, so the output bases of
the worktrees just removed are orphans and go in the same run (ADR 0014).

## Consequences

- Reclaims checkouts and, through Bazel, far more, without ever being the thing that loses work.
- Untracked-but-empty-looking leftovers (a `.typing-gen` directory of zero-byte files) keep a
  worktree. Deliberate: voom does not judge whether an untracked file matters.
- A stash is repository-wide, not per worktree, and is unaffected.
- A merged branch still checked out with uncommitted edits is reported, which doubles as an
  audit of forgotten work.

## Alternatives considered

- **Ancestry only**: rejected by the measurement above.
- **A separate subcommand**: the sweep already walks the repositories, and the follow-on Bazel
  cleanup belongs in the same run.
- **Deleting the merged branch too**: a separate decision with its own recovery story.
