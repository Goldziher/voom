# 0011 — Git Housekeeping: Delegated, Local, and On by Default

- Status: Accepted
- Date: 2026-08-28

## Context

A machine that accumulates build artifacts accumulates git slack alongside them, and it is the
same user with the same disk. Two kinds of it are reclaimable without anyone deciding anything:

- **Administrative files for worktrees that are gone.** `git worktree add` writes a directory
  under `.git/worktrees/<name>`; deleting the checkout does not remove it. Nothing else ever
  cleans these up, and a heavy worktree user carries dozens.
- **Loose objects that belong in a pack.** Git repacks on its own thresholds after fetch,
  commit, merge and rebase — but a repository that is read from and rarely written to can sit
  well past those thresholds for months.

voom is already walking the tree these repositories are in. The artifact walker meets `.git` on
its way past (`NEVER_DESCEND`, ADR 0005) and prunes it, so it knows where every repository is
and throws that knowledge away.

The hazard is obvious and is the whole of the design work here. Git's maintenance commands are
not uniformly safe: some of them are recovery mechanisms in reverse. `git reflog expire
--expire=now` and `gc --prune=now` between them turn "I can get that commit back" into "that
commit is gone", and they do it silently, in a tool whose other half deletes directories without
asking.

## Decision

**voom shells out to the `git` binary and runs the subset of git's own housekeeping that git
itself already runs unprompted. It never runs anything git would not.**

### Shell out; do not link a library

`git2`/`libgit2` and `gix` both exist and both would let voom prune worktrees and repack in
process. Both are rejected.

- **Delegating is strictly safer than reimplementing.** The correctness of a repack is a
  reachability argument over the object graph, including reflogs, worktree HEADs, stashes,
  bisect state, and whatever the user's `gc.*` configuration says. voom does not want to be
  right about that; it wants git to be right about it, as git is in every other program the user
  runs.
- **It respects the user's configuration for free.** `gc.auto`, `gc.pruneExpire`,
  `gc.reflogExpire`, `maintenance.*` and any repository-local override are read by the process
  that already knows how to read them. A library call with voom's own parameters would silently
  override policy the user set deliberately.
- **It adds no dependency**, and no dependency whose version skew could change what "prunable"
  means between two voom releases.

The cost is a subprocess per step per repository, and a hard dependency on a `git` being on
`PATH`. Both are accepted: the process cost is dwarfed by the work git does inside it, and a
machine with no git has no repositories worth pruning.

### What runs, exactly

By default, in every repository, both steps strictly local:

| Step | Command | Under `--dry-run` |
| --- | --- | --- |
| stale worktrees | `git worktree prune -v` | `git worktree prune -n -v` — really runs, and reports |
| repack | `git gc --auto` | not run |

`-v` is on in both modes because the report has to be able to say what was removed; without it
git prunes silently and voom would be reporting its own assumption rather than git's result.

Opt-in, on the explicit subcommand only:

| Step | Command | Why it is not a default |
| --- | --- | --- |
| gone upstreams | `git remote prune [-n] <remote>` | **contacts the network** |
| object census | `git count-objects -v` | replaces the withheld repack in a dry run; too noisy for a sweep |

**`gc --auto`, never a bare `gc`.** `--auto` is the invocation git itself makes after fetch,
commit, merge and rebase. It checks its own thresholds and is a cheap no-op on a repository that
does not need repacking, which is the great majority of them. A bare `git gc` repacks
unconditionally: across a home directory that is minutes of CPU and IO, and it would break the
claim — "sweeps a home directory in seconds" — that the rest of this project's performance work
exists to defend.

### What is never run — the line

**Not run, and not to be "improved" into the default later:**

- **`git reflog expire`, in any form.** The reflog is how somebody recovers a commit they
  thought they had lost: a bad rebase, a `reset --hard`, a deleted branch. A housekeeping
  command that empties it has converted a recovery mechanism into a liability, and the user
  finds out only at the moment they needed it.
- **`--prune=now`**, on `gc` or anywhere else. It removes unreachable objects that are inside
  git's grace period, which is precisely the window that makes recovery possible.
- **`gc --aggressive`.** Expensive by design, and it is a repacking *strategy* choice that
  belongs to the repository's owner, not to a disk-cleaning tool.

**`gc --auto` does expire the reflog, at git's default of 90 days for reachable entries and 30
for unreachable ones.** That is git's own policy, applied by git, reading the user's own
`gc.reflogExpire` if they set one. voom does not tighten it, does not pass a shorter expiry, and
must never grow a flag that does. The distinction the list above draws is not "no reflog is ever
expired" — it is that **voom never expires a reflog that git would not have expired on its
own**. Anyone reading this ADR while adding an option should treat that sentence as the rule.

### A subcommand *and* a default step of the sweep

