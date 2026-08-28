//! The result set, and the two renderers over it.
//!
//! voom deletes without asking (ADR 0006), so the report is not a convenience — it is the only
//! record of what happened. Both renderers are fed from one [`RunResult`], so the JSON can
//! never drift from what the human saw, which matters when the human output is the sole record
//! of a destructive act. See `adrs/0007-reporting.md`.
//!
//! Results are **sorted by path** before either renderer runs. Parallel traversal completes in
//! nondeterministic order, and an unsorted report cannot be diffed between two runs — which is
//! what would turn `--dry-run` from an audit step back into a gesture.

pub mod human;
pub mod json;

use std::path::PathBuf;
use std::time::Duration;

use crate::classify::{Finding, SkipReason};
use crate::delete::Outcome;
use crate::scan::{Skipped, WalkFailure};

/// The JSON shape's version.
///
/// A versioned schema is a compatibility commitment. This exists so the shape can be broken
/// deliberately rather than accidentally.
///
/// **2** — `partially_removed` is a status no version-1 consumer could have handled, and
/// `totals.bytes` now answers a different question for the same tree: every byte the run freed,
/// rather than only the bytes of artifacts that are entirely gone. Since the break was being
/// spent, `reason` also became specific on a failure instead of always `removal_failed`.
pub const SCHEMA_VERSION: u32 = 2;

/// Process exit codes (ADR 0007). Distinct codes let a hook branch without parsing text.
pub mod exit {
    /// Ran to completion; nothing failed.
    pub const SUCCESS: i32 = 0;
    /// Ran, but one or more artifacts could not be removed.
    pub const REMOVAL_FAILED: i32 = 1;
    /// Usage or configuration error; nothing was scanned.
    pub const USAGE: i32 = 2;
    /// `--dry-run --exit-code` and artifacts were found (for hooks, ADR 0009).
    pub const FINDINGS: i32 = 3;
}

/// One artifact and what became of it.
#[derive(Debug, Clone)]
pub struct Entry {
    /// The path, as walked.
    pub path: PathBuf,
    /// The catalog declaration that proved it.
    pub finding: Finding,
    /// Recursive size in bytes, measured once (ADR 0005).
    pub bytes: u64,
    /// What happened.
    pub outcome: Outcome,
}

impl Entry {
    /// The ecosystem id, for grouping and for the report line.
    ///
    /// `None` for a path a configured `include` named, which no ecosystem claimed.
    #[must_use]
    pub fn ecosystem(&self) -> Option<&'static str> {
        self.finding.ecosystem()
    }

    /// The declared artifact path this entry matched, when a marker proved one.
    #[must_use]
    pub fn artifact(&self) -> Option<&'static str> {
        self.finding.artifact().map(|id| id.artifact().path)
    }

    /// What this entry actually freed — the number the report's size column shows.
    ///
    /// `bytes` is what the artifact measured *before* the removal was attempted, which for a
    /// partial removal is not what came back. Both the footer's total and its per-ecosystem
    /// breakdown read this one accessor, because they used to decide independently and a
    /// partial removal is exactly the case that would have made them disagree.
    #[must_use]
    pub fn reclaimed_bytes(&self) -> u64 {
        match &self.outcome {
            Outcome::Removed | Outcome::WouldRemove => self.bytes,
            Outcome::PartiallyRemoved { freed, .. } => *freed,
            _ => 0,
        }
    }
}

/// Totals for the footer and for the JSON.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    /// Artifacts removed, or that would be removed in a dry run.
    pub reclaimed: usize,
    /// Bytes those artifacts account for.
    pub bytes: u64,
    /// Candidates passed over during the scan.
    pub skipped: usize,
    /// Artifacts a safety rail refused.
    pub refused: usize,
    /// Artifacts that were allowed but whose removal failed.
    pub failed: usize,
    /// Artifacts partly removed: real space freed, and the artifact still on disk.
    pub partial: usize,
    /// Bytes those partial removals freed. **Already included in `bytes`.**
    pub partial_bytes: u64,
}

/// Everything one run produced.
#[derive(Debug)]
pub struct RunResult {
    /// The roots that were scanned, as given.
    pub roots: Vec<PathBuf>,
    /// Artifacts, sorted by path.
    pub entries: Vec<Entry>,
    /// Candidates passed over, sorted by path. Populated only when detail was requested.
    pub skips: Vec<Skipped>,
    /// How many candidates were passed over, whether or not detail was collected.
    pub skipped_count: usize,
    /// Per-path walk failures.
    pub failures: Vec<WalkFailure>,
    /// Whether the final step was withheld.
    pub dry_run: bool,
    /// Wall-clock time for the run.
    pub elapsed: Duration,
}

impl RunResult {
    /// Sorts everything into the deterministic order both renderers rely on.
    ///
    /// Two runs over an unchanged tree must produce byte-identical output, so this is not
    /// cosmetic — it is what makes the report diffable and therefore reviewable.
    pub fn sort(&mut self) {
        self.entries.sort_by(|left, right| left.path.cmp(&right.path));
        self.skips.sort_by(|left, right| left.path.cmp(&right.path));
        self.failures.sort_by(|left, right| left.path.cmp(&right.path));
    }

