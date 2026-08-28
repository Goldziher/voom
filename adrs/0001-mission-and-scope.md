# 0001 — Mission and Scope: Fast, Safe, Recursive Build-Artifact Pruning

- Status: Accepted
- Date: 2026-08-28
- Updated: 2026-08-28 — `--caches` shipped; recorded the wider skip list and why.
- Updated: 2026-08-28 — dependency directories are in the catalog, off by default, behind
  `--clean-dependencies`. See the second amendment.

## Context

A developer machine accumulates build artifacts faster than anyone reclaims them. A single
workspace directory on the author's machine holds **16 distinct ecosystems** by marker file
(29 `package.json`, 27 `Cargo.toml`, 22 `pyproject.toml`, 9 `composer.json`, 7
`Package.swift`, 4 `Makefile`, 4 `go.mod`, 2 `CMakeLists.txt`, and one each of
`pubspec.yaml`, `Project.toml`, `pom.xml`, `mix.exs`, `Gemfile`, `DESCRIPTION`, `build.zig`,
`build.sbt`, `build.gradle.kts`). Each ecosystem leaves its own droppings in its own layout,
and no single tool sweeps all of them.

The existing tools are all partial. `cargo-sweep` and `cargo clean` know only Rust.
`npkill` is an interactive TUI restricted to `node_modules`. `kondo` covers more ground but
is interactive-first and its detection is name-driven. A hand-rolled
`find . -name target -exec rm -rf` is the tool most people actually reach for, and it is
exactly the thing that eventually deletes a hand-written `bin/` or a vendored `obj/`.

What is missing is a tool that can be pointed at an entire home directory, will finish in
seconds rather than minutes, will not delete source, and will say precisely what it did.

## Decision

voom is a **single statically-linked CLI binary** that recursively scans a tree, identifies
build artifacts across every ecosystem it finds, and deletes them — fast enough and safe
enough to run against `$HOME`.

In scope for v1:

- Recursive discovery across a directory tree, parallelized to saturate available cores
  (ADR 0005).
- Marker-anchored classification, never name-only matching (ADR 0002).
- A built-in, data-driven catalog of ecosystems (ADR 0003).
- Hierarchical TOML configuration with include/exclude paths and per-path, per-ecosystem
  keep policies covering age and size (ADR 0004).
- Deletion by default with `--dry-run`, behind non-bypassable safety rails (ADR 0006).
- Colored human reporting and machine-readable JSON (ADR 0007).
- A watch mode for continuous pruning (ADR 0008).
- Git hook integration for both the `pre-commit` framework and poly (ADR 0009).
- Distribution through crates.io, npm, PyPI, Homebrew, and GitHub release binaries
  (ADR 0010).

Explicitly **out** of scope for v1:

- **Dependency directories.** `node_modules/`, composer's `vendor/`, and mix's `deps/` are
  not artifacts — they are expensive-to-refetch caches whose removal costs the user network
  time and can break offline work. They are excluded from the built-in catalog entirely, and
  can only be reached through explicit user configuration.
- **Global tool caches.** `~/.cargo/registry`, `~/.npm/_cacache`, `~/.gradle/caches` and
  friends are machine-global, shared across every project, and have their own eviction
  tooling. Out of v1; possibly a later opt-in `--caches` mode.
- **A daemon, a GUI, or an interactive TUI.** voom is a non-interactive command. Watch mode
  is a foreground process, not a background service.
- **Anything that writes files.** voom only ever removes; it never rewrites, moves, or
  archives.

## Consequences

Positive:

- One command replaces a per-ecosystem zoo, which is the whole reason to build this.
- Being non-interactive by default makes it composable — usable from hooks, CI, cron, and
  scripts, which is where recurring cleanup actually belongs.
- Refusing to touch dependency directories and global caches keeps the blast radius to
  things that are cheap to regenerate from source that is already on disk. This is what
  makes "point it at `$HOME`" a defensible thing to suggest.
- A single binary with no runtime dependencies installs through five channels and works
  identically in a container, on CI, and on a laptop.

Negative / risks:

- Breadth means the catalog is never finished; every new ecosystem is a maintenance
  obligation, and a wrong entry is a data-loss bug rather than a cosmetic one. ADR 0002 and
  the testing rules exist to bound this.
- Excluding `node_modules` will surprise users who expect the biggest directory on their
  disk to be swept. This must be stated prominently in the README, not buried.
- "Safe enough for `$HOME`" is a strong claim that has to be earned on every release; a
  single false positive in the catalog damages trust permanently.

## Alternatives considered

- **A thin wrapper that shells out to each ecosystem's own clean command** (`cargo clean`,
  `./gradlew clean`, `mix clean`): rejected — requires every toolchain to be installed,
  spawns hundreds of slow processes, and many ecosystems have no clean command at all. The
  speed goal alone rules it out.
- **Extending an existing tool (kondo, cargo-sweep):** rejected — kondo's detection is
  name-driven and interactive-first, which is the opposite of both ADR 0002 and the
  composability goal; retrofitting either would be a rewrite wearing someone else's name.
- **A shell script or a documented `find` incantation:** rejected — no parallelism, no
  anchoring, no configuration, no reporting, and no way to test it. This is the status quo
  voom exists to replace.
- **Interactive TUI as the primary interface:** rejected — an interface a human must sit in
  front of cannot run from a git hook or a cron job, which is where cleanup should happen.