`voom git-prune <paths>` is the explicit surface: it walks for itself, reports every repository
it looked at, and is the only place the network step can be asked for.

An ordinary `voom <paths>` also runs the two local steps, because a cleanup tool people have to
remember to run a second time is a cleanup tool that gets run once. It is disableable two ways —
a CLI flag, and a key in `voom.toml` through the existing hierarchical pipeline (ADR 0004), with
the flag winning as every other option does.

It is not a *flag* on the sweep — that is, git housekeeping did not become just another catalog
entry — because it removes no build artifacts and its `--dry-run` means something different in
kind: withholding a repack is not withholding a deletion. Keeping the explicit surface separate
is what lets `voom git-prune --dry-run` mean "tell me about these repositories" while
`voom --dry-run` keeps meaning "tell me what you would delete".

### Discovery is free

The sweep does not walk twice. The artifact walker already has the `.git` entry in hand at the
moment it prunes it, so `git::repository_at` turns that entry into a repository path for one
name comparison and no syscall. A second full walk over `$HOME` would roughly double voom's
headline number, which ADR 0005 and the `perf-discipline` rule both refuse.

The subcommand keeps a walk of its own, configured ignore-blind exactly as `scan::configure` is,
because it can be pointed at a tree no sweep is running over.

### Safety

Every rule below is enforced in `src/git/discover.rs` and covered by a test.

- **A repository mid-operation is skipped, and said so.** `rebase-merge`, `rebase-apply`,
  `MERGE_HEAD`, `CHERRY_PICK_HEAD`, `REVERT_HEAD` and `BISECT_LOG` in the common directory —
  and in each linked worktree's private directory, because a rebase paused in a worktree holds
  its objects in the same shared store. Nothing runs; the reason names the marker.
- **Linked worktrees resolve to their main repository, and it is pruned exactly once.** A linked
  worktree has a `.git` *file* pointing into `<main>/.git/worktrees/<name>`. Deduplication is on
  the **common directory**, not the working tree, which is what makes "once per object store"
  true. A submodule's `.git` file points into `<super>/.git/modules/<name>`, which is an object
  store of its own — so a submodule survives deduplication and is pruned in its own right, as it
  should be.
- **The protected-path denylist is reused, not restated.** `delete::PROTECTED_PATHS`, matched
  against the canonical path and against its canonicalized form, because `/tmp` is
  `/private/tmp` on macOS.
- **Containment**: a repository must resolve at or below a scan root. *At*, not strictly below —
  the sweep's rail demands strictly below because the path there is about to be deleted and voom
  never deletes the tree it was pointed at. Nothing is deleted here, and `voom git-prune .`
  inside a repository is the ordinary invocation.
- **A per-repository timeout**, `DEFAULT_TIMEOUT` = 5 minutes, implemented as `spawn` plus a
  bounded `try_wait` poll and a kill, because `std::process::Command` has none. One repository on
  a stalled network mount must not wedge a sweep of a home directory. Both output streams are
  drained by their own threads, so a child that fills a pipe buffer is not mistaken for a hung
  one.
- **One repository's failure is reported and never aborts the run**, the same isolation ADR 0006
  gives one artifact's failure.
- **Symlinked `.git` entries are not followed**, as nothing else in voom is.

### Reporting

One result set, two renderers, deterministic order — ADR 0007's rules unchanged. Two audiences:

- `voom git-prune` names **every** repository it looked at, including the quiet ones. The user
  asked.
- A sweep prints **only** the repositories where something happened or a rail fired, and prints
  **nothing at all** — not a heading, not a count — when nothing did. `gc --auto` is a no-op in
  nearly every repository on a normal machine, so a section that always appeared would be noise
  on the one screen that has to stay readable.

## Consequences

Positive:

- Reclaims real space that nothing else on the machine ever reclaims, with a correctness
  argument that is entirely git's.
- Zero discovery cost in a sweep, and a repack step that is a no-op wherever git says so.
- The user's own git configuration governs, so a repository with deliberate `gc.*` settings
  behaves as its owner intended.
- No new dependency, and no reimplementation of reachability.

Negative / risks:

- **A subprocess per step per repository.** A home directory with three hundred repositories
  spawns six hundred short-lived processes. Each is cheap and they are fanned out over `rayon`,
  but it is not free, and on Windows process creation is dearer than on unix.
- **`gc.autoDetach`.** Git's default is to run an `--auto` repack in a detached background
  process on platforms that support it. Where that applies, `git gc --auto` returns almost
  immediately and the real work outlives voom — so voom's timeout does not bound it, and the
  report cannot say what the repack achieved. This is git's own behaviour after every commit and
  voom does not override it; the alternative, forcing `--no-detach`, would make voom slower than
  git is on the user's own machine for no safety gain.
