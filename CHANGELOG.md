# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- Keep a Changelog repeats Added/Changed/Fixed headings per version. -->
<!-- markdownlint-disable MD024 -->

## [0.4.3] - 2026-08-29

### Changed

- **The demo is a current recording, and a reproducible one.** The GIF in the README had shipped
  through seven releases showing 0.2.0 output — it predated the cache catalog, `voom suggest`,
  `--include` and the `alef` and `basemind` entries. The new one is three beats over a real
  550 MB tree: a `--dry-run`, the same sweep for real reclaiming an identical 493.77 MB, and then
  `ls` on the `target/` that no `Cargo.toml` proved, still sitting there. A symlinked `target/`
  is refused rather than followed in both runs.

  `assets/demo.tape` and `scripts/demo-fixture.sh` are committed alongside it. The previous
  recording was made ad hoc and could not be regenerated, which is most of why it went stale —
  and it meant the README's central claim could not be checked by anyone but its author.

## [0.4.2] - 2026-08-29

### Fixed

- **Git housekeeping did not protect everything the deletion guard protects.** Both are described
  in the code as one denylist — ADR 0006's append-only `PROTECTED_PATHS` — and the git module's
  comment said it was "reused here rather than restated". It was restated. The deletion guard
  additionally resolves Windows locations from `SystemRoot`, `ProgramFiles` and `SystemDrive`,
  precisely because the literals assume the system drive is `C:`; the git copy did not. On a
  machine where Windows lives on `D:`, `C:\Windows` fails to canonicalize and contributes
  nothing, so a repository under `D:\Windows` or `D:\Program Files` was protected from deletion
  and not from `git worktree prune` / `git gc --auto`.

  There is now one implementation and the git module calls it. Path comparison there is also
  case-insensitive on Windows now, matching the guard — both sides are canonicalized in the
  normal flow, so this was consistency rather than a hole.

  Nothing in the git path deletes anything, so the stakes were low. It is fixed because a reader
  who checks one list and not the other must not get two answers.

## [0.4.1] - 2026-08-29

### Fixed

Six catalog entries removed things their marker did not prove. Every fix narrows what voom
deletes. Found by auditing each entry against the tool that supposedly writes it — the same
check that found `.alef-cache/` in 0.3.2, and the one no test can perform, because the generated
fixtures assert each entry against itself.

- **`.NET`'s `bin/` deleted source code.** The ecosystem anchors `Ancestor(3)`, so a single
  `App.sln` licensed every `bin/` up to three directories beneath it. In a repository that is
  both .NET and Rust — the normal shape of a polyglot repo — that removed `src/bin/`, which is
  Cargo's standard multi-binary **source** layout, and `tools/scripts/bin/`, which is
  hand-written. `bin/` now anchors `Sibling`. ADR 0003 records that a sweep of a real machine
  found all 55 .NET artifacts at depth 0, so the climb never bought recall; it only widened
  what one marker could reach. `obj/` keeps the climb — nothing but MSBuild writes that name.
- **Ruby's `.bundle/` is gone from the catalog.** It holds `.bundle/config` — written by
  `bundle config set --local`, routinely committed, and the store for **private gem source
  credentials**. Nothing regenerates it. A `Gemfile` proves Ruby; it does not prove that
  directory is derived.
- **Ruby's `vendor/bundle/` is now opt-in.** It is where `bundle install --path` puts installed
  gems: restoring it costs a `bundle install` and a network, which is the cost that makes every
  other dependency install off by default. Added to `--clean-dependencies`.
- **Python's `.tox/` is now opt-in.** Each `.tox/<env>/` is a virtualenv, the same reinstall
  cost that already made `.venv/` opt-in two lines below it.
- **Python's and CMake's `build/` are now opt-in.** `build/` beside a `pyproject.toml` or a
  `CMakeLists.txt` is as often hand-written build scripts, CMake modules or toolchain files as
  it is output — the reason node's `build/` has been opt-in all along. CMake's
  `cmake-build-*/` and `CMakeFiles/` are unambiguous and stay on.
- **Bazel matches its three convenience symlinks by name, not `bazel-*/`.** Every genuine
  target is a symlink that the rails refuse anyway, so the glob's only *effective* removals
  were false positives — a vendored `bazel-toolchains/` beside a `WORKSPACE` was removed by
  default.

