# 0020 — Detached Removal, the Tracked-Files Rail, and the Index-Guided Walk

- Status: Accepted
- Date: 2026-10-24

## Context

A real `voom && voom /tmp && voom --clear-caches` over one developer machine took 5,360 seconds.
Almost all of it was unlinking: 49 GB of package caches and 181 GiB of Bazel output bases — tens
of millions of entries — removed while the user waited. The machine runs two endpoint-security
agents that vet file operations, and on it voom's descriptor-relative removal (ADR 0019) unlinks
about 25,000 entries a second against `rm -rf`'s 9,000. No change to the walk moves that floor by
the two orders of magnitude the run needed; only not waiting for it can.

The same runs removed `armis/tenant_server/fe/client/dist` from a repository's main checkout and
from every one of its linked worktrees. The directory sits beside a `package.json`, so the marker
proved it — and it holds an icon sprite the repository commits. Every checkout was left with
deleted tracked files.

The scan was the remaining cost: 48 seconds over `$HOME`, most of it in sixty checkouts of one
repository — about a million directories. Profiling showed `open` in the walker's `read_dir` as
70% of the scan. Measured over 905,000 of those directories: `open` alone 16.6 s, an `lstat` of
each 3.6 s. In one worktree 22,224 directories were tracked and 161 held anything untracked.

## Decision

**Detached removal.** In the CLI, once [`Guard::check`] has passed a path, the artifact is renamed
into a trash directory voom owns (`voom/trash` under the platform cache directory, or
`VOOM_TRASH_DIR`) in one `rename`, and the run reports it `Detached`. When the run ends it starts
`voom reap --background` in its own process group at lowered priority. The reaper takes an
exclusive lock on the trash, removes each entry through a `Guard` rooted at the trash — so every
rail runs again on what it frees — and records freed bytes in a ledger the next run (and
`voom reap`) reports. The entry's name carries whether the removal was forced. An artifact on
another filesystem than the trash, or one that contains it, is removed in place, as is everything
under `--wait` or `VOOM_WAIT=1`. The library default is unchanged: nothing detaches unless the
caller asks.

**The tracked-files rail.** After the policy stage, every inferred artifact about to be removed is
checked against its repository's index with one `git ls-files -- :(literal)…` per repository; one
holding a tracked file is refused (`tracked_by_git`). An `include` and a cache are not asked.

**The index-guided walk.** On APFS, where a directory's link count is its entry count plus two, a
work tree met by the walk is handed to `scan::guided` instead of being descended. It reads the
index once (`git ls-files -z -s -t --debug`), and visits tracked directories from it: a directory
whose link count matches its tracked names and whose `mtime` is no later than the newest entry the
index recorded below it is clean and costs one `lstat`. Only dirty directories are listed, and
every untracked entry — and every tracked directory — goes through the same `Visitor::decide` the
directory walk uses, so both walks report the same findings and skips.

## Consequences

Positive:

- Removal no longer bounds a run: the 5,360-second sweep's removals take milliseconds; the reaper
  frees the space behind it.
- A crash, an interrupt or a reboot loses nothing: the trash waits for the next reaper.
- No checkout loses tracked files to a sweep again.
- Over 43 linked worktrees the scan went from 20.9 s to 5.9 s, and over `$HOME` from 32 s to 15 s,
  with identical findings and skip counts apart from the refusals above.

Negative / risks:

- A detached artifact has no size in the report that detached it; the space is not back until
  the reaper is done, and the trash holds it until then.
- An artifact on another volume than the trash is still removed in place, at the old speed.
- A guided directory that lost tracked entries, gained as many untracked ones, and then had a
  tracked entry staged again agrees with the index on both count and time, and its untracked
  entries are not seen. That is recall in one directory, never safety.
- The guided walk depends on `git ls-files --debug`'s format. A listing it cannot parse, a git it
  cannot run, or an index it cannot read sends the work tree back to the directory walk.

## Alternatives considered

- **A faster unlink:** rejected — the measured floor under endpoint security is per entry, and
  ADR 0019 already tuned it; nothing in reach is a hundred times faster.
- **Measure, then detach, so the report keeps sizes:** rejected — measurement walks every entry
  too, at a fifth of removal's cost; it would keep the hour it was meant to save.
- **FSEvents history for an incremental scan:** rejected for now — on the measured machine the
  journal grows by 84,000 event ids a minute and replays at about 12,000 events a second, so an
  hour of history replays slower than the scan it would replace.
- **`git status --ignored` for the candidates:** rejected — it walks the work tree itself, and
  took 2.7 s per worktree, single-threaded.
- **`git ls-files -d -m` to know deleted files exactly:** rejected — it `lstat`s every tracked
  file, four times as many as there are tracked directories; the `mtime` rule covers deletions.
