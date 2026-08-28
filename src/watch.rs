//! Watch mode: keep a tree pruned as work moves on.
//!
//! A one-shot sweep reclaims disk at a moment in time; on an active machine the artifacts come
//! straight back. See `adrs/0008-watch-mode.md`.
//!
//! The load-bearing safety idea is the **quiet period**. A periodic re-scan has no way to know
//! whether a build is running, so it will eventually delete a `target/` a compiler is writing
//! into. Here an artifact is removed only once its subtree has been *idle* for the quiet
//! period: an active compiler keeps touching its output directory, so the timer never expires
//! while it runs. `min_age` from ADR 0004 applies on top.
//!
//! Everything else is the one-shot pipeline. Watch mode is the same classifier and the same
//! deleter driven by a different trigger, so it inherits every safety rail rather than
//! restating any of them.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use notify::RecursiveMode;
use notify_debouncer_full::new_debouncer;

use crate::classify::is_dependency_dir;
use crate::delete::{Guard, Removal};
use crate::error::{Error, Result};
use crate::report::RunResult;
use crate::run::{RunOptions, run};

/// How to watch.
#[derive(Debug, Clone)]
pub struct WatchOptions {
    /// Coalesce filesystem events over this window. A single `cargo build` emits thousands of
    /// events; reacting to each would mean re-deriving the same answer continuously.
    pub debounce: Duration,
    /// How long a subtree must be idle before its artifacts may be removed.
    pub quiet_period: Duration,
}

impl Default for WatchOptions {
    fn default() -> Self {
        Self {
            debounce: Duration::from_secs(5),
            quiet_period: Duration::from_secs(60),
        }
    }
}

/// Tracks when each directory was last touched, and answers whether it has gone quiet.
///
/// Separated from the event loop so the rule that actually protects an in-progress build can be
/// tested without a filesystem watcher, real timing, or a running compiler.
#[derive(Debug, Default)]
pub struct QuietTracker {
    touched: HashMap<PathBuf, Instant>,
}

impl QuietTracker {
    /// Creates an empty tracker.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records activity at a path, and at every directory above it that is still being tracked.
    ///
    /// A write deep inside `target/debug/incremental/` counts as activity on `target/`, which
    /// is what makes a running build hold its own output directory open.
    pub fn touch(&mut self, path: &Path, at: Instant) {
        let mut current = Some(path);
        while let Some(dir) = current {
            self.touched.insert(dir.to_path_buf(), at);
            current = dir.parent();
        }
    }

    /// Whether a path has been idle for at least the quiet period.
    ///
    /// A path never seen is quiet: it has not been touched since the watcher started, which is
    /// the strongest evidence of idleness available.
    #[must_use]
    pub fn is_quiet(&self, path: &Path, quiet_period: Duration, now: Instant) -> bool {
        let Some(last) = self.newest_under(path) else {
            return true;
        };
        now.duration_since(last) >= quiet_period
    }

    /// The most recent activity at or below a path.
    fn newest_under(&self, path: &Path) -> Option<Instant> {
        self.touched
            .iter()
            .filter(|(touched, _)| touched.starts_with(path))
            .map(|(_, at)| *at)
            .max()
    }

    /// Forgets everything at or below a path, after it has been removed.
    pub fn forget(&mut self, path: &Path) {
        self.touched.retain(|touched, _| !touched.starts_with(path));
    }

    /// How many paths are being tracked, so the loop can bound its own memory.
    #[must_use]
    pub fn len(&self) -> usize {
        self.touched.len()
    }

    /// Whether nothing has been recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.touched.is_empty()
    }

    /// Drops entries older than the quiet period, which can no longer change any answer.
    pub fn prune(&mut self, quiet_period: Duration, now: Instant) {
        self.touched.retain(|_, at| now.duration_since(*at) < quiet_period);
    }
}

/// Whether one filesystem *event* counts towards the quiet period.
///
/// The name is older than the behaviour: nothing here is given to the watcher, which registers
/// the root recursively and watches everything. This filters events, and it matches on the
/// event path's **final component** — so a write to `node_modules/left-pad/index.js` passes,
/// and only an event on the dependency directory itself is dropped.
///
/// That is the behaviour to keep now that `--clean-dependencies` can remove `node_modules/`. A
/// deep event marking its ancestors is what stops the quiet period expiring in the middle of an
/// `npm install`, and `QuietTracker::is_quiet` treats a path it has never seen as quiet — so
/// "correcting" this to ancestor matching would let a watch remove a dependency directory while
/// a package manager was writing into it.
#[must_use]
pub fn should_watch(path: &Path) -> bool {
    let Some(name) = path.file_name() else { return true };
    // The same list the walker never descends into, rather than a second copy of it: watching
    // one of these burns an inotify watch on a subtree nothing here would ever act on, and two
    // copies of a list like this drift.
    let vcs = crate::scan::NEVER_DESCEND
        .iter()
        .any(|never| name == std::ffi::OsStr::new(never));
    !(is_dependency_dir(name) || vcs)
}

