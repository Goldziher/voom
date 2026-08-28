# 0009 — Git Hook Integration: pre-commit and poly Catalogs

- Status: Accepted
- Date: 2026-08-28

## Context

Commit time is a natural moment to notice that a repository is carrying build artifacts: the
developer is already stopped, already reading tool output, and already in a state where a
surprise is cheap. It is also the moment when a destructive tool is *least* welcome — a hook
that deletes a `target/` directory mid-commit turns a two-second commit into a ten-minute
rebuild, and nobody will tolerate that twice.

Two hook ecosystems matter here. The `pre-commit` framework is the broad standard and
discovers hooks from a `.pre-commit-hooks.yaml` at the root of the producing repository.
poly, the linter used across these projects, has its own catalog format
(`poly-hooks.toml`) whose consumers opt in explicitly via `[[hooks.sources]]`. Both are
producer-side manifests: voom ships them, and other repositories consume them.

## Decision

voom ships **both catalogs**, each exposing **two hooks** with a deliberate asymmetry in
their defaults.

- **`voom-report`** — the default and the one documented first. Runs `--dry-run --report
  --exit-code`, so it *reports* artifacts and fails the commit without deleting anything.
  Non-destructive, fast, and safe to adopt blindly.
- **`voom-prune`** — actually deletes. Opt-in only, never suggested as the starting point.

`.pre-commit-hooks.yaml` uses `language: rust`, which makes the `pre-commit` framework
`cargo install` the crate from the pinned `rev`. This is how uncomment does it and it avoids
shipping a download shim, at the cost of requiring a Rust toolchain in the consumer's
environment. Both hooks set `pass_filenames: false` and `always_run: true` — voom operates on
a tree, not on a staged file list, so the file arguments `pre-commit` would otherwise pass
are meaningless to it.

`poly-hooks.toml` declares `version = 1` and, per poly's producer schema, gives each hook an
`id`, `stages`, and at least one `[[hooks.paths]]` block — setting `run` at the hook level is
a hard error in poly. Three channels are offered, matching voom's distribution (ADR 0010):

| Channel | `check` | `run` |
| --- | --- | --- |
| `system` | `command -v voom` | `voom` |
| `npx` | `command -v npx` | `npx -y @goldziher/voom@X.Y.Z` |
| `uvx` | `command -v uvx` | `uvx voom-cli==X.Y.Z` |

Both hooks are `workspace = true` (whole-project, no filenames) and pinned to an exact
version in every `run` and `install` string, so a consumer's behaviour cannot drift when a
new voom is released. The version pinning means `poly-hooks.toml` is a release surface and
must be rewritten by the version bump script (ADR 0010).

The `--exit-code` flag exists for exactly this purpose: it makes `--dry-run` exit `3` when
artifacts are found (ADR 0007), so a hook can fail on findings without parsing output.

## Consequences

Positive:

- The safe hook is the default and the documented one, so the common adoption path cannot
  cost anybody a rebuild. This is the whole reason for the two-hook split.
- Shipping both catalogs means consumers use whichever runner they already have, rather than
  voom dictating a toolchain.
- Three poly channels mean a consumer without a Rust toolchain still gets a working hook via
  `npx` or `uvx`, which `language: rust` alone cannot offer.
- Exact version pinning makes hook behaviour reproducible across a team and across time.

Negative / risks:

- `language: rust` requires a Rust toolchain and compiles voom on first use, which is slow
  and will surprise `pre-commit` users in non-Rust repositories. The poly `npx`/`uvx`
  channels are the answer, but `pre-commit` users do not get them.
- The pinned version appears in many places in `poly-hooks.toml`; forgetting to bump it ships
  a release whose hooks still install the previous one. The bump script must cover this file,
  and ADR 0010's CI version gate should check it.
- Offering `voom-prune` at all means somebody will enable it and lose a build cache at an
  inconvenient moment. Documenting it as opt-in is the only mitigation short of not shipping
  it.
- A commit-time full-tree scan adds latency proportional to repository size, even in report
  mode.

## Alternatives considered

- **Only ship a destructive hook:** rejected — deleting a build cache during a commit is a
  hostile default, and the first person it happens to uninstalls the hook permanently.
- **Only ship a reporting hook:** rejected — some repositories genuinely want artifacts gone
  before a commit, and refusing to support it would push them into hand-written hooks with no
  safety rails at all.
- **`language: script` with a downloader shim** (the pattern ai-rulez uses): rejected for
  `pre-commit` — it means maintaining a bash downloader with checksum verification for every
  platform, when `language: rust` gets correctness for free. The poly catalog already covers
  the no-toolchain case through `npx`/`uvx`.
- **A `pre-push` or `post-commit` stage instead of `pre-commit`:** rejected as the default —
  `pre-commit` is where developers already look, though the poly catalog's `stages` field
  leaves the choice open to consumers.
- **Floating the pinned version** (`@latest` in the `run` strings): rejected — a hook whose
  behaviour changes without a config change is unreviewable and unreproducible.
