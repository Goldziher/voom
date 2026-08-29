//! What the classifier concluded, and how it says so.
//!
//! These are the types the classifier produces and the reporter consumes: a [`Verdict`] per
//! candidate, a [`Finding`] describing an artifact it proved, and a [`SkipReason`] explaining
//! every candidate it passed over. The prose lives here rather than in the renderer because the
//! reason a candidate was skipped is the classifier's knowledge, not the report's — and because
//! `code()` is a stable JSON contract that must not drift from the sentence beside it.

use std::fmt;
use std::path::PathBuf;

use super::ArtifactId;

/// What the classifier concluded about one directory entry.
///
/// `#[non_exhaustive]` because a new verdict is a change this design expects to make, and after
/// 0.1.0 adding one to a bare enum would break every downstream `match` that has no wildcard.
/// [`Error`](crate::error::Error) and [`SkipReason`] carry it for the same reason.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Verdict {
    /// A marker proved it. This is the only verdict that can lead to a deletion.
    Artifact(Finding),
    /// It looked like an artifact but something held it back, with the reason.
    Skip(SkipReason),
    /// Its name matches nothing in the catalog. The common case, and the cheap one.
    NotACandidate,
}

/// What entitles voom to remove a path.
///
/// An enum rather than an optional [`ArtifactId`] so that the two cases cannot be confused at a
/// call site. Every reporter has to match on it, which means no output path can be added that
/// forgets to say a deletion was never proved by a marker.
///
/// `#[non_exhaustive]` because a third route to removal is plausible — ADR 0004 already
/// separates the two that exist — and adding one after 0.1.0 would otherwise be a breaking
/// change for every downstream matcher.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Provenance {
    /// A marker file proved the ecosystem at the declared anchor — ADR 0002's central rule, and
    /// the only route voom ever takes on its own.
    Anchored {
        /// Which catalog declaration proved it.
        artifact: ArtifactId,
        /// The directory whose marker did the proving, for `--verbose` and for JSON.
        marker_dir: PathBuf,
    },
    /// A configured `include` named it, so no marker was required.
    ///
    /// ADR 0004 makes this the only route to unanchored deletion and calls it a loaded foot-gun
    /// by design: the user has said what they mean and owns the result. Reports label it, which
    /// is why the pattern is carried here rather than discarded.
    Included {
        /// The `include` pattern that matched.
        pattern: String,
    },
    /// A named machine-global tool cache, proved by a marker the tool wrote inside it.
    ///
    /// ADR 0012's route. Distinct from `Anchored` because nothing in the ecosystem catalog is
    /// involved — the evidence is a location that names the tool plus its own declaration —
    /// and distinct from `Included` because the user named a *cache*, not a path, and voom
    /// still had to prove it before removing anything.
    Cache {
        /// The entry that claimed it.
        cache: &'static crate::caches::Cache,
    },
}

/// An artifact voom is entitled to remove.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// The path to remove.
    pub path: PathBuf,
    /// What entitles voom to remove it.
    pub provenance: Provenance,
}

impl Finding {
    /// The catalog declaration that proved it, if a marker did.
    #[must_use]
    pub fn artifact(&self) -> Option<ArtifactId> {
        match &self.provenance {
            Provenance::Anchored { artifact, .. } => Some(*artifact),
            Provenance::Cache { .. } | Provenance::Included { .. } => None,
        }
    }

    /// The ecosystem that claimed it, or the cache entry that did.
    ///
    /// A cache reports its own id (`cargo-registry`, `uv`) in the same column, because to a
    /// reader scanning the report the question is the same one — what kind of thing was this —
    /// and the answer is equally definite.
    #[must_use]
    pub fn ecosystem(&self) -> Option<&'static str> {
        match &self.provenance {
            Provenance::Anchored { artifact, .. } => Some(artifact.ecosystem().id),
            Provenance::Cache { cache } => Some(cache.id),
            Provenance::Included { .. } => None,
        }
    }
}

