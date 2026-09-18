//! Bazel output-base housekeeping — orphaned per-workspace scratch that nothing else reclaims.
//!
//! Every distinct workspace path that has run Bazel gets its own output base, and nothing
//! removes it when the workspace goes away: not `git worktree prune` (the output base is not
//! under `.git`), and not the [`caches`](crate::caches) table (that proves one fixed location,
//! not one of however many a user has ever pointed Bazel at). See
//! `adrs/0014-bazel-output-base-housekeeping.md`.
//!
//! Bazel already answers the question that matters — is the workspace this output base belongs
//! to still there — by writing `DO_NOT_BUILD_HERE` into the output base's root, containing the
//! absolute path of the owner. [`prune`] reads it, checks that one path, and removes only the
//! output bases whose owner no longer exists. Rendering lives in [`report`].

mod report;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub use report::{document, render_human, render_json};

use crate::delete::{Guard, Outcome, Removal};
use crate::error::Result;
use crate::size::measure_fully;

/// The JSON shape's version, versioned separately from the sweep's because it is a separate
/// document sharing only the envelope.
pub const SCHEMA_VERSION: u32 = 1;

/// The file Bazel writes into an output base's root, naming the workspace that owns it.
///
/// Read from the output base's own root first, and failing that from
/// `<output base>/execroot/DO_NOT_BUILD_HERE` — where a shared, sequentially-reused
/// output-user-root (one Docker-wrapping build tool measured for this ADR points every
/// workspace's build at the same output base in turn) puts it *instead of* the root location.
/// Not following that nesting is what excludes a shared root without a separate rule naming it:
/// it has no marker at the position this reads, only one a level further in.
const OWNER_MARKER: &str = "DO_NOT_BUILD_HERE";

/// What to search, and how.
#[derive(Debug, Clone)]
pub struct BazelPruneOptions {
    /// Output-user-roots to search. Each candidate is one of this root's immediate
    /// subdirectories — an output base is never nested any deeper than that.
    pub roots: Vec<PathBuf>,
    /// Withhold the removal and report what would happen.
    pub dry_run: bool,
    /// Retry a failed removal, repairing permissions inside the artifact first.
    pub force: bool,
    /// Whether removal stays on the root's filesystem.
    pub one_file_system: bool,
}

/// What the owner marker proved about one output base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputBaseState {
    /// No marker was readable at either location. The same rule as everywhere else in the
    /// catalog: no marker, no claim, nothing removed.
    Unproven,
    /// The marker names a workspace that still exists. Left alone and reported by the path it
    /// named — existence is not the same claim as membership, and a workspace nobody named to
    /// this invocation might still be somebody's.
    Owned {
        /// The path the marker recorded.
        owner: PathBuf,
    },
    /// The marker names a workspace that no longer exists anywhere on disk. The one state a
    /// marker Bazel wrote at build time can prove outright.
    Orphaned {
        /// The path the marker recorded.
        owner: PathBuf,
    },
}

/// One candidate output base and what became of it.
#[derive(Debug, Clone)]
pub struct OutputBase {
    /// The output base's own directory — `<output_user_root>/_bazel_<user>/<hash>`.
    pub path: PathBuf,
    /// What the marker proved.
    pub state: OutputBaseState,
    /// Its size, measured only for a state removal was attempted against.
    pub bytes: Option<u64>,
    /// What happened, for a state removal was attempted against.
    pub outcome: Option<Outcome>,
}

impl OutputBase {
    /// Whether this output base was found to be a removable orphan, whatever became of the
    /// removal attempt.
    #[must_use]
    pub fn is_orphaned(&self) -> bool {
        matches!(self.state, OutputBaseState::Orphaned { .. })
    }
}

/// The footer's numbers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    /// Output bases found across every root.
    pub found: usize,
    /// Orphaned, and actually removed.
    pub removed: usize,
    /// Orphaned, but a rail refused the removal.
    pub refused: usize,
    /// Not orphaned — owned, or unproven.
    pub kept: usize,
    /// Bytes reclaimed, or that would be.
    pub bytes: u64,
}

/// Everything one round of housekeeping produced.
#[derive(Debug)]
pub struct BazelPruneResult {
    /// The roots that were searched, canonicalized. A root that did not exist on this machine —
    /// which is the ordinary case for at least some of the conventional defaults — is silently
    /// absent rather than listed, since absence there is not a usage error.
    pub roots: Vec<PathBuf>,
    /// Output bases, sorted by path.
    pub output_bases: Vec<OutputBase>,
    /// Whether removal was withheld.
    pub dry_run: bool,
    /// Wall-clock time.
    pub elapsed: Duration,
}