/// Runs the watcher until interrupted.
///
/// A full scan establishes the baseline at startup; after that only the subtrees that
/// filesystem events name are re-examined, so the steady-state cost is near zero.
///
/// # Errors
///
/// [`Error::Watch`] if a watcher cannot be created or a root cannot be registered — including
/// the `inotify` limit, which is reported with the paths it could not watch rather than
/// silently degrading into a watcher that sees nothing.
pub fn watch(
    options: &RunOptions,
    watch_options: &WatchOptions,
    mut on_prune: impl FnMut(&RunResult) -> io::Result<()>,
) -> Result<()> {
    let roots = options.roots.clone();
    let mut tracker = QuietTracker::new();

    // The baseline pass *seeds* the tracker; it does not remove anything.
    //
    // At startup voom has no history for any subtree, so an artifact being written to right now
    // is indistinguishable from one abandoned last week. A baseline that removed immediately
    // would walk straight past the quiet period — which ADR 0008 makes the rail that stops
    // watch mode deleting an in-progress build — and `voom watch` started during a build would
    // take that build's output. Seeding costs one quiet period before the first removal, and
    // buys the rail applying to every artifact rather than only to the ones that appear later.
    let baseline = run(&RunOptions {
        dry_run: true,
        ..options.clone()
    })?;
    let started = Instant::now();
    for entry in &baseline.entries {
        tracker.touch(&entry.path, started);
    }

    let (sender, receiver) = std::sync::mpsc::channel();
    let mut debouncer = new_debouncer(watch_options.debounce, None, sender).map_err(|error| Error::Watch {
        reason: error.to_string(),
    })?;

    let mut unwatched = Vec::new();
    for root in &roots {
        if let Err(error) = debouncer.watch(root, RecursiveMode::Recursive) {
            unwatched.push(format!("{}: {error}", root.display()));
        }
    }
    if !unwatched.is_empty() {
        return Err(Error::Watch {
            reason: format!("could not watch {}", unwatched.join("; ")),
        });
    }

    // The loop wakes on events *and* on a timer. A subtree that has gone quiet by definition
    // stops producing events, so without the timer the quiet period could never be observed to
    // elapse and an artifact would sit there until the next unrelated build touched something.
    let tick = (watch_options.quiet_period / 2).max(Duration::from_secs(1));

    loop {
        match receiver.recv_timeout(tick) {
            Ok(Ok(events)) => {
                let now = Instant::now();
                for event in events {
                    for path in &event.paths {
                        if should_watch(path) {
                            tracker.touch(path, now);
                        }
                    }
                }
            }
            Ok(Err(errors)) => {
                if let Some(error) = errors.into_iter().next() {
                    return Err(Error::Watch {
                        reason: error.to_string(),
                    });
                }
            }
            // Nothing happened. Re-evaluate only if something is still being tracked; with an
            // idle tree this costs nothing at all.
            Err(RecvTimeoutError::Timeout) => {
                if tracker.is_empty() {
                    continue;
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }

        let now = Instant::now();

        // A dry run finds what is currently reclaimable without removing anything; the quiet
        // period then decides which of those are safe to take.
        let mut candidates = run(&RunOptions {
            dry_run: true,
            ..options.clone()
        })?;
        candidates.entries.retain(|entry| {
            entry.outcome.is_reclaimed() && tracker.is_quiet(&entry.path, watch_options.quiet_period, now)
        });

        if candidates.entries.is_empty() {
            tracker.prune(watch_options.quiet_period, now);
            continue;
        }

        // Remove exactly the artifacts that have gone quiet — not everything the sweep found.
        // This is the one-shot deleter, so every rail applies unchanged; only the trigger and
        // the scoping differ.
        let guard = Guard::new(&roots, options.one_file_system)?;
        // Watch never forces. It already re-runs on every quiet period, so the retry is
        // inherent — and repeatedly repairing permissions unattended, in a loop, against a tree
        // something is actively building is the last place that belongs.
        let removal = Removal {
            dry_run: options.dry_run,
            force: false,
        };
        for entry in &mut candidates.entries {
            entry.outcome = guard.remove(&entry.path, entry.bytes, removal);
            if entry.outcome.is_reclaimed() {
                tracker.forget(&entry.path);
            }
        }

        candidates.dry_run = options.dry_run;
        candidates.skips.clear();
        candidates.skipped_count = 0;
        on_prune(&candidates).map_err(|error| Error::Watch {
            reason: error.to_string(),
        })?;
        tracker.prune(watch_options.quiet_period, now);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, seconds: u64) -> Instant {
        base + Duration::from_secs(seconds)
    }

    /// The rule that keeps watch mode from deleting an in-progress build.
    #[test]
    fn should_hold_a_subtree_that_was_touched_recently() {
        let base = Instant::now();
        let mut tracker = QuietTracker::new();
        let quiet = Duration::from_secs(60);
        let target = Path::new("/project/target");

        tracker.touch(Path::new("/project/target/debug/app"), base);

        assert!(
            !tracker.is_quiet(target, quiet, at(base, 10)),
            "a build from 10s ago holds it"
        );
        assert!(
            !tracker.is_quiet(target, quiet, at(base, 59)),
            "still within the window"
        );
        assert!(
            tracker.is_quiet(target, quiet, at(base, 60)),
            "released once the window passes"
        );
    }

    /// Activity deep inside an artifact counts as activity on the artifact — this is what makes
    /// a compiler writing into `target/debug/incremental/` hold `target/` open.
    #[test]
    fn should_treat_activity_deep_inside_as_activity_on_the_artifact() {
        let base = Instant::now();
        let mut tracker = QuietTracker::new();
        tracker.touch(Path::new("/project/target/debug/incremental/foo/bar.o"), base);

        assert!(!tracker.is_quiet(Path::new("/project/target"), Duration::from_secs(60), at(base, 5)));
    }

    /// A continuously active build never goes quiet, however long it runs.
    #[test]
    fn should_never_release_a_subtree_while_it_is_being_written_to() {
        let base = Instant::now();
        let mut tracker = QuietTracker::new();
        let quiet = Duration::from_secs(60);
        let target = Path::new("/project/target");

        for second in 0..600 {
            tracker.touch(Path::new("/project/target/debug/app"), at(base, second));
            assert!(
                !tracker.is_quiet(target, quiet, at(base, second)),
                "an actively written subtree must never look idle"
            );
        }
    }

    #[test]
    fn should_treat_an_untouched_subtree_as_quiet() {
        let tracker = QuietTracker::new();
        assert!(tracker.is_quiet(Path::new("/project/target"), Duration::from_secs(60), Instant::now()));
    }

    /// A sibling's activity must not hold an unrelated artifact.
    #[test]
    fn should_not_let_one_project_hold_another() {
        let base = Instant::now();
        let mut tracker = QuietTracker::new();
        tracker.touch(Path::new("/a/target/debug/app"), base);

        assert!(!tracker.is_quiet(Path::new("/a/target"), Duration::from_secs(60), at(base, 1)));
        assert!(tracker.is_quiet(Path::new("/b/target"), Duration::from_secs(60), at(base, 1)));
    }

    #[test]
    fn should_forget_a_subtree_once_it_has_been_removed() {
        let base = Instant::now();
        let mut tracker = QuietTracker::new();
        tracker.touch(Path::new("/project/target/debug/app"), base);
        assert!(!tracker.is_empty());

        tracker.forget(Path::new("/project/target"));
        assert!(tracker.is_quiet(Path::new("/project/target"), Duration::from_secs(60), at(base, 1)));
    }

    /// Memory must not grow without bound on a long-lived watch over a busy tree.
    #[test]
    fn should_prune_entries_that_can_no_longer_change_an_answer() {
        let base = Instant::now();
        let mut tracker = QuietTracker::new();
        for index in 0..100 {
            tracker.touch(&PathBuf::from(format!("/project/{index}/file")), base);
        }
        assert!(tracker.len() > 100);

        tracker.prune(Duration::from_secs(60), at(base, 120));
        assert_eq!(
            tracker.len(),
            0,
            "entries older than the quiet period cannot hold anything"
        );
    }

    /// Watching a dependency cache or `.git` would exhaust `inotify` limits for no benefit —
    /// nothing in either is ever removable.
    #[test]
    fn should_not_watch_dependency_caches_or_vcs_directories() {
        assert!(!should_watch(Path::new("/project/node_modules")));
        assert!(!should_watch(Path::new("/project/vendor")));
        assert!(!should_watch(Path::new("/project/.git")));
        assert!(should_watch(Path::new("/project/src")));
        assert!(should_watch(Path::new("/project/target")));
    }

    #[test]
    fn default_windows_match_the_documented_ones() {
        let options = WatchOptions::default();
        assert_eq!(options.debounce, Duration::from_secs(5));
        assert_eq!(options.quiet_period, Duration::from_secs(60));
    }

    /// A write deep inside a dependency directory must keep its ancestors from going quiet.
    ///
    /// `--clean-dependencies` makes `node_modules/` removable, so an `npm install` in progress is
    /// exactly the moment a watch must not fire. [`should_watch`] matches the event path's final
    /// component, which is why a deep event passes; a version matching ancestors would drop it,
    /// and an unseen path counts as quiet.
    #[test]
    fn should_let_a_write_inside_a_dependency_directory_hold_its_ancestors() {
        let deep = Path::new("/projects/app/node_modules/left-pad/index.js");
        assert!(
            should_watch(deep),
            "a write inside a dependency directory has to reach the tracker"
        );
        assert!(
            !should_watch(Path::new("/projects/app/node_modules")),
            "an event on the directory itself still does not"
        );

        let mut tracker = QuietTracker::default();
        let now = Instant::now();
        tracker.touch(deep, now);

        assert!(
            !tracker.is_quiet(Path::new("/projects/app/node_modules"), Duration::from_secs(5), now),
            "and the dependency directory is held while the write is recent"
        );
    }
}