/// Why a candidate was not removed.
///
/// Variants are the machine-readable reason codes ADR 0007 puts in JSON, so they are defined
/// once here and rendered by both reporters rather than restated in either.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SkipReason {
    /// No marker proved the ecosystem — ADR 0002's central rule doing its job.
    NoMarker {
        /// The ecosystem that would have claimed it.
        ecosystem: &'static str,
        /// The markers that would have proved it.
        markers: &'static [&'static str],
    },
    /// Nothing inside the candidate proved it — the [`Inside`](crate::catalog::Anchor::Inside)
    /// counterpart of [`NoMarker`](SkipReason::NoMarker).
    ///
    /// Separate because the reader's next move is different: for `NoMarker` they look beside
    /// the directory for a project file, and here they look within it for the tool's own
    /// declaration. A tool that wants its cache swept writes one — `CACHEDIR.TAG` is the
    /// cross-tool spelling.
    NoInnerMarker {
        /// The ecosystem that would have claimed it.
        ecosystem: &'static str,
        /// The markers that would have proved it, looked for inside the candidate.
        markers: &'static [&'static str],
    },
    /// The artifact is off by default, or the ecosystem was not selected.
    NotEnabled {
        /// The `<ecosystem>.<artifact>` spec that would enable it.
        spec: String,
        /// Why it is off by default, when the catalog says.
        note: Option<&'static str>,
    },
    /// It is a machine-global tool cache or an installed toolchain (ADR 0001).
    ToolCache,
    /// A configured `exclude` matched.
    Excluded {
        /// The pattern that matched.
        pattern: String,
    },
    /// A keep policy held it.
    KeptByPolicy {
        /// Which rule — `min_age`, `min_size`, `max_size`.
        rule: &'static str,
        /// The comparison in human terms.
        detail: String,
    },
    /// It sits on a different filesystem than the scan root.
    FilesystemBoundary,
    /// It is a symlink, which is never followed and never deleted through.
    Symlink,
    /// Another artifact that is being removed already contains it.
    ///
    /// Only the opt-in dependency artifacts can produce this. The catalog declares build output
    /// *inside* two dependency directories (Ruby's `vendor/bundle/`, Julia's `deps/build/`), so
    /// enabling the outer directory as well makes the inner one redundant — and removing both
    /// would count its bytes twice and then refuse the second as `Vanished`.
    Covered {
        /// The artifact that contains it.
        by: PathBuf,
    },
    /// Its anchor directory could not be listed, so nothing could prove it.
    Unreadable,
}

impl SkipReason {
    /// The stable, enum-like code emitted in JSON so hooks can branch without parsing prose.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoMarker { .. } => "no_marker",
            Self::NoInnerMarker { .. } => "no_inner_marker",
            Self::NotEnabled { .. } => "not_enabled",
            Self::ToolCache => "tool_cache",
            Self::Excluded { .. } => "excluded",
            Self::KeptByPolicy { .. } => "kept_by_policy",
            Self::FilesystemBoundary => "filesystem_boundary",
            Self::Symlink => "symlink",
            Self::Covered { .. } => "covered",
            Self::Unreadable => "unreadable",
        }
    }
}

impl fmt::Display for SkipReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoMarker { ecosystem, markers } => {
                write!(f, "no {ecosystem} marker ({}) proves it", markers.join(", "))
            }
            Self::NoInnerMarker { ecosystem, markers } => {
                write!(f, "nothing inside it ({}) proves it is {ecosystem}", markers.join(", "))
            }
            // Deliberately not "off by default": this fires both for a catalog default and
            // for an artifact configuration turned off, and claiming the former when it was
            // the latter sends the user to the wrong file.
            Self::NotEnabled { spec, note } => match note {
                Some(note) => write!(f, "not enabled — {note} Enable with `{spec}`"),
                None => write!(f, "not enabled — enable with `{spec}`"),
            },
            Self::ToolCache => write!(f, "a tool cache, not build output — `--caches` sweeps these too"),
            Self::Excluded { pattern } => write!(f, "excluded by `{pattern}`"),
            Self::KeptByPolicy { rule, detail } => write!(f, "kept by {rule} ({detail})"),
            Self::FilesystemBoundary => write!(f, "on a different filesystem than the scan root"),
            Self::Symlink => write!(f, "is a symlink"),
            // The final component only: the covering artifact is always an ancestor of the path
            // this line already names, and the renderer prints that one relative to the scan
            // root — repeating the absolute path here would be the longest string on the line
            // and would say nothing the reader cannot see.
            Self::Covered { by } => {
                write!(
                    f,
                    "already inside {}",
                    by.file_name().unwrap_or(by.as_os_str()).display()
                )
            }
            Self::Unreadable => write!(f, "its anchor directory could not be read"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_reason_codes_are_stable() {
        assert_eq!(SkipReason::Symlink.code(), "symlink");
        assert_eq!(SkipReason::FilesystemBoundary.code(), "filesystem_boundary");
        assert_eq!(SkipReason::Unreadable.code(), "unreadable");
        assert_eq!(
            SkipReason::NoMarker {
                ecosystem: "rust",
                markers: &["Cargo.toml"]
            }
            .code(),
            "no_marker"
        );
    }
}
