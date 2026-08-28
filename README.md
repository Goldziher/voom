<!-- markdownlint-disable MD033 MD041 -->
<div align="center">

<img src="assets/banner.svg" alt="voom — clean up the pink snow" width="820">

<em>
Then the Voom&hellip;<br>
It went VOOM!<br>
And, oh boy!<br>
What a VOOM!<br>
<br>
Now, don't ask me what Voom is.<br>
I never will know.<br>
But, boy!<br>
Let me tell you it does clean up snow!
</em>

<sub>Dr. Seuss, <em>The Cat in the Hat Comes Back</em></sub>

<br>

voom finds and removes build artifacts across your whole disk — `target/`, `dist/`,
`__pycache__/`, `.gradle/`, `.zig-cache/` and the rest — in one parallel pass.
It proves every candidate against an ecosystem marker file before it deletes, so it will
never take your hand-written `bin/` directory.

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

- **Every major ecosystem** — Rust, Node, Python, Go, Zig, Swift, Xcode, Maven, Gradle,
  .NET, CMake, Dart, Elixir, Ruby, PHP, Scala, Haskell, OCaml, Julia, R, Nim, Elm,
  Terraform, Bazel
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
uvx --from voom-cli voom --dry-run ~
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

# Include tool caches and installed toolchains, which are skipped by default
voom --dry-run --caches ~

# Also remove dependency directories -- node_modules/, vendor/, deps/, .venv/
voom --dry-run --clean-dependencies ~

# Skip the git housekeeping an ordinary sweep does
voom --no-git ~

# Git housekeeping on its own, including the network step a sweep will not do
voom git-prune -n --remotes ~/projects

