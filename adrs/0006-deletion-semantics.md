# 0006 — Deletion Semantics: Default-Delete, Dry-Run, and the Safety Rails

- Status: Accepted
- Date: 2026-08-28
- Updated: 2026-08-28 — the rails hardened for Windows reparse points.

## Context

voom is meant to be run habitually — from a shell alias, a git hook, a scheduled job — and
pointed at large trees up to `$HOME` (ADR 0001). That use pattern argues for the plain
invocation doing the work: a tool whose default is to report and whose real action is behind
a flag trains users to type the flag reflexively, at which point the flag has stopped being a
control.

It also means the plain invocation is destructive against the largest possible target, so
the safety story cannot rest on the user reading the output.

Confirmation prompts are not available as an answer. ADR 0001 makes voom non-interactive so
it can run from hooks and CI; a prompt in the default path would either break those or be
suppressed by a flag that everyone adds to their alias. Prompt fatigue converts into
reflexive approval, which is worse than no prompt because it manufactures the appearance of
review.

## Decision

**`voom <path>` deletes.** `--dry-run` reports what would be removed and exits without
touching anything. There is **no confirmation prompt** in any mode.

The entire safety budget is therefore spent on rails that cannot be clicked through:

1. **Marker anchoring (ADR 0002).** Nothing is deleted without a marker file proving its
   ecosystem. This is the primary rail and does the majority of the work.
2. **Never follow symlinks.** The walker sets `follow_links(false)` and deletion refuses any
   path whose resolution differs from the path walked to it. A symlinked `target/` pointing
   at a source tree must not become a deletion of that source tree.
3. **Never cross filesystem boundaries** unless `--one-file-system=false` is passed. Mounted
   volumes, network shares, and external disks are outside the intent of "clean this tree".
   Skipped mounts are reported, not silently dropped.
4. **A hard-coded protected-path denylist**, checked against the canonicalized path
   immediately before every `remove_dir_all`: `/`, `/System`, `/usr`, `/bin`, `/sbin`,
   `/etc`, `/var`, `/Library`, `/Applications`, `$HOME` itself, any VCS directory (`.git`,
   `.hg`, `.svn`), and any filesystem root. The list is **append-only** — entries may be
   added, never removed.
5. **Containment.** Every deletion target must canonicalize to a path strictly *below* a
   resolved scan root. A candidate that escapes its root — via a symlink, a `..` in
   configuration, or a race — is refused and reported as an error.
6. **Keep policies (ADR 0004).** `min_age` in particular means a scheduled run never touches
   an artifact from an active build.
7. **Idempotent, isolated failure.** Each artifact is removed independently; a failure on one
   (permissions, a file vanishing mid-walk) is recorded and reported, and does not abort the
   run or leave the tool in a partial state that a re-run cannot resolve.

`--dry-run` is the same pipeline with the final step withheld, never a separate code path, so
what it reports is by construction what a real run would do.

Deletion is a permanent `remove_dir_all`, not a move to the system trash. Trashing a
multi-gigabyte `target/` copies it to a trash directory on the same volume, which reclaims
nothing until the user empties it — it converts a fast reclaim into a slow non-reclaim, and
on a `$HOME`-wide run it would fill the trash with tens of gigabytes.

## Consequences

Positive:

- The common case is one word and does the thing, so nobody develops a reflex to bypass a
  guard.
- The rails hold uniformly across interactive shells, hooks, CI, and cron, because none of
  them depend on a human being present.
- Anchoring plus containment plus the denylist are all testable properties. A prompt is not
  testable in any meaningful sense — it tests the user.
- `--dry-run` sharing the pipeline means its output is trustworthy rather than a
  best-effort simulation.

Negative / risks:

- **A mistyped path deletes immediately.** `voom ~/wrok` is harmless (nothing matches) but
  `voom ~` behaves exactly as documented, and a user who did not mean it has no undo. This is
  the accepted cost of the default, and it is the reason rails 1–5 are non-negotiable.
- No trash means no recovery for a genuine false positive. The catalog invariant tests
  (ADR 0003) and the per-entry false-positive fixtures are what stand in for recovery.
- The `same_file_system` default will surprise someone whose projects live on an external
  drive; reporting skipped mounts mitigates but does not remove the surprise.
- Canonicalizing every target before deletion costs a syscall per artifact. Negligible
  against the cost of the removal itself, and not optional.

## Alternatives considered

