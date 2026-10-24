# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- Keep a Changelog repeats Added/Changed/Fixed headings per version. -->
<!-- markdownlint-disable MD024 -->

## [Unreleased]

## [0.13.0] - 2026-10-24

### Changed

- **A real run detaches instead of unlinking.** Once every rail has passed, an artifact is renamed
  into voom's trash (`voom/trash` under the platform cache directory, or `VOOM_TRASH_DIR`) in one
  `rename`, and a background reaper started when the run ends frees the trash at low priority. A
  sweep of Bazel output bases and package caches that kept its user waiting 5,360 seconds now
  returns once the walk is done. Detached artifacts are reported `detached`, without a size; the
  reaper counts what it frees, and the next run and `voom reap` report it. `--wait` (or
  `VOOM_WAIT=1`) keeps the in-place removal; an artifact on another filesystem than the trash is
  still removed in place. The JSON totals gain `detached`. See ADR 0020.
- **Git work trees on APFS are walked from their index.** A tracked directory whose link count and
  modification time agree with the index holds nothing untracked and costs one `lstat` instead of
  an `open` and a listing. Over 43 linked worktrees the scan went from 20.9 s to 5.9 s, and over a
  home directory from 32 s to 15 s, with identical findings.
- Long functions across the scanner, pipeline, configuration, classifier, Bazel, worktree, watch
  and report modules are split into helpers, and the CLI and safety integration suites are split
  into smaller files, clearing every `poly lint` quality warning.
- Dependencies are refreshed; ai-rulez is pinned at 5.6.3, OpenCode agents get a
  provider-qualified model, and the `poly` MCP server declares its package.

### Added

- **`voom reap`** frees whatever earlier runs detached, waiting first for a background reaper, and
  says what reapers freed.

### Fixed

- **A sweep no longer removes a directory its repository tracks.** A `dist/` holding a committed
  icon sprite sat beside a `package.json`, so the marker proved it, and every checkout of the
  repository was left with deleted tracked files. An inferred artifact holding a tracked file is
  now refused (`tracked_by_git`).
- The cache-cleanup benchmark measures in-place removal (`VOOM_WAIT=1`) now that a run detaches.
- The npm wrapper no longer returns a value from a promise executor, and the PyPI wrapper and
  benchmark write their messages without `print`.

## [0.12.0] - 2026-10-18

### Added

- **`--aggressive`** enables every catalog artifact, dependencies, caches, permission repair and
  unused Docker cleanup, and removes eligible worktrees without an age limit — discarding
  uncommitted and ignored files — plus dangling checkouts whose git administration is gone.
  Detached commits are retained under `refs/voom/aggressive-worktrees/`. See ADR 0018.

### Changed

- **Cache traversal and deletion are faster.** The removal pool defaults to eight workers on Apple
  platforms, small directories are unlinked in one serial batch there, single-child prefixes no
  longer spend the parallel recursion budget, and a full Bazel download-cache clear removes and
  counts in one pass. `task bench` compares binaries against native `rm`. See ADR 0019.

### Fixed

- CI validates the Windows test leg and generation from a clean checkout, and governance
  validation follows the project lock.

## [0.11.3] - 2026-10-18

### Fixed

- **`voom git-prune` fans out on its own descriptor-aware pool** rather than rayon's core-wide
  global pool, so the standalone subcommand is bounded the same way a sweep is.

## [0.11.2] - 2026-10-18

### Fixed

- **A sweep no longer exhausts the process's file descriptors.** Every default pool was eight
  workers per core, capped at 128, whatever `RLIMIT_NOFILE` allowed. Each worker holds a
  descriptor for every level of a tree it walks and one per subprocess it spawns, so on a machine
  with a low soft limit a sweep failed repository after repository with `worktree prune: Too many
  open files (os error 24)`. The default width now stays under a conservative fraction of the
  descriptor limit — the removal pool with it — and the binary raises its soft limit to the hard
  one at startup. A 300-level removal that previously stalled now finishes in fifty milliseconds,
  and a 27-repository tree that produced 25 `EMFILE` failures under a limit of 128 now produces
  none. An explicit `-j` is still honoured as written.

