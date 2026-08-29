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
| Behaviour | `--no-git` | | off | Skip `git worktree prune` and `git gc --auto` in every repository walked. Also `[git] enabled = false`; the flag wins. |
| Behaviour | `--force` | | off | Retry a failed removal, first clearing read-only bits (and, on macOS, the immutable flag) on paths already inside a proven artifact. Never relaxes a rail, follows a symlink, or touches the artifact's parent. A repair that does not go on to succeed is not undone. |
| Behaviour | `--jobs <N>` | `-j` | every core | Worker threads for the walk, sizing and removal. Lower on spinning disks or network filesystems. |
| Output | `--verbose` | `-v` | off | Explain every candidate that was passed over. |
| Output | `--summary` | | off | Print only the totals footer. |
| Output | `--format <human\|json>` | | `human` | See [JSON output](#json-output). |
| Output | `--color <auto\|always\|never>` | | `auto` | `auto` colorizes when stdout is a terminal, honouring `NO_COLOR` and `CLICOLOR_FORCE`. |
| Selection | `--ecosystem <ID>` | `-e` | all | Only sweep this ecosystem. Repeatable. |
| Selection | `--enable <SPEC>` | | | Turn on an off-by-default artifact, e.g. `python.venv`. Repeatable. |
| Selection | `--disable <SPEC>` | | | Turn off an ecosystem or one of its artifacts. Repeatable. |
| Selection | `--clean-dependencies` | | off | Also remove `node_modules/`, Rust, composer and Go `vendor/`, mix `deps/` and Python virtualenvs. Sugar over `--enable`-ing each. |
| Selection | `--exclude <GLOB>` | | | Never scan this path. Repeatable. Relative to the tree being swept. |
| Selection | `--include <GLOB>` | | | Remove this path whether or not a marker proves it. Repeatable. Outranks every prune, cannot reach inside a pruned directory, and `--exclude` still wins. |
| Selection | `--clean-caches <IDS>` | | | Remove these named tool caches. Repeatable, or comma-separated — see `voom caches`. |
| Selection | `--caches` | | off | Also sweep tool caches and installed toolchains, which are skipped by default. |
| Selection | `--config <PATH>` | | discovered hierarchy | Use this configuration file instead of resolving `voom.toml` from the tree. |
| Keep policies | `--min-age <DURATION>` | | unset | Never remove an artifact modified more recently, e.g. `7d`. |
| Keep policies | `--min-size <SIZE>` | | unset | Skip artifacts smaller than this, e.g. `1MB`. |
| Keep policies | `--max-size <SIZE>` | | unset | Skip artifacts larger than this, e.g. `50GB`. |
| Safety | `--one-file-system[=BOOL]` | | `true` | Stay on the scan root's filesystem. `--one-file-system=false` opts out — the `=` is required, or clap reads the next argument as a path. |

**Subcommands**

| Command | What it does |
| --- | --- |
| `voom catalog` | Print the built-in ecosystem catalog. Always exits `0`. |
| `voom caches` | Print the tool-cache table, resolved against this machine. Always exits `0`. |
| `voom config show [PATH]` | Print the fully merged, resolved configuration and the files it came from. `PATH` defaults to `.`. Always exits `0`. |
| `voom suggest [PATHS]` | Rank repeated directories a `.gitignore` names that the catalog does not cover. Removes nothing. Takes `--one-file-system[=BOOL]` and `-j`/`--jobs <N>`. Always exits `0`. |
| `voom watch [PATHS]` | Keep a tree pruned continuously. `--debounce <DURATION>` (default `5s`), `--quiet-period <DURATION>` (default `60s`). Sweep flags come from the top-level arguments. |
| `voom git-prune [PATHS]` | Run git's own housekeeping on its own. `-n`/`--dry-run`, `--remotes`, `--timeout <DURATION>`, `--expire <TIME>` (default git's own three-month `gc.worktreePruneExpire`), `--format <human\|json>`. Exits `1` if a repository's housekeeping failed. |

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
| `walk_errors` | object[] | `{ path, detail, transient }` per directory the walker could not read; `transient` marks an interruption (`EINTR`) rather than a real problem. |
| `totals` | object | See below. |
| `elapsed_ms` | number | The whole run, in milliseconds. `timings_us` below is the same duration broken down, in microseconds — a small tree can finish under one millisecond, at which point this alone reads `0`. |
| `timings_us` | object | Microsecond stage timings: `total`, `scan`, `policy`, `size`, `delete`, `git`. The stages do not sum to `total` — the residue is configuration loading and worker-pool setup. |
| `git` | object or `null` | What git housekeeping did. `null` when it was off, found no repositories, or the machine has no git — a consumer must test the *value*, since the key is always present. |

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
