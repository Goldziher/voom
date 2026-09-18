# 0015 — Claude Code Job Scratch: Age-Gated, Never Trusted on State Alone

- Status: Accepted
- Date: 2026-09-18

## Context

Claude Code, the agent this feature was written by, keeps a background-job directory at
`~/.claude/jobs/<id>/`, and `<id>/tmp/` is free-form scratch the agent writes to while a job
runs — tarballs, cloned toolchains, build output — with nothing that reaps it afterwards. One
job directory measured on the machine this was written on held **4.65 GB** in `tmp/`, and its
`state.json` had said `"state": "stopped"` for eight days.

That `state.json` was wrong, in the way that matters most for a tool whose job is to delete
things. The job's daemon-managed worker really was killed on the day the file was last written.
Two days later, the same session was resumed in an ordinary foreground `claude --resume
<sessionId>` process — which does not go anywhere near `state.json` — and was **still running
six days after that**, at the moment this was checked. A rule reading the file alone would have
reported an abandoned job and deleted a live one's working scratch.

No stronger signal exists to replace it. There is no PID file, no lock file worth trusting (the
one lock file present is zero bytes whenever nobody happens to be writing at that instant, which
is nearly always), and the daemon's own log records only what its own worker did, never a later
manual resume of the same session. The only thing checkable at all is whether a *process on this
machine, right now* has the session's id on its command line — which only ever appears when
someone launched it with `--resume`, so a plain `claude` session leaves no trace `ps` can find
either. This is a proof of "definitely still active," available in exactly one shape, and never
a proof of "definitely gone."

## Decision

**`voom claude-prune` finds job directories whose `tmp/` has been quiet for a long time, checks
whether the recorded session is running right now, and reports every one it finds — removing
nothing unless told to, twice.**

### What is checked

For each `~/.claude/jobs/<id>/` found:

- `state.json` is read for `sessionId` and `lastTerminalAt`. Missing or unparsable, and the
  directory is unproven — reported, never removed, the same as anywhere else in voom a marker
  fails to hold up.
- **Age** is `now - lastTerminalAt`, not the state field beside it. `state` is not read for
  this decision at all: the measurement above is exactly the case where it lags reality, and a
  rule that consults it anyway would be trusting the thing that was already shown wrong.
- **Liveness** is `ps`'s own process list, matched for the recorded `sessionId` on any command
  line. A match **hard-blocks removal regardless of every other flag or threshold** — it is the
  one positive fact available, and it overrides an otherwise-satisfied age gate rather than
  merely lowering confidence in it. A miss proves nothing whatsoever, which is why it can only
  block, never justify.

### What removal requires — both, not either

A job's `tmp/` is a candidate for removal only when **all** of: no live process matches its
session, `lastTerminalAt` is older than `--min-age` (default 14 days — the shortest period the
one measured near-miss shows is unsafe, not a period this ADR claims is definitely safe), the
run was invoked with the explicit `--remove` flag, and it is not a `--dry-run`. Missing any one
of the first three reports the job as kept, with the reason. `voom claude-prune` with no flags
at all is therefore always a report: a user runs it, reads what it found, and decides.

This is one flag more cautious than `bazel-prune`, deliberately. An orphaned Bazel output
base's marker is a first-party record of a workspace path — existence is a filesystem fact.
Whether a session is "done" is a fact about a human's intent, which nothing on disk records
reliably, and the discovery that motivated this ADR is a case where the closest available proxy
was actively wrong. `--remove` is the difference between a tool a user can point at their own
disk with confidence and one that occasionally guesses at somebody else's plans and guesses
wrong.

### Only `tmp/`

The rest of a job directory — `state.json`, `timeline.jsonl`, `recap.trigger` — is left alone.
It is small (kilobytes, against gigabytes in `tmp/`) and it is the record of what happened,
which is worth keeping even for a job whose scratch is reclaimed.

### Removal goes through the same rails as everything else

Exactly as `bazel-prune`'s (ADR 0014): [`delete::Guard`], containment, symlink refusal, the
protected-path denylist, and a dry run that is the same code with the final step withheld.

## Consequences

Positive:

- Reclaims real space this tool's own author's use of Claude Code creates, and says so honestly
  instead of pretending a stronger guarantee exists than does.
- The report is useful on its own, with `--remove` never given: a user sees every job's age,
  size, and whether it is live right now, and can act on that with better judgement than any
  fixed rule.

Negative / risks:

- **`--min-age` is not a proof.** Fourteen days is a floor observed to be too short once, not a
  period confirmed to be long enough; a session set aside for a long trip is exactly the case
  this cannot see.
- **The liveness check is unreliable in the direction that matters least.** It can miss a live
  session (a plain `claude` with no `--resume` on its command line), but a miss only means the
  age gate is the only rail left, not that removal proceeds on a false "confirmed dead" — no
  claim this ADR makes ever asserts a job is dead, only that no evidence of life was found.
- **A `ps` invocation per run**, cheap relative to the job it might spare from an unwanted
  removal.

## Alternatives considered

- **Trusting `state.json`'s `state` field**, with no age gate: rejected — this is exactly the
  rule the measurement in Context disproved.
- **No liveness check, age alone**: rejected as strictly worse than adding the check, at
  negligible extra cost, given a real case existed where it would have mattered.
- **Removing the whole job directory, not only `tmp/`**: rejected — `state.json` and
  `timeline.jsonl` are the record of what a job did, are small, and losing them costs more than
  they save.
- **Folding into a sweep or `--enable`-style catalog opt-in**: rejected for the reason
  `bazel-prune` gives — there is no walk of `~/.claude/jobs` to ride along with, and this
  question is decided by more than a marker check the classifier's shape can express.