### Changed

- **ai-rulez is pinned at 5.5.0** across `.ai-rulez/`, `poly.toml`, `poly-hooks.lock` and
  CONTRIBUTING, and the generated outputs and lock are refreshed. The module-size-cap rule now
  records that a test enforces the cap, rather than saying nothing does.

## [0.11.1] - 2026-10-18

### Changed

- **Removal runs inside its own pool end to end.** The fan-out over artifacts is installed on the
  removal pool once instead of one install per artifact, so no sizing worker is parked feeding a
  pool of another width, and freed bytes are counted once per directory rather than once per
  unlinking entry.
- `voom docker-prune` takes its settings from flags only and does not read `[docker]`; the
  reference now says so.

### Fixed

- **`-j` is honoured by every run in the process, not only the first.** The removal pool was built
  once from the first width it saw; it is now rebuilt when the width changes.
- **A finding an interrupted (`EINTR`) directory read re-emits is de-duplicated.** Re-walking the
  directory could report an artifact twice, and the second removal was refused as vanished and
  counted as a safety refusal it never was.
- **A `--clean-caches` list that is only an exclusion is refused.** A lone `--clean-caches=-uv`
  subtracted from nothing and cleared no cache while reporting success.
- **Docker no longer swallows a problem.** A panicking lane and a stage error are both reported; a
  daemon's error text is stripped of terminal escapes before it reaches the terminal; an
  out-of-range daemon timestamp is refused rather than overflowing; and a Colima socket is placed
  only from a Colima config home, not any directory named `colima`.
- **A `--force` permission repair prefers a race-free `fchmodat`.** `AT_SYMLINK_NOFOLLOW` is tried
  first, with the plain form only where the kernel does not support it.
- **`git-prune` says so when the discovery classifier cannot be built**, rather than silently
  pruning less.
- A directory a mount point keeps non-empty is no longer re-swept three times before the same
  failure is reported.

## [0.11.0] - 2026-10-17

### Added

- **Ten more tool caches**, each proven by a marker its tool writes: `pnpm-store` (the
  content-addressable store, 20 GB on the machine that found it), `pip`, `homebrew`, `prek`,
  `pre-commit`, `ms-playwright`, `node-gyp`, `golangci-lint`, `tree-sitter-language-pack` and
  `huggingface`. A cache marker may now be a relative path (`v11/index.db`) or sit one directory
  down (`*/installVersion`). `huggingface` is excluded from a bare `--clean-caches` because models
  are expensive to fetch again; name it to remove it.
- `--sizes`, for `--list-caches`. See Changed.
- `claude-prune` reports an eligible job's scratch size in a dry run and in report-only mode, and
  `bytes` in its JSON is no longer `null` there.
- **`voom docker-prune`, and `--docker` on a sweep.** Removes dangling images, unused images older
  than `--max-age` (default `7d`, mapped to `until=`) and idle build cache; with `--volumes` also
  anonymous unmounted volumes, with `--volumes-all` all unmounted volumes. Never touches a
  container or anything one uses. A dry run is the same plan with the commands withheld, and a real
  run reports Docker's own reclaimed bytes. On Colima, Lima, Rancher Desktop and podman machine it
  then runs `fstrim` in the VM and reports the host bytes returned. A missing docker or stopped
  daemon is a one-line skip, never a failure. `[docker] enabled` / `max_age` set it from the user
  config. See ADR 0017.
- `voom git-prune --stale-worktrees <DURATION>`, as on a sweep.
- A checkout whose `.git` file points at a git administration directory that no longer exists is
  reported as `dangling` with its size, and never removed: nothing can prove it safe. See ADR 0016.

### Changed

