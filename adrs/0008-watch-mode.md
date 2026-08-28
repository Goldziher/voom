# 0008 — Watch Mode: Debounced `notify` Pruning

- Status: Proposed
- Date: 2026-08-28

## Context

A one-shot sweep reclaims disk at a moment in time; on an active machine the artifacts come
straight back. Someone iterating across several projects regenerates gigabytes a day, and the
useful behaviour is continuous: keep the tree pruned as work moves on, without anybody
remembering to run anything.

The obvious naive implementation — a timer that re-runs a full scan every few minutes — is
wasteful on a large tree and, worse, unsafe in a way a one-shot run is not. A periodic sweep
will eventually fire in the middle of a build and delete the `target/` directory a compiler
is actively writing into. A one-shot run is issued by a human who knows what they are doing
at that moment; a background loop has no such context.

## Decision

`voom watch <paths>` runs a **foreground, event-driven** pruner built on `notify` with
`notify-debouncer-full`, not a polling loop and not a background daemon (ADR 0001).

- **Event-driven, scoped re-classification.** Filesystem events identify which directories
  changed; only those subtrees are re-classified. A full scan happens once at startup to
  establish the baseline, and never again.
- **Debounced.** Events are coalesced over a window (default 5 s, `--debounce`). Builds
  produce event storms, and reacting per-event would mean thousands of redundant
  classifications.
- **Quiet-period requirement.** An artifact is only removed after its subtree has been
  *idle* for the quiet period (default 60 s, `--quiet-period`). This is the rail that keeps
  watch mode from deleting an in-progress build: an active compiler continuously touches its
  output directory, so the timer never expires while it is running. `min_age` from
  ADR 0004 applies on top.
- **Same rails as one-shot.** Marker anchoring (ADR 0002), the protected-path denylist,
  containment, and symlink and filesystem-boundary refusal (ADR 0006) apply unchanged. Watch
  mode is the same classifier and the same deleter driven by a different trigger.
- **Incremental reporting.** Each prune emits the same per-artifact line as a normal run
  (ADR 0007), timestamped. `--format json` emits one JSON object per prune event, newline
  delimited, so it can be piped into a log processor.
- **Bounded watch registration.** Watchers are registered per root with recursive mode, with
  dependency directories and `.git` excluded from watching entirely — on Linux these
  otherwise exhaust `inotify` watch limits on a large tree. If the OS limit is hit, voom
  reports the limit and the paths it could not watch rather than silently degrading.

Status is **Proposed** rather than Accepted: the quiet-period heuristic is the load-bearing
safety assumption and has not yet been validated against real build workloads across
ecosystems. It moves to Accepted once fixtures for at least Rust, Node, and Gradle
demonstrate no mid-build deletion.

## Consequences

Positive:

- Disk stays reclaimed continuously without anyone remembering to act, which is the actual
  goal — a cleanup tool people must remember to run is a cleanup tool that does not run.
- Event-driven scoping means the steady-state cost is near zero, unlike a periodic full scan
  over a home directory.
- The quiet period gives a defensible answer to the obvious objection ("won't this delete my
  build?") that does not depend on the user configuring anything.
- Reusing the one-shot classifier and deleter means watch mode inherits every safety rail
  automatically and cannot drift from them.

Negative / risks:

- **The quiet period is a heuristic, not a guarantee.** A build that pauses longer than the
  window — a slow test suite between compile phases, a debugger sitting at a breakpoint —
  can look idle. This is the central risk and the reason the ADR is Proposed; the default is
  deliberately conservative and `min_age` compounds it.
- `notify` behaves differently across platforms (FSEvents coalesces, `inotify` is per-directory
  and limited, Windows has its own semantics). Watch mode needs per-platform testing and will
  behave subtly differently on each.
- A long-lived process holding watches on a home directory is a resource commitment, and
  `inotify` limits on Linux will be hit by some users.
- Foreground-only means users will wrap it in their own supervisor, and their supervisor will
  restart it in ways voom cannot reason about. That is preferable to shipping a daemon
  (ADR 0001) but it does push a problem outward.

## Alternatives considered

- **A periodic full re-scan on a timer:** rejected — expensive on a large tree, and it has no
  way to know whether a build is running, so it will eventually delete an in-progress
  `target/`. This is the alternative watch mode exists to avoid.
- **A background daemon or launchd/systemd service:** rejected by ADR 0001 — voom stays a
  foreground command. Users who want supervision can wrap it, which keeps the lifecycle in
  their hands rather than in an installer.
- **Reacting to every filesystem event without debouncing:** rejected — a single `cargo build`
  emits thousands of events; the classifier would spend all its time re-deriving the same
  answer.
- **Polling `mtime` on known artifact directories instead of using `notify`:** rejected —
  simpler and more portable, but it cannot distinguish "build finished" from "build paused"
  any better than a timer, while giving up the event scoping that makes watch mode cheap.
- **Deleting immediately on the completion of a detectable build** (watching for the compiler
  process to exit): rejected — requires process inspection, is per-toolchain, and does not
  generalize across 23 ecosystems.
