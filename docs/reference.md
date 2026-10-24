# Reference

Every flag, subcommand and JSON field voom accepts or emits. `tests/docs.rs` checks this file
against `src/cli.rs`, `src/caches/catalog.rs` and `src/report/json.rs`, so a surface that is not
here fails the build rather than going quietly undocumented.

Back to the [README](../README.md).

## Options

The `Group` column matches `voom --help`'s own grouping.

| Group | Flag | Short | Default | What it does |
| --- | --- | --- | --- | --- |
| Behaviour | `--dry-run` | `-n` | off | Report what would be removed, without touching anything. |
| Behaviour | `--report` | | | Print the per-artifact report. Already the default; accepted so a git hook can name it explicitly. |
| Behaviour | `--exit-code` | | off | Exit `3` when a dry run finds artifacts, for a hook to fail on. |
| Behaviour | `--remove-merged-worktrees` | | off | Remove linked git worktrees already merged into the default branch (local `origin/HEAD`, `main` or `master`; never fetches). Kept when locked, current, outside the swept paths, or holding any change other than deleted tracked build output or ignored build-output trees. `-v` lists every kept worktree with its reason; a checkout whose git administration is gone is reported as `dangling` and never removed. Branches are never deleted. Not settable from `voom.toml`. With `-n`, only reports. |
| Behaviour | `--stale-worktrees <DURATION>` | | off | With `--remove-merged-worktrees`, also remove unmerged worktrees idle at least this long (last commit, checkout or rebase), when a ref still reaches their `HEAD` and only deleted build output differs. Branches are kept; a detached `HEAD` nothing else reaches is never removed. |
| Behaviour | `--no-bazel` | | off | Skip the Bazel housekeeping a sweep runs beside the artifacts: orphaned, abandoned and stale output bases, unused install bases, and downloads past the age limit. Only output bases whose workspace lies under a swept tree are considered; unmarked bases, install bases and the download cache only when a swept tree covers the home directory. Bases are removed with Bazel's read-only modes repaired, as `bazel clean` does. `--clear-caches` clears Bazel completely, machine-wide, instead. |
| Behaviour | `--bazel-max-age <DURATION>` | | `7d` | How long an output base may go unbuilt in, or a shared-cache download sit unused, before the sweep treats it as stale. |
| Behaviour | `--docker` | | off | Also prune Docker beside the sweep: dangling images, unused images older than `--docker-max-age`, build cache idle that long, then trim the VM's disk (Colima, Lima, Rancher Desktop, podman machine). Never touches containers or anything a container uses. A missing docker or stopped daemon is one line on stderr. Also `[docker] enabled = true` (user config only). See ADR 0017. |
| Behaviour | `--docker-max-age <DURATION>` | | `7d` | How old an unused Docker image or idle build-cache record must be, mapped to `until=` in whole hours. Also `[docker] max_age`. |
| Behaviour | `--docker-volumes` | | off | With `--docker`, also remove anonymous volumes no container mounts. Volumes are data. Command line only. |
| Behaviour | `--docker-volumes-all` | | off | With `--docker`, also remove every volume no container mounts, named ones included. |
| Behaviour | `--no-git` | | off | Skip `git worktree prune` and `git gc --auto` in every repository walked. Also `[git] enabled = false`; the flag wins. |
| Behaviour | `--force` | | off | Retry a failed removal, first clearing read-only bits (and, on macOS, the immutable flag) on paths already inside a proven artifact. Never relaxes a rail, follows a symlink, or touches the artifact's parent. A repair that does not go on to succeed is not undone. |
| Behaviour | `--wait` | | off | Unlink every artifact before exiting instead of detaching it into the trash for the background reaper (`voom reap`). Also `VOOM_WAIT=1`. See ADR 0020. |
| Behaviour | `--jobs <N>` | `-j` | see note | Worker threads for the walk, sizing and removal. Unset, the walk and sizing run eight per core (8–128), because they wait on the filesystem rather than compute, and removal runs sixteen, shared by every artifact being removed, because unlinking stops scaling well before reading does. Lower on spinning disks or network filesystems. |
| Output | `--verbose` | `-v` | off | Explain every candidate that was passed over. Also expands `--list-caches` to the whole table rather than only the caches present on this machine. |
| Output | `--summary` | | off | Print only the totals footer. |
| Output | `--format <human\|json>` | | `human` | See [JSON output](#json-output). |
| Output | `--color <auto\|always\|never>` | | `auto` | `auto` colorizes when stdout is a terminal, honouring `NO_COLOR` and `CLICOLOR_FORCE`. |
| Selection | `--ecosystem <ID>` | `-e` | all | Only sweep this ecosystem. Repeatable. |
| Selection | `--enable <SPEC>` | | | Turn on an off-by-default artifact, e.g. `python.venv`. Repeatable. |
| Selection | `--disable <SPEC>` | | | Turn off an ecosystem or one of its artifacts. Repeatable. |
| Selection | `--clean-dependencies` | | off | Also remove `node_modules/`, Rust, composer and Go `vendor/`, mix `deps/` and Python virtualenvs. Sugar over `--enable`-ing each. |
| Selection | `--exclude <GLOB>` | | | Never scan this path. Repeatable. Relative to the tree being swept. |
| Selection | `--include <GLOB>` | | | Remove this path whether or not a marker proves it. Repeatable. Outranks every prune, cannot reach inside a pruned directory, and `--exclude` still wins. |
| Selection | `--clean-caches[=<IDS>]` | | empty | Remove tool caches under the swept tree, each still on its marker. A bare `--clean-caches` (alias `--clear-caches`) removes every cache the table knows about; `--clean-caches=uv,go-build` removes named ones, repeatable or comma-separated; a `-` takes an id back out, so `--clean-caches=all,-bazel` is every cache without the machine-wide Bazel clear (which stops running Bazel servers). The `=` is required or clap reads the next argument as a path. See `--list-caches`. |
| Selection | `--clean-tagged` | | off | Remove any directory carrying a valid `CACHEDIR.TAG`, on that declaration alone. Reaches relocated and renamed build directories the catalog cannot name. Also `[tagged] enabled`. |
| Output | `--list-caches` | | | Print the cache table and exit without scanning. By default only the caches present on this machine, each with its path and nothing measured; `--verbose` shows every location the table knows about, its state, and the marker that would prove it. Always exits `0`. |
| Output | `--sizes` | | off | With `--list-caches`, measure each cache and print its size with the sweep's own sizer. Off by default because it walks tens of gigabytes. |
| Selection | `--config <PATH>` | | discovered hierarchy | Use this configuration file instead of resolving `voom.toml` from the tree. |
| Keep policies | `--min-age <DURATION>` | | unset | Never remove an artifact modified more recently, e.g. `7d`. |
| Keep policies | `--min-size <SIZE>` | | unset | Skip artifacts smaller than this, e.g. `1MB`. |
| Keep policies | `--max-size <SIZE>` | | unset | Skip artifacts larger than this, e.g. `50GB`. |
| Safety | `--one-file-system[=BOOL]` | | `true` | Stay on the scan root's filesystem. `--one-file-system=false` opts out — the `=` is required, or clap reads the next argument as a path. |

**Subcommands**

| Command | What it does |
| --- | --- |
| `voom catalog` | Print the built-in ecosystem catalog. Always exits `0`. |
| `voom config show [PATH]` | Print the fully merged, resolved configuration and the files it came from. `PATH` defaults to `.`. Always exits `0`. |
| `voom suggest [PATHS]` | Rank repeated directories a `.gitignore` names that the catalog does not cover. Removes nothing. Takes `--one-file-system[=BOOL]` and `-j`/`--jobs <N>`. Always exits `0`. |
| `voom watch [PATHS]` | Keep a tree pruned continuously. `--debounce <DURATION>` (default `5s`), `--quiet-period <DURATION>` (default `60s`). Sweep flags come from the top-level arguments. |
| `voom claude-prune [ROOTS]` | Age-gate and report Claude Code job scratch (`~/.claude/jobs/<id>/tmp`), removing nothing without `--remove`. `-n`/`--dry-run`, `--remove`, `--min-age <DURATION>` (default `14d`), `--force`, `--format <human\|json>`. Roots default to `~/.claude/jobs`. See ADR 0015. |
| `voom docker-prune` | Reclaim Docker disk space on its own, then trim the VM and report host bytes returned. `-n`/`--dry-run`, `--max-age <DURATION>` (default `7d`), `--volumes`, `--volumes-all`, `--no-trim`, `--timeout <DURATION>` (default `15m`), `--format <human\|json>`. Takes its settings from flags only — it does not read `[docker]`; `--max-age` here is not the sweep's `--docker-max-age`. Always exits `0`; a docker that cannot be reached is a one-line skip. See ADR 0017. |
| `voom reap` | Free everything earlier runs detached into the trash, waiting first for a background reaper already at work, and report what reapers freed. The trash is `voom/trash` in the platform cache directory, or `VOOM_TRASH_DIR`. `--background` is how a sweep starts the reaper: low priority, no output. See ADR 0020. |
| `voom git-prune [PATHS]` | Run git's own housekeeping on its own. `-n`/`--dry-run`, `--remotes`, `--remove-merged-worktrees`, `--github-merged-worktrees`, `--stale-worktrees <DURATION>`, `--timeout <DURATION>`, `--expire <TIME>` (default git's own three-month `gc.worktreePruneExpire`), `--format <human\|json>`. Exits `1` if a repository's housekeeping failed. |

