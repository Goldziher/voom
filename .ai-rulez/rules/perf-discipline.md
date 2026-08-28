---
priority: high
---

# Performance discipline — traversal is the hot path

voom's claim is that it sweeps a home directory in seconds. Everything expensive happens
while enumerating directory entries; classification, policy evaluation, and reporting are
noise by comparison. Optimize the walk, and leave the rest readable.

See `adrs/0005-traversal-and-parallelism.md` for the decisions behind this.

## Non-negotiable

- **The `ignore` walker's ignore handling stays disabled.** `git_ignore(false)`,
  `git_global(false)`, `git_exclude(false)`, `ignore(false)`, `parents(false)`,
  `hidden(false)`. Build artifacts are exactly what `.gitignore` lists — re-enabling any of
  these silently reduces recall to near zero, and no user would report it as a bug. There is
  a regression test asserting a gitignored `target/` is still found
  (`src/scan.rs::should_find_a_gitignored_target`); it does not get deleted or skipped.
  `follow_links(false)` is set in the same call and is a safety rail, not a tuning knob.
- **Prune subtrees, don't filter results.** Never descend into a matched artifact, a VCS
  directory, a dependency directory, a tool-cache root, or a configured `exclude` path.
  Directory-level pruning is where essentially all the wall-clock saving comes from — far
  more than any constant-factor tuning inside the loop. The one deliberate exception is an
  artifact declared *inside* a pruned directory (`vendor/bundle/`, `deps/build/`): the walker
  constructs that one sub-path, stats it, and still prunes the rest.

## In the classify path

- **One `read_dir` per anchor directory, memoized for the run.** Marker resolution (ADR 0002)
  probes an anchor directory once and caches the result, so an `Ancestor(n)` climb and every
  sibling candidate share one listing. There is a test counting the probes
  (`src/classify.rs::should_list_each_anchor_directory_exactly_once`) — keep it passing.
- **Memoize per directory**, and memoize the ancestor chain — an `Ancestor(n)` lookup in a
  deep tree must not re-check the same parents for every sibling. `voom.toml` discovery
  (ADR 0004) is a separate, separately memoized pass in `config/resolve.rs`, not a reader of
  the classifier's listing; each directory is answered once per run either way.
- **No allocation in the inner loop.** Match against `&Path` and `&OsStr`; do not build
  `String`s or `PathBuf`s to compare names. Compile globs once at config load, never per
  candidate.
- **`DirEntry::file_type()` before `metadata()`** — the walker often has the type already and
  a stray `metadata()` call is a syscall per entry.

## In the size/delete path

- Fan out over `rayon`; sizing and removal are independent per artifact.
- Size accounting walks each artifact once. Do not size an artifact you are about to delete
  twice because the reporter wanted a number.
- Default to available parallelism, honour `-j`, and remember that saturating cores is wrong
  on spinning disks and network filesystems.

## Measuring

Claims in the README come from `hyperfine` against a real tree, not from intuition. When you
change the walk, measure before and after on the same tree and put the numbers in the PR. A
change that is "obviously faster" and unmeasured does not land.

Related: [[safety-first]], [[module-size-cap]].
