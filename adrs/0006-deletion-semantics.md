# 0006 — Deletion Semantics: Default-Delete, Dry-Run, and the Safety Rails

- Status: Accepted
- Date: 2026-08-28

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