impl BazelPruneResult {
    /// The footer's numbers.
    #[must_use]
    pub fn totals(&self) -> Totals {
        let mut totals = Totals {
            found: self.output_bases.len(),
            ..Totals::default()
        };
        for base in &self.output_bases {
            match (&base.state, &base.outcome) {
                (OutputBaseState::Orphaned { .. }, Some(Outcome::Removed | Outcome::WouldRemove)) => {
                    totals.removed += 1;
                    totals.bytes += base.bytes.unwrap_or(0);
                }
                (OutputBaseState::Orphaned { .. }, Some(Outcome::Refused(_))) => totals.refused += 1,
                (OutputBaseState::Orphaned { .. } | OutputBaseState::Owned { .. } | OutputBaseState::Unproven, _) => {
                    totals.kept += 1;
                }
            }
        }
        totals
    }

    /// The process exit code: zero unless a removal was attempted and failed outright.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        let failed = self.output_bases.iter().any(|base| {
            matches!(
                base.outcome,
                Some(Outcome::Failed(_) | Outcome::PartiallyRemoved { .. })
            )
        });
        if failed {
            crate::report::exit::REMOVAL_FAILED
        } else {
            crate::report::exit::SUCCESS
        }
    }
}

/// The conventional output-user-roots to search when none were named.
///
/// A guess informed by one machine and Bazel's documented default locations, not a survey —
/// which is exactly why this feature is an explicit subcommand and not a sweep default. A root
/// that does not exist here is the ordinary case, not a usage error — and [`prune`] treats a
/// root named explicitly the same way, since every one of these names a convention rather than
/// a tree the caller has confirmed exists.
#[must_use]
pub fn conventional_roots() -> Vec<PathBuf> {
    let Some(user) = current_user() else {
        return Vec::new();
    };
    let name = format!("_bazel_{user}");
    let mut roots = vec![PathBuf::from("/tmp").join(&name), PathBuf::from("/var/tmp").join(&name)];
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".cache/bazel").join(&name));
    }
    roots
}

fn current_user() -> Option<String> {
    std::env::var("USER").or_else(|_| std::env::var("USERNAME")).ok()
}

/// Searches every root for output bases and removes the orphaned ones.
///
/// A root that does not exist or cannot be listed is silently absent from the result rather
/// than an error — the caller has already decided whether an empty root list is a problem
/// worth stopping for; here, the ordinary case is a machine that keeps its output bases
/// somewhere other conventional locations do not name.
///
/// # Errors
///
/// [`crate::error::Error::ReadDir`] if a root that does exist cannot be resolved to a canonical
/// path — a permission failure, not an absence.
pub fn prune(options: &BazelPruneOptions) -> Result<BazelPruneResult> {
    let started = Instant::now();
    let mut roots = Vec::new();
    let mut candidates = Vec::new();

    for root in &options.roots {
        let Ok(canonical) = root.canonicalize() else {
            continue;
        };
        candidates.extend(immediate_subdirectories(&canonical));
        roots.push(canonical);
    }

    let guard = Guard::new(&roots, options.one_file_system)?;
    let removal = Removal {
        dry_run: options.dry_run,
        force: options.force,
    };

    let mut output_bases: Vec<OutputBase> = candidates
        .into_iter()
        .map(|path| {
            let state = state_of(&path);
            let (bytes, outcome) = if matches!(state, OutputBaseState::Orphaned { .. }) {
                let measured = measure_fully(&path);
                (Some(measured.bytes), Some(guard.remove(&path, measured.bytes, removal)))
            } else {
                (None, None)
            };
            OutputBase {
                path,
                state,
                bytes,
                outcome,
            }
        })
        .collect();

    output_bases.sort_by(|left, right| left.path.cmp(&right.path));

    Ok(BazelPruneResult {
        roots,
        output_bases,
        dry_run: options.dry_run,
        elapsed: started.elapsed(),
    })
}

/// One output-user-root's immediate subdirectories — the only place an output base can be.
fn immediate_subdirectories(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|file_type| file_type.is_dir()))
        .map(|entry| entry.path())
        .collect()
}

fn state_of(output_base: &Path) -> OutputBaseState {
    let Some(owner) = read_owner(output_base) else {
        return OutputBaseState::Unproven;
    };
    if owner.exists() {
        OutputBaseState::Owned { owner }
    } else {
        OutputBaseState::Orphaned { owner }
    }
}

