# 0001 — Mission and Scope: Fast, Safe, Recursive Build-Artifact Pruning

- Status: Accepted
- Date: 2026-08-28

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
