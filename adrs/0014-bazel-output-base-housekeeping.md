# 0014 — Bazel Output-Base Housekeeping: Orphans Proven by Bazel's Own Marker

- Status: Accepted; amended 2026-10-04 (see [Amendment](#amendment-2026-10-04-stale-bases-worktrees-sweep-and-full-clear) — it supersedes the "not a sweep" and "age-based removal" positions below)
- Date: 2026-09-18

## Context

Every distinct workspace path that has run Bazel gets its own **output base** — a directory
named `<output_user_root>/_bazel_<user>/<hash>`, the hash a function of the workspace's
absolute path — holding the execroot, the action cache, and everything else a build produced.
Nothing removes it when the workspace goes away. A worktree checked out for one ticket, built
once, and later removed with `git worktree remove` leaves its output base exactly where it was.

Measured on one machine with seven live worktrees of one repository: **39 output-base
directories** under `_bazel_<user>`, of which **3** matched a `bazel-bin` symlink from a
worktree still on disk. The other **36**, an estimated **17–19 GB**, had no live worktree at
all — a monorepo checked out, built, and removed dozens of times over, at roughly 500 MB each.

This is the same shape of waste ADR 0011 found in git worktree administration — and it is not
that problem. `git worktree prune` cannot see it: an output base is not underneath the `.git`
directory a sweep or `git-prune` ever walks, and it is not addressed by any git command. It also
is not a [`CACHES`](0012-cache-catalog.md) entry: that table proves a *fixed, single*
location relative to `$HOME` regenerable in its own right — `~/.cache/bazel` as a whole, if a
user wants the entire build cache gone. An output base is neither fixed nor single: it is one of
however many a user has ever pointed Bazel at, discovered by walking a *different* directory
than any of them, and it needs a per-instance answer to a question `CACHES` never asks: **is the
workspace this one belongs to still there?**

Bazel already answers that question. Every output base carries a file at its root named
`DO_NOT_BUILD_HERE`, containing the absolute path of the workspace that owns it — verified by
reading four such files on the machine measured above and cross-checking each against
`git worktree list`. Two owned a live worktree. One named a directory that had never existed on
this machine at all — not a worktree of the repository being measured, evidence Bazel itself
wrote once and never revisited. One named a directory that still existed but was not part of
the family being measured — a different checkout entirely, worth noting rather than removing.

A repository's own shared output-base convention breaks this apart from the reused root down.
This machine's `~/.cache/bazel/_bazel_<user>/<toolchain>/` is a single output_user_root a
Docker-wrapping build tool (`dazel`) points every worktree's build at *sequentially* — one
shared execroot, not one per workspace. `DO_NOT_BUILD_HERE` there sits inside `execroot/` rather
than at the output base's own root, and it names whichever worktree built there *most
recently*, not an owner. Treating it as a per-workspace output base and removing it on that
account would repack a shared object nobody asked to lose. It has to be recognisable and
excluded, not swept along with the rest.

## Decision

**`voom bazel-prune <output-user-root>...` reads the `DO_NOT_BUILD_HERE` marker at each
output-base directory found directly beneath a given root, and removes only the ones whose
recorded owner no longer exists on disk.** Everything else about that directory — its name, its
age, its size — is not asked. The marker is the proof, exactly as ADR 0002 requires for
everything else voom removes, applied at a fourth position: not beside the candidate, not above
it, not inside it in the `CACHEDIR.TAG` sense of "this whole directory is regenerable" — inside
it in the sense of naming the *other* directory whose existence is the actual question.

### The three states, and only one is acted on

For each immediate subdirectory of a given root, voom looks for `DO_NOT_BUILD_HERE` at the
subdirectory's root and, failing that, at `<subdirectory>/execroot/DO_NOT_BUILD_HERE` — Bazel
writes it in both places on every output base this was checked against, and the second location
is where the shared, per-toolchain root above puts it *instead of* the first, which is exactly
the signal that excludes that root without a separate rule naming it by path.

- **No marker readable at either location: left alone, unproven.** The same rule as everywhere
  else in the catalog — no marker, no claim about what the directory is, and nothing removed.
  This is also what excludes a shared, sequentially-reused root: it has no root-level marker to
  find, only the per-builder one nested under `execroot/`, which this rule does not follow into
  for exactly that reason — a directory one level further in is not "at either location" and is
  not read.
- **Marker present, recorded path exists: left alone, reported as owned.** Existence is not the
  same claim as membership. The path recorded is not cross-checked against `git worktree list`
  or against any other notion of "one of mine" — a directory that exists is a workspace somebody
  might still use, on this machine or a shared one, and removing its build cache because it is
  not among the paths a particular invocation happened to be told about is a worse mistake than
  leaving alone one that truly is abandoned. `voom bazel-prune` reports it by the path it read,
  so a user deciding by hand has what they need.
- **Marker present, recorded path does not exist: removed.** This is the one state a marker
  written by Bazel itself, at the moment it built there, can prove without help: the workspace
  it named is not merely "not one I recognise" but **gone**, checkable with one existence test
  and nothing else.

### Not a sweep, and not `git-prune`'s shape either

`voom git-prune` reuses the artifact walker's own pass over a scan root — a `.git` it would
have pruned anyway becomes a repository for free, because the tree being searched and the tree
being swept are the same tree. An output base is not: `~/armis`'s output bases live under
`/var/tmp` or `~/.cache/bazel`, never under `~/armis`, so there is no walk to ride along with,
and folding this into the sweep would mean walking `$HOME` a second time on every invocation
whether or not the user's scan roots have anything to do with Bazel at all.

`bazel-prune` is therefore its own subcommand, taking the output-user-root(s) to search as its
own arguments, exactly as `voom git-prune <paths>` does — except the paths mean something
different: not a tree to walk for repositories, a directory whose *immediate children* are
candidate output bases. Given none, it searches the conventional locations
(`/tmp/_bazel_<user>`, `/var/tmp/_bazel_<user>`, and `~/.cache/bazel/_bazel_<user>`, `<user>`
read from `$USER`/`USERNAME`) and silently finds nothing at whichever do not exist on a given
machine. That absence is not a usage error, unlike a nonexistent path given to `voom` or
`voom git-prune` — and unlike those two, this is true whether the root was defaulted or named
explicitly. Every one of these roots names a convention voom is guessing at, not a tree the
user has confirmed exists by pointing voom at it, and finding nothing there is never unsafe —
only guessing at what an absent one *might* have held would be.

It is not folded into a default sweep, unlike git housekeeping. `git-prune`'s local steps are
cheap and universally safe enough to run unasked on every repository a sweep passes; discovering
output bases costs nothing extra only because the tree is already being walked for something
else, which is not true here, and a feature that silently begins reading files under `/var/tmp`
on every invocation of a tool most of whose users have never heard of `DO_NOT_BUILD_HERE` is a
bigger claim than "safe" alone justifies for a first release. Explicit, for now — the
subcommand exists exactly so this can be reconsidered once it has run on more than one machine's
Bazel convention.

### Removal goes through the same rails as everything else

Each removable output base — the whole `<output_user_root>/_bazel_<user>/<hash>` directory,
never a path constructed from the recorded owner — passes through [`delete::Guard`], the same
containment, symlink-refusal, and protected-path checks every artifact removal does (ADR 0006).
The output-user-root given to the subcommand is the `Guard`'s scan root, so an output base
outside it — there should never be one, since it was found by listing that root's own
children — is refused for the same reason a marker outside a scan root never proves anything
elsewhere in voom.

`--dry-run` is the same code with the final step withheld, as everywhere else. There is no
separate prediction path to disagree with a real run.

## Consequences

Positive:

- Reclaims real, previously invisible space in exactly the repositories that create it fastest —
  a Bazel monorepo used through disposable worktrees — with a correctness argument resting on a
  file Bazel wrote for its own purposes, not on an inference voom is making about it.
- The three-state report (removed, owned, unproven) is itself useful independent of removal: a
  user auditing `/var/tmp` sees which output bases are whose without reading a hash.
- Composes with nothing else. It touches no catalog entry and no `CACHES` row. (Amended: it now also runs after a sweep.)

Negative / risks:

- **A marker that exists but is stale in the other direction** — recording a path that has since
  been recreated for something unrelated, at the same absolute location, after the original
  workspace was removed — would read as "owned" and survive. This is the conservative direction
  to be wrong in, matching every other rail in this tool: a false negative costs disk, a false
  positive would cost someone's build cache for a workspace they still use.
- **The conventional root list is a guess**, informed by one machine and Bazel's documented
  default locations, not by a survey the way the cache catalog's bar (ADR 0012) demands. It is
  why the feature stays an explicit subcommand rather than a sweep default until it has been
  checked against more than one convention.
- **A shared, sequentially-reused output-user-root that happens to place `DO_NOT_BUILD_HERE` at
  its own root** — a different wrapper than the one measured here, doing the same sharing a
  different way — would be swept as if it were a ordinary orphan candidate, and a miss would
  remove a cache still in active use by whichever workspace last built there. Nothing currently
  known does this; the risk is recorded because the exclusion above is a fact about one measured
  tool, not a proof about every tool that shares an output base.

## Alternatives considered

- **A `CACHES` entry for `~/.cache/bazel` as a whole**: rejected as the *only* answer, though it
  remains worth adding separately — it proves and removes the entire shared cache in one step,
  which is the right tool for "I want to force a full rebuild," but it cannot express "remove
  only the parts nobody's workspace still needs," which is the actual waste this ADR measures.
- **Naming output bases from the git side** — walking `git worktree list` first and computing
  which output-base hash each live worktree *would* produce, then treating every other hash as
  orphaned: rejected. Bazel's hash function is not part of its public contract, guessing it
  wrong in either direction is silent, and it would require a `git` invocation and a real Bazel
  workspace root for every candidate, where reading one file already answers the question Bazel
  itself is best positioned to answer.
- **Folding into `git-prune` or the sweep**: rejected in the Decision above — the tree being
  searched is never the tree being swept, so nothing is discovered for free the way a `.git`
  entry is.
- **Age-based removal** (`--min-age` on the output base's mtime, with no marker check at all):
  rejected outright — this is exactly the name-only-adjacent matching ADR 0002 forbids, applied
  to a directory's age instead of its name. An output base for a workspace still in occasional
  use looks identical to an abandoned one until the next build touches it.

## Amendment 2026-10-04: stale bases, worktrees, sweep, and full clear

The original decision proved exactly one thing — the owner is gone — and that was correct for
what it measured: orphans. Running it against a workstation that uses disposable worktrees
showed it was not enough. Thirteen live worktrees under `/var/tmp` held 23 GB of output bases
and `~/.cache/bazel` another 27 GB; **every owner still existed**, so the command reclaimed
nothing. The waste is not orphaned bases but *idle* ones: a worktree checked out for a ticket,
built in for two days, and then left on disk for weeks with its several-gigabyte base. Two
positions above do not survive that.

### Age is admitted, but only on top of the marker

"Age-based removal" was rejected as name-only-adjacent matching. That objection stands for age
*alone*, and it is not what is adopted. An output base is now also removable when **all** of
these hold: the marker was read (or the directory is an unmarked direct child of a
`_bazel_<user>` directory, which is Bazel's own naming), no Bazel server is running against it,
and nothing has built there for longer than `--max-age` (default seven days). "Built there" is
the newest modification time among the entries Bazel touches on every invocation —
`command.log`, `lock`, `action_cache`, `execroot`, `external`, `server`, `java.log` — and the
base itself, not the age of the base's creation. A base someone still builds in refreshes all of
them, so the false-positive cost is a cold rebuild, never lost work, which is the same bar the
cache catalog (ADR 0012) sets.

Four states are added to the original three; a removable one is always reported with its
reason:

- **Abandoned** — the owner exists, but it is a linked git worktree whose `.git` file names an
  administrative directory that is gone (`git worktree remove` or `prune` ran, the directory
  survived on untracked files or a build symlink). A checkout of nothing. Checked with one
  read and one existence test.
- **Stale** — owner alive, idle past the age limit.
- **Running** — `server/server.pid.txt` names a live process whose command line names this
  output base or is the Bazel server. Never removed by age or orphaning, however idle its files
  look. A pid file outlives its server and the pid can be reused, so the file alone proves
  nothing; the process check is the proof.
- **Cleared** — removed because a full clear was asked for (below).

Worktrees are the reason the hash is per-path: every worktree is a distinct workspace with its
own base, and the owner marker is what ties each one back to a directory voom can ask git
about. Removing a base also unlinks the owner's `bazel-*` convenience symlinks, but only those
whose target lies inside the base just removed — a link that points anywhere else is not ours.
The workspace itself is never touched.

### The rest of `_bazel_<user>`

- **Install bases** (`install/<hash>`, roughly 200 MB each) are removed when no output base
  that survives this run links to them. Bazel re-extracts one on demand.
- **The shared cache** (`cache/`) is pruned per file by `bazel-prune`: anything not modified
  within the age limit, since Bazel touches an entry each time it is served, and directories left
  empty by that afterwards. A sweep deliberately skips this: it is a full recursive walk, and the
  sweep's cost budget is the reason the rest of this stage was allowed into it at all.
- The conventional roots gain `~/Library/Caches/bazel/_bazel_<user>`, which held output bases
  on the measured machine and was missing from the original list. `VOOM_BAZEL_ROOTS` (a path
  list, empty for none) replaces the guess entirely.

### Part of the sweep, and never silent about it

The original reasoning for keeping this out of a sweep — discovery is not free because the tree
being swept is never the tree being built from — is still true, and is not why it moves. It
moves because an opt-in nobody runs reclaims nothing, and the measured cost is one `read_dir`
per root plus a file read per base, with no tree walk. So an ordinary `voom` run now ends with
the same housekeeping over the conventional roots, printing a section only when it removed (or
would remove) something. `--no-bazel` and `[bazel] enabled = false` turn it off; `--bazel-max-age`
and `[bazel] max_age` set the age; the flag beats every file, as `--no-git` does. A JSON run gets
a one-line summary on stderr so stdout stays one document. The explicit `voom bazel-prune`
remains, and is where `--format json` lives for this data.

Because a sweep now reaches outside the swept tree, **anything that runs the binary against a
scratch tree must set `VOOM_BAZEL_ROOTS`**. This was learned the expensive way: the first
version of the integration ran the test suite's `--clean-caches` cases against the real
machine's `/var/tmp`, and cleared live output bases. The test helpers now set it empty.

### `--clear-caches` means all of it

The rejected alternative "a `CACHES` entry for `~/.cache/bazel` as a whole" was right that
force-a-full-rebuild is a different request from reclaiming the stale part. It is now served
without a `CACHES` row, because those rows are fixed locations under `$HOME` proven by an
inside marker and Bazel's state is neither. `bazel` is a reserved id accepted by
`--clean-caches`, included in a bare `--clean-caches`/`--clear-caches`, and handled here:

- every output base, install base and cached download under the roots is removed, live
  workspaces included;
- a running server is sent `SIGTERM` and given ten seconds to exit first — removing a base from
  under a live server wedges it on a tree that is gone. A server that will not exit keeps its
  base, reported as running;
- unmarked children are cleared only under a directory named `_bazel_*`, so pointing
  `bazel-prune --all` at an unrelated directory clears nothing;
- a dry run stops nothing and removes nothing.

Without the flag, nothing above runs: a sweep only prunes what is stale.

### Consequences

- The command now reclaims real space on the machine that motivated it, where it previously
  reclaimed none. The seven-day default is a judgment, not a measurement: on a machine where
  every base was built in within four days it correctly reclaims nothing.
- The staleness threshold is the first voom removal rule whose correctness is a *trade*
  (cold-build cost against disk) rather than a proof. It is kept honest by never applying to a
  running server, by reporting the idle time it acted on, and by `--max-age` being a per-run
  and per-repository setting.
- A dead-worktree check reads `.git` as a file, so a bare clone, a submodule and a non-git
  workspace are all left to the age rule and never classed as abandoned.
- The shared, sequentially-reused root (`dazel`) is handled by age alone: its marker names the
  last builder, so it ages out only when nothing has built in it at all.
- Not solved: a worktree whose branch has merged but which is still being built in will not be
  flagged; nothing on disk says it is done, and guessing from `git branch --merged` would be
  the inference this ADR exists to avoid.