# What does voom know about?
voom catalog
```

Run `voom --help` for the full, grouped list of options.

Exit codes: `0` clean, `1` some artifacts could not be removed, `2` usage or config error,
`3` findings with `--dry-run --exit-code` (for hooks).

### Using it safely the first time

Three steps, in this order. They take about a minute.

**1. Look before you sweep.** Start narrow and read what comes back:

```bash
voom --dry-run ~/projects
```

Every line names what proved the artifact. If the ecosystem column says `unanchored`, nothing
proved it — your own `include` asked for it (see [Configuration](#configuration)).

**2. Read the skips.** This is the step people miss, and it is the one that tells you whether
voom understands your tree:

```bash
voom --dry-run --verbose ~/projects
```

A skip says *why*: no marker proved it, a keep policy held it, an exclusion matched. A
directory you expected to be swept and that is missing from the list is a directory voom could
not prove — usually the right answer, occasionally a missing marker worth reporting.

**3. Run it.** Same command without `--dry-run`. The dry run is the same pipeline with the last
step withheld, so it removes exactly what it showed you.

Once you trust it on a project, widen to `~`. A worked example from the author's machine:
`voom --dry-run /private/tmp` predicted 19 artifacts and 162 GiB across stale build directories;
the real run removed those 19 and nothing else, and every source tree beside them was untouched.

### Guidance worth knowing

**Reach for `--min-age` on a schedule.** `voom --min-age 7d ~` in a weekly cron never touches
today's build. Without it, a scheduled run can delete something you are about to use.

**Lower `-j` on a spinning disk or a network filesystem.** voom defaults to every core, which is
right on an SSD and wrong on anything with a seek penalty or a round trip. `-j` bounds the walk
*and* the removal.

**"Bytes reclaimed" is not the same number on every platform.** On unix voom counts allocated
blocks, which is the space you actually get back. On Windows it counts apparent file size, which
ignores cluster rounding, compression and sparse files. Neither is wrong; they answer slightly
different questions, and on a filesystem that shares blocks between clones — APFS, for instance —
freeing one copy may return less than the sum of its files.

**When you are unsure, prefer the false negative.** That is the rule voom is built on: leaving an
artifact costs you some gigabytes, and deleting something you wrote costs you work you cannot get
back. If something is being kept that you want gone, name it in your config deliberately rather
than reaching for a broader flag.

## Safety

This is the part that matters, because voom deletes without asking.

**1. Nothing is removed without a marker file proving its ecosystem.** `target/` is only
Rust's build output when a `Cargo.toml` sits beside it. `obj/` is only .NET's when a project
file — `*.csproj`, `*.fsproj`, `*.vbproj` or `*.sln` — sits at most three directories above
it. A scan of one developer's workspace found 41 directories
named `bin`, 20 named `vendor`, and 12 named `obj` — each a build artifact in one ecosystem
and hand-written source in another. voom leaves every unproven one alone, and
`--verbose` tells you which marker was missing.

**2. Dependency directories are never touched unless you ask for them by name.**
`node_modules/`, composer's `vendor/`, Go's `vendor/`, mix's `deps/` and Python's `.venv/` are
network-fetched caches, not build output: removing one costs a re-download and breaks offline
work. Every one of them is off by default, and no ordinary run — however many flags it carries —
will take one. `--clean-dependencies` turns the group on, and `--enable node.node_modules` turns
on exactly one. voom still proves each against its marker first, so a `vendor/` with no
`composer.json` beside it is left alone like anything else.

**3. Tool caches and installed toolchains are skipped.** `~/.cargo/registry`, `~/.npm`,
`~/.pyenv`, `~/miniforge3`, `~/google-cloud-sdk` and friends are machine-global or are installed
programs, not this tree's build output. An installed pnpm under `~/.cache/node/corepack` sits
beside a real `package.json` and is, to the classifier, indistinguishable from a project someone
built — so location draws the line that a marker cannot. On the author's machine this is the
difference between 469 artifacts and 174. `--caches` opts back in, and naming one of those paths
as a scan root sweeps it without the flag.

**4. The rails you cannot switch off.** Symlinks are never followed or deleted through.
Filesystem boundaries are not crossed (`--one-file-system=false` to opt out). Every target is
canonicalized and must resolve strictly below a scan root. A hard-coded denylist protects
`/`, `$HOME` itself, `/usr`, `/System`, `.git` and friends.

**5. `--dry-run` is the real pipeline with the last step withheld**, not a simulation, so
what it prints is exactly what a real run would remove.

Full reasoning in [ADR 0002](adrs/0002-marker-anchored-classification.md) and
[ADR 0006](adrs/0006-deletion-semantics.md).

## Supported ecosystems

| Ecosystem | Marker | Artifacts |
| --- | --- | --- |
| Rust | `Cargo.toml` | `target/` |
| Node / TypeScript | `package.json` | `dist/`, `.next/`, `.nuxt/`, `.svelte-kit/`, `.astro/`, `.turbo/`, `.parcel-cache/`, `.vite/`, `.nyc_output/`, `*.tsbuildinfo`, `build/`†, `node_modules/`† |
| Python | `pyproject.toml`, `setup.py`, `setup.cfg` | `__pycache__/`, `.pytest_cache/`, `.mypy_cache/`, `.ruff_cache/`, `.tox/`, `*.egg-info/`, `htmlcov/`, `.coverage`, `build/`, `dist/`, `.venv/`† |
| Go | `go.mod` | `bin/`†, `vendor/`† |
| Zig | `build.zig` | `.zig-cache/`, `zig-cache/`, `zig-out/` |
| Swift | `Package.swift` | `.build/` |
| Xcode | `*.xcodeproj`, `*.xcworkspace` | `DerivedData/` |
| Maven | `pom.xml` | `target/` |
| Gradle / Kotlin | `build.gradle`, `build.gradle.kts`, `settings.gradle*` | `build/`, `.gradle/` |
| .NET | `*.csproj`, `*.fsproj`, `*.vbproj`, `*.sln` | `bin/`, `obj/` |
| CMake / C++ | `CMakeLists.txt` | `build/`, `cmake-build-*/`, `CMakeFiles/` |
| Dart / Flutter | `pubspec.yaml` | `.dart_tool/`, `build/` |
| Elixir | `mix.exs` | `_build/`, `.elixir_ls/`, `deps/`† |
| Ruby | `Gemfile`, `*.gemspec` | `.bundle/`, `vendor/bundle/`, `coverage/`, `pkg/`†, `tmp/`† |
| PHP | `composer.json` | `.phpunit.cache/`, `.phpunit.result.cache`, `vendor/`† |
| Scala / sbt | `build.sbt` | `target/`, `project/target/`, `.bloop/`, `.metals/` |
| Haskell | `*.cabal`, `stack.yaml` | `dist-newstyle/`, `.stack-work/` |
| OCaml / dune | `dune-project` | `_build/` |
| Julia | `Project.toml` | `deps/build/`† |
| R | `DESCRIPTION` | `*.Rcheck/`, `src/*.o`, `src/*.so` |
| Nim | `*.nimble` | `nimcache/` |
| Elm | `elm.json` | `elm-stuff/` |
| Terraform | `*.tf` | `.terraform/` |
| Bazel | `WORKSPACE`, `WORKSPACE.bazel`, `MODULE.bazel` | `bazel-*/` |

† Off by default — the name is also a plausible source directory in that ecosystem, or
removal is expensive rather than cheap. Turn one on for a single run with
`--enable python.venv`, or per-ecosystem and per-path in your config. `voom catalog` prints the
live table, with the reason each `†` entry needs an opt-in.

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

[git]
enabled = false      # skip the housekeeping a sweep otherwise does here

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

Paths listed in `include` are swept without a marker proving them. That is the one place voom
will remove something it cannot prove, so it is reported as `unanchored` rather than as an
ecosystem, and JSON marks it `"source": "config-include"`. `exclude` always wins over
`include`.

`include` only matches paths the walk actually visits, so it never reaches outside the roots
you named: running `voom ~/projects` with `include = ["~/scratch"]` sweeps nothing extra. It
*can* name a directory the walk would otherwise skip — a tool cache like `~/.npm`, say — and
have it removed. For a dependency directory, prefer `--clean-dependencies` or `--enable`: those
still prove the marker, and an `include` does not. It cannot reach *inside* one: skipping happens a whole
directory at a time, so `include = ["~/.npm/_cacache/pkg"]` matches nothing. Name the cache as
a scan root, or pass `--caches`.

**On Windows, write paths in single quotes.** TOML basic strings take escapes, so
`exclude = ["C:\Users\me\work\**"]` fails to parse — `\U` starts a unicode escape. Use a TOML
literal string instead, which takes none:

```toml
exclude = ['C:\Users\me\work\**']
```

`voom config show` lists the files the configuration was merged from, in ascending precedence,
and then the merged result. Details in [ADR 0004](adrs/0004-configuration.md).

## Git housekeeping

An ordinary sweep also runs git's own local housekeeping in every repository it walks past.
Discovery is free — the walker already meets `.git` on its way past, so there is no second pass —
and the two steps are the ones git itself runs after a commit:

| Step | What it does |
| --- | --- |
| `git worktree prune --expire` | drops the administration of worktrees whose checkout has been gone for three months |
| `git gc --auto` | repacks **only if** git's own thresholds say it is worth it |

Neither contacts a network. Neither expires a reflog beyond git's own policy, and voom never
passes `--prune=now`, `--aggressive`, or `git reflog expire`: the reflog is how you get a commit
back, and "prunable" does not mean "unreachable by you". The three months is git's own
`gc.worktreePruneExpire` default rather than `git worktree prune`'s, which expires everything —
a checkout on an unmounted disk is absent, and its administration holds the reflog for whatever
was committed there. Every invocation also runs with hooks disabled, so a repository cannot
choose what a housekeeping pass executes. A repository with an operation half
finished — a rebase, a merge, a cherry-pick, a bisect — is skipped and said so, and a linked
worktree is resolved to its object store and pruned once rather than once per checkout.

The report says nothing when nothing was found, because that is the usual case:

```text
  pruned  api  — 2 stale worktrees pruned
