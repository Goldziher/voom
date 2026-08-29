//! The per-entry decision, made on whichever worker thread produced the entry.
//!
//! Every branch here either prunes a subtree or emits one message; nothing accumulates, and
//! nothing allocates on the path that rejects an ordinary entry. The order of the checks is the
//! order of authority — `exclude` outranks `include`, which outranks every prune — and it is
//! load-bearing, not incidental.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use ignore::WalkState;

use super::{Message, NEVER_DESCEND, ScanOptions, Skipped, WalkFailure};
use crate::classify::{Classifier, Finding, Provenance, SkipReason, Verdict, is_dependency_dir};

/// Classifies one entry per call, from whichever worker thread produced it.
#[derive(Clone, Copy)]
pub(super) struct Visitor<'a> {
    pub(super) root: &'a Path,
    pub(super) classifier: &'a Classifier,
    pub(super) options: &'a ScanOptions,
}

impl Visitor<'_> {
    pub(super) fn visit(
        &self,
        result: std::result::Result<ignore::DirEntry, ignore::Error>,
        sender: &mpsc::Sender<Message>,
    ) -> WalkState {
        let entry = match result {
            Ok(entry) => entry,
            Err(error) => {
                let path = match &error {
                    ignore::Error::WithPath { path, .. } => Some(path.clone()),
                    _ => None,
                };
                // `EINTR` means the read was interrupted by a signal, not that the directory
                // could not be read. The walker's callback gives no way to resume the entry, so
                // it cannot be retried from here — but it can be told apart from a real failure
                // instead of being reported as one.
                let transient = error
                    .io_error()
                    .is_some_and(|io| io.kind() == std::io::ErrorKind::Interrupted);
                let failure = WalkFailure {
                    path,
                    message: error.to_string(),
                    transient,
                };
                // Sends cannot fail here: `scan_root` owns the `Receiver` and holds it until
                // `WalkBuilder::run` returns, which is after every worker has finished. The
                // same is true of the other sends in this file.
                let _ = sender.send(Message::Failure(Box::new(failure)));
                return WalkState::Continue;
            }
        };

        // Depth 0 is the scan root itself, which is never a candidate for its own removal.
        if entry.depth() == 0 {
            return WalkState::Continue;
        }

        let path = entry.path();
        // `file_type()` is what the walker already knows; a `metadata()` call here would cost
        // an extra syscall on every entry in the tree.
        let file_type = entry.file_type();
        let is_dir = file_type.is_some_and(|file_type| file_type.is_dir());
        let is_symlink = file_type.is_some_and(|file_type| file_type.is_symlink());

        if let Some(pattern) = self.options.exclude.matched(path) {
            self.note_skip(
                sender,
                path,
                SkipReason::Excluded {
                    pattern: pattern.to_owned(),
                },
            );
            return WalkState::Skip;
        }

        // `include` outranks every prune below but never `exclude`, which ADR 0004 makes
        // absolute. That ordering lets an explicit include *name* a directory the walk would
        // otherwise prune — a dependency directory or a tool cache, both of which ADR 0001
        // keeps out of the catalog and says configuration is the one way to reach.
        //
        // It does not let one reach *inside* such a directory: pruning is what makes the walk
        // fast, and once the walk declines to descend nothing below is offered to any matcher.
        // `.git` stays unreachable either way — the deletion guard refuses a VCS directory by
        // name whatever produced it.
        if let Some(pattern) = self.options.include.matched(path) {
            let finding = Finding {
                path: path.to_path_buf(),
                provenance: Provenance::Included {
                    pattern: pattern.to_owned(),
                },
            };
            let _ = sender.send(Message::Found(Box::new(finding)));
            return WalkState::Skip;
        }

        // A named cache is checked before the blanket skip below, so `--clean-caches uv`
        // reaches `~/.cache/uv` without `--caches` having to open every cache location at
        // once. The marker still has to prove it (ADR 0012): the id says which directory to
        // look at, never that it may go.
        if is_dir && let Some(cache) = self.options.caches.removable(path) {
            self.visit_named_cache(path, cache, sender);
            // Skipped either way: a cache is an indivisible unit like any other artifact, and
            // one that failed its marker check is still somewhere a walk must not wander.
            return WalkState::Skip;
        }

        // After `exclude`, which ADR 0004 makes absolute, and before anything that could
        // classify: an installed toolchain is shaped exactly like a project, so the classifier
        // would prove it correctly and take it (ADR 0001).
        if is_dir && self.options.caches.contains(path) {
            // A named cache underneath is still reached, by naming rather than by walking.
            // The prune below is unchanged, so nothing else in here is enumerated or
            // classified — see `CacheRoots::removable_inside`.
            for (cache_path, cache) in self.options.caches.removable_inside(path) {
                self.visit_named_cache(cache_path, cache, sender);
            }
            self.note_skip(sender, path, SkipReason::ToolCache);
            return WalkState::Skip;
        }

        // Git housekeeping discovers repositories here, at the moment the walker meets `.git`
        // and is about to prune it: one name comparison, no syscall (ADR 0011). A second walk
        // over `$HOME` to find the same directories would roughly double the run, which is the
        // whole reason this is a line in the artifact walk rather than a pass of its own.
        //
        // It does not return: the prune below still refuses to descend into a `.git` directory,
        // and a `.git` *file* — a linked worktree or a submodule — falls through to the
        // classifier, which has nothing to say about it.
        if self.options.collect_repositories
            && let Some(work_tree) = crate::git::repository_at(entry.file_name(), path, is_symlink)
        {
            let _ = sender.send(Message::Repository(work_tree.to_path_buf()));
        }

        if is_dir && is_never_descended(entry.file_name()) {
            return WalkState::Skip;
        }

        if is_dir && is_dependency_dir(entry.file_name()) {
            self.visit_dependency_dir(entry.file_name(), path, sender);
            return WalkState::Skip;
        }

        // A symlink is classified as though it were a directory so a symlinked `target/` is
        // *seen* rather than silently invisible. It can never become a deletion — the deletion
        // guard refuses to remove through a link (ADR 0006) — but it is reported as a finding
        // the guard then refuses, not folded into the anonymous skip count. A refusal names the
        // path in the artifacts block; a skip is a number unless `-v` was passed, and a
        // symlinked `target/` beside a real `Cargo.toml` is exactly the thing a reader wants
        // told about.
        match self.classifier.classify(self.root, path, is_dir || is_symlink) {
            Verdict::Artifact(finding) => {
                let _ = sender.send(Message::Found(Box::new(finding)));
                // A matched artifact is an indivisible unit: it is sized and removed wholesale,
                // so walking inside it is wasted work and would produce nested findings that
                // make the report unreadable. A symlink is not descended for the older reason:
                // the walker does not follow links at all.
                if is_dir || is_symlink {
                    return WalkState::Skip;
                }
            }
            // An `Inside` anchor that found no marker is the one skip worth pruning on. The
            // directory carries the name of a self-declaring cache and does not carry the
            // declaration — so it is either an older cache of that tool or somebody else's
            // directory, and in both cases voom has already decided not to remove it. Walking
            // it can only turn up *other* projects, and measured over one home directory it
            // turned up none at all while generating 181 of 2305 skips and taking the scan from
            // 1.3 s to 7.7 s. Pruning is a recall trade, never a safety one, and this one was
            // measured before it was made.
            Verdict::Skip(reason) => {
                let prune = is_dir && matches!(reason, SkipReason::NoInnerMarker { .. });
                self.note_skip(sender, path, reason);
                if prune {
                    return WalkState::Skip;
                }
            }
            Verdict::NotACandidate => {}
        }

        WalkState::Continue
    }

    /// Decides what a dependency directory is, without ever entering it.
    ///
    /// ~keep The caller prunes unconditionally on return, and that is not negotiable: declining
    /// to enumerate `node_modules/` is where essentially all of the walk's wall-clock saving
    /// comes from. What this adds is one classification of the *directory itself*, which costs
    /// at most a single memoized `read_dir` of its anchor — a directory the classifier has
    /// almost always listed already for the artifacts beside it — and never touches an entry
    /// below. `should_not_enumerate_inside_an_enabled_dependency_directory` pins both halves.
    ///
    /// Classification runs whether or not the artifact is enabled, because the enable decision
    /// belongs to the classifier and to the configuration resolved at the artifact's own
    /// directory (ADR 0004) — the walker does not get a second opinion on it. With the opt-in
    /// off, the verdict is a `NotEnabled` skip that says which spec would turn it on.
    fn visit_dependency_dir(&self, name: &OsStr, path: &Path, sender: &mpsc::Sender<Message>) {
        // The inside-check runs on every verdict, including `Artifact`. The classifier here is
        // permissive (`run.rs` builds it that way so a deeper `voom.toml` can still turn an
        // artifact on), so an `Artifact` verdict means "this could be taken", not "this will
        // be" — and skipping the check on it lost Ruby's `vendor/bundle/` on any tree that also
        // had a `composer.json` beside the same `vendor/`. Whether the outer directory is
        // actually taken is decided in `run.rs`, which also drops a finding the outer one
        // covers, so nothing is counted twice.
        self.check_inside_dependency(name, path, sender);
        match self.classifier.classify(self.root, path, true) {
            Verdict::Artifact(finding) => {
                let _ = sender.send(Message::Found(Box::new(finding)));
            }
            Verdict::Skip(reason) => self.note_skip(sender, path, reason),
            Verdict::NotACandidate => {}
        }
    }

    /// Classifies the specific paths the catalog declares inside a dependency directory.
    ///
    /// Nothing else below is walked: this constructs the declared paths directly and then the
    /// caller prunes, so a `node_modules/` with a hundred thousand entries still costs nothing.
    fn check_inside_dependency(&self, name: &OsStr, dir: &Path, sender: &mpsc::Sender<Message>) {
        for id in self.classifier.artifacts_inside(name) {
            let artifact = id.artifact();
            // The first segment is the dependency directory itself, which is `dir`.
            let relative: PathBuf = artifact.segments().skip(1).collect();
            let candidate = dir.join(relative);
            let Ok(metadata) = std::fs::symlink_metadata(&candidate) else {
                continue;
            };
            // Same rule as the walk above: a symlinked artifact is reported and then refused,
            // never quietly counted.
            let is_dir = metadata.is_dir() || metadata.file_type().is_symlink();
            match self.classifier.classify(self.root, &candidate, is_dir) {
                Verdict::Artifact(finding) => {
                    let _ = sender.send(Message::Found(Box::new(finding)));
                }
                Verdict::Skip(reason) => self.note_skip(sender, &candidate, reason),
                Verdict::NotACandidate => {}
            }
        }
    }

    /// Records a passed-over candidate, keeping only the count unless detail was asked for.
    /// Emits one named cache as a finding, or as the reason it was not one.
    ///
    /// The id says which directory to look at; the marker says whether it may go (ADR 0012). A
    /// location that does not exist on this machine says nothing at all — the table is written
    /// for every machine, and most entries are absent on any given one.
    fn visit_named_cache(&self, path: &Path, cache: &'static crate::caches::Cache, sender: &mpsc::Sender<Message>) {
        if !path.is_dir() {
            return;
        }
        if cache.proven_in(path) {
            let finding = Finding {
                path: path.to_path_buf(),
                provenance: Provenance::Cache { cache },
            };
            let _ = sender.send(Message::Found(Box::new(finding)));
        } else {
            self.note_skip(
                sender,
                path,
                SkipReason::NoInnerMarker {
                    ecosystem: cache.id,
                    markers: cache.markers,
                },
            );
        }
    }

    fn note_skip(&self, sender: &mpsc::Sender<Message>, path: &Path, reason: SkipReason) {
        let message = if self.options.collect_skips {
            Message::Skipped(Box::new(Skipped {
                path: path.to_path_buf(),
                reason,
            }))
        } else {
            Message::SkippedCount
        };
        let _ = sender.send(message);
    }
}

/// Whether a directory is one voom neither enters nor classifies.
fn is_never_descended(name: &OsStr) -> bool {
    NEVER_DESCEND.iter().any(|never| name == OsStr::new(never))
}