- **Sizing and removal walk through directory descriptors, many times faster.** Every directory is
  opened with `openat` against its parent, and on macOS listed with `getattrlistbulk(2)` — names,
  types, allocated sizes, link counts and inodes for a whole directory per call, instead of an
  `fstatat` per entry. Sizing and removal default to I/O-sized pools (eight workers per core, up
  to 128) rather than one per core. Over twenty Bazel output bases under two endpoint-security
  agents the Bazel stage went from 440 s to 128 s, with sizes identical to `du` to the byte.
- **Removal is parallel inside each artifact, single-pass, and bounded.** It is the same walk with
  `unlinkat` in it, post-order, counting what it frees as it goes, so a Bazel output base is no
  longer measured and then walked again to be deleted. Every removal shares one pool, sixteen
  wide unless `-j` says otherwise: unlinking stops scaling well before reading does (100,000
  files: `rm -rf` 58 s; voom 19 s on sixteen workers, 101 s on 128). It never crosses onto
  another filesystem mounted inside an artifact, and keeps going past an entry it cannot remove.
- **A Bazel output base is removed with its permissions repaired.** Bazel writes its outputs
  read-only, so removal restored the owner's bits only under `--force`, and an orphaned base
  without it failed part-way. Never the base's parent, never a hard-linked file.
- **A cache its tool writes read-only is removed whole.** Go's module cache is `0555` by design
  and came out part-way without `--force`; it is now removed with the owner's bits restored, as
  `go clean -modcache` does. The catalog marks such entries `read_only`.
- **The Bazel stage is scoped to the swept trees.** `voom ~/code/worktrees` considers only output
  bases whose workspace lies under it; what names no workspace (an unmarked base, install bases,
  the download cache) only when a swept tree covers the home directory. `--clean-caches=bazel`
  remains machine-wide.
- **`--format json` is one document holding every stage.** The sweep's document gains `bazel`,
  `docker` and `worktrees` keys (`null` when the stage did not run), where those stages used to
  be one summary line each on stderr — a 200 GB Bazel finding was easy to miss there.
- **Worktree cleanup says why it kept each worktree.** With `-v`, every kept worktree is listed
  with its reason (not merged into the default branch, locked, missing, outside the swept paths,
  current). A merged pull request whose head is not the checkout's `HEAD` (squash-merged, then
  amended locally) is named with its number, and the JSON document carries a `reason` for every
  worktree. `voom::worktrees::document` is public so a sweep can embed the result.
- **`git-prune` dry runs say `would prune` only where git has something to remove**; a repository
  that needs nothing reads `nothing to prune`, and the footer counts only the former.
- **Worktree status check is staged.** A cheap tracked-files status settles any checkout with real
  tracked changes before the enumeration of untracked and ignored files, which on a monorepo took
  tens of seconds per worktree; the verdict is unchanged. Status no longer takes git's optional
  index lock in a checkout another session may be using.
- **`git-prune` discovery is much faster** (65 s to about 2 s over a directory of twelve
  repositories including a monorepo and 40 of its worktrees): the walk prunes the same build
  output a sweep does and no longer enters linked-worktree checkouts. A repository nested inside a
  linked worktree is no longer discovered by `git-prune`; one nested inside a main checkout still is.
- **`--list-caches` no longer measures by default.** Sizing 70 GB took 11 to 21 seconds, to print a
  table. The listing is now instant; `--sizes` adds the figures.
- Directories the user may not read (`EPERM`/`EACCES`, the privacy-protected folders under
  `~/Library` on macOS) are one footer line with a count instead of a list of walk errors; `-v`
  lists them. A walk error's `path` is now filled in the JSON, which carried `null` and the path
  only inside `detail`, and each has an `inaccessible` flag.
- A directory read interrupted by a signal (`EINTR`) is walked again, up to three times, before it
  is reported at all.