    /// The footer's numbers.
    #[must_use]
    pub fn totals(&self) -> Totals {
        let mut totals = Totals {
            skipped: self.skipped_count,
            ..Totals::default()
        };
        for entry in &self.entries {
            // `bytes` counts every byte the run actually freed, whether or not the artifact it
            // came from is gone. `reclaimed` counts artifacts that *are* gone, so a partial
            // contributes to the first and not the second — it is still on disk.
            totals.bytes += entry.reclaimed_bytes();
            match &entry.outcome {
                Outcome::Removed | Outcome::WouldRemove => totals.reclaimed += 1,
                Outcome::Refused(_) => totals.refused += 1,
                Outcome::PartiallyRemoved { freed, .. } => {
                    totals.partial += 1;
                    totals.partial_bytes += freed;
                }
                _ => totals.failed += 1,
            }
        }
        totals
    }

    /// The process exit code.
    ///
    /// `exit_code` is the `--exit-code` flag: it makes a dry run that found something exit `3`,
    /// which is how the reporting git hook fails a commit without parsing output (ADR 0009).
    #[must_use]
    pub fn exit_code(&self, exit_code: bool) -> i32 {
        let totals = self.totals();
        // A partial removal is still an artifact that could not be removed, and a re-run is
        // warranted, so it exits the same way — which is also what it did before it had a name.
        if totals.failed > 0 || totals.partial > 0 {
            return exit::REMOVAL_FAILED;
        }
        if exit_code && self.dry_run && totals.reclaimed > 0 {
            return exit::FINDINGS;
        }
        exit::SUCCESS
    }

    /// The reason a candidate was passed over, for renderers that show detail.
    pub fn skip_reasons(&self) -> impl Iterator<Item = (&PathBuf, &SkipReason)> {
        self.skips.iter().map(|skipped| (&skipped.path, &skipped.reason))
    }
}

/// Result-set builders shared by the tests in this module and in both renderers.
///
/// Paths are fixed rather than temporary so renderer snapshots are stable across machines.
#[cfg(test)]
pub(crate) mod fixtures {
    use std::path::Path;

    use super::*;
    use crate::classify::ArtifactId;

    /// A fixture path literal, spelled the way the running platform spells an absolute path.
    ///
    /// The literals in these tests are written Unix-style because that is what the snapshots
    /// show. On Windows `Path::is_absolute` demands a drive prefix, so `/projects` is a
    /// *relative* path there — which is right, and which meant every fixture rooted at one
    /// exercised a different branch on each platform. Anything not starting with `/` is
    /// returned untouched, so a `~`-rooted config pattern keeps its own spelling.
    pub(crate) fn spell(path: &str) -> String {
        if cfg!(windows) && path.starts_with('/') {
            format!("C:{}", path.replace('/', "\\"))
        } else {
            path.to_owned()
        }
    }

    /// The inverse of [`spell`], applied to rendered output so one snapshot serves both
    /// platforms. A report carries no backslash of its own, so the replacement is unambiguous.
    pub(crate) fn unspell(text: &str) -> String {
        if cfg!(windows) {
            text.replace("C:\\", "/").replace('\\', "/")
        } else {
            text.to_owned()
        }
    }

    /// A removal failure of a given kind, built from a real `io::Error` so the fixtures carry
    /// the same classification the deleter would produce.
    pub(crate) fn failure(kind: std::io::ErrorKind) -> crate::delete::RemovalFailure {
        crate::delete::RemovalFailure::from_io(&std::io::Error::from(kind))
    }

    /// One entry naming a catalog declaration by its `<ecosystem>.<artifact>` spec.
    ///
    /// # Panics
    ///
    /// If the spec is not in the catalog, which means the test is wrong.
    pub(crate) fn entry(path: &str, spec: &str, bytes: u64, outcome: Outcome) -> Entry {
        let artifact = ArtifactId::from_spec(spec).expect("a catalog spec");
        let path = PathBuf::from(path);
        let marker_dir = path.parent().unwrap_or(Path::new("/")).to_path_buf();
        Entry {
            finding: Finding {
                path: path.clone(),
                provenance: crate::classify::Provenance::Anchored { artifact, marker_dir },
            },
            path,
            bytes,
            outcome,
        }
    }

    /// An entry for a path a configured `include` named, which no marker proved.
    pub(crate) fn included(path: &str, pattern: &str, bytes: u64, outcome: Outcome) -> Entry {
        let path = PathBuf::from(path);
        Entry {
            finding: Finding {
                path: path.clone(),
                provenance: crate::classify::Provenance::Included {
                    pattern: pattern.to_owned(),
                },
            },
            path,
            bytes,
            outcome,
        }
    }

