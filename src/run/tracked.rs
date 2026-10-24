//! The tracked-files rail: an inferred artifact holding files its repository tracks is refused.
//!
//! Split from the pipeline because it is a rail of its own with its own collaborator — the git
//! index — and nothing else in `run.rs` talks to git about an artifact.

use std::path::{Path, PathBuf};

use crate::classify::Finding;
use crate::delete::Outcome;
use crate::report::Entry;
use crate::size::Measured;

/// Takes out of `to_remove` every artifact that holds files its repository tracks, as a refusal.
///
/// Asked only of what is about to be removed, after every policy has spoken, and only of what the
/// walk *inferred*: an `include` names a path outright, and a cache is not inside a repository.
/// The question costs one `git ls-files` per repository holding a candidate, and nothing in a
/// tree without git.
pub(super) fn without_tracked(
    to_remove: &mut Vec<(Finding, Option<Measured>)>,
    repositories: &[PathBuf],
) -> Vec<Entry> {
    use crate::classify::Provenance;
    let inferred = |finding: &Finding| matches!(finding.provenance, Provenance::Anchored { .. } | Provenance::Tagged);
    let asked: Vec<&Path> = to_remove
        .iter()
        .filter(|(finding, _)| inferred(finding))
        .map(|(finding, _)| finding.path.as_path())
        .collect();
    if asked.is_empty() || repositories.is_empty() {
        return Vec::new();
    }
    let held = crate::git::holding_tracked_files(&asked, repositories);
    if held.is_empty() {
        return Vec::new();
    }
    let (refused, kept): (Vec<_>, Vec<_>) = std::mem::take(to_remove)
        .into_iter()
        .partition(|(finding, _)| inferred(finding) && held.contains(&finding.path));
    *to_remove = kept;
    refused
        .into_iter()
        .map(|(finding, measured)| Entry {
            path: finding.path.clone(),
            finding,
            bytes: measured.map_or(0, |measured| measured.bytes),
            unreadable: measured.map_or(0, |measured| measured.unreadable),
            outcome: Outcome::Refused(crate::delete::Refusal::Tracked),
        })
        .collect()
}