- `claude-prune` says why a job is kept when `state.json` proves nothing: no file, not JSON, never
  finished (no `lastTerminalAt`, as for a job parked on a question), or an unreadable timestamp,
  where it said "no readable state.json" for all four. In JSON, `detail.reason` carries the code.

### Fixed

- **A directory something writes into while it is removed is emptied again** (three times, with a
  short wait) before it is reported. A real sweep left residues in `go-build`, `uv` and Homebrew
  caches other work was using, and the next run had to take them.
- **`--clean-tagged` (and a full `--clean-caches`) no longer removes virtualenvs.** uv tags every
  venv it creates, tool installs included, so the tag alone took `~/.local/share/uv/tools/<tool>`
  — an installed command. A tagged directory holding `pyvenv.cfg` is left to the catalog.
- `--github-merged-worktrees` no longer reports a GitHub failure for a `HEAD` that exists only
  locally (HTTP 422 `No commit found for SHA`); it is simply not merged.

## [0.10.0] - 2026-10-14

### Added

- `--github-merged-worktrees`, with `--remove-merged-worktrees` on a sweep or `git-prune`,
  opts into exact-HEAD merged pull request proof through `gh api` for `origin` on github.com.
  Squash/rebase merges into any base branch qualify; failed lookups provide no proof. Local
  changes, ignored evidence and containment still protect worktrees. GitHub-proven commits, named or detached,
  without local ancestry proof are retained under `refs/voom/merged-worktrees/<SHA>` before
  removal; dry runs create no refs.

### Fixed

- Full `--clean-caches` / `--clear-caches` (including `=all`) now includes signature-verified
  tagged build caches, so relocated Cargo targets and tagged caches under the scan roots are
  reached. Named cache IDs stay scoped. `[caches] enable = ["all"]` enables tagged cleanup unless
  `[tagged] enabled = false` overrides it; full CLI clearing overrides the configuration.
  Default sweeps are unchanged, and worktree removal still requires its explicit flag.
- Merged-worktree cleanup now accepts ignored Elixir `deps/` beside a regular `mix.exs` and
  Python `.venv/` beside a regular `pyproject.toml`, `setup.py` or `setup.cfg` as regenerable.
  Candidates must be actual directories and neither candidates nor markers may be symlinks.
  Missing proof, configuration, notes and other ignored evidence still keep the checkout.

## [0.9.1] - 2026-10-08

### Fixed

- **Merged-worktree removal works on Windows.** The worktree path was handed to
  `git worktree remove` in the verbatim `\\?\C:\...` form, which git refuses, so
  `--remove-merged-worktrees` failed there. The path now goes to git in the form
  it accepts.

## [0.9.0] - 2026-10-08

### Added

- **`--stale-worktrees <DURATION>` removes idle unmerged worktrees too.** With
  `--remove-merged-worktrees`, a worktree not in the default branch goes when nothing has
  committed, checked out or rebased in it for the given time, some ref still reaches its `HEAD`
  (the branch is kept; a detached `HEAD` nothing else reaches is never stale), and nothing but
  deleted build output differs. Off unless given.

### Changed

- **A sweep also age-prunes the shared Bazel download cache**, which it used to leave to
  `bazel-prune`. `--bazel-max-age` sets the age, `--no-bazel` skips Bazel, and
  `--clear-caches=bazel` clears it completely.
- **The sweep is much wider.** The walk defaults to eight threads per core, capped at 128 (it was
  capped at 12), because it waits on the filesystem rather than computing; the Bazel stage runs
  beside the sweep instead of after it and fans out over output bases and cache files; git
  housekeeping overlaps sizing and removal. A single large checkout dropped from 35 s to 9 s.
- **Merged-worktree removal clears nested build output.** A deleted tracked file under a `dist/`,
  `target/`, `node_modules/` or `__pycache__/` at any depth no longer keeps an otherwise merged
  worktree; `build/` and `out/` still count only at the top level.
