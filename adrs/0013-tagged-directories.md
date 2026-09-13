# 0013 — Tagged Directories: The Tool's Own Declaration, Opt-In by Flag

- Status: Accepted
- Date: 2026-09-13

## Context

ADR 0002 proves an artifact from a marker at an anchor *relative to the candidate's name*. ADR
0012 proves a cache from a marker *inside a location that names the tool*. Both need something to
recognise first — a name or a path — and a machine accumulates build output with neither.

Measured on one workstation immediately after `voom ~`, `voom --caches ~` and `voom /tmp`:

252 tag files, of which **98 carry the specified signature and 154 do not** — the invalid ones all
written by one tool with a wrong constant, and covering 76.63 GB (see "The signature is verified"
below, which is how they were found). Counting only the 98 valid ones, 148.74 GB:

| Tagged space | Reached by |
| --- | --- |
| 38.61 GB | `--clean-caches` plus the default catalog |
| 6.12 GB | `.venv/`, already covered by the `python.venv` opt-in |
| **104.00 GB across 53 directories** | **nothing. No flag, no config, no catalog entry.** |

The 104 GB was not exotic:

- `/private/tmp/enterprise-{pro,rag,worker}-target` — 63.7 GB. A `CARGO_TARGET_DIR` pointed at
  `/tmp`. There is no project within reach of any anchor; the directory is alone in `/tmp`.
- `~/workspace/xberg-io/.build-cache/remediation-api-828ecb73` — 19.3 GB, *inside the swept tree*.
  A Cargo target directory under a name the catalog does not know. voom descended all 19 GB of it,
  emitted thousands of `no node marker (package.json) proves it` skips, and reported nothing.
- `/private/tmp/xberg-1721-cargo/registry` — 5.9 GB, and a `crawlberg-fixes/target` at 4.4 GB.
- `~/.cargo-target-shared-alef` — 4.2 GB, same shape.

Every one of them carried a valid `CACHEDIR.TAG` at its root. The evidence was sitting there.

The catalog cannot fix this. An entry matches a *name*, and these directories are named whatever
the person who set `CARGO_TARGET_DIR` felt like naming them — `enterprise-pro-target`,
`.build-cache`, `pr2141-scale-target`. `CACHE_DIRS` cannot fix it either: `/private/tmp` is not a
cache root, and a `CACHES` entry needs a location that names the tool.

## Decision

**A directory carrying a valid `CACHEDIR.TAG` is an artifact, on that declaration alone,
independent of its name and its location.** Reached only by `--clean-tagged` or
`[tagged] enabled = true`; never on by default.

ADR 0012 already made the argument and then declined to spend it:

> A declaration a tool wrote into its own cache is *stronger* evidence than a file that happens to
> lie beside a directory.