### Fixed (performance)

- **A sweep got roughly five times slower after 0.4.0, and found nothing extra for it.** Moving
  `.alef/` to an `Inside` anchor meant an *untagged* one stopped being a matched artifact — so
  the walker stopped pruning it and descended into tens of gigabytes of cache instead. Measured
  over one home directory: `scan` 1.3 s → 7.7 s, 181 extra skips, and **zero** artifacts found
  by any of that walking.

  A directory carrying an `Inside`-anchored artifact's name but not its marker is now pruned.
  It is either an older cache of that tool or somebody else's directory of the same name, and
  either way voom has already declined to remove it, so walking it can only surface other
  projects nested inside a cache. That is a recall trade rather than a safety one, and it was
  measured at zero cost before it was made.

  Only reaches artifacts that are on by default: an off-by-default one reports `not_enabled`
  before its marker is consulted, and that must never prune — it fires for `build/` and `bin/`
  too, where descending is the point.

### Added

- **The README's interface documentation is now tested.** `tests/docs.rs` checked the ecosystem
  table and nothing else, which is how six flags, two configuration keys and the whole JSON
  schema went undocumented while the one tested table stayed perfect. Every long flag, every
  subcommand and every cache id must now appear in the README, introspected from clap rather
  than parsed out of the source.

## [0.4.0] - 2026-08-29

### Changed

- **`.alef/` is now proven by the `CACHEDIR.TAG` inside it, not by climbing to `alef.toml`.**
  The `Ancestor(6)` bound was measured honestly and inferred wrongly: alef resolves `.alef/`
  against the *invoking* directory rather than against the config that governs it, so the climb
  distance is a property of your repository's depth and where you ran alef from. It is unbounded
  in principle, and a larger `n` is not closer to correct. `Inside` removes the question instead
  of bounding it — the tag is in the directory or the directory is not proven, whatever sits
  above it.

  **A `.alef/` written by an alef too old to write the tag is now left alone**, until one run of
  a current alef restores it. That is a real drop in recall and the safe direction: a false
  negative costs disk, a false positive costs work.

  `MarkerOutOfReach` (0.3.1) keeps its place. It was added because of alef and stays because
  `Ancestor` still anchors .NET and Python, where a silent ceiling would be the same defect.

## [0.3.2] - 2026-08-29

### Fixed

- **`.alef-cache/` was removed on a marker that does not prove it.** 0.3.0 shipped it as an
  on-by-default artifact of the `alef` ecosystem, anchored on `alef.toml`. alef does not create
  that directory — the only mention of the name in alef's source is an ignore-list entry — and
  the one found in the wild held a **zig** global cache, in zig's own `b h o z` shard layout. So
  voom was deleting a directory on the strength of an unrelated tool's marker, which is precisely
  what ADR 0002 exists to prevent, and it was on by default.

  The entry is gone rather than turned off: `alef.toml` cannot prove a directory alef does not
  write, at any `default_on` setting. Nothing else in the catalog claims the name, so reaching it
  now takes an explicit `include`. `.alef/` is unaffected and still on by default.

  Found by alef's own maintainer while verifying the tagging work, not by a test here — a catalog
  entry is only as good as the claim that its marker proves its artifact, and no test can check
  that claim. Reviewing an entry against the tool that supposedly writes it is the check that
  works.

## [0.3.1] - 2026-08-29

### Added

- **`poly` cache entry.** poly 0.21.12 writes a `CACHEDIR.TAG` into its cache home, so
  `~/.cache/poly` is now provable and `--clean-caches poly` reaches it — 22.56 GB on the machine
  this was measured on. ADR 0012 said the fix for a cache whose contents are only hex-named
  directories belonged upstream rather than in a special case here; this is that path working,
  same day, with no special case.

### Fixed