```

Turn it off with `--no-git`, or with `[git] enabled = false` in a `voom.toml`, which lets one
repository set a policy for its own subtree. The flag beats the file.

`voom git-prune` runs the same housekeeping on its own, and is the only place the network step
lives:

```bash
voom git-prune -n ~/projects              # what would be pruned
voom git-prune --remotes ~/projects       # also drop remote-tracking branches whose upstream is gone
voom git-prune --expire 2.weeks.ago ~/p   # a different worktree expiry
```

`--remotes` is deliberately not part of a sweep: it is one network round trip per remote per
repository, which hangs without connectivity and can block on a credential prompt.

## Watch mode

```bash
voom watch ~/projects
```

Prunes continuously as you work. Filesystem events are coalesced over a debounce window
(`--debounce`, default 5s), and an artifact is only removed once its subtree has been idle for
a quiet period (`--quiet-period`, default 60s), so an in-progress build is never touched. See
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

Measured with `hyperfine` on an M-series Mac against a real machine — a 130 GB `~/workspace`
holding 16 ecosystems, and a `$HOME` above it. Dry runs, so the walk and the size accounting
are timed without the removal:

| Sweep | Mean | Found |
| --- | --- | --- |
| `~/workspace` (130 GB) | 5.3 s ± 0.8 | 172 artifacts |
| `$HOME` | 10.7 s ± 0.7 | 174 artifacts |
| `$HOME --caches` | 15.3 s ± 3.0 | 475 artifacts |

`-j` genuinely throttles the whole sweep, not just the walk: the same `~/workspace` run takes
14.3 s ± 0.4 at `-j 1`, 2.7× the default. That is the point of the flag on a spinning disk or a
network filesystem, where saturating the cores is the wrong thing to do.

Measure your own the same way — and if you change the walk, measure before and after on the same
tree and put both numbers in the pull request.

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
