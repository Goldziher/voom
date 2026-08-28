---
priority: critical
---

# Module size cap — 1000 lines

No file in `src/` exceeds **1000 lines**.

**Nothing enforces this.** There is no `rust-max-lines` hook, no CI step, and no test that
checks it — `poly.toml` runs `lint`, `fmt`, `commit`, `file_safety` and `cargo`, none of which
counts lines. The cap holds because contributors keep it, so check it yourself before you
commit:

```bash
wc -l src/*.rs src/*/*.rs | sort -rn | head
```

`classify.rs` (975) is the closest to the cap, then `run.rs` (942) and `delete.rs` (875). Any
of the three will cross it before the rest of the tree gets near.

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