## JSON output

`--format json` renders one JSON object per run on stdout; diagnostics stay on stderr, so
`voom ~ --format json | jq` works without filtering. It implies `--verbose` — a machine consumer
has no way to ask for detail after the fact — and suppresses the scan spinner. The shape is
versioned (`schema_version`, currently `3`) so a breaking change is deliberate rather than
accidental.

```bash
voom --dry-run --format json ~ | jq '.totals'
voom --format json ~ | jq '.artifacts[] | select(.outcome.status == "failed")'
```

Top level:

| Key | Type | Meaning |
| --- | --- | --- |
| `schema_version` | number | `3`. Bumped only on a breaking change. |
| `dry_run` | boolean | Whether the final step was withheld. |
| `roots` | string[] | The scan roots, as given. |
| `artifacts` | object[] | One per artifact found; see below. |
| `skipped` | object[] | `{ path, reason, detail }` for every candidate passed over — always populated in JSON. |
| `walk_errors` | object[] | `{ path, detail, transient, inaccessible }` per directory the walker could not read; `path` is the directory; `transient` marks an interruption (`EINTR`) that survived three retries; `inaccessible` marks `EPERM`/`EACCES`, which the human report counts in one footer line. |
| `totals` | object | See below. |
| `elapsed_ms` | number | The whole run, in milliseconds. `timings_us` below is the same duration broken down, in microseconds — a small tree can finish under one millisecond, at which point this alone reads `0`. |
| `timings_us` | object | Microsecond stage timings: `total`, `scan`, `policy`, `size`, `delete`, `git`. The stages do not sum to `total` — the residue is configuration loading and worker-pool setup. |
| `git` | object or `null` | What git housekeeping did. `null` when it was off, found no repositories, or the machine has no git — a consumer must test the *value*, since the key is always present. |
| `bazel` | object or `null` | The Bazel stage's own document — `output_bases[]` with `state`, `bytes` and `outcome`, and `totals` — or `null` when it was off. A dry run's `bytes` are measured; a real run's are what the removal counted as it unlinked. |
| `docker` | object or `null` | The Docker stage's document (`lanes`, `trim`, `totals`, or `status: "skipped"` with `skipped_reason`), or `null` without `--docker`. |
| `worktrees` | object or `null` | The merged-worktree stage's document, one entry per worktree with its `state` and `reason`, or `null` without `--remove-merged-worktrees`. |

