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
pub const SCHEMA_VERSION: u32 = 1;

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
            match &entry.outcome {
                Outcome::Removed | Outcome::WouldRemove => {
                    totals.reclaimed += 1;
                    totals.bytes += entry.bytes;
                }
                Outcome::Refused(_) => totals.refused += 1,
                Outcome::Failed(_) => totals.failed += 1,
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
        if totals.failed > 0 {
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
    fn should_total_only_reclaimed_bytes() {
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
                    Outcome::Failed("boom".to_owned()),
                ),
            ],
            false,
        );
        let totals = result.totals();
        assert_eq!(totals.reclaimed, 2);
        assert_eq!(totals.bytes, 3_000, "refused and failed artifacts reclaim nothing");
        assert_eq!(totals.refused, 1);
        assert_eq!(totals.failed, 1);
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
                Outcome::Failed("no".to_owned()),
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