- **Dry-run by default, `--apply` to delete:** rejected — for a tool intended for habitual
  use, the flag becomes muscle memory and stops functioning as a control, while costing every
  correct invocation an extra step. The rails above are the real control.
- **Confirmation prompt on a TTY, `-y` to skip:** rejected — incompatible with the
  non-interactive design (ADR 0001), and prompt fatigue makes it security theatre. Noted here
  because it is the most commonly requested alternative and will be requested again.
- **Move to the OS trash instead of unlinking:** rejected — on the same volume it reclaims no
  space until the trash is emptied, turning a fast operation into a slow one that does not
  achieve the tool's purpose. `--trash` remains a plausible opt-in later.
- **Confirm only above a size or count threshold:** rejected — a threshold that fires rarely
  is one a user always approves, and the runs it would fire on (a `$HOME` sweep) are exactly
  the intended use.
- **A denylist alone, without marker anchoring:** rejected — the denylist protects system
  paths, not the user's own hand-written `bin/`, which is where the real risk lives
  (ADR 0002).

## Amendment — 2026-08-28

Three of the six rails above were absent or dead on Windows until today (`src/delete.rs`). Each
looked like paranoia to add and was in fact a hole a reader would not see without testing on the
platform.

**Rail 4, the protected-path denylist, could not fire at all.** `Guard::check` compares the
*canonicalized* path against the literals in `PROTECTED_PATHS`, and Windows `canonicalize()`
returns the verbatim `\\?\C:\…` form — a form no typed `C:\` literal is ever equal to. The
literals are now resolved into that same verbatim form at startup (`resolved_protected_paths`),
alongside the real system locations read from the environment (`SystemRoot`, `ProgramFiles`,
`ProgramFiles(x86)`, `ProgramW6432`, `ProgramData`, `USERPROFILE`, and `SystemDrive`'s root), so a
machine that does not boot from `C:` is protected too. `PROTECTED_PATHS` itself stays untouched
and append-only; the resolved list is additive to it, not a replacement.

**Path comparison was byte-exact.** Windows filesystems compare names case-insensitively, so
`.GIT`, a differently-cased protected path, or a scan root spelled in another case each walked
straight past a name-based rail. Comparison (`names_equal`, `paths_equal`, `path_starts_with`) is
now component-wise and case-insensitive on Windows, and stays byte-exact on unix, where `Target`
and `target` really are two directories and folding them together would answer a question about
one with the other.

**Reparse points beyond the symlink rail.** Rust's `FileType::is_symlink` does report junctions
on Windows — verified on the Windows CI leg, not assumed — so rail 2 already caught the main
hazard (a junction standing in for a symlinked `target/`). A separate refusal
(`Refusal::ReparsePoint`) now covers the reparse tags Rust does not classify as a link: a volume
mount point, a OneDrive Files-On-Demand placeholder, a dedup stub. These are refused rather than
followed, on the same reasoning as rail 2: a redirection voom did not expect can cost work that
cannot be got back, while a directory left on disk only costs the space it occupies.

**Rail 3, `--one-file-system`, was a documented no-op off unix.** The rail was `cfg(unix)`, and the
walker's `same_file_system` is implemented against `dev()`, which Windows has no equivalent of on
stable Rust — `MetadataExt::volume_serial_number` is unstable (rust-lang/rust#63010). The boundary
is instead enforced from the path prefix: a candidate whose canonical path names a different
drive letter or UNC share than its root is refused. The completeness argument is the reason this
is sound rather than a partial fix: the only way to reach another volume while staying below the
root is through a volume mount point, which is a reparse point, which rail 2's backstop now
refuses unconditionally on Windows — so `--one-file-system=false` there is stricter than it
literally asks for, which is the direction to err in.

Windows is verified by the CI leg only — there is no Windows machine in the development loop.
Every claim above is backed by a test in `src/delete.rs` that runs there — the junction refusal,
the resolved-denylist match, the case-insensitive comparison, and the volume-prefix read all have
one — and none of it has been run against real hardware by a contributor.

## Amendment — `--force` — 2026-08-28

The first real sweeps of a home directory produced a class of failure the semantics above had no
answer for: a removal that stopped part-way with real bytes still on disk, because something
inside the artifact could not be unlinked or because a build wrote into a directory between voom
emptying it and unlinking it. `remove_dir_all` removes a tree's contents before the tree itself,
so a *partial* removal is the ordinary failure mode rather than an exotic one.

`--force` answers the two of those voom can do something about, and nothing else. It is a
setting on what happens **after** `Guard::check` has returned — the rails are decided before the
flag is read.

**Containment is a type argument, not a convention.** Every function in `src/delete/force.rs`
takes a `Verified`, and the only thing that mints one is `Guard::check`. A permission repair
therefore cannot be reached with a path that has not been canonicalized, checked against the
denylist, proven strictly below a scan root, and proven not to be a symlink. The rail tests are
parameterised over `[Removal::new(), Removal::forced()]`, so a rail added later is covered under
force without anyone remembering to.

**What it does.** `DirectoryNotEmpty` is retried on a 50 ms / 200 ms / 800 ms backoff, because
the cause is timing; `PermissionDenied` triggers one permission repair inside the artifact and
one immediate retry, because a mode does not change by waiting. A read-only mount and an I/O
error are reported, never retried.

**What it will not do**, each a deliberate limit rather than an omission:

- **It never touches the artifact's parent.** An artifact held by a read-only parent directory is
  therefore reported and not reclaimed. That is containment working, not a gap to close.
- **It never follows a symlink.** The repair walk stats with `symlink_metadata` and skips links
  entirely rather than descending or chmod'ing through them.
- **It never widens a hard-linked file.** A mode bit and a BSD flag belong to the inode, while
  the removal only unlinks the one name inside the artifact — so relaxing a file with `nlink > 1`
  would change data voom is not deleting, possibly outside the scan root entirely. Such a file is
  skipped, which costs at most a failure reported instead of reclaimed.
- **It only ever widens the owner's bits**, and only the ones an unlink needs: read, write and
  execute on a directory (all three, because a directory that cannot be listed cannot be
  emptied), write on a file. Group and other are never touched, so a tree whose removal fails
  anyway is not left more open than voom found it.
- **It never chowns, and it never clears the system-immutable flags** — those need root, and at a
  raised securelevel cannot be cleared at all.

**The residual race.** The repair checks a path and then names it again to `read_dir` and
`set_permissions`; `std` has no `openat`-style way to close that window. A writer that swaps a
checked directory for a symlink in between can be followed. This is the same race
`remove_dir_all` itself carries and is recorded here because this code changes modes rather than
only unlinking.

`--force` is filed under **Behaviour**, not **Safety**, in `--help`. Listing it beside
`--one-file-system` would invite reading it as a protection, and it is the opposite: it is the
flag that pushes through. Watch mode forces it off — watch already re-runs, so the retry is
inherent, and unattended repeated permission repair against a live build is the last place it
belongs. `--dry-run --force` is accepted and is a strict no-op by construction, since force lives
entirely after the step a dry run withholds.

## Amendment — a refused symlink is named, not counted — 2026-08-28

Rail 2 is unchanged: the walker still sets `follow_links(false)`, and deletion still refuses any
path that resolves through a link. What changed is where the refusal appears.

The walker used to convert a symlinked artifact into a `SkipReason::Symlink` and never pass it
down the pipeline, so it landed in the "candidates passed over" count — a bare number unless the
run was given `-v`. A link standing exactly where an artifact belongs is the shape of a mistake
worth looking at, and a number does not tell anyone it is there. `src/catalog/infra.rs` already
claimed Bazel's `bazel-*` convenience symlinks were "found, reported, and refused"; they were
found and refused, and not reported.

A symlinked artifact is now a `Finding` that rail 2 refuses by name:

```text
  refused           0 B  rust  target  — is a symlink
```

This is strictly a reporting change and cannot become a deletion: the finding reaches
`Guard::check` like any other and is refused there, which is the same code path as before and is
what `should_never_delete_through_a_symlink` asserts. Its size reads `0 B` because `size::measure`
measures a link as a link — which is honest, since removing it would reclaim nothing.

**Traversal was considered and not changed.** Following links was on the table for this release,
and the measurement that settled it is worth recording so it is not re-opened without new
evidence: a sweep of a real `~/workspace` found around 200 symlinked directories, roughly 190 of
them pnpm's internal store, and following them surfaced **no artifact the walk did not already
reach by its real path**. The reason is structural rather than incidental. A link's target is
either below a scan root, in which case the walk arrives by the real name and descending is a
duplicate, or it is not, in which case containment (rail 5) would refuse everything found there —
so descending buys a wall of refusals and no reclaimable bytes. `follow_links(false)` stays.