/// Reads the owner marker, at its own root first and then nested under `execroot/` — see
/// [`OWNER_MARKER`] for why the second location is checked and not followed further.
fn read_owner(output_base: &Path) -> Option<PathBuf> {
    for candidate in [
        output_base.join(OWNER_MARKER),
        output_base.join("execroot").join(OWNER_MARKER),
    ] {
        if let Ok(contents) = std::fs::read_to_string(&candidate) {
            let trimmed = contents.trim();
            if !trimmed.is_empty() {
                return Some(PathBuf::from(trimmed));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::tree;

    fn options(root: &Path) -> BazelPruneOptions {
        BazelPruneOptions {
            roots: vec![root.to_path_buf()],
            dry_run: false,
            force: false,
            one_file_system: true,
        }
    }

    #[test]
    fn should_remove_an_output_base_whose_owner_no_longer_exists() {
        let fixture = tree(&[
            "_bazel_dev/abc123/execroot/armis/BUILD",
            "_bazel_dev/abc123/DO_NOT_BUILD_HERE",
        ]);
        let root = fixture.path().join("_bazel_dev");
        std::fs::write(root.join("abc123/DO_NOT_BUILD_HERE"), "/nonexistent/workspace").unwrap();

        let result = prune(&options(&root)).expect("the root resolves");

        assert_eq!(result.output_bases.len(), 1);
        let base = &result.output_bases[0];
        assert!(
            matches!(&base.state, OutputBaseState::Orphaned { owner } if owner == Path::new("/nonexistent/workspace"))
        );
        assert_eq!(base.outcome, Some(Outcome::Removed));
        assert!(!base.path.exists(), "the orphan is actually gone");
    }

    #[test]
    fn should_leave_an_owned_output_base_alone() {
        let fixture = tree(&["_bazel_dev/abc123/DO_NOT_BUILD_HERE", "workspace/WORKSPACE"]);
        let root = fixture.path().join("_bazel_dev");
        let owner = fixture.path().join("workspace");
        std::fs::write(root.join("abc123/DO_NOT_BUILD_HERE"), owner.display().to_string()).unwrap();

        let result = prune(&options(&root)).expect("the root resolves");

        let base = &result.output_bases[0];
        assert!(matches!(&base.state, OutputBaseState::Owned { .. }));
        assert!(
            base.outcome.is_none(),
            "nothing is even attempted against an owned base"
        );
        assert!(base.path.exists(), "an owned output base survives");
    }

    #[test]
    fn should_leave_an_unmarked_directory_unproven() {
        let fixture = tree(&["_bazel_dev/abc123/execroot/armis/BUILD"]);
        let root = fixture.path().join("_bazel_dev");

        let result = prune(&options(&root)).expect("the root resolves");

        assert_eq!(result.output_bases[0].state, OutputBaseState::Unproven);
    }

    /// The shared, sequentially-reused root this ADR measured: the marker sits under
    /// `execroot/` instead of at the output base's own root, which is read — but a root-level
    /// marker still wins when both exist, since it is the more specific claim.
    #[test]
    fn should_read_the_execroot_marker_only_when_no_root_marker_exists() {
        let fixture = tree(&["_bazel_dev/shared/execroot/DO_NOT_BUILD_HERE"]);
        let root = fixture.path().join("_bazel_dev");
        std::fs::write(
            root.join("shared/execroot/DO_NOT_BUILD_HERE"),
            "/nonexistent/last-builder",
        )
        .unwrap();

        let result = prune(&options(&root)).expect("the root resolves");

        assert!(matches!(
            &result.output_bases[0].state,
            OutputBaseState::Orphaned { owner } if owner == Path::new("/nonexistent/last-builder")
        ));
    }

    #[test]
    fn should_leave_a_missing_root_silently_absent() {
        let fixture = tree(&["keep.txt"]);
        let missing = fixture.path().join("does-not-exist");

        let result = prune(&options(&missing)).expect("a missing root is not an error");

        assert!(result.roots.is_empty());
        assert!(result.output_bases.is_empty());
    }

    #[test]
    fn should_respect_dry_run_and_leave_the_orphan_on_disk() {
        let fixture = tree(&["_bazel_dev/abc123/DO_NOT_BUILD_HERE"]);
        let root = fixture.path().join("_bazel_dev");
        std::fs::write(root.join("abc123/DO_NOT_BUILD_HERE"), "/nonexistent/workspace").unwrap();

        let mut dry = options(&root);
        dry.dry_run = true;
        let result = prune(&dry).expect("the root resolves");

        assert_eq!(result.output_bases[0].outcome, Some(Outcome::WouldRemove));
        assert!(root.join("abc123").exists(), "a dry run removes nothing");
    }
}
