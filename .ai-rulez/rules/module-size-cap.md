---
priority: critical
---

# Module size cap — 1000 lines

No file in `src/` exceeds **1000 lines**. This is enforced by poly's `rust-max-lines` hook at
commit time, so a violation blocks the commit rather than arriving in review.

## Why it is critical here

The cap is not about aesthetics. voom's correctness argument (ADR 0002) rests on a classifier
that a reviewer can hold in their head and a catalog whose entries can each be checked
individually. A 2000-line `classify.rs` is one where a wrong anchor hides, and a wrong anchor
deletes someone's source directory.

## The layout it protects

```text
src/
  cli.rs        argument parsing and command dispatch
  config/       hierarchical TOML load, merge, and resolution (ADR 0004)
  catalog/      the static ecosystem table (ADR 0003) — one module per group
  scan.rs       the ignore-blind parallel walker (ADR 0005)
  classify.rs   marker anchoring: candidate + markers -> verdict (ADR 0002)
  policy.rs     keep policies: age, size, include/exclude (ADR 0004)
  delete.rs     the safety rails and the actual removal (ADR 0006)
  report/       human and JSON renderers (ADR 0007)
  watch.rs      debounced notify loop (ADR 0008)
```

## When a file approaches the cap

Split along a seam that already exists, not at an arbitrary line. The catalog splits by
ecosystem group. The reporter splits by output format. `config/` splits by load, merge, and
resolve. If no seam presents itself, that is the signal the module is doing two things — find
the two things before reaching for the line count.

Do not evade the cap with `include!`, by moving code into a macro, or by reformatting to
longer lines. If a genuine exception is needed, raise it as an ADR amendment.

Related: [[perf-discipline]], [[safety-first]].
