# 0018 — Aggressive Cleanup: Explicitly Discard Rebuildable and Worktree Contents

- Status: Accepted
- Date: 2026-10-18

## Context

ADR 0016 correctly separates merged commits from uncommitted checkout contents. Its default
rules retain modified, staged, untracked and non-build ignored files, and retain dangling
checkouts whose Git administration has disappeared. A post-cleanup workstation audit still
found completed worktrees consuming space for exactly these reasons. The owner explicitly
requested their removal, including local contents, without recovery archives.

The owner also requested that aggressive cleanup include dependency directories and all other
supported cleanup categories. A cache-clear request alone cannot express the worktree part:
a checkout can contain irreplaceable source. Requiring a separate opt-in for every category
makes a deliberate full cleanup harder to discover.

## Decision

Add an explicit CLI-only `--aggressive` flag. On a sweep it enables every catalog artifact,
including off-by-default dependencies, every supported cache including Hugging Face models,
signature-verified tagged directories, permission repair (`--force`), worktree removal, and
unused Docker image/build-cache cleanup. Docker's minimum age defaults to zero in this mode;
`--docker-max-age` overrides it. Volumes still require `--docker-volumes` or
`--docker-volumes-all`, and containers are never removed. Explicit artifact disables,
excludes and keep policies still govern artifacts and caches. Ecosystem markers and cache proofs
remain mandatory.

On both a sweep and `git-prune`, it relaxes ADR 0016's worktree rules for that invocation:

- Merged worktrees may be removed with modified, staged, untracked or ignored contents.
- Unmerged worktrees qualify with a zero minimum idle age unless an explicit
  `--stale-worktrees` age overrides it. Named branches remain available. A detached HEAD
  reached by no other ref is retained at `refs/voom/aggressive-worktrees/<SHA>` before removal.
  Failure to retain it keeps the checkout; dry-run writes no ref.
- A dangling checkout identified by its `.git` file pointing to missing worktree administration
  may be removed despite the unavailable HEAD and index. Ordinary runs continue to report it
  and leave it in place.
- No recovery archive is created. Existing branches and commit-retention rules remain intact.
- Local-content status checks are skipped because those contents are explicitly disposable.
  Checkout identity, HEAD and path checks still apply. GitHub merge queries remain a separate
  `--github-merged-worktrees` opt-in.

Main checkouts, locked worktrees and the checkout containing the current directory remain
protected. Targets must remain strictly below a supplied scan root and pass the existing
protected-path, containment, symlink and filesystem guards. Aggressive mode is not permission to
remove arbitrary directories of source code. Git still refuses force-removal of worktrees
containing submodules. Dry-run applies the same decisions and withholds removal.

`--clear-caches` alone retains its existing scope and does not enable worktree removal. A complete
home-and-temporary cleanup names its roots, for example
`voom --aggressive ~ /tmp`; flags do not widen an implicit `.` root.

This adds explicit exceptions to ADR 0016's local-content, stale-age and dangling-checkout rules
rather than changing the ordinary sweep default. Artifact and cache opt-ins retain their
existing proof requirements. An untagged custom Cargo target is not identified by Cargo-shaped
files alone: Cargo may have built into a directory that also holds source. A caller can use
`--include` after verifying that the whole named directory is disposable.

The implementation lives in `src/worktrees/`, with CLI selection
in `src/cli/`.

## Consequences

Positive:

- One explicit flag requests artifact, dependency, cache, worktree and unused Docker cleanup.
- Dangling checkouts no longer require manual deletion after a deliberate aggressive sweep.
- Ordinary worktree removal keeps the conservative rules that motivated ADR 0016.

Negative / risks:

- Dependencies, models and downloads may require expensive reinstalls or be unavailable later.
- Local files removed in aggressive mode are permanently lost, including ignored configuration
  and uncommitted source. A merged HEAD does not prove those files were completed elsewhere.
- A dangling checkout cannot prove that its former commits remain reachable. Removing it accepts
  that uncertainty; preserving a repository branch cannot recover its lost local files.

## Alternatives considered

- **Always discard local contents of merged worktrees:** rejected — ancestry proves commits,
  not uncommitted work, as ADR 0016 demonstrates.
- **Keep all dangling and dirty checkouts:** rejected for an explicit aggressive request — it
  leaves the requested cleanup incomplete and requires manual deletion.
- **Archive every checkout before removal:** rejected — the owner requested no recovery archive,
  and another copy would retain the disk usage being reclaimed.
- **Make cache clearing imply worktree removal:** rejected — clearing regenerable caches and
  discarding source checkout contents express different user intent.
