# 0012 — The Cache Catalog: Proof From Inside, Opt-In by Name

- Status: Accepted
- Date: 2026-08-29

## Context

v0.2.0 shipped a `--caches` flag whose help text said it would "also sweep machine-global tool
caches and installed toolchains". Measured on one workstation immediately after a sweep:

| Root | Size on disk | What `--caches` found |
| --- | --- | --- |
| `~/.cache` | 55 GB | 252 MB |
| `~/Library/Caches` | 24 GB | 0 B |
| `~/.cargo` + `~/.rustup` | 24 GB | 41 kB |

103 GB in, 252 MB out — a quarter of one percent, and the 252 MB was incidental project-shaped
debris *inside* the caches rather than any cache itself.

This was never a tuning problem. `--caches` empties `CACHE_DIRS`, which is the list of places a
walk refuses to enter; ADR 0002 then still requires a marker to prove each candidate, and a
cache has no sibling that could be one. `~/.cargo/registry`'s siblings under `~/.cargo` are
unrelated to it; `~/.cache/uv` sits next to `zig`, `huggingface`, `poly` and whatever else the
machine has. The flag could open the door but there was no mechanism behind it that could ever
remove a cache, and nothing in the help text said so.

The name is no help either. `registry`, `caches`, `mod`, `_cacache` and `packages` are not names
voom could match anywhere on a disk without matching a great deal it must never touch.

## Decision

**A second table, `CACHES` (`src/caches/catalog.rs`), where an entry is a location that names
the tool plus a marker the tool wrote inside the directory.** Nothing in it is ever on by
default; each entry is reached only by its id, through `--clean-caches <id>` or
`[caches] enable = [...]`.

The inside marker is not a weakening of ADR 0002 — it is that ADR's rule applied at a third
anchor position (`Anchor::Inside`, added in the same round). `CACHEDIR.TAG`, whose
[specification](https://bford.info/cachedir/) states that the directory holds data the
application can regenerate, is the cross-tool spelling of exactly this, and was found already
present in `~/.cargo/registry`, `~/.cargo/git`, `~/.cache/uv` and `~/.gradle/caches` — 17.7 GB
declared regenerable by the tools' own authors. A declaration a tool wrote into its own cache is
*stronger* evidence than a file that happens to lie beside a directory.

### Why a second table rather than catalog entries

Three reasons, any one sufficient:

1. **The classifier's marker bitmask is a `u32`,** so `MAX_ECOSYSTEMS` is 32 and the catalog
   already holds 26. The caches would not fit.
2. **Catalog entries match by *name*.** A cache is identified by its *location*; a `registry/`
   artifact would match every directory of that name on the disk.
3. **They answer different questions.** The catalog says what build output looks like anywhere;
   this table says which specific places on a machine can be emptied and what that costs.

### The bar for an entry

Both halves must be unambiguous, and **the marker must carry weight the location does not**. A
cache whose only contents are shard directories named `0`–`f` or `a`–`z` fails that: the marker
proves nothing the path had not already said, so there is no evidence to weigh.

Deliberately excluded on that ground, with what each holds on the machine this was measured on:

| Not shipped | Held | Why |
| --- | --- | --- |
| sccache (`~/Library/Caches/Mozilla.sccache`) | 11 GB | hex shard directories only |
| zig (`~/.cache/zig`) | 6.8 GB | single-letter shards `b h o p z` |
| NuGet (`~/.nuget/packages`) | 2.3 GB | package-name directories only |
| Maven (`~/.m2/repository`) | 340 MB | package-name directories only |
| poly (`~/.cache/poly`) | 21 GB | hex-named directories only |
| bun (`~/.bun/install/cache`) | — | scoped package directories only |

**The remedy is upstream, not here.** Any of these becomes a shippable entry the day its tool
writes a `CACHEDIR.TAG`, and the entry then needs no special case. For poly and alef that is
ours to do, and it is the recommendation this ADR makes rather than a reason to lower the bar.

### Reaching a cache without opening its neighbourhood

Most locations sit under a broader `CACHE_DIRS` skip — `~/.cache/uv` under `~/.cache`,
`~/.npm/_cacache` under `~/.npm`, everything under `~/Library/Caches`. Removing the ancestor
from the skip list would let the classifier loose inside it, which is precisely what
`CACHE_DIRS` exists to prevent: an installed pnpm really does sit beside a real `package.json`.

So the walker does here what it already does for an artifact declared inside a pruned dependency
directory (ADR 0001's amendment): at the pruned ancestor it constructs the one path it was told
about, checks it, and prunes everything else regardless. No traversal, one `read_dir` of the
cache itself, and only when the run named it.

### What does not change

- **Containment.** A cache goes through the same pipeline and the same six deletion rails. A
  removal target must still resolve strictly below a scan root, so
  `voom ~/workspace --clean-caches uv` correctly finds nothing.
- **`CACHE_DIRS` stays append-only,** and `--caches` keeps its existing meaning: open the
  locations to the walk. Naming a cache does not require it.
- **A default run is unchanged.** It reports no cache, removes no cache, and enumerates inside
  none.

## Consequences

Positive:

- 41.6 GB reachable on the machine that motivated this, against 252 MB before, and every byte
  of it opt-in and proven.
- `voom caches` resolves the table against the machine it runs on, so a reader sees whether an
  entry is present, absent, or *present but unproven*. On the measured machine `~/.deno` reports
  the third: it is the deno toolchain, not its cache, and the marker refuses it in public.
- A tool author now has a documented way to make their cache sweepable, which is a `CACHEDIR.TAG`
  and nothing else.

Negative / risks:

- **A cache id and an ecosystem id share one column in the report,** so a collision would leave
  a reader unable to tell which table a line came from. `no_cache_id_collides_with_an_ecosystem_id`
  forbids it; `gradle-caches` and `alef-global` are named as they are because of it.
- Removing a cache costs a re-download, and for `cargo-registry`, `go-mod`, `npm`, `pub` and
  `deno` that needs network. Each entry's `note` says so and `voom caches` prints it.
- The table is machine-shaped: entries are written for every platform and most are absent on any
  given one. That is why `voom caches` reports presence rather than asserting it.
- `SCHEMA_VERSION` goes to 3: `source` gained the value `"cache"`, which no version-2 consumer
  handles.

## Alternatives considered

- **Delegate to each tool's own prune command** — `uv cache prune`, `go clean -modcache`,
  `pnpm store prune`, `brew cleanup`, `dotnet nuget locals all --clear`. This is what ADR 0011
  did for git, and the parallel is real. Rejected: it means N external binaries and N failure
  modes, it needs each tool installed and often a network, and it does not exist at all for
  sccache, gradle, zig or the cargo caches — so it would have covered less, not more, at higher
  cost. Worth reopening for a specific entry where the tool's own command is meaningfully
  smarter than removal, which `uv cache prune` genuinely is.
- **Trust the location alone**, with no marker. Rejected: it is exactly the name-only matching
  ADR 0002 exists to refuse, moved from a directory name to a path. `~/.deno` is the standing
  counter-example on the machine this was measured on.
- **A `--clean-caches` that takes no ids and enables everything.** Rejected: the entries differ
  by an order of magnitude in what losing them costs, and one flag cannot express that. `all` is
  not accepted for the same reason.
- **Extend `--caches` to remove rather than adding a flag.** Rejected: `--caches` is shipped and
  its meaning — where a walk may go — is worth keeping distinct from what a run may remove.