- Dependencies are at their latest releases (`cargo upgrade --incompatible`, `cargo update`).

### Removed

- **`voom bazel-prune`.** A plain sweep does everything it did. The explicit output-user-root
  arguments and `--all` went with it; `VOOM_BAZEL_ROOTS` still names the roots and
  `--clear-caches=bazel` replaces `--all`.

## [0.8.1] - 2026-10-05

### Fixed

- **A repository's `voom.toml` can no longer drive machine-global removal.** `[caches]` and
  `[bazel]` act on state outside the swept tree — tool caches and Bazel output bases — so they are
  read only from the user configuration or an explicit `--config`. A repository that sets one is a
  hard error rather than a silent no-op, so cloning a repository can never let it clear your
  machine's caches.
- **Merged-worktree removal keeps gitignored files.** `git status` hides ignored files and
  `git worktree remove` deletes them, so a worktree whose only content was a gitignored `.env` was
  removed and the file lost. Ignored files are now inspected and any non-build-output one keeps
  the worktree, and a deleted tracked file under a deeper directory that merely shares a
  build-output name (`src/build/`) is treated as source and keeps it too.
- **Bazel removal no longer decides by name alone.** `install/` and `cache/` are Bazel's only
  under a `_bazel_<user>` root, and an unmarked child of one is removed only when it carries
  Bazel's own `execroot/`; a `_bazel_`-named parent is not proof. A running server is detected by
  a process whose command line names the output base, and on a platform with no process check a
  recorded pid is treated as live.
- **Control files are read behind a size and type guard.** `DO_NOT_BUILD_HERE`, a `.git` file, a
  `commondir` and `state.json` are refused unless they are small regular files, so a FIFO or an
  enormous file at one of those paths cannot block or exhaust a run.
- **Concurrent `git worktree remove` calls within one repository are serialised**, removing a race
  on git's own `worktrees/` bookkeeping.
- **`--list-caches` now says the reserved `bazel` id exists** and what it clears.

## [0.8.0] - 2026-10-05

### Added

- **Bazel housekeeping now runs as part of an ordinary sweep, and reclaims idle output bases as
  well as orphaned ones.** An output base is removed when its recorded owner is gone, when its
  owner is a linked git worktree whose administrative directory has vanished, or when nothing has
  built there for longer than `--max-age` (default `7d`). "Built there" is the newest modification
  among the files Bazel touches on every invocation, so a base still in use refreshes and is kept,
  and a base with a running server is never touched. Reclaiming one also unlinks the owner's
  dangling `bazel-*` convenience symlinks. Install bases no surviving output base points at go the
  same way; entries in the shared download cache untouched within the age limit are pruned by
  `bazel-prune`, not by a sweep, because that prune is a full recursive walk. `--no-bazel` skips the
  stage, `--bazel-max-age` and `[bazel] max_age` set the age, and `[bazel] enabled = false` turns it
  off for a tree. Because a sweep now reaches outside the swept tree, anything driving the binary
  against a scratch tree sets `VOOM_BAZEL_ROOTS` to nothing.
- **`--remove-merged-worktrees` removes linked git worktrees whose work is already merged.** A
  worktree goes only when its `HEAD` is an ancestor of the default branch (the local `origin/HEAD`,
  else `main`/`master`; voom never fetches, so a stale ref errs toward keeping), it is not the main
  worktree, locked, missing, the directory voom was started in, or stored outside the scanned paths,
  and its working tree holds nothing but unstaged deletions of tracked build output or ignored
  build-output trees such as `target/`. Any modified, staged, added, renamed or untracked path, and
  any ignored non-build-output file such as a `.env`, keeps it, reported as merged-with-local-changes.
  The flag is off by default, has no `voom.toml` key, and never deletes a branch; `-n` runs every
  check and removes nothing. It works on a sweep and on `voom git-prune`, and runs before the Bazel
  stage so the output bases of the worktrees just removed are reclaimed in the same run.