- **`--clean-dependencies` did not mention Rust.** Its help text listed composer and Go
  `vendor/` but not Rust's, which 0.3.0 added to the group — so the flag removed a `cargo
  vendor` tree that nothing in the interface said it would.
- **An `Ancestor(n)` climb that ran out of bound is no longer reported as a missing marker.** The
  two are opposite statements and were indistinguishable: "no marker proves it" is a fact about
  the tree, while a climb that stopped short of the scan root is a fact about voom — the
  directory may well be an artifact whose project sits one level further up. An artifact past the
  bound was therefore not merely unswept, it read exactly like a directory that was never that
  ecosystem's. Reported as `marker_out_of_reach` now, naming the bound it stopped at.

  This matters more than a bigger bound would. Raised by alef's maintainer while reviewing the
  0.3.0 catalog entry: alef resolves `.alef/` against the *invoking* directory rather than
  against the config that governs it, so the distance is a property of the consumer's repository
  layout and no value of `n` is universally correct. A loud ceiling beats a larger silent one.

### Changed

- **The README is a README again.** It had accumulated a version-history section, tutorial
  hand-holding and a version-over-version benchmark table — 742 lines, of which the interface
  was the smaller part. Cut to 533: history belongs in this file, rationale in `adrs/`, and
  neither belongs in a README. Nothing documented was lost — every flag, subcommand,
  configuration key, JSON field and cache id is still there, and the cache table now lists the
  ids `--clean-caches` actually takes rather than naming the tools in prose. Badges match the
  accent the sibling repositories use.

## [0.3.0] - 2026-08-29

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
- **A cache catalog: `voom caches` and `--clean-caches <ids>`.** `--caches` promised to sweep
  machine-global tool caches and reached 0.25% of them — 252 MB out of 103 GB across
  `~/.cache`, `~/Library/Caches`, `~/.cargo` and `~/.rustup` — because it only re-opens the
  *walk* and the classifier then still wants a marker a cache does not have beside it. Fifteen
  named locations can now be removed outright: cargo's registry and git checkouts, uv, Gradle,
  the Go build and module caches, npm, pnpm, pub, deno and alef's global cache. Each is proven
  by a marker the tool wrote *inside* it — most often the cross-tool `CACHEDIR.TAG` — never by
  its path. None is on by default, none is reachable without naming its id, and every deletion
  rail applies unchanged. 41.6 GB reachable on the machine this came from. Caches whose
  contents are only shard directories (sccache, zig, NuGet, Maven) are deliberately absent:
  see [ADR 0012](adrs/0012-cache-catalog.md), and the fix is for those tools to write a
  `CACHEDIR.TAG`.
- **`voom suggest`.** Reports the repeated directories in a tree that git already ignores where
  they sit, ranked by size, with the `voom.toml` `include` line that would sweep each. It
  removes nothing. The signal is ADR 0005's own premise read backwards: the walker disables
  every ignore mechanism *because* build artifacts are exactly what `.gitignore` lists, which
  is what makes the same file the right thing to rank a suggestion by — and never good enough
  to delete on, since a `.gitignore` also lists local configuration and scratch data.
- **`--include <GLOB>`**, the command-line spelling of the `voom.toml` key. `include` was
  config-only while `--exclude` had a flag, so build output the catalog does not know about —
  a tool's per-project cache, an unconventional target directory — could not be reached from
  the command line at all. Same precedence as the config key: it outranks every prune, cannot
  reach inside one, and `--exclude` still beats it.

### Fixed

- **A sweep printed skip lines nobody could act on.** Sweeping `/tmp` named four repositories
  as `outside every scan root` or `could not resolve its .git file` — a worktree whose store
  lives in a home directory, and submodule pointers into a parent repository that no longer
  exists. Both are permanent, neither is anything voom can do about, and the first reads as
  though voom had wanted to touch the home directory. They are `--verbose` now. A repository
  skipped mid-rebase or on the protected denylist is still named, because that one the reader
  can act on.
- **Running out of file descriptors said nothing useful and was never retried.** A real sweep
  reported `failed 0 B rust vzdeep/a/target — Too many open files (os error 24)` with no
  explanation and no retry, because no stable `ErrorKind` names EMFILE so it landed in the
  catch-all. It is now recognised (EMFILE and ENFILE, and Windows'
  `ERROR_TOO_MANY_OPEN_FILES`), says what happened, and is retried under `--force` on the same
  backoff as a directory that was not empty — it is the same kind of problem, a fact about the
  moment rather than about the artifact. The trigger was roughly twenty large removals in
  flight at once; the artifact alone removes cleanly.
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
- **JSON `schema_version` is 3.** The `source` field gained the value `"cache"`, which no
  version-2 consumer handles. For a cache entry `ecosystem` carries the cache id and `artifact`
  is null, since no catalog declaration was involved.

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