It spent that evidence only at named locations, for a reason it states plainly — a `registry/`
artifact would match every directory of that name on the disk. That reason does not apply to the
tag itself. `CACHEDIR.TAG` is not a name voom guessed at; it is a
[specification](https://bford.info/cachedir/) whose whole purpose is to let a tool tell any
program that a directory can be rebuilt, and `tar --exclude-caching`, `rsync --exclude-tag`, Borg
and restic all act on it already.

### The signature is verified, not the filename

This is the one difference from how the catalog treats the same file, and it is the difference
that makes a location-free rule safe.

A catalog marker is matched by name, which is sound *there* because the name is only half the
evidence: `alef`'s `CACHEDIR.TAG` proves a candidate already called `.alef/`, and a cache entry's
proves one already at the tool's own path. This route has neither constraint — it will remove a
directory of any name anywhere below a scan root — so the filename alone cannot be the proof. A
file somebody wrote for their own reasons and called `CACHEDIR.TAG` must not license the removal
of the directory holding it.

This is not hypothetical. Verifying rather than trusting the name is what found that **154 of the
252 tag files on the machine this was measured on are not valid tags at all** — one tool defined its
signature constant as `Signature: <its own hex>-cache-directory-tag` instead of the specification's
fixed `Signature: 8a477f597d28d172789f06886806bc55`, under a doc comment asserting it was
"byte-for-byte per the spec". 76.63 GB sat behind those files, and every backup tool that honours
the tag — which was the author's actual reason for writing it — had been ignoring them all along.
Its own test suite passed throughout, because every test compared against the same wrong constant:
a self-referential constant test cannot detect a wrong constant. A name-only match here would have
inherited that bug silently and deleted on the strength of it.

So `tagged::is_tagged` reads the first 43 bytes and compares them to the specified signature. It
answers `false` for everything it cannot prove: no tag, an unreadable one, a short or mis-signed
one, a tag that is not a regular file, or a tag that is a symlink. Each of those is the safe
direction, per the `safety-first` rule: a false negative costs gigabytes, a false positive costs
work nobody can get back.

### It prunes, and that is why it is not a cost

A tagged directory is an indivisible unit like every other artifact, so the walk takes it and
declines to descend. That makes a sweep **faster**, which is unusual enough to state: today voom
walks the entire 19 GB of `.build-cache/` to emit skips and find nothing, and this change replaces
that traversal with one `symlink_metadata` and a 43-byte read.

`should_prune_a_tagged_directory_instead_of_walking_into_it` asserts on *skips* rather than on
findings, deliberately: `run.rs` drops a finding an outer one covers, so a nested finding would
vanish even if the walk had descended. Only the skips expose the traversal.

The cost, when the flag is on, is one `symlink_metadata` per directory the classifier declined —
paid nowhere else, since nothing calls it unless the run asked.

### Precedence: the catalog wins

The tagged check runs only where the classifier returned no artifact. Cargo tags `target/`, but a
reader wants to be told that directory was Rust's, not that it carried a tag — the anchored
verdict names an ecosystem and an artifact somebody can act on. So `target/` beside a `Cargo.toml`
is still reported as `rust`.

It sits after `exclude` (absolute, ADR 0004), after `include`, after the named-cache check, after
the `CACHE_DIRS` prune, after `.git`/`.hg`/`.svn`, and after dependency directories. The order in
`scan/visitor.rs` is the order of authority and this change does not reorder any of it.

### Why not on by default

The tag says "regenerable", and voom could argue that is enough. It is not enough *here*, because
this is the only route whose candidate set is not bounded by anything voom can enumerate. A
default run must be predictable from the catalog and the flags; one that removes a directory
because of a file voom found inside it, under a name nobody declared, is not. `.venv/` carries a
tag too, and ADR 0001's amendment already decided a virtualenv is not swept by default.

### What does not change

- **All six rails.** Containment (strictly below a scan root, so a tagged scan root is refused),
  the protected denylist, symlink refusal, the filesystem boundary, per-artifact failure
  isolation. A tagged directory goes through the same `delete::Guard` as everything else.
- **Every keep policy.** `min_age`, `min_size`, `max_size` and `[[paths]]` rules apply unchanged.
- **`--dry-run`** is the same pipeline with the last step withheld.
- **A default run.** It reports no tagged directory and removes none.

## Consequences

Positive:

- 104.00 GB reachable on the machine that motivated this, with the strongest evidence voom has:
  not a name voom recognised, but a statement the directory's author wrote inside it.
- A sweep over a tree containing a relocated build directory gets *faster*, not slower.
- It is open-ended in the right direction. A tool voom has never heard of becomes sweepable by
  writing one file, with no catalog entry, no release, and no special case — which is exactly the
  upstream path ADR 0012 recommended and which poly and alef both took.

Negative / risks:

- **It will remove a directory of any name**, which is why the signature is verified, why it is
  off by default, and why the negative fixture
  (`should_never_remove_an_untagged_directory`) is the regression test that matters most here.
- **A tool that tags a directory holding something it cannot actually regenerate** hands voom a
  false declaration. Nothing voom can do about that, and the same exposure every backup tool
  honouring the tag already has. It is an argument for the flag, not against the mechanism.
- **`SCHEMA_VERSION` goes to 4**: `source` gained `"tagged"`, which no version-3 consumer handles.
- **A symlinked tagged directory is passed over silently**, unlike a symlinked *named* artifact,
  which is reported and then refused. There is no way to learn the link's target is tagged without
  reading through the link, which is the one thing voom does not do. Stated here because the
  asymmetry looks like an oversight and is not.
- One `symlink_metadata` per declined directory when the flag is on.

## Alternatives considered

- **A recursive file glob** (`*.ext4`, `*.o`, `*.pyc`). Rejected: it is the name-only matching
  ADR 0002 exists to refuse, moved from a directory to a file. The case that prompted the thought
  was 122 GB of `budget.ext4` loopback images under `~/.codex/task-evidence`, and the right answer
  there is the one ADR 0012 gives: **the remedy is upstream.** codex writes a `CACHEDIR.TAG` and
  this ADR reaches all of it with no special case.
- **A catalog entry for each known relocation** (`*-target/`, `.build-cache/`). Rejected: the names
  are arbitrary — they are whatever the person setting `CARGO_TARGET_DIR` typed — so the table
  would grow forever and still miss the next one, while `*-target/` alone would match plenty voom
  must never touch.
- **Trusting the filename, as the catalog does.** Rejected above: the catalog's name match is safe
  only because a name or a location already bounds the candidate, and here neither does.
- **On by default.** Rejected above.
- **Extending `Anchor::Inside` to cover it** rather than adding a source. Rejected: `Inside` is a
  marker position for a candidate the catalog already named, and this has no catalog entry at all.
  Conflating them would have made ADR 0002's anchor rule mean two different things.
- **Following a nested tag and reporting both.** Rejected: an artifact is indivisible, and counting
  a tag inside a tag would double the bytes and break ADR 0007's
  `sum(artifacts[].reclaimed_bytes) == totals.bytes`. 464 tags on the measured machine collapse to
  259 topmost.

## Container stores are not a tagged directory, and not a catalog entry either

Recorded here because it came up in the same investigation and the hazard is real.

Docker Desktop keeps every image, container and **named volume** inside one sparse host file,
`~/Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw` — 56 GB allocated, 120 GB
apparent, the apparent figure being the configured cap rather than a measurement. Reclaiming any
*subset* of it is impossible from the host: the contents are ext4 objects inside a virtio-blk
device, so the only host-side moves are "delete the whole image" and "nothing".

**`PROTECTED_PATHS` contains `/Library` and `/Users` but not `~/Library`, and the check is
exact-path rather than prefix.** So that `Data` directory is not protected, is not a symlink, is on
the same filesystem, and sits below `$HOME` — a cache entry naming it would pass **all six rails**
and `remove_dir_all` every named volume on the machine in one call. ADR 0012's bar would not stop
it either: the directory is full of tool-written files a careless author could nominate as a marker.

So: **no `CACHES` entry, no catalog entry, and no delegated prune step.** Delegating to `docker`
was considered on ADR 0011's precedent and rejected — each of that ADR's three pillars is absent
(discovery is not free, there is no path to contain, and docker has no `--auto` whose behaviour
voom would merely be repeating), and docker reports bytes with no path while the host file may not
shrink at all, which would recreate precisely the accounting lie ADR 0007 was amended to kill. If
anything ships here it is a `suggest`-style report: measure the store, say the apparent size is a
cap, print the command, remove nothing.
