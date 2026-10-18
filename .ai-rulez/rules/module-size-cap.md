---
type: Decision
title: Module Size Cap
x-ai-rulez:
  kind: rule
  id: module-size-cap
  metadata:
    priority: critical
---

# Module size cap — 1000 lines

No file in `src/` exceeds **1000 lines**.

**One test enforces this.** `tests/docs.rs::no_source_file_exceeds_the_module_size_cap` walks `src/`
and fails on any file over the cap, so the limit is checked by the ordinary test run rather than
by a hook or CI step of its own. Check by hand as well — a file added since the last test run is
one it has not seen yet:

```bash
wc -l src/*.rs src/*/*.rs | sort -rn | head
```

`cli/mod.rs` and `scan/mod.rs` sit nearest the cap, then `delete/mod.rs` (a directory module since
its removal pool moved to `delete/pool.rs`). One of them crosses it before the rest of the tree
gets near; keep the `sort -rn` output in the pull request that gets close.

## Why it is critical here

The cap is not about aesthetics. voom's correctness argument (ADR 0002) rests on a classifier
that a reviewer can hold in their head and a catalog whose entries can each be checked
individually. A 2000-line `classify.rs` is one where a wrong anchor hides, and a wrong anchor
deletes someone's source directory.

## The layout it protects

One module per pipeline stage, plus the shared pieces each stage reaches for. The full map,
with what every file is for, is in [[architecture]] — keep that one current rather than a
second copy here.

## When a file approaches the cap

Split along a seam that already exists, not at an arbitrary line. The catalog splits by
ecosystem group. The reporter splits by output format. `config/` splits by load, merge, and
resolve. If no seam presents itself, that is the signal the module is doing two things — find
the two things before reaching for the line count.

Do not evade the cap with `include!`, by moving code into a macro, or by reformatting to
longer lines. If a genuine exception is needed, raise it as an ADR amendment.

Related: [[perf-discipline]], [[safety-first]].