- **`~/Library/Caches/bazel/_bazel_<user>` is a conventional output-user-root**, and
  `VOOM_BAZEL_ROOTS` (a path list) replaces the whole guess.

### Changed

- **`--clean-caches` clears Bazel completely.** `bazel` is a reserved id it accepts, and a bare
  `--clean-caches`/`--clear-caches` includes it: every output base, install base and cached download
  under the roots is removed, live workspaces included, after any running server is sent `SIGTERM`
  and given ten seconds to exit. Without the flag, a sweep only prunes what is stale.
- **A sweep always collects the repositories its walk passes**, so `--remove-merged-worktrees`
  finds them whether or not git housekeeping is enabled.

### Fixed

- **`BUILD`/`BUILD.bazel` are no longer Python markers.** They prove Bazel, not Python; treating
  every Bazel package directory as Python let a committed `dist/`, `*.egg-info/`, `htmlcov/` or
  `.coverage` be removed by default. The `WorkspaceRoot`-anchored Bazel entry still proves Python's
  cache artifacts in a Bazel repository.
- **`bazel-prune <dir>` no longer reads any `cache/` or `install/` child as Bazel's.** Those names
  are special only under a `_bazel_<user>` root, so an unrelated directory passed explicitly is
  left alone as ADR 0014 promises.

## [0.7.0] - 2026-09-18

### Added

- **A `WorkspaceRoot` marker anchor** lets an ecosystem be proven from the scan root down for a
  repository that governs packages with per-directory build files rather than a per-package
  manifest — Bazel's monorepo layout, where a 45-directory sample of 1,557 `__pycache__`
  directories found not one `pyproject.toml`/`setup.py`/`setup.cfg` anywhere above it. Python's four
  cache artifacts gain a second declaration, under the Bazel markers, proven that way, and
  `BUILD`/`BUILD.bazel` join Python's sibling markers as the cheaper fix for the common case.
- **`voom bazel-prune` reclaims orphaned Bazel output bases.** It reads the `DO_NOT_BUILD_HERE`
  marker Bazel writes into every output base, naming the workspace that owns it, and removes a base
  whose recorded owner no longer exists on disk; one whose owner survives is left alone and named.
  A shared, sequentially-reused output-user-root is recognised by its nested marker and never
  mistaken for a per-workspace base.
- **`voom claude-prune` age-gates and reports Claude Code job scratch** (`~/.claude/jobs/<id>/tmp`).
  It never trusts `state.json`'s own `state` field: it reads `lastTerminalAt` for age and checks the
  process list for the recorded session as a hard block, and removes nothing regardless of
  `--dry-run` unless the age gate, the liveness check and an explicit `--remove` all agree.

## [0.6.1] - 2026-09-17

### Added

- **`--clear-caches` is an accepted spelling of `--clean-caches`.** `clear` is the verb people
  reach for; it is a visible alias, so both appear in help and behave identically. The deletion
  rails are unchanged.

### Changed

- **`--list-caches` now lists this machine's caches by default.** It prints only the cache
  locations that exist on this machine, each with its path and how much disk it holds — measured
  with the same sizer a sweep uses, so the figure is what `--clean-caches` would reclaim.
  `--verbose` prints the whole table, absent locations, per-entry state and markers included.

## [0.6.0] - 2026-09-16

### Changed

- **The cache surface is now two flags.** `--clean-caches` gained a bare form that removes
  every cache the table knows about; `--clean-caches=<ids>` keeps its named form (with a
  required `=`, matching `--one-file-system`, so the flag can never swallow the next path
  argument). `voom caches` is now `--list-caches`, which prints the table and exits without
  scanning.

### Removed

