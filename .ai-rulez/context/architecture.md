---
priority: high
---

# voom architecture

voom is a single Rust binary (edition 2024) with a thin `main` over a testable library. It
takes a directory tree and removes build artifacts from it, fast, without deleting source.

Authoritative design decisions live in `adrs/` — start with `0001-mission-and-scope.md` and
`0002-marker-anchored-classification.md`.

## The pipeline

```text
roots ──▶ scan ──▶ classify ──▶ policy ──▶ size ──▶ delete ──▶ report
          (0005)    (0002)      (0004)    (0005)    (0006)     (0007)
```

Each stage is a pure function of the previous one's output where it can be. `--dry-run` is
the same pipeline with `delete` withheld — never a separate path.

1. **scan** — `ignore::WalkBuilder` in parallel mode with *all* ignore handling disabled
   (artifacts are precisely what `.gitignore` lists). Prunes `.git/`, dependency
   directories, configured excludes, and matched artifacts at the directory level.
2. **classify** — for each candidate directory, resolve the ecosystem by finding a marker
   file at the entry's declared anchor (`Sibling` or `Ancestor(n)`). No marker, no artifact.
   Marker and `voom.toml` lookups reuse the walker's directory listing and are memoized.
3. **policy** — apply keep rules (`min_age`, `min_size`, `max_size`) and include/exclude
   globs from the merged configuration.
4. **size** — measure each artifact exactly once, fanned out over `rayon`. The one number
   feeds both the keep policies and the report.
5. **delete** — canonicalize, check the protected denylist and containment, refuse symlinks
   and filesystem crossings, then `remove_dir_all`. Failures are per-artifact and reported.
6. **report** — sort deterministically by path, render human or JSON from one result set.

`run.rs` is where the stages are wired together, and it is the file to read to see the whole
pipeline in one place.

## Module map

```text
src/
  main.rs       thin binary: parse, dispatch, map errors to exit codes
  lib.rs        library root — everything testable lives below here
  cli.rs        clap definitions and command dispatch
  run.rs        the pipeline, wired: RunOptions in, RunResult out
  config/       hierarchical TOML: load, merge, resolve (ADR 0004)
  catalog/      static ecosystem table (ADR 0003), one module per group
  caches.rs     the machine-global cache and toolchain roots a walk skips (--caches)
  scan.rs       parallel walker + subtree pruning (ADR 0005)
  classify.rs   marker anchoring (ADR 0002)
  names.rs      allocation-free file-name matching behind markers and candidates
  policy.rs     keep policies and path rules (ADR 0004)
  size.rs       per-artifact size accounting, measured once (ADR 0005)
  delete.rs     safety rails + removal (ADR 0006)
  report/       human and JSON renderers (ADR 0007)
  watch.rs      debounced notify loop (ADR 0008)
  error.rs      the library's typed error enum
  testing.rs    fixture-tree helpers, behind the `test-support` feature
```

`[lib]` + `[[bin]]` split exists so the catalog, classifier, and policy engine can be unit
tested without spawning a process. No file exceeds 1000 lines ([[module-size-cap]]).

`testing.rs` is compiled only under `#[cfg(test)]` or the `test-support` feature. The feature
exists because an integration suite in `tests/` links the library as a *non-test* target and
cannot see its `#[cfg(test)]` modules; `cargo test` turns it on through the crate's
dev-dependency on itself. Two copies of a fixture helper drift, and a drifted fixture is a
snapshot trusting output the other suite could not produce.

## Key dependencies and why

| Crate | Why |
| --- | --- |
| `clap` | CLI, derive + color + wrap_help |
| `ignore` | parallel walker with subtree pruning — used with ignore handling *off* |
| `rayon` | fan-out for sizing and deletion |
| `globset` | compiled glob matching for markers, artifacts, and config paths |
| `dirs` | the home directory, for `~` expansion and the cache-root skip list |
| `owo-colors` + `anstream` | color that respects TTY, `NO_COLOR`, and Windows consoles |
| `indicatif` | scan progress on stderr, TTY only |
| `serde` + `toml` | configuration |
| `serde_json` | `--format json` |
| `notify` + `notify-debouncer-full` | watch mode |
| `humansize` | human-readable byte counts |
| `thiserror` / `anyhow` | typed errors in the library, context at the binary boundary |

## Where to be careful

The walker configuration in `scan.rs` and the anchor logic in `classify.rs` are the two
places where a plausible-looking change causes either silent total failure (respecting
`.gitignore`) or data loss (dropping the marker requirement). Both are covered by regression
tests that exist for exactly this reason — if one starts failing, do not "fix" the test.

Related: [[artifact-catalog]], [[code-style]], [[safety-first]].
