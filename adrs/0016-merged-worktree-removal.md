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
2. `git status --porcelain --ignored=matching` reports nothing except unstaged deletions of
   tracked files under a build-output directory and ignored trees under those names anywhere.
   `dist`, `target`, `node_modules` and `__pycache__` count at any depth — a monorepo commits
   `client/dist/` several levels down and a sweep deletes it — while `build` and `out` count only
   at the top level. Those are regenerable and the only content removal discards; `--force` is
   passed to git only in that case. A deleted tracked file under a nested `src/build/` or
   `internal/out/` is source, and keeps the worktree. Any modified, staged, added,
   renamed, untracked, or ignored non-build-output path — a gitignored `.env`, a local database,
   notes — keeps it too, reported as merged-with-local-changes with a count;
   A caller may also name `--stale-worktrees <age>`, which lets an *unmerged* worktree through
   condition 1 when it has been idle at least that long — the later of its `HEAD` commit date
   and the modification time of its `HEAD` reflog, which `git status` does not touch — and some
   ref contains its `HEAD`, so removing the checkout (which keeps the branch) loses no commit.
   Conditions 2–4 apply unchanged;
3. it is not the main worktree, not locked, not missing (`git worktree prune` owns that), and
   voom was not started inside it;
4. it resolves strictly below a path voom was told to sweep. Repositories come from the walk the
   sweep already does (or `git-prune`'s discovery) and are deduplicated by git's common directory,
   but a linked worktree can live anywhere on disk; one stored outside every scan root is reported
   and kept, so the containment rail holds for a deletion target the walker did not produce.

Branches are never deleted. The flag is off by default, has no `voom.toml` key, and is not
implied by `--clear-caches`: every other voom default can be wrong only in disk or rebuild
time, and this one can be wrong in lost work. `-n` runs every check and removes nothing.

Repositories come from the walk the sweep already does (or `git-prune`'s discovery), are
deduplicated by git's common directory, and each is examined from its main checkout.

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

## Amendment — 2026-10-14: Optional GitHub merge proof for exact HEADs

Default-branch ancestry cannot prove squash or rebase merges, and a pull request merged into a
release or integration branch can remain absent from the default branch. With
`--remove-merged-worktrees`, the additional `--github-merged-worktrees` flag accepts hosted merge
proof on both a sweep and `git-prune`. This is an explicit network opt-in using
`gh api --hostname github.com`; ordinary ancestry checks remain offline and voom does not fetch.

The supported github.com `origin` URL supplies the repository identity. A returned pull request
must have a nonempty `merged_at`, an exact head SHA equal to the worktree's HEAD, and a base
repository equal to that origin repository. Any base branch qualifies. Branch names, associated
commits with a different head SHA, closed-but-unmerged PRs, and another repository's PR do not
prove a merge. Unsupported remotes, missing `gh` or authentication, malformed or oversized
responses, timeouts and API/network failures fail closed: none supplies hosted merge proof.
Independent local ancestry can still prove a merge.

The working-tree check still protects modifications and local evidence. Current-directory,
locked-worktree, protected-path and containment checks remain in force. Ignored evidence is retained in place rather than
archived to make removal possible, and submodule worktrees are not force-removed. Before removing
any HEAD proved only by GitHub, named or detached, voom creates `refs/voom/merged-worktrees/<SHA>` in the
common repository so the commit remains locally reachable. Failure to retain it prevents removal;
dry-run does not write the ref. Branches remain untouched.

Neither `--clear-caches` nor configuration enables worktree removal. Scan roots remain explicit:
worktrees outside a supplied root are kept, so a home-and-temporary audit uses roots such as
`voom -n --remove-merged-worktrees --github-merged-worktrees ~ /tmp`.

## Amendment — 2026-10-14: Marker-proven ignored dependency directories

Ignored Elixir `deps/` beside a regular `mix.exs`, and Python `.venv/` beside a regular
`pyproject.toml`, `setup.py` or `setup.cfg`, now count as regenerable content when judging a
worktree. This applies at the directory's own parent, including nested packages; a marker
elsewhere in the checkout does not prove it. The candidate must be an actual directory, not a
file or symlink, and the sibling marker must be a regular file, not a symlink. Missing or invalid
proof keeps the worktree as `LocalChanges`.

These exceptions allow a clean, merged worktree with fetched dependencies to be removed without
classifying arbitrary `deps` or `.venv` paths by name alone. They apply only to ignored status
entries: modified, staged and untracked content still keeps the checkout. Configuration, notes,
databases and other ignored evidence remain protected. No automatic evidence archive, forceful
submodule removal or default worktree-removal behavior is introduced.

## Amendment — 2026-10-17: Keep reasons, dangling checkouts, a staged status

**Every kept worktree has a reason.** Not merged, locked, missing, outside the swept paths, current
directory, merged-with-local-changes and unchecked are all stated, in the `-v` human report and in
the JSON document (`state` and `reason` per worktree). `LocalChanges`, `Unchecked`, the two cases
below and the totals appear without `-v`; the rest are counted. A local-only `HEAD` is a plain
"not merged" for the hosted proof: GitHub answers HTTP 422 `No commit found for SHA` for a commit
it has never seen, which is an answer and not an authentication or network failure. When the proof
is requested and GitHub has never seen the `HEAD`, one further lookup by branch
(`pulls?state=closed&head=<owner>:<branch>`) lets the report say that a pull request for the branch
merged but this checkout's `HEAD` is not its head (squash-merged, then amended). That is reported
as `head_differs` and still kept: the exact-head rule is unchanged, only the explanation improved.
A merged pull request that contains the `HEAD` without being headed by it gets the same state.

**Dangling checkouts are reported, never removed.** A checkout whose `.git` file names
`<main>/.git/worktrees/<name>` that no longer exists cannot be examined by git at all: no `HEAD`
to test for ancestry or reachability, no index to compare with the tree, no reflog. Removal needs
every one of those proofs, and none of them can be rebuilt. The tempting substitute — remove it if
nothing in it is newer than the administration's disappearance — has no moment to compare against:
the deletion leaves no timestamp except the `worktrees/` directory's own mtime, which any later
`worktree add` or `prune` moves, so it bounds nothing from below and a file edited after the loss
would pass. Reachability cannot be proven either, because the commit it sat on is unrecorded. voom
therefore lists such a checkout below a swept root, not the current directory, as `dangling` with
its size and the missing gitdir, counts it in the totals, and leaves the decision to the user. The
repository it belonged to is derived from the gitdir path for grouping only. Outside every swept
root or the current directory it is kept for the same reasons as any worktree.

**The working-tree check is staged, with the same verdict.** `git status --ignored=matching`
enumerates every directory of the checkout, including `node_modules` and Bazel trees, and took
10-60 s per worktree of the monorepo. The check now asks first for tracked changes only
(`--untracked-files=no`, answered from the index in about a second). Any tracked change that is
not a deleted build-output file keeps the worktree there; only a checkout that passes runs the full
status. The first stage's entries are a subset of the second's and classified by the same function,
so the keep/remove decision is identical (an ignored `.env` still keeps, ignored build output still
does not); only the *count* in a `LocalChanges` result is a lower bound when the first stage
decided. Status is run with `GIT_OPTIONAL_LOCKS=0`, so it never rewrites the index of a checkout
another session may be using. `--stale-worktrees` is also accepted by `git-prune`.
