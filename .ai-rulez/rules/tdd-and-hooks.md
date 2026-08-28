---
priority: high
---

# TDD and the commit gate

## Red, green, refactor — with real trees

Write the failing test first. For voom that almost always means building a **fixture
directory tree** with `tempfile::TempDir` and asserting on what survives, because the unit
under test is "what does this tool do to a filesystem" and nothing smaller is convincing.

Every catalog entry (ADR 0003) requires **two** fixtures, and the second is mandatory:

1. **Positive** — the marker file is present, the artifact is present, voom removes it.
2. **Negative** — the artifact directory is present *without* its marker, voom leaves it
   alone. This is the data-loss regression test. An entry without one does not land.

Beyond per-entry fixtures, keep these covered:

- A gitignored `target/` is still discovered (the ADR 0005 walker configuration).
- A symlinked artifact pointing outside the root is refused, and the target survives.
- A path that escapes its scan root is refused.
- Every protected denylist path is refused.
- `--dry-run` over a fixture leaves the tree byte-identical, and its report matches what a
  real run then removes.

Use `insta` for report snapshots (both human and JSON) so output changes are reviewed rather
than discovered. Use `assert_cmd` + `predicates` for the CLI surface and exit codes.

## Never test against a real user directory

Tests build their own trees under `TempDir`. A test that touches `$HOME`, `/tmp` directly, or
any path outside its fixture is a bug regardless of whether it passes.

## The commit gate

Before committing:

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
poly hooks run pre-commit --all-files
```

`poly hooks run pre-commit --all-files` is the authoritative gate — it runs the same checks CI
will, including the module-size cap and the ai-rulez validation. Do not commit with
`--no-verify`.

## Commits

Conventional commits, signed. `feat:`, `fix:`, `perf:`, `docs:`, `refactor:`, `test:`,
`build:`, `ci:`. A change to the catalog is `feat(catalog):` when it adds an ecosystem and
`fix(catalog):` when it corrects an anchor — the distinction matters because the latter may
have been deleting the wrong thing.

Related: [[safety-first]], [[release-versioning]].
