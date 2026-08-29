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

**Reclaim every build artifact on your disk in one parallel pass — and prove each one before deleting it.**

voom finds and removes build output anywhere in a tree: `target/`, `dist/`, `__pycache__/`,
`.gradle/`, `.zig-cache/` and the rest. A directory counts as an artifact only when a marker file
proves its ecosystem, so a hand-written `bin/` or `vendor/` is never taken.

Every major ecosystem · marker-anchored classification · six deletion rails · a dry run that is the
real pipeline · hierarchical TOML · a named tool-cache catalog · watch mode · git hooks

[![crates.io](https://img.shields.io/crates/v/voom?style=flat-square&color=007ec6)](https://crates.io/crates/voom)
[![npm](https://img.shields.io/npm/v/@goldziher/voom?style=flat-square&color=007ec6&label=npm)](https://www.npmjs.com/package/@goldziher/voom)
[![PyPI](https://img.shields.io/pypi/v/voom-cli?style=flat-square&color=007ec6)](https://pypi.org/project/voom-cli/)
[![CI](https://img.shields.io/github/actions/workflow/status/Goldziher/voom/ci.yml?style=flat-square&color=007ec6&label=CI)](https://github.com/Goldziher/voom/actions)
[![license: MIT](https://img.shields.io/badge/license-MIT-007ec6?style=flat-square)](./LICENSE)
[![Sponsor](https://img.shields.io/badge/Sponsor-%E2%9D%A4-007ec6?style=flat-square&logo=github-sponsors)](https://github.com/sponsors/Goldziher)

[Install](#installation)&nbsp;·&nbsp;[Usage](#usage)&nbsp;·&nbsp;[Safety](#safety)&nbsp;·&nbsp;[Caches](#cache-catalog)&nbsp;·&nbsp;[Suggest](#finding-what-the-catalog-does-not-cover)&nbsp;·&nbsp;[Ecosystems](#supported-ecosystems)&nbsp;·&nbsp;[Configuration](#configuration)&nbsp;·&nbsp;[Git hooks](#git-hooks)&nbsp;·&nbsp;[Reference](docs/reference.md)

</div>

---

<!-- markdownlint-disable MD013 -->
<p align="center"><img src="assets/demo.gif" alt="voom previewing a tree with --dry-run, running the same sweep for real with identical results, and leaving a target directory that no Cargo.toml proved" width="820"></p>
<p align="center"><em>A <code>--dry-run</code>, then the same sweep for real: identical to the byte, because the dry run is the same pipeline with the last step withheld. A symlinked <code>target/</code> is refused rather than followed — and the <code>target/</code> that no <code>Cargo.toml</code> proved is still there afterwards.</em></p>
<!-- markdownlint-enable MD013 -->

---

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
voom ~                # actually clean it up — no confirmation prompt, see Safety
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

Start narrow: `voom --dry-run --verbose ~/projects`, and read the skips before widening to `~`.

```bash
voom --dry-run --verbose ~                                  # preview, with a reason for every skip
voom --min-age 7d ~                                          # only artifacts untouched for a week
voom --ecosystem rust --ecosystem node ~                     # only these ecosystems
voom --format json ~ | jq '.totals'                          # machine-readable
voom --dry-run --caches ~                                    # include tool caches, skipped by default
voom --dry-run --clean-caches cargo-registry,uv,go-build ~   # remove named tool caches by id
voom --dry-run --include .mytool-cache ~/projects            # sweep something the catalog misses
voom --dry-run --clean-dependencies ~                        # also node_modules/, vendor/, deps/, .venv/
voom --no-git ~                                               # skip the git housekeeping a sweep does
voom --summary --config ./ci-voom.toml ~/projects             # totals only, explicit config file
```

Run `voom --help` for the full, grouped list, or see the
[options reference](docs/reference.md#options).

Exit codes: `0` clean, `1` some artifacts could not be removed (a partial removal counts), `2`
usage or config error, `3` findings with `--dry-run --exit-code` (for hooks). Git housekeeping
never fails a sweep. `catalog`, `caches`, `config show` and `suggest` always exit `0`.

### Options

Every flag and subcommand, with defaults and short forms, is in the
[reference](docs/reference.md#options). `voom --help` prints the same list grouped the same way.

## Safety

voom deletes by default, with no confirmation prompt. Safety comes from these rules:

**1. Nothing is removed without a marker file proving its ecosystem.** `target/` is only Rust's
build output when a `Cargo.toml` sits beside it; `obj/` is only .NET's when a project file —
`*.csproj`, `*.fsproj`, `*.vbproj` or `*.sln` — sits at most three directories above it. A marker's
position relative to the candidate is one of three anchors: `Sibling` (beside the artifact, most
ecosystems), `Ancestor(n)` (up to `n` directories above, for artifacts nested inside a project),
or `Inside` (the marker is written *inside* the candidate itself — `~/.cargo/registry`'s
`CACHEDIR.TAG`, `.basemind/`'s `agent-id`). `--verbose` says which marker was missing.

**2. Dependency directories are off by default.** `node_modules/`, `vendor/` (Rust, PHP composer,
Go), mix's `deps/` and Python's `.venv/` are network-fetched caches, not build output — removing
one costs a re-download and can break offline work. `--clean-dependencies` turns the group on and
`--enable <spec>` turns on exactly one; a marker still has to prove each.

**3. Tool caches and installed toolchains are skipped by location**, not name — an installed pnpm
sits beside a real `package.json` and is otherwise indistinguishable from a project someone built.
`--caches` opts back in; naming one of those paths as a scan root sweeps it regardless.

**4. Six rails you cannot switch off.** Symlinks and Windows reparse points are refused rather than
followed. Every target is canonicalized. An append-only denylist protects `/`, `$HOME` itself,
`/usr`, `/System`, `.git` and friends. The result must resolve strictly below a scan root.
Filesystem boundaries are not crossed (`--one-file-system=false` to opt out). A failure on one
artifact is reported and isolated rather than aborting the run.

**5. `--dry-run` is the real pipeline with the last step withheld**, not a simulation.

Full reasoning in [ADR 0002](adrs/0002-marker-anchored-classification.md) and
[ADR 0006](adrs/0006-deletion-semantics.md).

## Cache catalog

The ecosystem catalog covers build output; it has nothing to say about `~/.cargo/registry` or
`~/.cache/uv`, which are machine-global downloads rather than anything a project built. `voom
caches` lists the ones voom knows how to empty by name:

| id | Cache | Location(s) |
| --- | --- | --- |
| `cargo-registry` | Cargo registry | `~/.cargo/registry` |
| `cargo-git` | Cargo git checkouts | `~/.cargo/git` |
| `uv` | uv | `~/.cache/uv` |
| `gradle-caches` | Gradle caches | `~/.gradle/caches` |
| `go-build` | Go build cache | `~/Library/Caches/go-build`, `~/.cache/go-build` |
| `go-mod` | Go module cache | `~/go/pkg/mod` |
| `npm` | npm cache | `~/.npm/_cacache` |
| `pnpm` | pnpm metadata cache | `~/Library/Caches/pnpm`, `~/.cache/pnpm` |
| `pub` | Dart pub cache | `~/.pub-cache` |
| `deno` | Deno cache | `~/Library/Caches/deno`, `~/.cache/deno`, `~/.deno` |
| `poly` | poly cache | `~/.cache/poly`, `~/Library/Caches/poly`, `~/AppData/Local/poly` |
| `alef-global` | alef global cache | `~/.cache/alef` |

The id is what `--clean-caches` and `[caches] enable` take — not always the tool's name, since a
cache id and an ecosystem id sharing one word would leave a report unable to say which table a
line came from. Each is proven the way everything else is: not by its path, but by a marker the
tool wrote *inside* it, most often the cross-tool [`CACHEDIR.TAG`](https://bford.info/cachedir/).
None is on by default and none is reachable without naming it:

```bash
voom caches                                  # the table, resolved against this machine
voom --dry-run --clean-caches uv,go-build ~  # then remove the ones you named
```

A cache whose contents are only shard directories — sccache, zig, NuGet, Maven — is deliberately
absent: the marker would prove nothing the path had not already said. See
[ADR 0012](adrs/0012-cache-catalog.md).

## Finding what the catalog does not cover

The catalog covers ecosystems, not the cache your own tooling writes into every package
directory. `include` ([ADR 0004](adrs/0004-configuration.md)) is the answer, but you have to know
the directory's name to write the line. `voom suggest` walks a tree and reports the repeated
directories git already ignores where they sit, ranked by size, with the line that would sweep
each:

```console
$ voom suggest ~/projects
     42.70 GB  .alef  across 69 directories
      3.00 GB  .basemind  across 13 directories

Add to voom.toml what you recognise:

  include = [".alef"]
  include = [".basemind"]

Read the list before pasting it. A .gitignore also lists local configuration and
scratch data, which is why voom suggests rather than sweeps.
```

It removes nothing. `--include <GLOB>` is the command-line equivalent of `voom.toml`'s `include`
key, for when you already know the name.

## Supported ecosystems

| Ecosystem | Marker | Artifacts |
| --- | --- | --- |
| Rust | `Cargo.toml` | `target/`, `vendor/`† |
| Node / TypeScript | `package.json` | `dist/`, `.next/`, `.nuxt/`, `.svelte-kit/`, `.astro/`, `.turbo/`, `.parcel-cache/`, `.vite/`, `.nyc_output/`, `*.tsbuildinfo`, `build/`†, `node_modules/`† |
| Python | `pyproject.toml`, `setup.py`, `setup.cfg` | `__pycache__/`, `.pytest_cache/`, `.mypy_cache/`, `.ruff_cache/`, `*.egg-info/`, `htmlcov/`, `.coverage`, `dist/`, `.tox/`†, `build/`†, `.venv/`† |
| Go | `go.mod` | `bin/`†, `vendor/`† |
| Zig | `build.zig` | `.zig-cache/`, `zig-cache/`, `zig-out/` |
| Swift | `Package.swift` | `.build/` |
| Xcode | `*.xcodeproj`, `*.xcworkspace` | `DerivedData/` |
| Maven | `pom.xml` | `target/` |
| Gradle / Kotlin | `build.gradle`, `build.gradle.kts`, `settings.gradle*` | `build/`, `.gradle/` |
| .NET | `*.csproj`, `*.fsproj`, `*.vbproj`, `*.sln` | `bin/`, `obj/` |
| CMake / C++ | `CMakeLists.txt` | `cmake-build-*/`, `CMakeFiles/`, `build/`† |
| Dart / Flutter | `pubspec.yaml` | `.dart_tool/`, `build/` |
| Elixir | `mix.exs` | `_build/`, `.elixir_ls/`, `deps/`† |
| Ruby | `Gemfile`, `*.gemspec` | `coverage/`, `vendor/bundle/`†, `pkg/`†, `tmp/`† |
| PHP | `composer.json` | `.phpunit.cache/`, `.phpunit.result.cache`, `vendor/`† |
| Scala / sbt | `build.sbt` | `target/`, `project/target/`, `.bloop/`, `.metals/` |
| Haskell | `*.cabal`, `stack.yaml` | `dist-newstyle/`, `.stack-work/` |
| OCaml / dune | `dune-project` | `_build/` |
| Julia | `Project.toml` | `deps/build/`† |
| R | `DESCRIPTION` | `*.Rcheck/`, `src/*.o`, `src/*.so` |
| Nim | `*.nimble` | `nimcache/` |
| Elm | `elm.json` | `elm-stuff/` |
| Terraform | `*.tf` | `.terraform/` |
| Bazel | `WORKSPACE`, `WORKSPACE.bazel`, `MODULE.bazel` | `bazel-bin/`, `bazel-out/`, `bazel-testlogs/` |
| alef | `CACHEDIR.TAG` | `.alef/`‡ |
| basemind | `agent-id` | `.basemind/`†‡ |

‡ Proven from *inside*: `agent-id` is the file basemind writes into `.basemind/` when it
creates the index, so the directory declares itself rather than relying on a sibling config
that is usually absent. Every other row looks for its marker beside or above the artifact.
A tool that wants its caches swept can do the same with the cross-tool
[`CACHEDIR.TAG`](https://bford.info/cachedir/).

† Off by default — the name is also a plausible source directory in that ecosystem, or removal is
expensive rather than cheap. Turn one on for a single run with `--enable python.venv`, or per
ecosystem and per path in your config. `voom catalog` prints the live table, with the reason each
`†` entry needs an opt-in.

## Configuration

voom merges built-in defaults, `~/.config/voom/config.toml`, and every `voom.toml` from the scan
root down to each candidate — nearest wins, so a repository can protect its own quirks and still
be swept correctly from `$HOME`. Every table rejects unknown fields, so a typo is a hard load
error rather than a guard that silently never took effect.

```toml
exclude = ["~/work/client-x/**"]   # never scanned, always wins
include = ["~/scratch/build-junk"] # swept without a marker — explicit intent

[keep]
min_age = "7d"                     # never remove something modified more recently

[caches]
enable = ["uv"]                    # opt into a named tool cache
```

Keep policies, per-ecosystem and per-path overrides, `[ecosystems]`, `[git]` and the Windows
quoting rule are in the [configuration reference](docs/configuration.md).
`voom config show [PATH]` prints the merged result and the files it came from.

## JSON output

`--format json` writes one object per run to stdout; diagnostics stay on stderr. It implies
`--verbose` and suppresses the spinner. The shape is versioned and documented field by field in
the [reference](docs/reference.md#json-output).

```bash
voom --dry-run --format json ~ | jq '.totals'
```

## Git housekeeping

An ordinary sweep also runs git's own local housekeeping in every repository it walks past —
free, since the walker already meets `.git` on its way past:

| Step | What it does |
| --- | --- |
| `git worktree prune --expire` | drops the administration of worktrees whose checkout has been gone for three months |
| `git gc --auto` | repacks **only if** git's own thresholds say it is worth it |

Neither contacts a network or expires a reflog beyond git's own policy, and voom never passes
`--prune=now`, `--aggressive`, or `git reflog expire`. Hooks are disabled for every invocation. An
in-progress rebase, merge, cherry-pick or bisect is skipped, and a linked worktree is resolved to
its object store and pruned once rather than per checkout. The report says nothing when nothing
was found:

```text
  pruned  api  — 2 stale worktrees pruned
```

Turn it off with `--no-git`, or `[git] enabled = false` in a `voom.toml` (the flag wins). See
[ADR 0011](adrs/0011-git-housekeeping.md).

`voom git-prune` runs the same housekeeping on its own, and is the only place the network step
(`--remotes`) lives — one round trip per remote per repository, which hangs without connectivity
and can block on a credential prompt, so it is deliberately not part of an ordinary sweep:

```bash
voom git-prune -n ~/projects              # what would be pruned
voom git-prune --remotes ~/projects       # also drop remote-tracking branches whose upstream is gone
voom git-prune --format json ~/projects   # machine-readable, same shape embedded under a sweep's "git" key
```

A repository whose housekeeping fails or times out makes `git-prune` exit `1`; a skip is a rail
doing its job and exits `0`.

## Watch mode

```bash
voom watch ~/projects
```

Prunes continuously as you work. Filesystem events are coalesced over a debounce window
(`--debounce`, default `5s`), and an artifact is only removed once its subtree has been idle for a
quiet period (`--quiet-period`, default `60s`), so an in-progress build is never touched. Watch
mode never forces. See [ADR 0008](adrs/0008-watch-mode.md).

## Git hooks

The reporting hook is the one to start with — it fails the commit on findings but deletes nothing.

**pre-commit**

```yaml
repos:
  - repo: https://github.com/Goldziher/voom
    rev: v0.4.3
    hooks:
      - id: voom-report      # reports, deletes nothing
      # - id: voom-prune     # actually deletes — opt in deliberately
```

**poly**

```toml
[[hooks.sources]]
id = "voom"
git = "https://github.com/Goldziher/voom.git"
revision = "v0.4.3"
hooks = ["voom-report"]
```

**Lefthook**

```yaml
pre-commit:
  commands:
    voom:
      run: voom --dry-run --report --exit-code .
```

## Performance

voom is I/O-bound, so the wins come from not walking things: it prunes matched artifacts, `.git/`,
dependency directories and tool caches at the directory level rather than descending into them,
and fans sizing and removal across every core. `-j` throttles the whole sweep, not just the walk —
the point of the flag on a spinning disk or a network filesystem. Measure your own tree with
`hyperfine`; see [ADR 0005](adrs/0005-traversal-and-parallelism.md) for the walk's design.

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

Issues and pull requests are welcome, especially new ecosystems.

If voom is useful to you, consider [sponsoring development](https://github.com/sponsors/Goldziher).

## License

[MIT](./LICENSE)