- **`--caches` is gone.** It never removed anything — it only let the walk enumerate inside
  installed toolchains and tool caches, buying incidental debris for hours of scan time (see
  ADR 0012's context). The location skip is now unconditional, and a cache is reached by
  `--clean-caches[=<ids>]` or `[caches] enable` rather than by opening the location to the walk.
  `git-prune` no longer carries a corresponding option either.

## [0.4.5] - 2026-08-31

### Fixed

- **A sweep of a home directory could abort the process** with
  `user-provided comparison function does not correctly implement a total order`. Git
  housekeeping sorts its resolved repositories before deduplicating them, and the comparator
  was not a total order: two repositories compared on whether they were linked worktrees
  first, while a repository against a refused path compared on path alone. A linked worktree,
  the main repository it shares a store with, and a refusal could therefore form a cycle —
  `a < b`, `b < c`, `c < a`. Rust's sort detects that and panics, killing the run.

  It needed a couple of dozen repositories of mixed kinds to surface, which is why it only
  ever appeared on a real machine and never in the fixtures. The ordering is now derived from
  a tuple key, which cannot express a cycle, and keeps the behaviour it was written for: a
  main repository still sorts before a linked worktree of the same repository, so it is the
  one kept and the one the report names.

## [0.4.4] - 2026-08-29

### Changed

- **The reference material moved out of the README**, which is 414 lines instead of 742. The
  options table, the JSON field tables and the full `voom.toml` schema are things a reader
  consults rather than reads; they are in [`docs/reference.md`](docs/reference.md) and
  [`docs/configuration.md`](docs/configuration.md) now, with a short version and a link in the
  README. Nothing was dropped.

  `tests/docs.rs` checks the documentation *set* rather than the front page: every flag,
  subcommand and cache id must appear somewhere a reader is pointed at, and every `voom.toml`
  example in any of the three files still loads through the real configuration pipeline.
  Checking only `README.md` would have made moving a table look identical to deleting one.

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

[Unreleased]: https://github.com/Goldziher/voom/compare/v0.13.0...HEAD
[0.13.0]: https://github.com/Goldziher/voom/compare/v0.12.0...v0.13.0
[0.12.0]: https://github.com/Goldziher/voom/compare/v0.11.3...v0.12.0
[0.11.3]: https://github.com/Goldziher/voom/compare/v0.11.2...v0.11.3
[0.11.2]: https://github.com/Goldziher/voom/compare/v0.11.1...v0.11.2
[0.11.1]: https://github.com/Goldziher/voom/compare/v0.11.0...v0.11.1
[0.11.0]: https://github.com/Goldziher/voom/compare/v0.10.0...v0.11.0
[0.10.0]: https://github.com/Goldziher/voom/compare/v0.9.1...v0.10.0
[0.9.1]: https://github.com/Goldziher/voom/compare/v0.9.0...v0.9.1
[0.9.0]: https://github.com/Goldziher/voom/compare/v0.8.1...v0.9.0
[0.8.1]: https://github.com/Goldziher/voom/compare/v0.8.0...v0.8.1
[0.8.0]: https://github.com/Goldziher/voom/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/Goldziher/voom/compare/v0.6.1...v0.7.0
[0.6.1]: https://github.com/Goldziher/voom/compare/v0.6.0...v0.6.1
[0.6.0]: https://github.com/Goldziher/voom/compare/v0.5.0...v0.6.0
[0.4.5]: https://github.com/Goldziher/voom/compare/v0.4.4...v0.4.5
[0.4.4]: https://github.com/Goldziher/voom/compare/v0.4.3...v0.4.4
[0.4.3]: https://github.com/Goldziher/voom/compare/v0.4.2...v0.4.3
[0.4.2]: https://github.com/Goldziher/voom/compare/v0.4.1...v0.4.2
[0.4.1]: https://github.com/Goldziher/voom/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/Goldziher/voom/compare/v0.3.2...v0.4.0
[0.3.2]: https://github.com/Goldziher/voom/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/Goldziher/voom/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/Goldziher/voom/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/Goldziher/voom/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/Goldziher/voom/releases/tag/v0.1.0