- **The timeout can kill a legitimate long repack** on very large repositories on slow storage.
  `gc` is written to be interruptible — it builds new packs and swaps them — so the cost is
  wasted work and some temporary files a later `gc` clears, not a damaged repository.
- **The network step, when asked for, is as slow as the network.** It is opt-in for exactly this
  reason and can never become a default.
- **A machine without `git`** gets a clear typed error from the subcommand, and silence from a
  sweep. The second is deliberate: failing a sweep that reclaimed 40 GB because git is missing
  would be the wrong answer.

## Alternatives considered

- **Linking `git2` or `gix`:** rejected above — reimplementing reachability, ignoring the user's
  configuration, and taking on a dependency whose behaviour can drift.
- **A bare `git gc` as the default:** rejected — unconditional repacking across a home directory
  costs minutes and breaks the performance claim. `--auto` is what git itself does.
- **`git maintenance run --auto`:** attractive, since it is git's own umbrella command, but its
  task list includes `prefetch`, which contacts the network, and its availability and defaults
  vary across the git versions users have. `worktree prune` plus `gc --auto` is the stable
  subset that is local on every version.
- **Running `git remote prune` by default:** rejected — one network round trip per remote per
  repository, hangs without connectivity, and can block on credentials for a private remote.
- **Making git housekeeping a catalog entry:** rejected — the catalog is marker-anchored
  classification of *build output* (ADR 0002, ADR 0003), and a repository is not an artifact.
- **Asking `git rev-parse --git-common-dir` to resolve each `.git` file** instead of reading it:
  rejected — authoritative, but a subprocess per candidate during discovery, and the file's
  format is stable and documented.
- **A confirmation prompt before repacking:** rejected for the reason ADR 0006 gives for the
  sweep — a prompt users learn to click through is not a safety mechanism. The safety is in the
  step list.

## Amendment — what adversarial review changed — 2026-08-28

Four defects, all found before release and all reproduced. Each is recorded here because the
original decision above reads as though it had considered them, and it had not.

**`git worktree prune` was more destructive than the `git gc` this ADR claims to defer to.** On
its own it expires *everything*: it removes the administration for every worktree whose directory
is absent right now, at any age. `git gc` never does that — it prunes with `gc.worktreePruneExpire`,
three months by default. A checkout is absent for entirely ordinary reasons (an unmounted external
disk, a network share not yet up, an encrypted volume not yet opened), and
`.git/worktrees/<name>` holds that worktree's `HEAD` and **its reflog** — the recovery path for
anything committed there and not merged. Removing it makes that work unreachable, and the
`gc --auto` in the same visit starts the clock on collecting it. voom now passes `--expire` with
git's own default, and `voom git-prune --expire` can widen or narrow it. A repository that
configures its own `gc.worktreePruneExpire` is not consulted, because reading it costs a
subprocess per repository; three months is the conservative direction to be wrong in.

Establishing this took an experiment rather than a reading: git compares the expiry against the
mtime of `.git/worktrees/<name>/index`, not the `gitdir` file and not the private directory.
Backdating either of those two leaves the worktree unpruned. The test helper says so.

**Containment was checked on the checkout, never on the object store.** A `--separate-git-dir`
repository inside a scan root could send `worktree prune` and `gc` at a store outside it: voom
was pointed at one tree and rewrote another, and the report named only the path it had not
touched. Both the working tree and the common directory are now checked against the denylist and
against containment, and every invocation is pinned with `--git-dir=<store>` — without which git
repeats its own discovery from the working directory and can resolve to something other than what
voom vetted.

**The linked-worktree rule only recognised the default layout.** It inferred a shared store by
looking for an ancestor named `worktrees` beneath a directory named `.git`. A bare clone
(`repo.git/worktrees/<name>`) and the common `project/.bare` arrangement both fail that name test,
so every checkout of one was treated as a repository in its own right — repacking one store once
per checkout, and, worse, checking the in-progress markers against a private directory that cannot
see a rebase paused in a sibling worktree. The store is now read from the `commondir` file, which
is the documented answer and still costs no subprocess.

**`git gc --auto` runs the repository's `pre-auto-gc` hook**, in the foreground, as the invoking
user. With housekeeping on by default, a plain `voom ~` would execute arbitrary shell from every
repository under a home directory that carries one. Hooks are not transferred by `git clone`,
which bounds it — but a repository unpacked from a tarball, copied from shared storage, or
carrying its own `core.hooksPath` does have them, and voom has no business running them for a
pass nobody typed. Every invocation now passes an empty `core.hooksPath`, which is git's own way
of saying "no hooks" and modifies nothing in the repository.

One consequence worth stating plainly, because it did not survive review either: the recorded
command in the report is now built once and used for both the spawn and the record. They were
built separately, so the report — and every test reading it — could describe a command that was
not the one that ran.