## Amendment — 2026-08-28

The "possibly a later opt-in `--caches` mode" floated above has shipped, as `--caches`
(`src/caches.rs`). The skip list it toggles is wider than the original wording promised.
Alongside machine-global download caches (`~/.cargo/registry`, `~/.npm`, `~/.gradle`), it also
covers *installed toolchains* (`~/.pyenv`, `~/miniforge3`, `~/google-cloud-sdk`, `~/fvm`,
`~/.hermes`) and per-user app-data roots (`~/Library/Application Support`, `~/AppData/Local`,
`~/AppData/Roaming`).

The wider scope follows directly from `safety-first`'s preference for a false negative over a
false positive, applied to a case marker anchoring (ADR 0002) cannot resolve on its own. An
installed pnpm under `~/.cache/node/corepack/*/pnpm/*/dist` sits beside a real `package.json`
and is, to the classifier, indistinguishable from a project that was built there — "a project
that was built here" and "a program that was installed here" are the same shape on disk, so
nothing short of location can tell them apart. `--caches`'s skip list draws that line before
classification runs. Drawing it wide costs a user some reclaimable disk space; drawing it narrow
costs them a working toolchain, which is the more expensive mistake by the same reasoning ADR
0002 already applies to markers.

The numbers behind the decision were measured on the author's `$HOME`, not estimated: before the
skip, a sweep found 469 artifacts, 301 of them inside what became the skip list — including that
installed pnpm. After the skip, the same sweep finds 174 artifacts, 172 of them real projects
under `~/workspace`. `--caches` restores the full set (475).

Like the protected-path denylist (ADR 0006), `CACHE_DIRS` (`src/caches.rs`) is append-only:
entries may be added, but removing or narrowing one requires an ADR amendment explaining why it
is safe, never a drive-by edit.

## Amendment — dependency directories become reachable, off by default — 2026-08-28

The "Explicitly out of scope" list above says dependency directories "are excluded from the
built-in catalog entirely, and can only be reached through explicit user configuration", and
ADR 0003 restates it as "no flag turns them on". That is the hardest line in this document and
it is now amended, at users' explicit request: `node_modules/`, composer's `vendor/`, Go's
`vendor/`, and mix's `deps/` **are in the catalog**, as ordinary `default_on = false` entries,
and `--clean-dependencies` turns the group on in one flag.

**Nothing about a default run changes.** Every one of these entries is `Artifact::off` with a
mandatory note, which is the same mechanism `python.venv` has used since 0.1.0. A run that asks
for nothing sweeps none of them, reports none of them, and — because the walker still prunes at
the directory level — does not so much as enumerate one.
`a_dependency_directory_artifact_is_never_on_by_default` (`src/catalog/mod.rs`) asserts the
`default_on` half over the whole table, and
`should_never_remove_a_dependency_directory_by_default` (`tests/safety.rs`) asserts the
end-to-end half against a real tree. The second was watched failing against an
`Artifact::on("node_modules/")`.

**The original exclusion was right and remains the default.** The reasoning in the decision
above is unchanged: these are network-fetched caches, removing one costs a re-download and can
break offline work, and that is a materially different bargain from removing build output that
regenerates from source already on disk. It is the *default* that this reasoning determines,
not the availability. `safety-first`'s "prefer a false negative" is a rule about what voom does
when nobody has told it otherwise; a user who types `--clean-dependencies` has told it
otherwise.

**Why the catalog rather than a bespoke mechanism.** Every one of these directories has a real
marker file — `package.json`, `composer.json`, `go.mod`, `mix.exs` — so marker anchoring
(ADR 0002) applies to them unchanged, and a `node_modules/` with nothing proving it is left
alone exactly like an unproven `target/`. Expressing them as catalog entries means the
generated fixtures cover them (both the positive and the negative), configuration layering and
`[[paths]]` rules address them by spec, keep policies apply, and both renderers report them —
none of which a special case would have inherited. `--clean-dependencies` is sugar over
`--enable`, listed in `catalog::DEPENDENCY_ARTIFACTS`; there is no second selection path.

The alternative this replaces — telling users to write an unanchored `include` — was strictly
worse. `include` (ADR 0004) removes a path with *no* marker requirement at all, so the advice
voom was giving amounted to "disable the safety rail and name the directory yourself", on the
one case where users most predictably want the behaviour. An opt-in that keeps marker anchoring
is safer than the workaround it removes the need for.

What does **not** change:

- `.git/`, `.hg/` and `.svn/` are unreachable by this or any flag. They are in
  `scan.rs::NEVER_DESCEND` for a different reason and are neither descended into nor
  classified.
- The walk never enters a dependency directory. Classification happens at the directory itself
  and the walker still returns `WalkState::Skip`, so an enabled `node_modules/` costs one
  memoized anchor listing and zero entries below it
  (`should_not_enumerate_inside_an_enabled_dependency_directory`).
- Build output declared *inside* a dependency directory — Ruby's `vendor/bundle/`, Julia's
  `deps/build/` — is reached exactly as before, with the flag on or off.
- `bower_components/` stays out. It is pruned, but no ecosystem declares it, because no marker
  distinguishes a Bower install from any other directory of that name.
- Machine-global caches and installed toolchains remain a *location* exclusion under
  `--caches`, for the reason the previous amendment gives: marker anchoring cannot tell an
  installed program from a project built in place.
