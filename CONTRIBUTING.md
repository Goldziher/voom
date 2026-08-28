# Contributing to voom

Thanks for helping out. voom deletes directories permanently, by default, on trees as large
as `$HOME` — so the bar here is correctness before features, and the review question for
almost every change is "could this delete something the user wrote?"

Read [`adrs/0001-mission-and-scope.md`](adrs/0001-mission-and-scope.md) and
[`adrs/0002-marker-anchored-classification.md`](adrs/0002-marker-anchored-classification.md)
before your first change. They are short and they explain most of the design.

## Setup

```bash
git clone https://github.com/Goldziher/voom.git
cd voom
cargo build
poly hooks install
```

## The gate

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
poly hooks run pre-commit --all-files   # authoritative — runs what CI runs
```

Do not commit with `--no-verify`. Commits are conventional and signed
(`feat:`, `fix:`, `perf:`, `docs:`, `refactor:`, `test:`, `build:`, `ci:`).

## Adding an ecosystem

This is the most common contribution. The checklist:

1. **Find a marker file that *proves* the ecosystem.** Not one that's commonly present —
   one that proves it. `build.zig` proves Zig. `Makefile` proves nothing. If you cannot find
   one, stop and open an issue instead; an ambiguous marker is worse than no entry.
2. **Determine the anchor.** Is the marker a sibling of the artifact, or can it be up to `n`
   levels above? For `Ancestor(n)`, justify `n` from real project layouts.
3. **Add the entry** to the right module under `src/catalog/`.
4. **Write both fixtures** under `tests/`:
   - marker present + artifact present → the artifact is removed;
   - **artifact present, marker absent → the artifact survives.**

   The second is mandatory. It is the regression test for data loss, and a PR without one
   will not be merged.
5. **Mark ambiguous artifacts `default_on: false`** with a `note` explaining the opt-in —
   for names that are also plausible source directories in that ecosystem (`build/`,
   `bin/`, `pkg/`) or where removal is expensive rather than cheap (`.venv/`).
6. **Update the docs**: the ecosystem table in `README.md` and the table in
   [`adrs/0003-builtin-catalog.md`](adrs/0003-builtin-catalog.md).

Dependency directories (`node_modules/`, composer `vendor/`, Go `vendor/`, mix `deps/`) are
out of scope and will not be accepted — see ADR 0001.

## Things that will get a PR rejected

- Classifying by directory name without a marker.
- Removing or narrowing an entry in the protected-path denylist without an ADR amendment.
- Re-enabling any of the walker's ignore mechanisms. Build artifacts are exactly what
  `.gitignore` lists; this silently reduces recall to nearly zero. See
  [ADR 0005](adrs/0005-traversal-and-parallelism.md).
- Making `--dry-run` a separate code path from a real run.
- "Fixing" a failing safety test by changing the test. Those tests are the specification.

## Architecture decisions

Substantive design changes get an ADR in [`adrs/`](adrs/). Follow the conventions in
[`adrs/README.md`](adrs/README.md): amend an existing ADR in place with an `Updated:` line
when a decision evolves, and only write a new one when a decision is genuinely replaced.

## Releasing

Maintainers only. The version lives in five files and they must agree:

```bash
./scripts/bump-version.sh 0.2.0
git add -A && git commit -m "chore(release): v0.2.0"
git tag v0.2.0 && git push origin main v0.2.0
```

Always use the script — `poly-hooks.toml` pins the version into every hook channel string
and is the one people forget. Pushing the tag triggers
[`.github/workflows/publish.yaml`](.github/workflows/publish.yaml), which builds five
platform binaries, publishes to crates.io, npm, and PyPI, and updates the Homebrew tap.

**Never rename `publish.yaml`.** OIDC trusted publishing on all three registries is bound to
that exact filename. See [ADR 0010](adrs/0010-distribution-and-naming.md).

Release candidates use `vX.Y.Z-rc.N`; PyPI sees `X.Y.ZrcN`. Homebrew is skipped for
pre-releases.
