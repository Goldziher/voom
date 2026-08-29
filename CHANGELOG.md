# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- Keep a Changelog repeats Added/Changed/Fixed headings per version. -->
<!-- markdownlint-disable MD024 -->

## [Unreleased]

### Added

- **`Anchor::Inside` — a marker looked for inside the candidate rather than around it.** Some
  directories carry no proof beside them and never will: nothing next to `~/.cargo/registry`
  says what it is, and `~/.cache/uv` sits beside unrelated caches. The proof is *within* —
  `CACHEDIR.TAG`, whose spec declares the directory regenerable, is already present in cargo's
  registry and git caches, uv's cache and gradle's. A first-party declaration is stronger
  evidence than a sibling file, not weaker, so this extends ADR 0002 rather than bending it.
  A candidate anchored `Inside` with no marker is left alone exactly as before, and reports
  `no_inner_marker` so the reader knows where to look.
- **`alef` and `basemind` catalog entries.** `alef` anchors `Ancestor(6)` on `alef.toml` and
  is on by default — it holds snippet build output that alef rebuilds in seconds. `basemind`
  anchors `Inside` on the `agent-id` file it writes into every index, and is **off by default**:
  the proof is good, but rebuilding an index means re-reading and re-embedding a whole
  repository, so it comes out only when you name `basemind.basemind`. Both anchors are
  measured, not guessed — `basemind.toml` sat beside only 3 of 13 real indexes while `agent-id`
  was inside all 13. On one workstation this reaches 69 `.alef/` directories that no flag could
  previously touch.
- **`rust.vendor`.** `vendor/` is pruned by the walker as a dependency directory, and the
  dependency-artifact list had entries for Go and PHP but none for Rust — so a `cargo vendor`
  tree was unreachable by any flag, not merely off by default. 1.41 GB of one on the machine
  this was found on. Off by default with a note, like every other dependency directory, and
  reached by `--clean-dependencies` or `--enable rust.vendor`.
- **`--include <GLOB>`**, the command-line spelling of the `voom.toml` key. `include` was
  config-only while `--exclude` had a flag, so build output the catalog does not know about —
  a tool's per-project cache, an unconventional target directory — could not be reached from
  the command line at all. Same precedence as the config key: it outranks every prune, cannot
  reach inside one, and `--exclude` still beats it.

### Fixed

- **A relative `--exclude` pattern silently did nothing.** `--exclude build` compiled to a glob
  that could never match a path the walker produces, so voom reported success and swept the
  directory anyway. Only absolute patterns worked, and nothing said so. Relative flag patterns
  are now anchored to the tree being swept, the same rule ADR 0004 already states for
  `voom.toml`. This is a silent failure on the flag whose only job is to protect a path, so
  check any `--exclude` you rely on: it may not have been doing anything.
- An artifact taken by `include` said it came "from config" even when a flag named it. It now
  says `included by \`<pattern>\``.

### Changed

- `Anchor` is now `#[non_exhaustive]`, matching every other public enum in the crate.

## [0.2.0] - 2026-08-28

The `uvx` and publish fixes below were prepared as `0.1.1` and never tagged, so they ship here
instead.

### Added

- **A removal that only partly succeeded is now reported as one, and its bytes are counted.**
  `remove_dir_all` unlinks a tree's contents before unlinking the tree itself, so a failure
  part-way through leaves real space reclaimed and the artifact still on disk. voom used to
  report that as a plain failure worth zero bytes — so a sweep that freed roughly 130 GB
  printed a footer saying it had reclaimed 25. The report is the only record of a destructive
  act, and it was wrong by an order of magnitude. Partial removals now show what was freed and
  what is left, the headline counts every byte the run actually freed, and the footer says so
  explicitly: `1 artifact partly removed: 130 GB freed and counted above, 500 MB left`.
- **Failures say what went wrong in words.** `Directory not empty (os error 66)` became
  *"a directory was not empty when voom unlinked it, most likely because something wrote into it
  during the removal"* — hedged to exactly the strength the standard library's own documentation
  uses, and followed by what would plausibly help (`re-run to finish`). The classification is by
  `io::ErrorKind`, never by the error number: ENOTEMPTY is 66 on macOS and 39 on Linux, and 39
  on macOS means something else entirely.
- **An ordinary sweep now runs git's own local housekeeping** in every repository it walks
  past: `git worktree prune`, and `git gc --auto`, which repacks only when git's own thresholds
  say it is worth it. Discovery is free — the walker already meets `.git` on its way past — so
  there is no second pass over the tree. Nothing is force-removed: voom never passes
  `--prune=now`, `--aggressive`, or `git reflog expire`, and never contacts a network. A
  repository with a rebase, merge, cherry-pick or bisect half finished is skipped and said so,
  and a linked worktree is resolved to its object store and pruned once rather than once per
  checkout. Off with `--no-git` or `[git] enabled = false` in a `voom.toml`; the flag wins.
- **`voom git-prune`** runs the same housekeeping on its own, and is the only place the network
  step lives — `--remotes` drops remote-tracking branches whose upstream is gone. It is
  deliberately not part of a sweep: one round trip per remote per repository hangs without
  connectivity and can block on a credential prompt.
- **`--clean-dependencies`** removes dependency directories — `node_modules/`, composer's
  `vendor/`, Go's `vendor/`, mix's `deps/` and Python's `.venv/`. [ADR 0001](adrs/0001-mission-and-scope.md)
  said no flag would ever turn these on; that line is reversed by amendment, and only as far as
  an explicit opt-in reaches. They are ordinary off-by-default catalog entries proved against
  their own markers, no ordinary run takes one, and the footer names it when one is removed.