Each entry in `artifacts[]`:

| Field | Meaning |
| --- | --- |
| `path` | The path, as walked. |
| `ecosystem` | The catalog ecosystem id; the cache id for a `source: "cache"` entry; `null` for `config-include`. |
| `artifact` | The declared artifact path (`target/`) that matched, or `null` for `cache`/`config-include`. |
| `bytes` | Size measured before removal was attempted. |
| `unreadable_directories` | Directories inside the artifact that could not be listed — non-zero means `bytes` is a lower bound. |
| `reclaimed_bytes` | What the removal actually freed. `sum(artifacts[].reclaimed_bytes) == totals.bytes`, always. |
| `source` | `"marker"`, `"config-include"`, or `"cache"`. |
| `marker_dir` | The directory the marker was found in; `null` for `config-include` and `cache`. |
| `include_pattern` | The `include` pattern that matched; `null` unless `source` is `"config-include"`. |
| `outcome` | `{ status, ... }` — `removed`, `would_remove`, `refused` (+ `reason`, `detail`), `partially_removed` (+ `freed_bytes`, `remaining_bytes`, `os_error`, `os_message`), or `failed` (+ the same failure fields). |

`totals`: `reclaimed`, `bytes`, `skipped`, `refused`, `failed`, `partial`, `partial_bytes` (a
subset of `bytes`), `dependencies` (a subset of `reclaimed`), `unmeasured`.

`bytes` and `reclaimed_bytes` follow `du`'s rule on unix — allocated blocks, each hard-linked
inode counted once — and apparent file size on Windows, which has no portable link count to
dedup by.