    pub(crate) fn result(entries: Vec<Entry>, dry_run: bool) -> RunResult {
        let mut result = RunResult {
            roots: vec![PathBuf::from("/projects")],
            entries,
            skips: Vec::new(),
            skipped_count: 0,
            failures: Vec::new(),
            dry_run,
            elapsed: Duration::from_millis(1234),
        };
        result.sort();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{entry, result};
    use super::*;
    use crate::delete::Refusal;

    #[test]
    fn should_sort_entries_by_path_whatever_order_they_arrived_in() {
        let result = result(
            vec![
                entry("/projects/zeta/target", "rust.target", 1, Outcome::Removed),
                entry("/projects/alpha/target", "rust.target", 1, Outcome::Removed),
                entry("/projects/mid/dist", "node.dist", 1, Outcome::Removed),
            ],
            false,
        );
        let paths: Vec<_> = result
            .entries
            .iter()
            .map(|entry| entry.path.display().to_string())
            .collect();
        assert_eq!(
            paths,
            vec!["/projects/alpha/target", "/projects/mid/dist", "/projects/zeta/target"]
        );
    }

    #[test]
    fn should_total_the_bytes_a_run_actually_freed() {
        let result = result(
            vec![
                entry("/projects/a/target", "rust.target", 1_000, Outcome::Removed),
                entry("/projects/b/target", "rust.target", 2_000, Outcome::WouldRemove),
                entry(
                    "/projects/c/target",
                    "rust.target",
                    4_000,
                    Outcome::Refused(Refusal::Symlink),
                ),
                entry(
                    "/projects/d/target",
                    "rust.target",
                    8_000,
                    Outcome::Failed(fixtures::failure(std::io::ErrorKind::Other)),
                ),
                entry(
                    "/projects/e/target",
                    "rust.target",
                    16_000,
                    Outcome::PartiallyRemoved {
                        freed: 10_000,
                        remaining: 6_000,
                        failure: fixtures::failure(std::io::ErrorKind::DirectoryNotEmpty),
                    },
                ),
            ],
            false,
        );
        let totals = result.totals();
        assert_eq!(totals.reclaimed, 2, "only artifacts that are actually gone are counted");
        assert_eq!(
            totals.bytes, 13_000,
            "every byte the run freed, including the 10_000 from an artifact still on disk — \
             reporting that one as zero is what made a 130 GB sweep claim it reclaimed 25"
        );
        assert_eq!(totals.refused, 1);
        assert_eq!(totals.failed, 1, "a removal that freed nothing is a plain failure");
        assert_eq!(totals.partial, 1);
        assert_eq!(totals.partial_bytes, 10_000, "a documented subset of `bytes`");
    }

    /// A partial removal leaves the artifact on disk, so a re-run is warranted and the exit code
    /// has to say so — which is also what it did before the state had a name.
    #[test]
    fn should_exit_one_when_a_removal_was_only_partial() {
        let result = result(
            vec![entry(
                "/projects/a/target",
                "rust.target",
                1_000,
                Outcome::PartiallyRemoved {
                    freed: 900,
                    remaining: 100,
                    failure: fixtures::failure(std::io::ErrorKind::DirectoryNotEmpty),
                },
            )],
            false,
        );

        assert_eq!(result.exit_code(false), exit::REMOVAL_FAILED);
    }

    #[test]
    fn should_exit_zero_when_everything_succeeded() {
        let result = result(
            vec![entry("/projects/a/target", "rust.target", 1, Outcome::Removed)],
            false,
        );
        assert_eq!(result.exit_code(false), exit::SUCCESS);
    }

    #[test]
    fn should_exit_one_when_a_removal_failed() {
        let result = result(
            vec![entry(
                "/projects/a/target",
                "rust.target",
                1,
                Outcome::Failed(fixtures::failure(std::io::ErrorKind::Other)),
            )],
            false,
        );
        assert_eq!(result.exit_code(false), exit::REMOVAL_FAILED);
    }

    /// The contract the committed `voom-report` hook depends on: `--dry-run --exit-code` fails
    /// the commit when there is something to reclaim, and passes when there is not.
    #[test]
    fn should_exit_three_for_a_dry_run_with_findings_and_exit_code() {
        let found = result(
            vec![entry("/projects/a/target", "rust.target", 1, Outcome::WouldRemove)],
            true,
        );
        assert_eq!(found.exit_code(true), exit::FINDINGS);
        assert_eq!(
            found.exit_code(false),
            exit::SUCCESS,
            "without --exit-code a dry run is informational"
        );

        let clean = result(Vec::new(), true);
        assert_eq!(clean.exit_code(true), exit::SUCCESS);
    }

    #[test]
    fn a_refusal_is_not_a_failure() {
        let result = result(
            vec![entry(
                "/projects/a/target",
                "rust.target",
                1,
                Outcome::Refused(Refusal::Protected),
            )],
            false,
        );
        assert_eq!(
            result.exit_code(false),
            exit::SUCCESS,
            "a rail doing its job is not a run failure"
        );
    }
}
