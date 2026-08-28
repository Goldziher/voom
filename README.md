<!-- markdownlint-disable MD033 MD041 -->
<div align="center">

<img src="assets/banner.svg" alt="voom — clean up the pink snow" width="820">

**Reclaim every build artifact on your disk in one parallel pass — and prove each one before deleting it.**

voom finds and removes build output anywhere in a tree: `target/`, `dist/`, `__pycache__/`,
`.gradle/`, `.zig-cache/` and the rest. A directory counts as an artifact only when a marker file
proves its ecosystem — `target/` is Rust's when a `Cargo.toml` sits beside it, and somebody's data
otherwise — so a hand-written `bin/` or `vendor/` is never taken.

Every major ecosystem · marker-anchored classification · six deletion rails · a dry run that is the
real pipeline · hierarchical TOML · watch mode · git hooks

[![crates.io](https://img.shields.io/crates/v/voom?style=flat-square&color=2dd4bf)](https://crates.io/crates/voom)
[![npm](https://img.shields.io/npm/v/@goldziher/voom?style=flat-square&color=2dd4bf&label=npm)](https://www.npmjs.com/package/@goldziher/voom)
[![PyPI](https://img.shields.io/pypi/v/voom-cli?style=flat-square&color=2dd4bf)](https://pypi.org/project/voom-cli/)
[![CI](https://img.shields.io/github/actions/workflow/status/Goldziher/voom/ci.yml?style=flat-square&label=CI)](https://github.com/Goldziher/voom/actions)
[![License: MIT](https://img.shields.io/badge/License-MIT-2dd4bf?style=flat-square)](./LICENSE)
[![Sponsor](https://img.shields.io/badge/Sponsor-%E2%9D%A4-2dd4bf?style=flat-square&logo=github-sponsors)](https://github.com/sponsors/Goldziher)

[Install](#installation)&nbsp;·&nbsp;[Safety](#safety)&nbsp;·&nbsp;[Usage](#usage)&nbsp;·&nbsp;[Ecosystems](#supported-ecosystems)&nbsp;·&nbsp;[Configuration](#configuration)&nbsp;·&nbsp;[Git hooks](#git-hooks)

</div>

---

<!-- markdownlint-disable MD013 -->
<p align="center"><img src="assets/demo.gif" alt="voom previewing a tree with --dry-run, running the same sweep for real, then summarising a whole home directory" width="820"></p>
<p align="center"><em>A <code>--dry-run</code>, then the same sweep for real — it removes exactly what it showed, because the dry run is the same pipeline with the last step withheld. Then a summary of a whole home directory.</em></p>
<!-- markdownlint-enable MD013 -->

---

## What you get

| Capability | What it does |
| --- | --- |
| **Every major ecosystem** | Rust, Node, Python, Go, Zig, Swift, Xcode, Maven, Gradle, .NET, CMake, Dart, Elixir, Ruby, PHP, Scala, Haskell, OCaml, Julia, R, Nim, Elm, Terraform, Bazel — [the table below](#supported-ecosystems), or `voom catalog` for the live one |
| **Proof before deletion** | Never a directory name alone: a marker file has to prove the ecosystem at the artifact's declared anchor, or voom walks past |
| **Six deletion rails** | No symlink following, no filesystem crossing, canonicalization, an append-only protected-path denylist, containment below a scan root, and per-artifact failure isolation |
| **One pass** | Parallel traversal that prunes whole subtrees — matched artifacts, `.git/`, dependency directories, tool caches — rather than descending and filtering |
| **A report you can audit** | Sorted, per-artifact, with a reason for everything it did *not* take; `--format json` for machines; stage timings in the footer |
| **Keep policies** | `--min-age`, `--min-size`, `--max-size`, plus hierarchical `voom.toml` with per-path and per-ecosystem overrides |
| **Watch mode** | Keeps a tree pruned as you work, without touching an in-progress build |
| **Git housekeeping** | `git worktree prune` and `git gc --auto` in every repository the sweep walks past, free of a second traversal |
| **Git hooks** | For `pre-commit` and poly, reporting by default |

## New in 0.2.0

- **A removal that only partly succeeded is reported as one, with the bytes it actually freed.**
  `remove_dir_all` unlinks a tree's contents before the tree itself, so a failure part-way through
  leaves real space reclaimed. voom used to score that zero — a sweep that freed 130 GB printed a
  footer saying 25. The headline now counts every byte the run freed and names what is left.
- **Failures say what happened in words.** `Directory not empty (os error 66)` became *"a directory
  was not empty when voom unlinked it, most likely because something wrote into it during the
  removal; re-run to finish"*. Classified by `io::ErrorKind`, never by error number.
- **`--force`** retries a failed removal, repairing permissions *inside* the artifact first —
  read-only bits, and on macOS the user-immutable flag. It never relaxes a rail, never follows a
  symlink, never widens a hard-linked file, and never touches the artifact's parent. Four attempts
  in a little over a second, then it reports and moves on.
- **`--clean-dependencies`** opts into `node_modules/`, composer and Go `vendor/`, mix `deps/` and
  Python virtualenvs. They are ordinary off-by-default catalog entries, still proved against their
  own markers, and the footer names them when one is removed.
- **Git housekeeping runs by default** — `git worktree prune` and `git gc --auto`, both local, both
  what git runs itself after a commit. `--no-git`, or `[git] enabled = false`, turns it off.
- **The footer says where the time went**: `scan 2.61s · policy 645µs · size 2.28s · delete 1.42s`.
  JSON carries the same in `timings_us`.
- **Sizes are correct on hard-linked trees.** Cargo hard-links into `target/`, pnpm links its store,
  a Bazel output base is largely links. Counting each inode once (`du`'s rule) took the largest real
  artifact on the author's machine from a reported 113.91 GB to 84.45 GB; `du` says 84.51 GB.

A dry run of a real `~/workspace` is **1.48× faster than v0.1.0** — see [Performance](#performance).
Full detail in [`CHANGELOG.md`](./CHANGELOG.md); the JSON schema is now version `2`.

## Installation

| Channel | Command |
| ------- | ------- |
| Homebrew (macOS/Linux) | `brew install goldziher/tap/voom` |
| Cargo (Rust) | `cargo install voom` |
| npm (Node.js) | `npm install -g @goldziher/voom` |
| pip (Python) | `pip install voom-cli` |

The binary is called `voom` however you install it. The package names differ because `voom` was
already taken on npm and PyPI — see [ADR 0010](adrs/0010-distribution-and-naming.md).

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
voom --dry-run ~      # see what's reclaimable across everything, without touching anything
voom ~                # actually clean it up
voom ./my-project     # clean one project
```

What a run prints:

```text
/projects
  removed   4.20 GB  rust  api/target
  refused       0 B  rust  link/target    — is a symlink
  removed  18.50 MB  node  web/dist

2 artifacts, 4.22 GB reclaimed in 1.23s
  scan 900.00ms · policy 12.00ms · size 210.00ms · delete 98.00ms
  rust 4.20 GB (1), node 18.50 MB (1)
  1 artifact refused by a safety rail
```

## Usage

**voom deletes by default.** `--dry-run` previews. There is no confirmation prompt — the safety
comes from the rules below, not from a question you would learn to click through.

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

# Retry failed removals, repairing permissions inside the artifact
voom --force ~

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

Exit codes: `0` clean, `1` some artifacts could not be removed (a partial removal counts), `2`
usage or config error, `3` findings with `--dry-run --exit-code` (for hooks).

### Using it safely the first time

**1. Look before you sweep.** Start narrow and read what comes back:

```bash
voom --dry-run ~/projects
```

Every line names what proved the artifact. If the ecosystem column says `unanchored`, nothing proved
it — your own `include` asked for it (see [Configuration](#configuration)).

**2. Read the skips.** This is the step people miss, and the one that tells you whether voom
understands your tree:

```bash
voom --dry-run --verbose ~/projects
```

A skip says *why*: no marker proved it, a keep policy held it, an exclusion matched. A directory you
expected to be swept and cannot find in the list is one voom could not prove — usually the right
answer, occasionally a missing marker worth reporting.

**3. Run it.** The same command without `--dry-run`. Once you trust it on a project, widen to `~`.

### Guidance worth knowing

**Reach for `--min-age` on a schedule.** `voom --min-age 7d ~` in a weekly cron never touches
today's build. Without it, a scheduled run can delete something you are about to use.

**Lower `-j` on a spinning disk or a network filesystem.** voom defaults to every core, which is
right on an SSD and wrong on anything with a seek penalty or a round trip. `-j` bounds the walk, the
sizing, *and* the removal.

**"Bytes reclaimed" is not the same number on every platform.** On unix voom counts allocated blocks
and counts each hard-linked inode once — `du`'s rule. On Windows it counts apparent file size, which
ignores cluster rounding, compression and sparse files, and has no portable link count to dedup by.
On a filesystem that shares blocks between clones — APFS, for instance — freeing one copy may return
less than the sum of its files.

**When you are unsure, prefer the false negative.** Leaving an artifact costs you gigabytes;
deleting something you wrote costs you work you cannot get back. If something is being kept that you
want gone, name it in your config deliberately rather than reaching for a broader flag.

## Safety

This is the part that matters, because voom deletes without asking.

**1. Nothing is removed without a marker file proving its ecosystem.** `target/` is only Rust's
build output when a `Cargo.toml` sits beside it. `obj/` is only .NET's when a project file —
`*.csproj`, `*.fsproj`, `*.vbproj` or `*.sln` — sits at most three directories above it. `bin/`,
`vendor/` and `obj/` are each build output in one ecosystem and hand-written source in another; voom
leaves every unproven one alone, and `--verbose` says which marker was missing.

**2. Dependency directories are never touched unless you ask for them by name.** `node_modules/`,
composer's `vendor/`, Go's `vendor/`, mix's `deps/` and Python's `.venv/` are network-fetched caches,
not build output: removing one costs a re-download and breaks offline work. Every one is off by
default, and no ordinary run — however many flags it carries — takes one. `--clean-dependencies`
turns the group on and `--enable node.node_modules` turns on exactly one; voom still proves each
against its marker, so a `vendor/` with no `composer.json` beside it is left alone like anything
else.

**3. Tool caches and installed toolchains are skipped.** `~/.cargo/registry`, `~/.npm`, `~/.pyenv`,
`~/miniforge3`, `~/google-cloud-sdk` and friends are machine-global or are installed programs, not
this tree's build output. An installed pnpm under `~/.cache/node/corepack` sits beside a real
`package.json` and is, to the classifier, indistinguishable from a project someone built — so
location draws the line a marker cannot. On the author's machine this is the difference between 174
artifacts and 475. `--caches` opts back in, and naming one of those paths as a scan root sweeps it
without the flag.

**4. Six rails you cannot switch off.** Symlinks and Windows reparse points are refused rather than
followed. Every target is canonicalized. An append-only denylist protects `/`, `$HOME` itself,
`/usr`, `/System`, `.git` and friends. The result must resolve strictly below a scan root. Filesystem
boundaries are not crossed (`--one-file-system=false` to opt out). A failure on one artifact is
reported and isolated rather than aborting the run.

**5. `--dry-run` is the real pipeline with the last step withheld**, not a simulation, so what it
prints is exactly what a real run would remove.

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

† Off by default — the name is also a plausible source directory in that ecosystem, or removal is
expensive rather than cheap. Turn one on for a single run with `--enable python.venv`, or per
ecosystem and per path in your config. `voom catalog` prints the live table, with the reason each
`†` entry needs an opt-in.

## Configuration

voom merges configuration from built-in defaults, `~/.config/voom/config.toml`, and every
`voom.toml` from the scan root down to each candidate — nearest wins. A repository can therefore
protect its own quirks and still be swept correctly from `$HOME`.

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

Paths listed in `include` are swept without a marker proving them. That is the one place voom will
remove something it cannot prove, so it is reported as `unanchored` rather than as an ecosystem, and
JSON marks it `"source": "config-include"`. `exclude` always wins over `include`.

`include` only matches paths the walk actually visits, so it never reaches outside the roots you
named: running `voom ~/projects` with `include = ["~/scratch"]` sweeps nothing extra. It *can* name
a directory the walk would otherwise skip — a tool cache like `~/.npm`, say — and have it removed.
For a dependency directory, prefer `--clean-dependencies` or `--enable`: those still prove the
marker, and an `include` does not. It cannot reach *inside* one: skipping happens a whole directory
at a time, so `include = ["~/.npm/_cacache/pkg"]` matches nothing. Name the cache as a scan root, or
pass `--caches`.

**On Windows, write paths in single quotes.** TOML basic strings take escapes, so
`exclude = ["C:\Users\me\work\**"]` fails to parse — `\U` starts a unicode escape. Use a TOML
literal string instead, which takes none:

```toml
exclude = ['C:\Users\me\work\**']
```

`voom config show` lists the files the configuration was merged from, in ascending precedence, and
then the merged result. Details in [ADR 0004](adrs/0004-configuration.md).

## Git housekeeping

An ordinary sweep also runs git's own local housekeeping in every repository it walks past.
Discovery is free — the walker already meets `.git` on its way past — and the two steps are the ones
git itself runs after a commit:

| Step | What it does |
| --- | --- |
| `git worktree prune --expire` | drops the administration of worktrees whose checkout has been gone for three months |
| `git gc --auto` | repacks **only if** git's own thresholds say it is worth it |

Neither contacts a network. Neither expires a reflog beyond git's own policy, and voom never passes
`--prune=now`, `--aggressive`, or `git reflog expire`: the reflog is how you get a commit back, and
"prunable" does not mean "unreachable by you". The three months is git's own `gc.worktreePruneExpire`
default rather than `git worktree prune`'s, which expires everything — a checkout on an unmounted
disk is absent, and its administration holds the reflog for whatever was committed there. Every
invocation runs with hooks disabled, so a repository cannot choose what a housekeeping pass
executes. A repository with an operation half finished — a rebase, a merge, a cherry-pick, a bisect
— is skipped and said so, and a linked worktree is resolved to its object store and pruned once
rather than once per checkout.

The report says nothing when nothing was found, because that is the usual case:

```text
  pruned  api  — 2 stale worktrees pruned
```

Turn it off with `--no-git`, or with `[git] enabled = false` in a `voom.toml`, which lets one
repository set a policy for its own subtree. The flag beats the file. See
[ADR 0011](adrs/0011-git-housekeeping.md).

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
(`--debounce`, default 5s), and an artifact is only removed once its subtree has been idle for a
quiet period (`--quiet-period`, default 60s), so an in-progress build is never touched. Watch mode
never forces. See [ADR 0008](adrs/0008-watch-mode.md).

## Git hooks

The reporting hook is the one to start with — it fails the commit on findings but deletes nothing.

<details>
<summary><b>pre-commit</b></summary>

```yaml
repos:
  - repo: https://github.com/Goldziher/voom
    rev: v0.2.0
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
revision = "v0.2.0"
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

voom is I/O-bound, so the wins come from not walking things: it prunes matched artifacts, `.git/`,
dependency directories and tool caches at the directory level rather than descending into them, and
fans the remaining work — sizing and removal, and now the inside of each artifact — across every
core.

Measured with `hyperfine` on an M-series Mac against a real 144 GB `~/workspace`, dry runs, so the
walk and the size accounting are timed without the removal:

| Version | Mean | Found |
| --- | --- | --- |
| v0.1.0 | 8.89 s | 85 artifacts |
| **0.2.0** | **6.01 s** | 85 artifacts |

**1.48× faster over ten runs, finding exactly the same artifacts.** Most of it is the sizing
fan-out: a sweep's artifacts are wildly uneven — one `target/` can be most of the tree — so
per-artifact parallelism left the largest one to a single thread while every other core idled.
Dividing the work *inside* each artifact fixed that, and the reported sizes are byte-identical to
the serial walk.

`-j` genuinely throttles the whole sweep rather than just the walk, which is the point of the flag
on a spinning disk or a network filesystem. Measure your own the same way — and if you change the
walk, measure before and after on the same tree and put both numbers in the pull request.

## The name

<div align="center">

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

</div>

Voom is what Little Cat Z pulls out of his hat to clear every last speck of pink snow at once.

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

Issues and pull requests are welcome — especially new ecosystems, which need a marker that genuinely
proves the ecosystem and both a positive and a negative test fixture.

If voom is useful to you, consider [sponsoring development](https://github.com/sponsors/Goldziher).

## License

[MIT](./LICENSE)
