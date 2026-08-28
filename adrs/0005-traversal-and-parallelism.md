# 0005 — Traversal and Parallelism: Ignore-Blind Walking Over rayon

- Status: Accepted
- Date: 2026-08-28

## Context

voom's headline claim is that it can sweep an entire home directory in seconds (ADR 0001).
That makes traversal the hot path: everything else — classification, policy evaluation,
reporting — is cheap relative to the cost of enumerating a few million directory entries.

The obvious building block is the `ignore` crate, which powers ripgrep: a fast parallel
directory walker with per-directory `.gitignore` handling built in. But its defining
feature is a trap here. **Build artifacts are precisely what `.gitignore` lists.** A walker
that respects `.gitignore` skips `target/`, `dist/`, `__pycache__/`, and `.venv/` — which is
to say, it skips everything voom exists to find. Enabled by accident, voom would report a
clean machine and reclaim nothing.

The same crate is still the right choice for its other half: a work-stealing parallel walker
with cheap `DirEntry` metadata and a `filter_entry`-style hook that can prune whole subtrees
before descending.

## Decision

Use `ignore::WalkBuilder` **with every ignore mechanism explicitly disabled**:

```rust
WalkBuilder::new(root)
    .git_ignore(false)      // artifacts are gitignored — this is the whole point
    .git_global(false)
    .git_exclude(false)
    .ignore(false)          // .ignore / .rgignore
    .parents(false)         // no ignore file above the scan root
    .hidden(false)          // .venv, .gradle, .zig-cache, .terraform are all hidden
    .follow_links(false)    // ADR 0006
    .same_file_system(true) // ADR 0006, overridable
    .threads(jobs)
    .build_parallel()
```

Every one of these is load-bearing and each is covered by a regression test asserting that a
`.gitignore`d `target/` is still found. Getting any of them backwards silently reduces
recall to near zero, which is a failure mode no user would report as a bug — they would just
conclude their disk was already clean.

Subtree pruning comes from voom's own logic instead:

- **Never descend into a matched artifact.** Once a directory is classified, it is a unit —
  its size is measured and it is removed wholesale. Walking inside it is wasted work and
  would produce nested findings that confuse the report.
- **Never descend into `.git/`,** or into the dependency directories ADR 0001 puts out of
  scope (`node_modules/`, composer `vendor/`, mix `deps/`). These are the largest, deepest
  subtrees on a typical disk and skipping them is where most of the wall-clock saving comes
  from.
- **Never descend into an `exclude` path** from configuration (ADR 0004).

Work is split across two phases, both `rayon`-parallel:

1. **Discover and classify.** The parallel walker emits directory entries; the classifier
   resolves markers (ADR 0002) and `voom.toml` files (ADR 0004) from the directory listing
   the walker has already produced, memoized per directory, so anchoring costs no extra
   `read_dir`. Ancestor lookups walk a cached chain rather than re-statting.
2. **Size and delete.** Confirmed artifacts are fanned out over a `rayon` pool — recursive
   size accounting and removal are independent per artifact and embarrassingly parallel.

Concurrency defaults to the available parallelism and is capped by `-j <N>`.

## Consequences

Positive:

- Near-linear speedup on multi-core machines, on a workload that is I/O- and syscall-bound
  and therefore benefits from many in-flight operations.
- Pruning `.git/`, `node_modules/`, and matched artifacts at the directory level removes the
  overwhelming majority of entries a naive walk would visit — the single largest win
  available, and larger than any constant-factor tuning.
- Folding marker and `voom.toml` resolution into the walker's existing directory listing
  means ADR 0002's safety and ADR 0004's hierarchy cost close to nothing.
- Treating a matched artifact as an indivisible unit keeps the report one line per artifact,
  which is what makes ADR 0007's output readable.

Negative / risks:

- The disabled-ignore configuration is the highest-consequence, lowest-visibility setting in
  the codebase. A well-meaning "why aren't we respecting gitignore?" change would gut the
  tool silently. The comment above must survive, and the regression test is mandatory.
- Saturating cores on a spinning disk or a network filesystem can be slower than a serial
  walk and will make the machine unresponsive; `-j` is the escape hatch and the README should
  say so.
- Parallel traversal yields nondeterministic ordering, so the reporter must sort (ADR 0007).
- Memoizing marker resolution per directory holds memory proportional to the number of
  directories in flight; on a very wide tree this needs bounding.
- `same_file_system(true)` means a `$HOME`-wide run silently skips mounted volumes. This is
  the safe default (ADR 0006) but must be reported, not hidden.

## Alternatives considered

- **`ignore` with default settings:** rejected — respects `.gitignore` and skips hidden
  directories, which between them hide essentially every build artifact. Recorded here
  explicitly because it is the mistake a reader is most likely to make.
- **`walkdir`:** rejected — serial, and lacks the subtree-pruning hook that provides the
  largest performance win. `jwalk` was also considered but `ignore` is already the workspace's
  walker of choice (uncomment uses it) and gives the same parallelism.
- **Hand-rolled traversal over `std::fs::read_dir` with a work queue:** rejected — reimplements
  `ignore`'s work-stealing walker with worse platform coverage for no benefit.
- **async / tokio:** rejected — the work is syscall- and CPU-bound, not socket-bound; rayon's
  data parallelism is the right model and matches the surrounding Rust projects.
- **A single fused phase (classify and delete in the walker callback):** rejected — deletion
  during traversal mutates the tree being walked, and it makes `--dry-run` a different code
  path rather than the same pipeline with the final step withheld.
