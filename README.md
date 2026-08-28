<div align="center">

# voom

**Clean up the pink snow.**

voom finds and removes build artifacts across your whole disk — `target/`, `dist/`,
`__pycache__/`, `.gradle/`, `.zig-cache/` and 20+ ecosystems more — in one parallel pass.
It proves every candidate against an ecosystem marker file before it deletes, so it will
never take your hand-written `bin/` directory.

20+ ecosystems&nbsp;·&nbsp;marker-anchored&nbsp;·&nbsp;parallel&nbsp;·&nbsp;`$HOME`-safe&nbsp;·&nbsp;dry-run&nbsp;·&nbsp;watch mode

[![crates.io](https://img.shields.io/crates/v/voom?style=flat-square&color=2dd4bf)](https://crates.io/crates/voom)
[![npm](https://img.shields.io/npm/v/@goldziher/voom?style=flat-square&color=2dd4bf&label=npm)](https://www.npmjs.com/package/@goldziher/voom)
[![PyPI](https://img.shields.io/pypi/v/voom-cli?style=flat-square&color=2dd4bf)](https://pypi.org/project/voom-cli/)
[![CI](https://img.shields.io/github/actions/workflow/status/Goldziher/voom/ci.yml?style=flat-square&label=CI)](https://github.com/Goldziher/voom/actions)
[![License: MIT](https://img.shields.io/badge/License-MIT-2dd4bf?style=flat-square)](./LICENSE)
[![Sponsor](https://img.shields.io/badge/Sponsor-%E2%9D%A4-2dd4bf?style=flat-square&logo=github-sponsors)](https://github.com/sponsors/Goldziher)

[Install](#installation)&nbsp;·&nbsp;[Safety](#safety)&nbsp;·&nbsp;[Usage](#usage)&nbsp;·&nbsp;[Ecosystems](#supported-ecosystems)&nbsp;·&nbsp;[Configuration](#configuration)&nbsp;·&nbsp;[Git hooks](#git-hooks)

</div>

---

## Why voom

Your disk is full of build output you will never look at again. It is spread across every
project you have touched, in a different directory name for every language, and the tools
that clean it up each know exactly one ecosystem — `cargo clean` for Rust, `./gradlew clean`
for Gradle, nothing at all for the rest.

So most people reach for `find . -name target -exec rm -rf {} +`, and sooner or later that
deletes a hand-written `bin/`.

voom sweeps all of it in one pass, and it does not guess. A directory is only an artifact
when a **marker file proves the ecosystem** sits next to it: a `target/` with a `Cargo.toml`
beside it is Rust's build output, a `target/` with nothing beside it is somebody's data and
stays exactly where it is.

> The name is from *The Cat in the Hat Comes Back* — Voom is what Little Cat Z pulls out of
> his hat to clear every last speck of pink snow at once.

## Features

- **20+ ecosystems** — Rust, Node, Python, Go, Zig, Swift, Maven, Gradle, .NET, CMake, Dart,
  Elixir, Ruby, PHP, Scala, Haskell, OCaml, Julia, R, Nim, Elm, Terraform, Bazel
- **Marker-anchored** — never deletes on a directory name alone, so your `bin/`, `vendor/`
  and `obj/` source directories are safe
- **Fast** — parallel traversal that prunes whole subtrees and saturates every core
- **Safe by construction** — no symlink following, no crossing filesystem boundaries, a
  protected-path denylist, and containment checks on every removal
- **Configurable** — hierarchical TOML with include/exclude paths and per-path, per-ecosystem
  keep policies for age and size
- **Reports properly** — deterministic colored output, `--format json` for machines, and a
  reason for everything it *didn't* take
- **Watch mode** — keep a tree pruned continuously, without touching in-progress builds
- **Git hooks** — for both `pre-commit` and poly, reporting by default

## Installation

| Channel | Command |
| ------- | ------- |
| Homebrew (macOS/Linux) | `brew install goldziher/tap/voom` |
| Cargo (Rust) | `cargo install voom` |
| npm (Node.js) | `npm install -g @goldziher/voom` |
| pip (Python) | `pip install voom-cli` |

The binary is called `voom` however you install it. The package names differ because `voom`
was already taken on npm and PyPI — see [ADR 0010](adrs/0010-distribution-and-naming.md).

Prefer prebuilt binaries? [`cargo binstall voom`](https://github.com/cargo-bins/cargo-binstall)
downloads a release archive instead of compiling from source.

Run without installing:

```bash
npx -y @goldziher/voom@latest --dry-run ~
uvx voom-cli --dry-run ~
```

<details>
<summary><b>Build from source</b></summary>

```bash
git clone https://github.com/Goldziher/voom.git
cd voom
cargo install --path .
```

Requires Rust 1.88+ (edition 2024).

</details>

## Quick start

```bash
# See what's reclaimable across everything, without touching anything
voom --dry-run ~

# Actually clean it up
voom ~

# Clean one project
voom ./my-project
```

## Usage

**voom deletes by default.** `--dry-run` previews. There is no confirmation prompt — the
safety comes from the rules below, not from a question you would learn to click through.

```bash
# Preview, with a reason for every skip
voom --dry-run --verbose ~

# Only artifacts untouched for a week
voom --min-age 7d ~

# Only Rust and Node
voom --ecosystem rust --ecosystem node ~

# Machine-readable
voom --dry-run --format json ~ | jq '.totals'

# Cap parallelism (useful on spinning disks or network filesystems)
voom -j 4 ~

# What does voom know about?
voom catalog
```

Run `voom --help` for the full, grouped list of options.

Exit codes: `0` clean, `1` some artifacts could not be removed, `2` usage or config error,
`3` findings with `--dry-run --exit-code` (for hooks).

## Safety

This is the part that matters, because voom deletes without asking.

**1. Nothing is removed without a marker file proving its ecosystem.** `target/` is only
Rust's build output when a `Cargo.toml` sits beside it. `obj/` is only .NET's when a
`*.csproj` is in its project root. A scan of one developer's workspace found 41 directories
named `bin`, 20 named `vendor`, and 12 named `obj` — each a build artifact in one ecosystem
and hand-written source in another. voom leaves every unproven one alone, and
`--verbose` tells you which marker was missing.

**2. Dependency directories are never touched.** `node_modules/`, composer's `vendor/`, Go's
`vendor/`, mix's `deps/` — these are network-fetched caches, not build output. They are not
in the catalog and no flag adds them. Deleting them costs you a re-download and breaks
offline work.

**3. The rails you cannot switch off.** Symlinks are never followed or deleted through.
Filesystem boundaries are not crossed (`--one-file-system=false` to opt out). Every target is
canonicalized and must resolve strictly below a scan root. A hard-coded denylist protects
`/`, `$HOME` itself, `/usr`, `/System`, `.git` and friends.

**4. `--dry-run` is the real pipeline with the last step withheld**, not a simulation, so
what it prints is exactly what a real run would remove.

Full reasoning in [ADR 0002](adrs/0002-marker-anchored-classification.md) and
[ADR 0006](adrs/0006-deletion-semantics.md).

## Supported ecosystems

| Ecosystem | Marker | Artifacts |
| --- | --- | --- |
| Rust | `Cargo.toml` | `target/` |
| Node / TypeScript | `package.json` | `dist/`, `.next/`, `.nuxt/`, `.svelte-kit/`, `.astro/`, `.turbo/`, `.parcel-cache/`, `.vite/`, `.nyc_output/`, `*.tsbuildinfo`, `build/`† |
| Python | `pyproject.toml`, `setup.py` | `__pycache__/`, `.pytest_cache/`, `.mypy_cache/`, `.ruff_cache/`, `.tox/`, `*.egg-info/`, `htmlcov/`, `build/`, `dist/`, `.venv/`† |
| Go | `go.mod` | `bin/`† |
| Zig | `build.zig` | `.zig-cache/`, `zig-cache/`, `zig-out/` |
| Swift | `Package.swift` | `.build/` |
| Xcode | `*.xcodeproj` | `DerivedData/` |
| Maven | `pom.xml` | `target/` |
| Gradle / Kotlin | `build.gradle`, `build.gradle.kts` | `build/`, `.gradle/` |
| .NET | `*.csproj`, `*.sln` | `bin/`, `obj/` |
| CMake / C++ | `CMakeLists.txt` | `build/`, `cmake-build-*/`, `CMakeFiles/` |
| Dart / Flutter | `pubspec.yaml` | `.dart_tool/`, `build/` |
| Elixir | `mix.exs` | `_build/`, `.elixir_ls/` |
| Ruby | `Gemfile`, `*.gemspec` | `.bundle/`, `vendor/bundle/`, `coverage/`, `pkg/`†, `tmp/`† |
| PHP | `composer.json` | `.phpunit.cache/` |
| Scala / sbt | `build.sbt` | `target/`, `project/target/`, `.bloop/`, `.metals/` |
| Haskell | `*.cabal`, `stack.yaml` | `dist-newstyle/`, `.stack-work/` |
| OCaml / dune | `dune-project` | `_build/` |
| Julia | `Project.toml` | `deps/build/`† |
| R | `DESCRIPTION` | `*.Rcheck/`, `src/*.o`, `src/*.so` |
| Nim | `*.nimble` | `nimcache/` |
| Elm | `elm.json` | `elm-stuff/` |
| Terraform | `*.tf` | `.terraform/` |
| Bazel | `WORKSPACE`, `MODULE.bazel` | `bazel-*` |

† Off by default — the name is also a plausible source directory in that ecosystem, or
removal is expensive rather than cheap. Enable per-ecosystem or per-path in your config.
`voom catalog` prints the live table.

## Configuration

voom merges configuration from built-in defaults, `~/.config/voom/config.toml`, and every
`voom.toml` from the scan root down to each candidate — nearest wins. A repository can
therefore protect its own quirks and still be swept correctly from `$HOME`.

```toml
# Top-level keys come first: TOML would otherwise read them as part of the table above.
exclude = ["~/work/client-x/**", "**/fixtures/**"]   # never scanned, always wins
include = ["~/scratch/build-junk"]                   # swept without a marker — explicit intent

[ecosystems]
enable  = ["python.venv"]     # opt into a † artifact
disable = ["terraform"]

[keep]
min_age  = "7d"      # never remove something modified more recently
min_size = "1MB"     # not worth the rebuild below this
max_size = "50GB"    # probably not what you meant

[keep.rust]
min_age = "1d"

[[paths]]
match = "~/work/**"
keep  = { min_age = "30d" }
```

`voom config show` prints the fully merged result with the file each value came from.
Details in [ADR 0004](adrs/0004-configuration.md).

## Watch mode

```bash
voom watch ~/projects
```

Prunes continuously as you work. An artifact is only removed once its subtree has been idle
for a quiet period (default 60s), so an in-progress build is never touched. See
[ADR 0008](adrs/0008-watch-mode.md).

## Git hooks

The reporting hook is the one to start with — it fails the commit on findings but deletes
nothing.

<details>
<summary><b>pre-commit</b></summary>

```yaml
repos:
  - repo: https://github.com/Goldziher/voom
    rev: v0.1.0
    hooks:
      - id: voom-report      # reports, deletes nothing
      # - id: voom-prune     # actually deletes — opt in deliberately
```

</details>

<details>
<summary><b>poly</b></summary>

```toml
[[hooks.sources]]
id = "voom"
git = "https://github.com/Goldziher/voom.git"
revision = "v0.1.0"
hooks = ["voom-report"]
```

</details>

<details>
<summary><b>Lefthook</b></summary>

```yaml
pre-commit:
  commands:
    voom:
      run: voom --dry-run --report --exit-code .
```

</details>

## Performance

voom is I/O-bound, so the wins come from not walking things: it prunes matched artifacts,
`.git/`, and dependency directories at the directory level rather than descending into them,
and fans the remaining work across every core.

Measure your own with `hyperfine`. Numbers here will be filled in from a real corpus once
`0.1.0` ships rather than estimated.

## Development

```bash
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all
poly hooks run pre-commit --all-files      # the authoritative gate
```

Design decisions live in [`adrs/`](./adrs) — start with
[0001](adrs/0001-mission-and-scope.md) and [0002](adrs/0002-marker-anchored-classification.md).
See [`CONTRIBUTING.md`](./CONTRIBUTING.md) for adding an ecosystem, which is the most common
contribution and has a checklist.

## Contributing

Issues and pull requests are welcome — especially new ecosystems, which need a marker that
genuinely proves the ecosystem and both a positive and a negative test fixture.

If voom is useful to you, consider [sponsoring development](https://github.com/sponsors/Goldziher).

## License

[MIT](./LICENSE)
