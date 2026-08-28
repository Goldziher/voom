---
priority: critical
---

# Safety first — voom deletes, and there is no undo

voom removes directories permanently, by default, with no confirmation prompt, against trees
as large as `$HOME`. Every other concern in this repository is subordinate to not deleting
something the user wrote. Read `adrs/0002-marker-anchored-classification.md` and
`adrs/0006-deletion-semantics.md` before touching classification, the catalog, or deletion.

## The rules

- **Never classify by directory name alone.** A candidate is an artifact only when a marker
  file proves its ecosystem at the declared anchor position. `target/` is Rust's only when a
  `Cargo.toml` sits beside it. A catalog entry with no marker glob is a bug, not a shortcut,
  and a construction-time test rejects it.
- **Every new catalog entry ships two fixtures**: one tree where the marker is present and
  the artifact is removed, and one where the artifact directory exists *without* its marker
  and is left untouched. The second is the one that matters — it is the regression test for
  data loss.
- **The protected-path denylist is append-only.** Entries may be added. Removing or narrowing
  one requires an ADR amendment explaining why it was safe, never a drive-by edit.
- **Never weaken containment.** Every deletion target is canonicalized and must resolve
  strictly below a scan root. Do not add a code path that deletes a path the walker did not
  produce.
- **Symlinks are never followed and never deleted through.** A symlinked `target/` pointing
  into a source tree must not become a deletion of that source tree.
- **`--dry-run` is the same pipeline with the final step withheld** — never a parallel
  implementation. If dry-run and a real run could ever disagree, the design is wrong.
- **Dependency directories stay out of the built-in catalog.** `node_modules/`, composer
  `vendor/`, Go `vendor/`, mix `deps/` are not artifacts. No flag turns them on; only an
  explicit user path reaches them.

## When you are unsure

Prefer a false negative. Leaving an artifact on disk costs the user some gigabytes; deleting
a directory they wrote costs them work they cannot get back. If a marker only *probably*
proves an ecosystem, mark the entry `default_on = false` and say why in its `note`, rather
than enabling it and hoping.

Related: [[perf-discipline]], [[tdd-and-hooks]], [[artifact-catalog]].
