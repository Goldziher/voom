---
priority: high
---

# TDD and the commit gate

## Red, green, refactor — with real trees

Write the failing test first. For voom that almost always means building a **fixture
directory tree** with `tempfile::TempDir` and asserting on what survives, because the unit
under test is "what does this tool do to a filesystem" and nothing smaller is convincing.

Every catalog entry (ADR 0003) is covered by **two** fixtures, and the second is the one that
matters:

1. **Positive** — the marker file is present, the artifact is present, voom removes it.
2. **Negative** — the artifact directory is present *without* its marker, voom leaves it
   alone. This is the data-loss regression test.

**Both are generated, not hand-written.** `tests/catalog_fixtures.rs` iterates `CATALOG` and
builds each tree from `tests/support/mod.rs::fixture`, so a new entry is covered the moment it
lands and no entry can be missing one. `tests/fixtures/` on disk is empty; do not add a tree
there for an ordinary entry.

What that suite does *not* cover is ecosystem-specific shape. The generated tree puts the
artifact and the marker at the root, so an `Ancestor(n)` anchor is only exercised at distance
zero. An entry whose anchor climbs needs its own test asserting the climb and the bound — see
`tests/safety.rs::should_sweep_a_nested_dotnet_artifact_and_stop_at_the_ancestor_bound`.

Beyond the per-entry fixtures, keep these covered:

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
will: lint, format, file safety, the cargo whole-project tools, and `ai-rulez-validate`. Do not
commit with `--no-verify`.

It does **not** check the 1000-line module cap; nothing does. Check that one by hand
([[module-size-cap]]).

## Commits

Conventional commits, signed. `feat:`, `fix:`, `perf:`, `docs:`, `refactor:`, `test:`,
`build:`, `ci:`. A change to the catalog is `feat(catalog):` when it adds an ecosystem and
`fix(catalog):` when it corrects an anchor — the distinction matters because the latter may
have been deleting the wrong thing.

Related: [[safety-first]], [[release-versioning]].
