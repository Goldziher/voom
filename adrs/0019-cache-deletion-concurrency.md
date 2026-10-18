# 0019 — Cache Deletion: Bound Metadata Contention and Preserve Deep Fan-Out

- Status: Accepted
- Date: 2026-10-18

## Context

ADR 0005 selected a shared sixteen-worker removal pool from measurements on a
100,000-file tree. Follow-up cache cleanup remained slow, and increasing concurrency
did not consistently improve it. File count and directory layout determine the number
of metadata operations; bytes per second alone does not explain this workload.

`src/tree/mod.rs` also spent its ten-level parallel recursion budget on every directory
level. A cache with a long single-child prefix reached its wide subtree after that
budget expired and removed the subtree serially. Parallel unlink batches for small
directories added scheduling work and competed with independent directory branches.

Exploratory runs on this macOS host favored four workers for sharded and package-shaped
fixtures, but a fresh flat-file fixture regressed when the whole pool was reduced from
sixteen to four. Copies sharing APFS extents also behaved differently from freshly written
files. A four-permit gate for small-directory mutations also slowed real-cache copies
relative to a smaller shared pool. The default needs to balance these layouts without
adding another synchronization layer.

## Decision

Set the shared removal pool in `src/delete/pool.rs` to default to eight workers on Apple
platforms, subject to the existing descriptor cap. Keep the sixteen-worker default
elsewhere because the new cache measurements cover macOS only. An explicit `-j` overrides
the pool width. Use the pool itself to bound concurrency without an additional mutation gate.

On Apple platforms, `src/tree/mod.rs` unlinks fewer than 256 files in a directory serially
to avoid nested tiny batches. Larger flat directories retain parallel 64-file batches and
can use the full removal pool. Other Unix platforms use 64-file batches without the Apple
small-directory threshold.

Store single-child directory prefixes on an explicit stack during removal, spending the
ten-level recursion budget only when a directory branches. This preserves parallelism
below deep prefixes while bounding recursive branch depth. Descriptors still remain open
along the current path, and existing descriptor-exhaustion handling remains necessary.
Sizing keeps its existing depth budget.

When a full Bazel download-cache clear is requested, remove and count in one pass instead
of counting files first. `OutputBaseState::CacheCleared` records this decision accurately
without inventing stale/retained file counts; the JSON state remains `cache`.

Keep a repeatable comparison in `scripts/bench-cache-cleanup.py`, exposed as `task bench`.
It compares baseline and candidate binaries with native `rm` on flat, sharded,
package-shaped and deeply nested temporary fixtures. Setup is untimed, execution order
is seeded and shuffled, and results include individual timings and files per second.
Every voom run must remove its selected artifact and report its exact allocated byte
total without failures before its timing is accepted. Representative real-cache copies
still need separate measurement before publishing performance claims.

## Consequences

Positive:

- Apple defaults reduce metadata contention while retaining parallel flat-file removal.
- A deep prefix no longer serializes an otherwise parallel cache removal.
- Small directories leave workers available for independent branches.
- Full Bazel download-cache clearing avoids a redundant traversal.
- Repeated comparisons detect skipped work and inaccurate accounting as well as slowdowns.

Negative / risks:

- Eight workers may underutilize other Apple filesystems or workloads; `-j` is the override.
- Filesystem and security software can still dominate individual unlink operations.
- Long prefixes still require descriptors proportional to their depth.
- Warm temporary fixtures do not reproduce all real-cache, cold-cache or device behavior.

## Alternatives considered

- **Increase the removal pool width:** rejected — the measured Apple workload slowed down
  on directory-heavy layouts, and more concurrency can amplify metadata contention.
- **Reduce the entire removal pool to four workers:** rejected — freshly written flat-file
  fixtures regressed despite improvements on directory-heavy layouts.
- **Gate small-directory mutations with four permits inside a sixteen-worker pool:** rejected
  — real-cache copies were slower than with a smaller shared pool; blocking workers on a
  second concurrency limit introduced contention of its own.
- **Use one worker for every directory:** rejected — it sacrifices independent-directory
  parallelism and leaves a large flat cache on one worker.
- **Remove the depth limit entirely:** rejected — deeply branching trees would permit
  unbounded recursive scheduling; only single-child prefixes bypass the budget.
- **Change the default on every platform:** rejected — the new measurements cover macOS,
  not other operating systems or filesystems.
- **Compare reported MB/s alone:** rejected — file size changes the rate without changing
  metadata cost; fixture shape, exact cleanup results and elapsed time are also required.
