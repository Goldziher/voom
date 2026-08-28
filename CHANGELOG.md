# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- Keep a Changelog repeats Added/Changed/Fixed headings per version. -->
<!-- markdownlint-disable MD024 -->

## [Unreleased]

## [0.1.0] - 2026-08-28

The first functional release. `0.0.0` on crates.io and PyPI was a name placeholder and does
nothing; this is the tool.

### Added

- **`voom <path>` finds and removes build artifacts anywhere under a tree, in parallel.** One
  pure-Rust binary, no runtime dependency, no daemon. A dry sweep of a 130 GB `~/workspace`
  takes 5.3 s and of a whole `$HOME` 10.7 s on an M-series Mac.
- **Marker-anchored classification across 24 ecosystems** — Rust, Node, Python, Go, Zig, Swift,
  Xcode, Maven, Gradle, .NET, CMake, Dart, Elixir, Ruby, PHP, Scala, Haskell, OCaml, Julia, R,
  Nim, Elm, Terraform, and Bazel. A directory is never an artifact because of its name: `target/`
  is Rust's build output only when a `Cargo.toml` sits beside it, and `obj/` is .NET's only when
  a `*.csproj` is found within three levels above it. Every catalog entry carries two generated
  fixtures — marker present and the artifact goes, marker absent and it stays — so the
  data-loss case is covered the moment an entry lands.
- **Six deletion rails, applied to every removal**: symlinks and Windows reparse points are
  refused rather than followed; the target is canonicalized before anything else; an
  append-only denylist refuses system and VCS paths; the result must resolve strictly below a
  scan root; `--one-file-system` refuses to cross a device boundary; and a failure on one
  artifact is reported and isolated rather than aborting the run. The denylist is resolved
  against the running machine, so it covers what each platform's layout actually resolves to —
  macOS's `/private/etc` behind `/etc`, a usrmerge Linux's `/usr/bin` behind `/bin`, and
  Windows' verbatim `\\?\C:\Windows` — rather than only the literal spellings.
- **Command-line keep policies outrank every configuration layer.** A `voom.toml` committed in
  someone else's repository cannot release an artifact that the `--min-age` you typed is
  holding, which is what makes a blanket flag over an unfamiliar `$HOME` mean anything.
- **`--dry-run`, which is the same pipeline with the final step withheld** rather than a second
  implementation. A test asserts a dry run leaves a fixture byte-identical and that its report
  names exactly what a real run then removes.
- **Hierarchical `voom.toml` configuration** — per-directory files merged from the scan root
  down, with `include`/`exclude` globs, per-ecosystem and per-artifact enables, and `min_age`,
  `min_size` and `max_size` keep policies. `include` authorizes a removal no marker proves; the
  report labels those `unanchored` in both renderers so an unproven deletion can never be
  mistaken for a proven one.
- **Deterministic reporting in two formats.** Human output is sorted by path, grouped into
  artifact, skip and failure blocks, printed relative to the scan root, and carries a reason for
  every skip — the missing marker, the keep policy, the exclusion, or the boundary.
  `--format json` renders the same result set with a `schema_version`, and `--summary` prints
  the footer alone.
- **Machine-global caches and installed toolchains are skipped by location**, not by marker —
  fifty roots in all, `~/.cargo/registry`, `~/.npm`, `~/.pyenv` and `~/google-cloud-sdk` among
  them. Marker anchoring cannot tell an installed program from a project built in place, because
  an installed pnpm really does sit beside a real `package.json`. `--caches` opts back in; on
  this machine it is the difference between 174 artifacts and 475.
- **Watch mode** (`voom watch`) — a debounced `notify` loop that keeps a tree pruned without
  touching an in-progress build.
- **Git hook integration** for both `pre-commit` and `poly`, reporting by default and never
  deleting from a hook unless asked.
- **`-j` bounds the entire sweep**, not just the walk: sizing and deletion run in a scoped rayon
  pool rather than on the global one. `-j 1` takes 2.7× the default, which is the point of the
  flag on a spinning disk or a network filesystem.
- **Distribution through five channels from one binary** — crates.io (`voom`), npm
  (`@goldziher/voom`), PyPI (`voom-cli`), Homebrew (`Goldziher/homebrew-tap`), and
  `cargo-binstall`, with prebuilt archives for macOS arm64/x86_64, Linux arm64/x86_64
  (manylinux glibc floor), and Windows x86_64.

### Notes

- **Dependency directories are permanently out of scope.** `node_modules/`, composer `vendor/`,
  Go `vendor/` and mix `deps/` are network-fetched caches, not build output; removing them costs
  a re-download and breaks offline work. No flag turns them on — only an explicit `include` in
  your own configuration reaches them. See [ADR 0001](adrs/0001-mission-and-scope.md).
- **Both denylists are append-only.** Entries may be added to the protected-path list and to the
  cache-root list; removing or narrowing one requires an ADR amendment, and a test now fails if
  either shrinks.
- **The project was renamed from its working title `pruner`**, which was unavailable on
  crates.io, npm, and PyPI. [ADR 0010](adrs/0010-distribution-and-naming.md) records why the
  package names differ across registries (`voom`, `@goldziher/voom`, `voom-cli`) while the
  binary is `voom` everywhere.
- **A walk interrupted by `EINTR` is reported separately from a directory that genuinely could
  not be read.** The `ignore` callback API offers no way to resume a subtree, so the interrupted
  case is surfaced under `--verbose` rather than retried; the limitation is recorded in
  [ADR 0005](adrs/0005-traversal-and-parallelism.md) instead of being papered over.
- Size accounting sums allocated blocks, so on a copy-on-write filesystem such as APFS the bytes
  reported can exceed the space actually freed where files are shared between clones.

[Unreleased]: https://github.com/Goldziher/voom/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/Goldziher/voom/releases/tag/v0.1.0