- **The footer says where the run's time went**, not just how long it took:
  `scan 2.61s · policy 645µs · size 2.28s · delete 1.42s`. One number cannot be acted on — a
  sweep that spends its time walking wants a different fix from one that spends it sizing. JSON
  carries the same in `timings_us`.
- **`--force`** retries a failed removal, repairing permissions inside the artifact first —
  read-only bits, and on macOS the user-immutable flag. It answers the two failures voom can do
  something about: a concurrent writer, which is a question of time, and a mode that forbids an
  unlink. A read-only mount and an I/O error are reported rather than retried, because neither
  improves by waiting.

  `--force` never relaxes a safety rail. Every part of it takes a path that has already passed
  all six, and there is no way to obtain one otherwise. It never follows a symlink, and never
  touches anything outside the artifact — **including the artifact's own parent**, so an
  artifact held by a read-only parent is reported rather than forced. It also never widens a
  hard-linked file: a mode bit belongs to the inode, so relaxing one would change data voom is
  not deleting, possibly outside the scan root entirely. Watch mode never forces.
- **A `.git` directory holding nothing is no longer reported as a failed repository.** A tool
  that sets a checkout up and never initialises it leaves one behind, and voom said
  `fatal: not a git repository` about it on every sweep. Found by a dry run over a real home
  directory.

### Performance

- **Sizing is divided across cores inside each artifact, not just across artifacts.** The new
  stage breakdown is what found this: on a 127 GB `~/workspace` dry run, sizing cost more than
  the walk did. A sweep's artifacts are wildly uneven — one `target/` was 79 GB of that total —
  so the largest tree was measured by a single thread while every other core idled. A whole dry
  run of that tree went from **6.88 s to 4.63 s**, a 1.49× speedup measured with `hyperfine`
  over 8 runs. Reported sizes are byte-identical to the serial walk.
- **Git steps no longer pay a fixed 25 ms poll each.** A git step against a local repository
  returns in one to three milliseconds, so a flat poll missed once and then charged 25 ms of a
  blocked worker to every step. Backing off from 100 µs makes a dry run over 200 repositories
  **1.47× faster**.
- **Measuring no longer resolves each entry's path from the root.** `DirEntry::metadata` is an
  `fstatat` against the handle the walk already holds; the old route allocated a path and
  resolved it again, at about 0.4 µs per entry.

Together with the sizing fan-out, a dry run of a real `~/workspace` is **1.48× faster than
v0.1.0** (8.89 s → 6.01 s, hyperfine, 10 runs) while finding exactly the same 85 artifacts.

### Fixed

- **A symlinked artifact is named in the report instead of counted.** A `target/` that is a
  symlink was turned into a skip and never entered the pipeline, so it showed up only inside the
  anonymous "candidates passed over" number unless `-v` was given. A link standing exactly where
  an artifact belongs is the shape of a mistake worth looking at. It is now a finding the same
  safety rail refuses by name — `refused 0 B rust target — is a symlink` — which also makes the
  catalog's claim about Bazel's `bazel-*` convenience links true. Nothing about the rail changed
  and it cannot become a deletion.
- **The `uvx` channel of both git hooks now runs.** `voom` was taken on PyPI, so the
  distribution is `voom-cli` while the command it installs is `voom` — and `uvx` looks for a
  command matching the *package* name, finds none, and refuses. Both published hooks pinned
  `uvx voom-cli`, so that channel failed for every user on first run. It is
  `uvx --from voom-cli voom` now, with a test pinning the form because the asymmetry is
  permanent (see [ADR 0010](adrs/0010-distribution-and-naming.md)).
- **The crates.io publish no longer fails on its own error log.** The release workflow
  redirected `cargo publish`'s stderr to a file inside the checkout; the shell creates that file
  before cargo runs, and `cargo publish` refuses a working directory with uncommitted changes.
  0.1.0 reached crates.io only on a re-run.
- **Installed binaries are left alone.** `~/.cargo/bin`, `~/go/bin`, `~/Library/Developer/Toolchains`
  and `~/.local/share/gh` join the cache list — around 5.6 GB of installed tools on the machine
  this was found on, previously resting on the marker rule alone.

### Changed

**Breaking, for anyone using voom as a library or parsing its JSON.**

- `Outcome` gains a `PartiallyRemoved` variant and is now `#[non_exhaustive]`, matching
  `Refusal` and `Error`. `Outcome::Failed` carries a structured `RemovalFailure` instead of a
  string, so a consumer can branch on the kind without matching on the operating system's prose.
- `Guard::remove` takes the artifact's pre-removal size and a `Removal`, which replaces the bare
  `dry_run` flag.
- `RunResult::elapsed` becomes `RunResult::timings.total`, and `RunResult` gains `git`.
- `Totals` gains `dependencies` alongside `partial` and `partial_bytes`, and is
  `#[non_exhaustive]`. **`Totals::bytes` now
  means every byte the run freed**, of which `partial_bytes` is a documented subset;
  `Totals::reclaimed` still counts only artifacts that are entirely gone.
- **JSON `schema_version` is now `2`.** A failed artifact's `reason` is specific
  (`not_empty`, `permission_denied`, `read_only_filesystem`) rather than always
  `removal_failed`, and carries `os_error` and `os_message`. Artifacts gain `reclaimed_bytes`
  beside `bytes`, so `sum(artifacts[].reclaimed_bytes) == totals.bytes` holds for every run.

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

[Unreleased]: https://github.com/Goldziher/voom/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/Goldziher/voom/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/Goldziher/voom/releases/tag/v0.1.0
